//! Rhue's bytecode. Instruction encoding, opcodes, function prototypes, the
//! arithmetic the constant folder and VM share, the verifier that checks a
//! prototype before it runs, and the binary format chunks get saved in.
//!
//! This is part of [Rhue](https://docs.rs/rhue)'s guts. If you're embedding
//! Rhue, use the `rhue` crate, that's the API that stays stable.

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
