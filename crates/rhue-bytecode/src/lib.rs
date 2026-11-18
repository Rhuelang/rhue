//! Rhue's bytecode: instruction encoding, opcodes, function prototypes, and the
//! arithmetic semantics shared by the compiler's constant folder and the VM,
//! the verifier that checks a prototype before the VM runs it, and the
//! binary form chunks are saved in.
//!
//! Part of [Rhue](https://docs.rs/rhue)'s implementation. To embed Rhue, use
//! the `rhue` crate: its API is the one kept stable.

mod arith;
mod instr;
mod op;
mod proto;
mod serial;
mod verify;

pub use arith::{ArithOp, arith, floor, format_number};
pub use instr::Instr;
pub use op::Op;
pub use proto::{Constant, InlineCache, LocalVar, Proto, TableTemplate, UpvalDesc, Verified};
pub use serial::{ChunkTypes, FORMAT_VERSION, LoadError, MAGIC, is_chunk, read_chunk, write_chunk};
pub use verify::{MAX_STACK, MAX_TEMPLATE_SIZE, VerifyError, verify, verify_once};
