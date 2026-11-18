//! Rust values that scripts can hold, index and call methods on.

use std::any::TypeId;
use std::borrow::Cow;
use std::cell::{Ref, RefCell, RefMut};
use std::marker::PhantomData;
use std::rc::Rc;

use rhue_vm::NativeFn;

use crate::context::ScriptError;
use crate::convert::{ConversionError, FromArg, FromValue, IntoValue};
use crate::native::{IntoNative, IntoNativeResult, WithContext, fn_signature, run_native};
use crate::{AnyUserData, Context, Table, Value};

/// A Rust type scripts can use. Usually derived:
///
/// ```
/// use rhue::{Engine, UserData};
///
/// #[derive(UserData, Clone)]
/// struct Player {
///     name: String,
///     hp: f64,
///     #[rhue(readonly)]
///     id: u32,
///     #[rhue(skip)]
///     secret: String,
/// }
///
/// let mut engine = Engine::new();
/// engine
///     .register_type::<Player>()
///     .method("heal", |p: &mut Player, amount: f64| p.hp += amount);
/// let p = Player { name: "Aalto".into(), hp: 50.0, id: 7, secret: String::new() };
/// engine.set_global("player", p);
/// engine.eval("player:heal(25) player.name ..= '!'").unwrap();
/// assert_eq!(engine.eval_as::<f64>("return player.hp").unwrap(), 75.0);
/// ```
///
/// The derive exposes every named field as a property that scripts can
/// read (`p.hp`) and write (`p.hp = 10`); the field's type must be
/// `Clone + IntoValue + FromValue`. `#[rhue(readonly)]` drops the setter,
/// `#[rhue(skip)]` hides the field, `#[rhue(name = "...")]` renames it, and
/// `#[rhue(name = "...")]` on the struct renames the type.
///
/// The derive rejects what it can't support, at compile time:
///
/// ```compile_fail
/// #[derive(rhue::UserData)]
/// struct Wrapper<T> { inner: T } // generic: implement UserData by hand
/// ```
///
/// ```compile_fail
/// #[derive(rhue::UserData)]
/// enum State { On, Off } // enums: implement UserData by hand
/// ```
///
/// ```compile_fail
/// #[derive(rhue::UserData, Clone)]
/// struct P { #[rhue(hidden)] x: f64 } // unknown option
/// ```
///
/// ```compile_fail
/// #[derive(rhue::UserData)]
/// struct P { handle: std::fs::File } // fields must be Clone + IntoValue + FromValue
/// ```
pub trait UserData: Sized + 'static {
    /// The name scripts see (`typeof(x)`, error messages).
    const NAME: &'static str;

    /// Adds fields and methods. Runs once per engine, the first time the
    /// type is used; [`Engine::register_type`](crate::Engine::register_type)
    /// can add more afterwards.
    fn register(_t: &mut TypeBuilder<'_, Self>) {}
}

/// The VM's type id for `T`, setting the type up on first use.
pub(crate) fn type_id_of<T: UserData>(vm: &mut rhue_vm::Vm) -> u32 {
    let (ty, new) = vm.userdata_type(TypeId::of::<T>(), T::NAME);
    if new {
        crate::type_env(vm).declare_class(T::NAME);
        T::register(&mut TypeBuilder {
            vm,
            ty,
            _t: PhantomData,
        });
    }
    ty
}

/// A typed handle to a userdata object. Clones share the object: changes
/// made through one (or by scripts) are seen by all.
pub struct Ud<T: UserData> {
    pub(crate) any: AnyUserData,
    pub(crate) cell: Rc<RefCell<T>>,
}

impl<T: UserData> Ud<T> {
    /// Panics if a method on this object is running (it holds a mutable
    /// borrow); use [`try_borrow`](Self::try_borrow) to avoid that.
    pub fn borrow(&self) -> Ref<'_, T> {
        self.cell.borrow()
    }

    /// Borrows the Rust value mutably (panics if it's already borrowed).
    pub fn borrow_mut(&self) -> RefMut<'_, T> {
        self.cell.borrow_mut()
    }

    /// Borrows the Rust value, or `None` if it's mutably borrowed.
    pub fn try_borrow(&self) -> Option<Ref<'_, T>> {
        self.cell.try_borrow().ok()
    }

    /// Borrows the Rust value mutably, or `None` if it's borrowed.
    pub fn try_borrow_mut(&self) -> Option<RefMut<'_, T>> {
        self.cell.try_borrow_mut().ok()
    }

    /// The same object, without its Rust type.
    pub fn as_any(&self) -> &AnyUserData {
        &self.any
    }
}

impl<T: UserData> Clone for Ud<T> {
    fn clone(&self) -> Self {
        Ud {
            any: self.any.clone(),
            cell: self.cell.clone(),
        }
    }
}

impl<T: UserData> PartialEq for Ud<T> {
    fn eq(&self, other: &Self) -> bool {
        self.any == other.any
    }
}

impl<T: UserData + std::fmt::Debug> std::fmt::Debug for Ud<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.cell.try_borrow() {
            Ok(v) => f.debug_tuple("Ud").field(&*v).finish(),
            Err(_) => write!(f, "Ud(<borrowed {}>)", T::NAME),
        }
    }
}

/// Moves the value into the VM as a new object.
impl<T: UserData> IntoValue for T {
    fn into_value(self, cx: &mut Context<'_>) -> Value {
        Value::UserData(cx.create_userdata(self).any)
    }

    fn luau_type() -> Cow<'static, str> {
        T::NAME.into()
    }

    fn declare_types(vm: &mut rhue_vm::Vm) {
        type_id_of::<T>(vm);
    }
}

/// The same object (no copy).
impl<T: UserData> IntoValue for Ud<T> {
    fn into_value(self, _: &mut Context<'_>) -> Value {
        Value::UserData(self.any)
    }

    fn luau_type() -> Cow<'static, str> {
        T::NAME.into()
    }

    fn declare_types(vm: &mut rhue_vm::Vm) {
        type_id_of::<T>(vm);
    }
}

impl<T: UserData> FromValue for Ud<T> {
    fn from_value(value: Value, _: &mut Context<'_>) -> Result<Self, ConversionError> {
        match &value {
            Value::UserData(u) => u
                .downcast()
                .ok_or_else(|| ConversionError::new(T::NAME, &value)),
            _ => Err(ConversionError::new(T::NAME, &value)),
        }
    }

    fn luau_type() -> Cow<'static, str> {
        T::NAME.into()
    }

    fn declare_types(vm: &mut rhue_vm::Vm) {
        type_id_of::<T>(vm);
    }
}

/// A copy of the object's current state. Take [`Ud<T>`] instead to change
/// the object itself.
impl<T: UserData + Clone> FromValue for T {
    fn from_value(value: Value, cx: &mut Context<'_>) -> Result<Self, ConversionError> {
        let ud = Ud::<T>::from_value(value, cx)?;
        ud.try_borrow()
            .map(|v| v.clone())
            .ok_or_else(|| ConversionError {
                expected: T::NAME.into(),
                got: format!("{} that is being modified", T::NAME),
            })
    }

    fn luau_type() -> Cow<'static, str> {
        T::NAME.into()
    }

    fn declare_types(vm: &mut rhue_vm::Vm) {
        type_id_of::<T>(vm);
    }
}

/// Adds methods, properties and static functions to userdata type `T`.
pub struct TypeBuilder<'a, T: UserData> {
    pub(crate) vm: &'a mut rhue_vm::Vm,
    pub(crate) ty: u32,
    pub(crate) _t: PhantomData<T>,
}

/// The receiver of a method or property call: argument #1 must be a `T`.
fn receiver<T: UserData>(
    cx: &mut Context<'_>,
    args: &[rhue_vm::Value],
    what: &str,
) -> Result<Rc<RefCell<T>>, ScriptError> {
    let first = args.first().copied().unwrap_or(rhue_vm::Value::Nil);
    if let rhue_vm::Value::UserData(u) = first
        && let Ok(cell) = cx
            .vm
            .heap()
            .userdata(u)
            .data
            .clone()
            .downcast::<RefCell<T>>()
    {
        return Ok(cell);
    }
    let got = cx.lift(first);
    Err(ScriptError::new(format!(
        "invalid argument #1 to '{what}' ({} expected, got {})",
        T::NAME,
        got.type_of()
    )))
}

fn busy<T: UserData>() -> ScriptError {
    ScriptError::new(format!(
        "{} is already in use (a method on it is still running)",
        T::NAME
    ))
}

impl<T: UserData> TypeBuilder<'_, T> {
    /// `obj:name(...)`. The closure takes `&mut T` first, then any
    /// arguments, like a registered function; it may also take
    /// `&mut Context` before `&mut T`.
    pub fn method<M, F: IntoMethod<T, M>>(&mut self, name: &str, f: F) -> &mut Self {
        F::declare_types(self.vm);
        let sig = F::signature();
        declare(
            crate::type_env(self.vm).declare_method(T::NAME, name, &sig),
            &sig,
        );
        let native = f.into_method(Rc::from(name));
        let v = self.vm.native(name, native);
        self.vm.add_method(self.ty, name, v);
        self
    }

    /// A read-only property: `obj.name` returns `get(&obj)`.
    pub fn getter<V: IntoValue>(
        &mut self,
        name: &str,
        get: impl Fn(&T) -> V + 'static,
    ) -> &mut Self {
        V::declare_types(self.vm);
        let ty = V::luau_type();
        declare(
            crate::type_env(self.vm).declare_property(T::NAME, name, &ty, false),
            &ty,
        );
        let field: Rc<str> = Rc::from(name);
        let native: NativeFn = Rc::new(move |ctx, args| {
            run_native(ctx, &args[..1], 1, |cx, _, out| {
                let this = receiver::<T>(cx, &args[..1], &field)?;
                let v = get(&*this.try_borrow().map_err(|_| busy::<T>())?);
                crate::convert::IntoValues::lower_into(v, cx, out);
                Ok(())
            })
        });
        self.vm.add_getter(self.ty, name, native);
        self
    }

    /// Makes a property writable: `obj.name = v` calls `set(&mut obj, v)`.
    pub fn setter<V: FromValue>(
        &mut self,
        name: &str,
        set: impl Fn(&mut T, V) + 'static,
    ) -> &mut Self {
        V::declare_types(self.vm);
        let ty = V::luau_type();
        declare(
            crate::type_env(self.vm).declare_property(T::NAME, name, &ty, true),
            &ty,
        );
        let field: Rc<str> = Rc::from(name);
        let native: NativeFn = Rc::new(move |ctx, args| {
            run_native(ctx, args, 1, |cx, args_in, _| {
                let this = receiver::<T>(cx, args, &field)?;
                // Skip the receiver; the value is argument #2.
                let _ = Value::from_arg(args_in, cx);
                let v = V::from_arg(args_in, cx).map_err(|e| {
                    ScriptError::new(format!(
                        "invalid value for '{field}' of {} ({})",
                        T::NAME,
                        e.error
                    ))
                })?;
                set(&mut *this.try_borrow_mut().map_err(|_| busy::<T>())?, v);
                Ok(())
            })
        });
        self.vm.add_setter(self.ty, name, native);
        self
    }

    /// A readable and writable property backed by a field.
    pub fn field<V: IntoValue + FromValue>(
        &mut self,
        name: &str,
        get: impl Fn(&T) -> V + 'static,
        set: impl Fn(&mut T, V) + 'static,
    ) -> &mut Self {
        self.getter(name, get).setter(name, set)
    }

    /// A function in the global table named after the type, e.g. a
    /// constructor `Player.new(...)`.
    pub fn function<M, F: IntoNative<M>>(&mut self, name: &str, f: F) -> &mut Self {
        F::declare_types(self.vm);
        let sig = F::signature();
        declare(
            crate::type_env(self.vm).declare_global_field(T::NAME, name, &sig),
            &sig,
        );
        let native = f.into_native(Rc::from(format!("{}.{name}", T::NAME)));
        let func = self.vm.native(name, native);
        let mut cx = Context::new(self.vm);
        let table: Table = match cx.global::<Option<Table>>(T::NAME) {
            Ok(Some(t)) => t,
            _ => {
                let t = cx.new_table();
                cx.set_global(T::NAME, t.clone());
                t
            }
        };
        let func = cx.lift(func);
        cx.set(&table, name, func)
            .expect("a string key is always valid");
        self
    }
}

/// A Rust function usable as a method of `T`: `Fn(&mut T, args...)`, or
/// `Fn(&mut Context, &mut T, args...)`. See [`TypeBuilder::method`].
pub trait IntoMethod<T, Marker> {
    /// Wraps the function as a method named `name` (the name appears in argument errors).
    fn into_method(self, name: Rc<str>) -> NativeFn;

    /// The method's Luau type, with the object as its first parameter.
    fn signature() -> String
    where
        Self: Sized;

    #[doc(hidden)]
    fn declare_types(_vm: &mut rhue_vm::Vm)
    where
        Self: Sized,
    {
    }
}

/// A host declaration that doesn't parse is a bug in a `luau_type`
/// implementation; it shouldn't stop the engine, so it's skipped (the item
/// is then simply unknown to the checker).
pub(crate) fn declare(result: Result<(), Vec<rhue_common::Diagnostic>>, ty: &str) {
    if let Err(e) = result {
        debug_assert!(false, "bad host type `{ty}`: {}", e[0].message);
    }
}

macro_rules! methods {
    ($($A:ident),*) => {
        impl<T, Func, Ret, $($A,)*> IntoMethod<T, (Ret, $($A,)*)> for Func
        where
            T: UserData,
            Func: for<'t> Fn(&'t mut T, $($A),*) -> Ret + 'static,
            Ret: IntoNativeResult,
            $($A: FromArg,)*
        {
            #[allow(non_snake_case, unused_variables, unused_mut)]
            fn into_method(self, name: Rc<str>) -> NativeFn {
                Rc::new(move |ctx, args| {
                    let rest = args.get(1..).unwrap_or(&[]);
                    let receiver_arg = &args[..args.len().min(1)];
                    run_native(ctx, rest, 2, |cx, it, out| {
                        let this = receiver::<T>(cx, receiver_arg, &name)?;
                        $(let $A = $A::from_arg(it, cx)
                            .map_err(|e| ScriptError::argument(&name, e))?;)*
                        let ret = {
                            let mut guard = this.try_borrow_mut().map_err(|_| busy::<T>())?;
                            (self)(&mut guard, $($A),*)
                        };
                        ret.lower_result(cx, out)
                    })
                })
            }

            fn signature() -> String {
                fn_signature(&[T::NAME.to_string(), $($A::param_type()),*], &Ret::result_type())
            }

            #[allow(unused_variables)]
            fn declare_types(vm: &mut rhue_vm::Vm) {
                $($A::declare_types(vm);)*
                Ret::declare_types(vm);
            }
        }

        impl<T, Func, Ret, $($A,)*> IntoMethod<T, (WithContext, Ret, $($A,)*)> for Func
        where
            T: UserData,
            Func: for<'a, 'b, 't> Fn(&'a mut Context<'b>, &'t mut T, $($A),*) -> Ret + 'static,
            Ret: IntoNativeResult,
            $($A: FromArg,)*
        {
            #[allow(non_snake_case, unused_variables, unused_mut)]
            fn into_method(self, name: Rc<str>) -> NativeFn {
                Rc::new(move |ctx, args| {
                    let rest = args.get(1..).unwrap_or(&[]);
                    let receiver_arg = &args[..args.len().min(1)];
                    run_native(ctx, rest, 2, |cx, it, out| {
                        let this = receiver::<T>(cx, receiver_arg, &name)?;
                        $(let $A = $A::from_arg(it, cx)
                            .map_err(|e| ScriptError::argument(&name, e))?;)*
                        let ret = {
                            let mut guard = this.try_borrow_mut().map_err(|_| busy::<T>())?;
                            (self)(cx, &mut guard, $($A),*)
                        };
                        ret.lower_result(cx, out)
                    })
                })
            }

            fn signature() -> String {
                fn_signature(&[T::NAME.to_string(), $($A::param_type()),*], &Ret::result_type())
            }

            #[allow(unused_variables)]
            fn declare_types(vm: &mut rhue_vm::Vm) {
                $($A::declare_types(vm);)*
                Ret::declare_types(vm);
            }
        }
    };
}
methods!();
methods!(A);
methods!(A, B);
methods!(A, B, C);
methods!(A, B, C, D);
methods!(A, B, C, D, E);
methods!(A, B, C, D, E, F);
methods!(A, B, C, D, E, F, G);
