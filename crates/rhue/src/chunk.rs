//! [`Chunk`], a compiled script.

use std::fmt;
use std::rc::Rc;

use rhue_bytecode::Proto;

/// A compiled script, from [`Engine::compile`](crate::Engine::compile) or
/// [`Engine::load`](crate::Engine::load). Run it with
/// [`Engine::run`](crate::Engine::run) as many times as you want, cloning one
/// is cheap.
///
/// What's inside is the VM's business and changes between releases, so all a
/// chunk lets you do is save it, print it and run it.
#[derive(Clone)]
pub struct Chunk(pub(crate) Rc<Proto>);

impl Chunk {
    /// Saves it for [`Engine::load`](crate::Engine::load). Unlike
    /// [`Engine::compile_to_bytes`](crate::Engine::compile_to_bytes), the
    /// bytes don't carry types, so checked code that loads it sees `any`.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.0.to_bytes()
    }

    /// The bytecode as text, for seeing what the compiler made. The format
    /// isn't stable.
    pub fn disassemble(&self) -> String {
        self.0.disassemble()
    }

    /// The name it was compiled under.
    pub fn name(&self) -> &str {
        &self.0.source.name
    }

    // for the test suite, which builds and damages bytecode by hand
    #[doc(hidden)]
    pub fn from_proto(proto: Rc<Proto>) -> Self {
        Chunk(proto)
    }

    #[doc(hidden)]
    pub fn proto(&self) -> &Rc<Proto> {
        &self.0
    }
}

impl fmt::Debug for Chunk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Chunk").field(&self.name()).finish()
    }
}
