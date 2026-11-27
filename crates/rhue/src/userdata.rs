//! Rust values that scripts can hold, index and call methods on.

use std::any::TypeId;
use std::borrow::Cow;
use std::cell::{Ref, RefCell, RefMut};
use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;

use rhue_vm::NativeFn;

use crate::context::ScriptError;
use crate::convert::{Args, ConversionError, FromArg, FromValue, IntoValue};
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
/// The derive exposes every named field as a property scripts can read
/// (`p.hp`) and write (`p.hp = 10`), and the field's type has to be
/// `Clone + IntoValue + FromValue`. `#[rhue(readonly)]` drops the setter,
/// `#[rhue(skip)]` hides the field, `#[rhue(name = "...")]` renames it, and
/// `#[rhue(name = "...")]` on the struct renames the type.
///
/// The derive refuses what it can't handle, at compile time:
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

    /// Adds fields and methods. Runs once per engine, the first time the type
    /// gets used, and [`Engine::register_type`](crate::Engine::register_type)
    /// can add more after.
    fn register(_t: &mut TypeBuilder<'_, Self>) {}
}

/// The VM's type id for `T`, setting the type up on first use.
pub(crate) fn type_id_of<T: UserData>(vm: &mut rhue_vm::Vm) -> u32 {
    let (ty, new) = vm.userdata_type(TypeId::of::<T>(), T::NAME);
    if new {
        crate::type_env(vm).declare_class(T::NAME, None);
        T::register(&mut TypeBuilder {
            vm,
            ty,
            _t: PhantomData,
        });
    }
    ty
}

/// A typed handle to a userdata object. Clones share the object, so changes
/// made through one (or by scripts) show up in all of them.
pub struct Ud<T: UserData> {
    pub(crate) any: AnyUserData,
    pub(crate) cell: Rc<RefCell<T>>,
}

impl<T: UserData> Ud<T> {
    /// Panics if a method on this object is running (it's holding a mutable
    /// borrow). Use [`try_borrow`](Self::try_borrow) to dodge that.
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

/// A copy of the object's current state. Take [`Ud<T>`] instead to change the
/// object itself.
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

/// The receiver of a method or property call, argument #1 has to be a `T`.
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
    /// `obj:name(...)`. The closure takes `&mut T` first, then any arguments,
    /// like a registered function. It can also take `&mut Context` before `&mut
    /// T`.
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
                // skip the receiver, the value is argument #2
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

    /// A function in the global table named after the type, like a constructor
    /// `Player.new(...)`.
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
    /// Wraps the function as a method named `name` (the name shows up in
    /// argument errors).
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

/// A host declaration that doesn't parse is a bug in some `luau_type`
/// implementation. It shouldn't take the engine down, so it gets skipped (the
/// item is just unknown to the checker then).
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

// ─ Handle Classes ────────────────────────────────────────────────────────────

/// A class of handle objects, made with [`Engine::define_class`]: a host type
/// `H` that stands behind a whole tree of classes (a `Box` that's a `Shape`
/// that's a `Node`). Cheap to copy, and only good in the engine that made it.
///
/// [`Engine::define_class`]: crate::Engine::define_class
pub struct Class<H> {
    pub(crate) id: u32,
    pub(crate) vm: u64,
    pub(crate) name: Rc<str>,
    _h: PhantomData<fn() -> H>,
}

impl<H> Clone for Class<H> {
    fn clone(&self) -> Self {
        Class {
            id: self.id,
            vm: self.vm,
            name: self.name.clone(),
            _h: PhantomData,
        }
    }
}

impl<H> fmt::Debug for Class<H> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Class({})", self.name)
    }
}

impl<H> Class<H> {
    /// The class's name, what `tostring` shows for its objects.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Adds members to a handle class (see [`Engine::define_class`]). Members get
/// the object as `&H`: there's no `RefCell`, so a member that calls back into
/// a script and gets the same object back just works. The handle does its own
/// locking, if it needs any.
///
/// [`Engine::define_class`]: crate::Engine::define_class
pub struct ClassBuilder<'a, H: 'static> {
    pub(crate) vm: &'a mut rhue_vm::Vm,
    pub(crate) name: Rc<str>,
    pub(crate) parent: Option<(u32, Rc<str>)>,
    pub(crate) id: Option<u32>,
    pub(crate) _h: PhantomData<fn() -> H>,
}

impl<H: 'static> ClassBuilder<'_, H> {
    /// Makes it a child of `parent`: its objects go wherever the parent's do,
    /// and have its members (ones the parent gets later too). Set it before
    /// adding members.
    ///
    /// # Panics
    ///
    /// If members were added already, or `parent` is from another engine.
    pub fn parent(&mut self, parent: &Class<H>) -> &mut Self {
        assert!(self.id.is_none(), "set the parent before adding members");
        assert_eq!(
            parent.vm,
            self.vm.id(),
            "a class from one Engine was used in another"
        );
        self.parent = Some((parent.id, parent.name.clone()));
        self
    }

    fn id(&mut self) -> u32 {
        if let Some(id) = self.id {
            return id;
        }
        let id = self
            .vm
            .define_class(
                &self.name,
                self.parent.as_ref().map(|p| p.0),
                TypeId::of::<H>(),
            )
            .expect("a parent holding the same H, the type parameter makes sure of that");
        crate::type_env(self.vm).declare_class(&self.name, self.parent.as_ref().map(|p| &*p.1));
        self.id = Some(id);
        id
    }

    /// Declares a `_dyn` member to the checker, under the class's name, and
    /// reports a type string it couldn't resolve.
    fn declare_dyn(
        &mut self,
        result: impl FnOnce(&mut rhue_types::Env, &str) -> Result<(), Vec<rhue_common::Diagnostic>>,
        ty: &str,
    ) -> Result<(), crate::Error> {
        let class = self.name.clone();
        result(crate::type_env(self.vm), &class).map_err(|d| {
            crate::Error::new(
                crate::ErrorKind::Syntax,
                d,
                &rhue_common::SourceFile::new("<declaration>", ty),
            )
        })
    }

    fn install_getter(
        &mut self,
        name: &str,
        get: impl Fn(&mut Context<'_>, &H, &mut rhue_vm::Rets) -> Result<(), ScriptError> + 'static,
    ) {
        let ty = self.id();
        let field: Rc<str> = Rc::from(name);
        let class = self.name.clone();
        let native: NativeFn = Rc::new(move |ctx, args| {
            run_native(ctx, &args[..1], 1, |cx, _, out| {
                let this = handle_receiver::<H>(cx, &args[..1], &field, &class)?;
                get(cx, &this, out)
            })
        });
        self.vm.add_getter(ty, name, native);
    }

    /// `set` gets the arguments with the receiver skipped, so the value is
    /// next, and the property's and class's names for errors.
    fn install_setter(
        &mut self,
        name: &str,
        set: impl Fn(&mut Context<'_>, &H, &mut Args, &str, &str) -> Result<(), ScriptError> + 'static,
    ) {
        let ty = self.id();
        let field: Rc<str> = Rc::from(name);
        let class = self.name.clone();
        let native: NativeFn = Rc::new(move |ctx, args| {
            run_native(ctx, args, 1, |cx, args_in, _| {
                let this = handle_receiver::<H>(cx, args, &field, &class)?;
                // skip the receiver, the value is argument #2
                let _ = Value::from_arg(args_in, cx);
                set(cx, &this, args_in, &field, &class)
            })
        });
        self.vm.add_setter(ty, name, native);
    }

    /// A read-only property: `obj.name` returns `get(&obj)`.
    pub fn getter<V: IntoValue>(
        &mut self,
        name: &str,
        get: impl Fn(&H) -> V + 'static,
    ) -> &mut Self {
        self.getter_with(name, move |_, h| Ok(get(h)))
    }

    /// A read-only property whose getter takes the [`Context`] and can fail.
    /// An `Err` raises in the script where it read the property.
    pub fn getter_with<V: IntoValue>(
        &mut self,
        name: &str,
        get: impl Fn(&mut Context<'_>, &H) -> Result<V, ScriptError> + 'static,
    ) -> &mut Self {
        self.id();
        V::declare_types(self.vm);
        let luau = V::luau_type();
        declare(
            crate::type_env(self.vm).declare_property(&self.name, name, &luau, false),
            &luau,
        );
        self.install_getter(name, move |cx, h, out| {
            let v = get(cx, h)?;
            crate::convert::IntoValues::lower_into(v, cx, out);
            Ok(())
        });
        self
    }

    /// Makes a property writable: `obj.name = v` calls `set(&obj, v)`.
    pub fn setter<V: FromValue>(&mut self, name: &str, set: impl Fn(&H, V) + 'static) -> &mut Self {
        self.setter_with(name, move |_, h, v| {
            set(h, v);
            Ok(())
        })
    }

    /// Makes a property writable, with a setter that takes the [`Context`]
    /// and can fail, so it can refuse a value or call back into scripts (fire
    /// a changed signal, say). An `Err` raises in the script where it wrote
    /// the property.
    pub fn setter_with<V: FromValue>(
        &mut self,
        name: &str,
        set: impl Fn(&mut Context<'_>, &H, V) -> Result<(), ScriptError> + 'static,
    ) -> &mut Self {
        self.id();
        V::declare_types(self.vm);
        let luau = V::luau_type();
        declare(
            crate::type_env(self.vm).declare_property(&self.name, name, &luau, true),
            &luau,
        );
        self.install_setter(name, move |cx, h, args, field, class| {
            let v = V::from_arg(args, cx).map_err(|e| {
                ScriptError::new(format!(
                    "invalid value for '{field}' of {class} ({})",
                    e.error
                ))
            })?;
            set(cx, h, v)
        });
        self
    }

    /// A readable and writable property.
    pub fn property<V: IntoValue + FromValue>(
        &mut self,
        name: &str,
        get: impl Fn(&H) -> V + 'static,
        set: impl Fn(&H, V) + 'static,
    ) -> &mut Self {
        self.getter(name, get).setter(name, set)
    }

    /// A read-only property with its script type as a string, for a host
    /// that registers classes from its own reflection data instead of a
    /// closure per Rust type. The getter returns any [`Value`], and nothing
    /// checks it against `ty` at run time.
    ///
    /// # Errors
    ///
    /// If `ty` doesn't parse. A name the checker hasn't seen becomes a class,
    /// so classes can name each other in any order.
    pub fn getter_dyn(
        &mut self,
        name: &str,
        ty: &str,
        get: impl Fn(&mut Context<'_>, &H) -> Result<Value, ScriptError> + 'static,
    ) -> Result<&mut Self, crate::Error> {
        self.id();
        self.declare_dyn(
            |env, class| env.declare_property(class, name, ty, false),
            ty,
        )?;
        self.install_getter(name, move |cx, h, out| {
            let v = get(cx, h)?;
            out.push(cx.lower(v));
            Ok(())
        });
        Ok(self)
    }

    /// Makes a property writable, with its script type as a string (see
    /// [`getter_dyn`](Self::getter_dyn)). The setter gets whatever the script
    /// wrote, so a host that wants it to match `ty` checks it there.
    ///
    /// # Errors
    ///
    /// If `ty` doesn't parse. A name the checker hasn't seen becomes a class,
    /// so classes can name each other in any order.
    pub fn setter_dyn(
        &mut self,
        name: &str,
        ty: &str,
        set: impl Fn(&mut Context<'_>, &H, Value) -> Result<(), ScriptError> + 'static,
    ) -> Result<&mut Self, crate::Error> {
        self.id();
        self.declare_dyn(|env, class| env.declare_property(class, name, ty, true), ty)?;
        self.install_setter(name, move |cx, h, args, _, _| {
            let v = Value::from_arg(args, cx).unwrap_or(Value::Nil);
            set(cx, h, v)
        });
        Ok(self)
    }

    /// [`getter_dyn`](Self::getter_dyn) and [`setter_dyn`](Self::setter_dyn)
    /// in one.
    ///
    /// # Errors
    ///
    /// If `ty` doesn't parse. A name the checker hasn't seen becomes a class,
    /// so classes can name each other in any order.
    pub fn property_dyn(
        &mut self,
        name: &str,
        ty: &str,
        get: impl Fn(&mut Context<'_>, &H) -> Result<Value, ScriptError> + 'static,
        set: impl Fn(&mut Context<'_>, &H, Value) -> Result<(), ScriptError> + 'static,
    ) -> Result<&mut Self, crate::Error> {
        self.getter_dyn(name, ty, get)?.setter_dyn(name, ty, set)
    }

    /// `obj:name(...)` with its script type as a string, the object first:
    /// `"(Node, string, boolean?) -> Node?"`. `call` gets the arguments after
    /// the object, as the script passed them, and returns the results.
    ///
    /// # Errors
    ///
    /// If `sig` doesn't parse. A name the checker hasn't seen becomes a class,
    /// so classes can name each other in any order.
    pub fn method_dyn(
        &mut self,
        name: &str,
        sig: &str,
        call: impl Fn(&mut Context<'_>, &H, Vec<Value>) -> Result<Vec<Value>, ScriptError> + 'static,
    ) -> Result<&mut Self, crate::Error> {
        let ty = self.id();
        self.declare_dyn(|env, class| env.declare_method(class, name, sig), sig)?;
        let method: Rc<str> = Rc::from(name);
        let class = self.name.clone();
        let native: NativeFn = Rc::new(move |ctx, args| {
            let receiver_arg = &args[..args.len().min(1)];
            run_native(ctx, receiver_arg, 1, |cx, _, out| {
                let this = handle_receiver::<H>(cx, receiver_arg, &method, &class)?;
                let given = args.iter().skip(1).map(|&v| cx.lift(v)).collect();
                for v in call(cx, &this, given)? {
                    let v = cx.lower(v);
                    out.push(v);
                }
                Ok(())
            })
        });
        let v = self.vm.native(name, native);
        self.vm.add_method(ty, name, v);
        Ok(self)
    }

    /// `obj:name(...)`. The closure takes `&H` first, then any arguments, and
    /// can take `&mut Context` before `&H`.
    pub fn method<M, F: IntoHandleMethod<H, M>>(&mut self, name: &str, f: F) -> &mut Self {
        let ty = self.id();
        F::declare_types(self.vm);
        let sig = F::signature(&self.name);
        declare(
            crate::type_env(self.vm).declare_method(&self.name, name, &sig),
            &sig,
        );
        let native = f.into_method(Rc::from(name), self.name.clone());
        let v = self.vm.native(name, native);
        self.vm.add_method(ty, name, v);
        self
    }

    /// The class, to make objects of ([`Context::create_object`]) and to be
    /// the parent of other classes.
    ///
    /// [`Context::create_object`]: crate::Context::create_object
    pub fn finish(&mut self) -> Class<H> {
        let id = self.id();
        Class {
            id,
            vm: self.vm.id(),
            name: self.name.clone(),
            _h: PhantomData,
        }
    }
}

/// The receiver of a handle class member, argument #1 has to hold an `H`.
fn handle_receiver<H: 'static>(
    cx: &mut Context<'_>,
    args: &[rhue_vm::Value],
    what: &str,
    class: &str,
) -> Result<Rc<H>, ScriptError> {
    let first = args.first().copied().unwrap_or(rhue_vm::Value::Nil);
    if let rhue_vm::Value::UserData(u) = first
        && let Ok(h) = cx.vm.heap().userdata(u).data.clone().downcast::<H>()
    {
        return Ok(h);
    }
    let got = cx.lift(first);
    Err(ScriptError::new(format!(
        "invalid argument #1 to '{what}' ({class} expected, got {})",
        got.type_of()
    )))
}

/// A Rust function usable as a method of a handle class: `Fn(&H, args...)` or
/// `Fn(&mut Context, &H, args...)`. See [`ClassBuilder::method`].
pub trait IntoHandleMethod<H, Marker> {
    /// Wraps the function as a method named `name` of class `class`.
    fn into_method(self, name: Rc<str>, class: Rc<str>) -> NativeFn;

    /// The method's Luau type, with the object (of class `class`) as its first
    /// parameter.
    fn signature(class: &str) -> String
    where
        Self: Sized;

    #[doc(hidden)]
    fn declare_types(_vm: &mut rhue_vm::Vm)
    where
        Self: Sized,
    {
    }
}

macro_rules! handle_methods {
    ($($A:ident),*) => {
        impl<H, Func, Ret, $($A,)*> IntoHandleMethod<H, (Ret, $($A,)*)> for Func
        where
            H: 'static,
            Func: for<'h> Fn(&'h H, $($A),*) -> Ret + 'static,
            Ret: IntoNativeResult,
            $($A: FromArg,)*
        {
            #[allow(non_snake_case, unused_variables, unused_mut)]
            fn into_method(self, name: Rc<str>, class: Rc<str>) -> NativeFn {
                Rc::new(move |ctx, args| {
                    let rest = args.get(1..).unwrap_or(&[]);
                    let receiver_arg = &args[..args.len().min(1)];
                    run_native(ctx, rest, 2, |cx, it, out| {
                        let this = handle_receiver::<H>(cx, receiver_arg, &name, &class)?;
                        $(let $A = $A::from_arg(it, cx)
                            .map_err(|e| ScriptError::argument(&name, e))?;)*
                        (self)(&this, $($A),*).lower_result(cx, out)
                    })
                })
            }

            fn signature(class: &str) -> String {
                fn_signature(&[class.to_string(), $($A::param_type()),*], &Ret::result_type())
            }

            #[allow(unused_variables)]
            fn declare_types(vm: &mut rhue_vm::Vm) {
                $($A::declare_types(vm);)*
                Ret::declare_types(vm);
            }
        }

        impl<H, Func, Ret, $($A,)*> IntoHandleMethod<H, (WithContext, Ret, $($A,)*)> for Func
        where
            H: 'static,
            Func: for<'a, 'b, 'h> Fn(&'a mut Context<'b>, &'h H, $($A),*) -> Ret + 'static,
            Ret: IntoNativeResult,
            $($A: FromArg,)*
        {
            #[allow(non_snake_case, unused_variables, unused_mut)]
            fn into_method(self, name: Rc<str>, class: Rc<str>) -> NativeFn {
                Rc::new(move |ctx, args| {
                    let rest = args.get(1..).unwrap_or(&[]);
                    let receiver_arg = &args[..args.len().min(1)];
                    run_native(ctx, rest, 2, |cx, it, out| {
                        let this = handle_receiver::<H>(cx, receiver_arg, &name, &class)?;
                        $(let $A = $A::from_arg(it, cx)
                            .map_err(|e| ScriptError::argument(&name, e))?;)*
                        (self)(cx, &this, $($A),*).lower_result(cx, out)
                    })
                })
            }

            fn signature(class: &str) -> String {
                fn_signature(&[class.to_string(), $($A::param_type()),*], &Ret::result_type())
            }

            #[allow(unused_variables)]
            fn declare_types(vm: &mut rhue_vm::Vm) {
                $($A::declare_types(vm);)*
                Ret::declare_types(vm);
            }
        }
    };
}
handle_methods!();
handle_methods!(A);
handle_methods!(A, B);
handle_methods!(A, B, C);
handle_methods!(A, B, C, D);
handle_methods!(A, B, C, D, E);
handle_methods!(A, B, C, D, E, F);
handle_methods!(A, B, C, D, E, F, G);
