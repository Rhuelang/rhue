//! Shared foundations for every Rhue crate: source spans and diagnostics.
//!
//! Part of [Rhue](https://docs.rs/rhue)'s implementation. To embed Rhue, use
//! the `rhue` crate: its API is the one kept stable.

mod diagnostic;
mod source;
mod span;
mod suggest;

pub use diagnostic::{Diagnostic, Fix, Label, Severity};
pub use source::SourceFile;
pub use span::Span;
pub use suggest::closest;
