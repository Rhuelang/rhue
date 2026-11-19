//! Turning Rust closures into script functions.

use std::rc::Rc;

use rhue_vm::{NativeCtx, NativeError, NativeFn};

use crate::context::ScriptError;
use crate::convert::{Args, FromArg, IntoValues};
use crate::{Context, Value};

/// What a native can return: any [`IntoValues`] (a value, `()`, a tuple, a
/// [`Variadic`](crate::Variadic)), or a `Result` of one whose error converts
/// into [`ScriptError`] (`String` and `&str` do).
pub trait IntoNativeResult {
    /// Converts a function's return value into its results, or an error to
    /// raise in the script.
    fn into_native_result(self, cx: &mut Context<'_>) -> Result<Vec<Value>, ScriptError>;

    /// Like `into_native_result`, but appends VM values to `out`.
    #[doc(hidden)]
    fn lower_result(self, cx: &mut Context<'_>, out: &mut rhue_vm::Rets) -> Result<(), ScriptError>
    where
        Self: Sized,
    {
        for v in self.into_native_result(cx)? {
            let v = cx.lower(v);
            out.push(v);
        }
        Ok(())
    }

    /// The results' Luau type (see [`IntoValues::pack_type`]).
    fn result_type() -> String
    where
        Self: Sized;

    #[doc(hidden)]
    fn declare_types(_vm: &mut rhue_vm::Vm)
    where
        Self: Sized,
    {
    }
}

impl<T: IntoValues> IntoNativeResult for T {
    fn into_native_result(self, cx: &mut Context<'_>) -> Result<Vec<Value>, ScriptError> {
        Ok(self.into_values(cx))
    }

    fn lower_result(
        self,
        cx: &mut Context<'_>,
        out: &mut rhue_vm::Rets,
    ) -> Result<(), ScriptError> {
        self.lower_into(cx, out);
        Ok(())
    }

    fn result_type() -> String {
        T::pack_type()
    }

    fn declare_types(vm: &mut rhue_vm::Vm) {
        T::declare_types(vm)
    }
}

impl<T: IntoValues, E: Into<ScriptError>> IntoNativeResult for Result<T, E> {
    fn into_native_result(self, cx: &mut Context<'_>) -> Result<Vec<Value>, ScriptError> {
        self.map(|v| v.into_values(cx)).map_err(Into::into)
    }

    fn lower_result(
        self,
        cx: &mut Context<'_>,
        out: &mut rhue_vm::Rets,
    ) -> Result<(), ScriptError> {
        self.map(|v| v.lower_into(cx, out)).map_err(Into::into)
    }

    fn result_type() -> String {
        T::pack_type()
    }

    fn declare_types(vm: &mut rhue_vm::Vm) {
        T::declare_types(vm)
    }
}

/// A Luau function type from parameter and result types.
pub(crate) fn fn_signature(params: &[String], result: &str) -> String {
    format!("({}) -> {result}", params.join(", "))
}

/// Marks natives whose first parameter is `&mut Context`.
pub struct WithContext;

/// A Rust function that can be registered as a script function: a closure or
/// `fn` whose parameters are [`FromArg`] (any [`FromValue`](crate::FromValue)
/// type, or a trailing [`Variadic`](crate::Variadic)) and whose result is
/// [`IntoNativeResult`]. It can take `&mut Context` first to call back into
/// scripts. `Marker` just tells the impls apart, it's always inferred.
pub trait IntoNative<Marker> {
    /// Wraps the function as a native named `name` (the name shows up in
    /// argument errors).
    fn into_native(self, name: Rc<str>) -> NativeFn;

    /// The function's Luau type, for the type checker:
    /// `(number, number) -> number`.
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

/// Converts the VM's arguments, runs `body`, converts its results back.
pub(crate) fn run_native(
    ctx: &mut NativeCtx<'_>,
    args: &[rhue_vm::Value],
    first_index: usize,
    body: impl FnOnce(&mut Context<'_>, &mut Args, &mut rhue_vm::Rets) -> Result<(), ScriptError>,
) -> Result<rhue_vm::Rets, NativeError> {
    let mut cx = Context::new(ctx.vm());
    let mut args = Args::raw(args, first_index);
    let mut out = rhue_vm::Rets::new();
    match body(&mut cx, &mut args, &mut out) {
        Ok(()) => Ok(out),
        Err(e) => Err(e.into_native(&mut cx)),
    }
}

macro_rules! natives {
    ($($A:ident),*) => {
        impl<Func, Ret, $($A,)*> IntoNative<(Ret, $($A,)*)> for Func
        where
            Func: Fn($($A),*) -> Ret + 'static,
            Ret: IntoNativeResult,
            $($A: FromArg,)*
        {
            #[allow(non_snake_case, unused_variables, unused_mut)]
            fn into_native(self, name: Rc<str>) -> NativeFn {
                Rc::new(move |ctx, args| {
                    run_native(ctx, args, 1, |cx, args, out| {
                        $(let $A = $A::from_arg(args, cx)
                            .map_err(|e| ScriptError::argument(&name, e))?;)*
                        (self)($($A),*).lower_result(cx, out)
                    })
                })
            }

            fn signature() -> String {
                fn_signature(&[$($A::param_type()),*], &Ret::result_type())
            }

            #[allow(unused_variables)]
            fn declare_types(vm: &mut rhue_vm::Vm) {
                $($A::declare_types(vm);)*
                Ret::declare_types(vm);
            }
        }

        impl<Func, Ret, $($A,)*> IntoNative<(WithContext, Ret, $($A,)*)> for Func
        where
            Func: for<'a, 'b> Fn(&'a mut Context<'b>, $($A),*) -> Ret + 'static,
            Ret: IntoNativeResult,
            $($A: FromArg,)*
        {
            #[allow(non_snake_case, unused_variables, unused_mut)]
            fn into_native(self, name: Rc<str>) -> NativeFn {
                Rc::new(move |ctx, args| {
                    run_native(ctx, args, 1, |cx, args, out| {
                        $(let $A = $A::from_arg(args, cx)
                            .map_err(|e| ScriptError::argument(&name, e))?;)*
                        (self)(cx, $($A),*).lower_result(cx, out)
                    })
                })
            }

            fn signature() -> String {
                fn_signature(&[$($A::param_type()),*], &Ret::result_type())
            }

            #[allow(unused_variables)]
            fn declare_types(vm: &mut rhue_vm::Vm) {
                $($A::declare_types(vm);)*
                Ret::declare_types(vm);
            }
        }
    };
}
natives!();
natives!(A);
natives!(A, B);
natives!(A, B, C);
natives!(A, B, C, D);
natives!(A, B, C, D, E);
natives!(A, B, C, D, E, F);
natives!(A, B, C, D, E, F, G);
natives!(A, B, C, D, E, F, G, H);

/// An async Rust function that can be registered with
/// [`Engine::register_async`](crate::Engine::register_async). Parameters work
/// like [`IntoNative`], and it returns a future whose output is
/// [`IntoNativeResult`]. The future doesn't borrow the engine, so it takes its
/// arguments by value (`async move`).
pub trait IntoAsyncNative<Marker> {
    /// Wraps the function as a native that parks its future in `state`.
    #[doc(hidden)]
    fn into_async_native(
        self,
        name: Rc<str>,
        state: Rc<std::cell::RefCell<crate::asyncs::AsyncState>>,
    ) -> NativeFn;

    /// The function's Luau type (what the script sees, the future's output and
    /// not the future).
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

macro_rules! async_natives {
    ($($A:ident),*) => {
        impl<Func, Fut, Ret, $($A,)*> IntoAsyncNative<(Fut, Ret, $($A,)*)> for Func
        where
            Func: Fn($($A),*) -> Fut + 'static,
            Fut: std::future::Future<Output = Ret> + 'static,
            Ret: IntoNativeResult + 'static,
            $($A: FromArg,)*
        {
            #[allow(non_snake_case, unused_variables, unused_mut)]
            fn into_async_native(
                self,
                name: Rc<str>,
                state: Rc<std::cell::RefCell<crate::asyncs::AsyncState>>,
            ) -> NativeFn {
                Rc::new(move |ctx, args| {
                    run_native(ctx, args, 1, |cx, args, _out| {
                        $(let $A = $A::from_arg(args, cx)
                            .map_err(|e| ScriptError::argument(&name, e))?;)*
                        let future = (self)($($A),*);
                        let future = Box::pin(async move {
                            let out = future.await;
                            Box::new(move |cx: &mut Context<'_>| out.into_native_result(cx))
                                as crate::asyncs::Finish
                        });
                        crate::asyncs::suspend(cx, &name, &state, future)
                    })
                })
            }

            fn signature() -> String {
                fn_signature(&[$($A::param_type()),*], &Ret::result_type())
            }

            #[allow(unused_variables)]
            fn declare_types(vm: &mut rhue_vm::Vm) {
                $($A::declare_types(vm);)*
                Ret::declare_types(vm);
            }
        }
    };
}
async_natives!();
async_natives!(A);
async_natives!(A, B);
async_natives!(A, B, C);
async_natives!(A, B, C, D);
async_natives!(A, B, C, D, E);
async_natives!(A, B, C, D, E, F);
