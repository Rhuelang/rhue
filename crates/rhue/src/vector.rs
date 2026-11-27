//! Host members on the built-in `vector`, see [`Engine::vector`](crate::Engine::vector).

use std::rc::Rc;

use rhue_vm::NativeFn;

use crate::convert::IntoValue;
use crate::native::run_native;
use crate::userdata::declare;
use crate::{Context, IntoNative, Table};

/// Adds properties and methods to every `vector`, and a constructor table if
/// the host wants one. Made with [`Engine::vector`](crate::Engine::vector).
///
/// `x`, `y` and `z` (and the capitals) stay what they are, a member with one of
/// those names never gets reached.
pub struct VectorBuilder<'a> {
    pub(crate) vm: &'a mut rhue_vm::Vm,
}

impl VectorBuilder<'_> {
    /// A read-only property: `v.name` returns `get(v)`.
    pub fn property<V: IntoValue>(
        &mut self,
        name: &str,
        get: impl Fn([f32; 3]) -> V + 'static,
    ) -> &mut Self {
        V::declare_types(self.vm);
        let ty = V::luau_type();
        declare(
            crate::type_env(self.vm).declare_property("vector", name, &ty, false),
            &ty,
        );
        let native: NativeFn = Rc::new(move |ctx, args| {
            run_native(ctx, &args[..1], 1, |cx, _, out| {
                // only ever called with the vector being indexed
                let Some(&rhue_vm::Value::Vector(v)) = args.first() else {
                    unreachable!("a vector getter gets its vector")
                };
                crate::convert::IntoValues::lower_into(get(v), cx, out);
                Ok(())
            })
        });
        self.vm.add_vector_getter(name, native);
        self
    }

    /// `v:name(...)`. The function takes the vector first, `[f32; 3]`, then
    /// whatever arguments it wants, like any registered function.
    pub fn method<M, F: IntoNative<M>>(&mut self, name: &str, f: F) -> &mut Self {
        F::declare_types(self.vm);
        let sig = F::signature();
        declare(
            crate::type_env(self.vm).declare_method("vector", name, &sig),
            &sig,
        );
        let native = f.into_native(Rc::from(name));
        let v = self.vm.native(name, native);
        self.vm.add_vector_method(name, v);
        self
    }

    /// A global table called `name` with `new(x, y, z?)`, `zero` and `one`,
    /// for hosts whose scripts expect `Vector3.new(1, 2, 3)` and not
    /// `vector.create(1, 2, 3)`. Add more to it with [`function`](Self::function).
    pub fn constructor(&mut self, name: &str) -> &mut Self {
        self.function(name, "new", |x: f32, y: f32, z: Option<f32>| {
            [x, y, z.unwrap_or(0.0)]
        });
        for (field, v) in [("zero", [0.0; 3]), ("one", [1.0; 3])] {
            declare(
                crate::type_env(self.vm).declare_global_field(name, field, "vector"),
                "vector",
            );
            let mut cx = Context::new(self.vm);
            let table = global_table(&mut cx, name);
            cx.set(&table, field, v)
                .expect("a string key is always valid");
        }
        self
    }

    /// A function in the global table `table` (made if it isn't there),
    /// `Vector3.fromAxis(...)` say.
    pub fn function<M, F: IntoNative<M>>(&mut self, table: &str, name: &str, f: F) -> &mut Self {
        F::declare_types(self.vm);
        let sig = F::signature();
        declare(
            crate::type_env(self.vm).declare_global_field(table, name, &sig),
            &sig,
        );
        let native = f.into_native(Rc::from(format!("{table}.{name}")));
        let func = self.vm.native(name, native);
        let mut cx = Context::new(self.vm);
        let t = global_table(&mut cx, table);
        let func = cx.lift(func);
        cx.set(&t, name, func)
            .expect("a string key is always valid");
        self
    }
}

fn global_table(cx: &mut Context<'_>, name: &str) -> Table {
    match cx.global::<Option<Table>>(name) {
        Ok(Some(t)) => t,
        _ => {
            let t = cx.new_table();
            cx.set_global(name, t.clone());
            t
        }
    }
}
