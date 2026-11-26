//! Rhue's type checker, Luau-style gradual typing.
//!
//! This is part of [Rhue](https://docs.rs/rhue)'s guts. If you're embedding
//! Rhue, use the `rhue` crate, that's the API that stays stable.
//!
//! A chunk opts in with a directive on its first lines. `--!strict` checks
//! everything that can be inferred, `--!nonstrict` only checks what's
//! annotated, and `--!nocheck` (or no directive, by default) checks
//! nothing. Types never change how code runs, the compiler erases them.
//!
//! ```
//! use rhue_types::{Env, check_chunk};
//! use rhue_parser::{ast::TypeMode, parse_chunk};
//!
//! let chunk = parse_chunk("local x: number = 'one'").unwrap();
//! let res = rhue_compiler::resolve::resolve(&chunk).unwrap();
//! let out = check_chunk(&chunk, &res, TypeMode::Strict, &Env::new());
//! assert_eq!(out.errors[0].message, "Type '\"one\"' could not be converted into 'number'");
//! ```

mod check;
mod env;
mod export;
mod expr;
mod fixes;
mod meta;
mod portable;
mod relate;
mod types;

pub use check::{Analysis, CheckOutput, MAX_TYPE_ERRORS, analyze_chunk, check_chunk};
pub use env::{ClassMember, ClassType, Env, any_function};
pub use export::Exported;
pub use portable::portable;

/// The checker's maps, keyed by small ids and names and hashed with foldhash
/// (SipHash, the default, was about 5% of checking a big file).
pub type HashMap<K, V> = std::collections::HashMap<K, V, foldhash::fast::RandomState>;
pub type HashSet<T> = std::collections::HashSet<T, foldhash::fast::RandomState>;
pub use types::{FnType, Pack, TableType, Type, intersection, union};

#[cfg(test)]
mod tests;
