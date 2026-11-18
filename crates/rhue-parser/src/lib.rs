//! Rhue's parser: hand-written recursive descent for statements, Pratt
//! parsing for expressions.
//!
//! Part of [Rhue](https://docs.rs/rhue)'s implementation. To embed Rhue, use
//! the `rhue` crate: its API is the one kept stable.
//!
//! The parser recovers from errors at statement boundaries, so one run can
//! report several independent mistakes.

pub mod ast;
mod parser;
mod strings;

pub use parser::{MAX_DEPTH, MAX_ERRORS, parse_chunk, parse_expr, parse_type};

#[cfg(test)]
mod tests;
