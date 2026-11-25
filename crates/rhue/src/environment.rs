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

use std::cell::RefCell;
use std::rc::Rc;

use crate::modules;
use crate::{Engine, Error, Table, Task, Value};

/// The globals chunks run in an environment have defined, as the checker sees
/// them.
type Defined = Rc<RefCell<Vec<(Rc<str>, rhue_types::Type)>>>;

/// A separate set of globals (see the module docs). Cheap to clone, and clones
/// share the same globals.
#[derive(Clone, Debug)]
pub struct Environment {
    table: Table,
    /// The globals chunks run here have defined, as the checker sees them.
    types: Defined,
}

impl Environment {
    /// The table holding the environment's own globals. Set entries in it
    /// (through a [`Context`](crate::Context)) to give scripts run here globals
    /// nobody else sees.
    pub fn globals(&self) -> &Table {
        &self.table
    }
}

impl Engine {
    /// A new, empty environment.
    pub fn create_environment(&mut self) -> Environment {
        Environment {
            table: self.context().new_table(),
            types: Rc::default(),
        }
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
        let own = env.types.borrow().clone();
        let shared = crate::type_env(&mut self.vm).swap_script_globals(Vec::new());
        let layered = shared.iter().cloned().chain(own).collect();
        crate::type_env(&mut self.vm).swap_script_globals(layered);
        let result = modules::compile_typed(&mut self.vm, name, source);
        crate::type_env(&mut self.vm).swap_script_globals(shared);
        let compiled = result?;
        let mut types = env.types.borrow_mut();
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
