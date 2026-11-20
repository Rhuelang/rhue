//! Spans and diagnostics, shared by every Rhue crate.
//!
//! This is part of [Rhue](https://docs.rs/rhue)'s guts. If you're embedding
//! Rhue, use the `rhue` crate, that's the API that stays stable.

mod diagnostic;
mod source;
mod span;
mod suggest;

pub use diagnostic::{Diagnostic, Fix, Label, Severity};
pub use source::SourceFile;
pub use span::{Span, offset};
pub use suggest::closest;
