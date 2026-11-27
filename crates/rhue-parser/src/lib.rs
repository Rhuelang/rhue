//! Rhue's parser. Hand-written recursive descent for statements, Pratt parsing
//! for expressions.
//!
//! This is part of [Rhue](https://docs.rs/rhue)'s guts. If you're embedding
//! Rhue, use the `rhue` crate, that's the API that stays stable.
//!
//! It recovers from errors at statement boundaries, so one run can report a
//! bunch of separate mistakes.

pub mod ast;
mod parser;
mod strings;

pub use parser::{MAX_DEPTH, MAX_ERRORS, parse_chunk, parse_definitions, parse_expr, parse_type};

#[cfg(test)]
mod tests;
