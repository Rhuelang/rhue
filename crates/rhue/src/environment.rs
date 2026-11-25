//! Separate sets of globals for scripts sharing an engine.
//!
//! A script run in an [`Environment`] reads and writes globals in the
//! environment's own table. A read that finds nothing there falls back to the
//! engine's globals, so the standard library and everything the host registered
//! are still visible, but every global the script assigns stays in its
//! environment. Functions it defines keep using that environment wherever they
//! get called from. Type checking does the same, a chunk gets checked against
//! the engine's globals plus the ones its own environment defined, not anyone
//! else's.

use std::any::Any;
use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use crate::modules;
use crate::{Context, Engine, Error, Table, Task, Value};

/// What an environment's clones share.
#[derive(Default)]
pub(crate) struct Shared {
    /// The globals chunks run here have defined, as the checker sees them.
    types: RefCell<Vec<(Rc<str>, rhue_types::Type)>>,
    /// Whatever the host hung on it ([`Environment::set_data`]).
    data: RefCell<Option<Rc<dyn Any>>>,
}

/// A separate set of globals (see the module docs). Cheap to clone, and clones
/// share the same globals.
#[derive(Clone)]
pub struct Environment {
    table: Table,
    shared: Rc<Shared>,
}

impl fmt::Debug for Environment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Environment")
            .field("table", &self.table)
            .finish_non_exhaustive()
    }
}

impl PartialEq for Environment {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.shared, &other.shared)
    }
}

impl Environment {
    /// The table holding the environment's own globals. Set entries in it
    /// (through a [`Context`](crate::Context)) to give scripts run here globals
    /// nobody else sees.
    pub fn globals(&self) -> &Table {
        &self.table
    }

    /// Hangs `data` on the environment (which script or plugin it belongs
    /// to, its permissions), replacing what was there. Every clone sees it,
    /// and a native finds it through
    /// [`Context::caller_environment`](crate::Context::caller_environment).
    pub fn set_data<T: Any>(&self, data: T) {
        *self.shared.data.borrow_mut() = Some(Rc::new(data));
    }

    /// What [`set_data`](Self::set_data) hung on it, if it's a `T`.
    pub fn data<T: Any>(&self) -> Option<Rc<T>> {
        self.shared.data.borrow().clone()?.downcast().ok()
    }
}

/// The environment whose globals are `table`, if it's still around.
pub(crate) fn find(cx: &mut Context<'_>, table: rhue_vm::TableRef) -> Option<Environment> {
    let shared = modules::host(cx.vm)
        .environments
        .iter()
        .find(|(t, _)| *t == table)
        .and_then(|(_, s)| s.upgrade())?;
    let Value::Table(table) = cx.lift(rhue_vm::Value::Table(table)) else {
        unreachable!("lifting a table gives a table")
    };
    Some(Environment { table, shared })
}

impl Engine {
    /// A new, empty environment.
    pub fn create_environment(&mut self) -> Environment {
        let env = Environment {
            table: self.context().new_table(),
            shared: Rc::default(),
        };
        let table = self.context().table_ref_of(&env.table);
        let list = &mut modules::host(&mut self.vm).environments;
        // forget the ones nobody holds any more
        list.retain(|(_, s)| s.strong_count() > 0);
        list.push((table, Rc::downgrade(&env.shared)));
        env
    }

    /// Compiles a chunk to run in `env`, type-checked against the engine's
    /// globals and `env`'s own (see [`run_in`](Self::run_in)).
    pub fn compile_in(
        &mut self,
        env: &Environment,
        name: &str,
        source: &str,
    ) -> Result<crate::Chunk, Error> {
        // the checker sees the engine's script globals with this environment's
        // on top (reads fall back to the engine's), and whatever the chunk
        // defines gets recorded as this environment's only
        let own = env.shared.types.borrow().clone();
        let shared = crate::type_env(&mut self.vm).swap_script_globals(Vec::new());
        let layered = shared.iter().cloned().chain(own).collect();
        crate::type_env(&mut self.vm).swap_script_globals(layered);
        let result = modules::compile_typed(&mut self.vm, name, source);
        crate::type_env(&mut self.vm).swap_script_globals(shared);
        let compiled = result?;
        let mut types = env.shared.types.borrow_mut();
        for (n, t) in compiled.globals {
            match types.iter_mut().find(|(m, _)| *m == n) {
                Some(slot) => slot.1 = t,
                None => types.push((n, t)),
            }
        }
        Ok(crate::Chunk(compiled.proto))
    }

    /// Runs a compiled chunk in `env`.
    pub fn run_in(&mut self, env: &Environment, chunk: &crate::Chunk) -> Result<Vec<Value>, Error> {
        let proto = &chunk.0;
        self.verify(proto)?;
        let table = self.context().table_ref_of(&env.table);
        let values = self
            .vm
            .execute_in(proto, table)
            .map_err(|e| Error::from_runtime(&e))?;
        let cx = self.context();
        Ok(values.into_iter().map(|v| cx.lift(v)).collect())
    }

    /// Compiles and runs `source` in `env`.
    pub fn eval_in(&mut self, env: &Environment, source: &str) -> Result<Vec<Value>, Error> {
        let proto = self.compile_in(env, "<eval>", source)?;
        self.run_in(env, &proto)
    }

    /// [`spawn`](Self::spawn), in `env`.
    pub fn spawn_in(&mut self, env: &Environment, name: &str, source: &str) -> Result<Task, Error> {
        let chunk = self.compile_in(env, name, source)?;
        self.spawn_chunk_in(env, &chunk)
    }

    /// Runs a compiled chunk as a task in `env`, until it first yields or
    /// finishes. Compile once and spawn it as often as you like: each task
    /// gets its own thread, and a chunk isn't tied to the environment it was
    /// compiled in, so one script can run in many. The checker only knows the
    /// globals it defines in the environment [`compile_in`](Self::compile_in)
    /// compiled it for.
    pub fn spawn_chunk_in(
        &mut self,
        env: &Environment,
        chunk: &crate::Chunk,
    ) -> Result<Task, Error> {
        self.verify(&chunk.0)?;
        let table = self.context().table_ref_of(&env.table);
        let f = self.vm.load_in(&chunk.0, table);
        let crate::Value::Function(f) = self.context().lift(f) else {
            unreachable!("loading a chunk gives a function")
        };
        self.spawn_function(&f, Vec::new())
    }
}
