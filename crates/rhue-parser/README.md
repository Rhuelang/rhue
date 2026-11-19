<div align="center">
  <img src="https://raw.githubusercontent.com/rhuelang/rhue/main/assets/Logo.svg" width="140" alt="Rhue logo">
  <h1>rhue-parser</h1>
  <p>The parser for Rhue's Luau-shaped syntax. Recursive descent for statements, Pratt parsing for expressions, and the AST it all turns into.</p>
  <p>
    <a href="https://crates.io/crates/rhue-parser"><img src="https://img.shields.io/crates/v/rhue-parser?style=for-the-badge&amp;color=e07b4f" alt="rhue-parser on crates.io"></a>
    <a href="https://docs.rs/rhue-parser"><img src="https://img.shields.io/docsrs/rhue-parser?style=for-the-badge" alt="docs.rs"></a>
    <a href="https://github.com/rhuelang/rhue/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/rhuelang/rhue/ci.yml?branch=main&amp;style=for-the-badge&amp;label=ci" alt="CI status"></a>
    <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-1.88%2B-e07b4f?style=for-the-badge&amp;logo=rust" alt="Rust 1.88 or newer"></a>
    <a href="https://github.com/rhuelang/rhue#-license"><img src="https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-blue?style=for-the-badge" alt="MIT or Apache-2.0"></a>
  </p>
</div>

This is part of [Rhue](https://github.com/rhuelang/rhue)'s insides. If you just want to run scripts from Rust, use [`rhue`](https://crates.io/crates/rhue), it's the only crate whose API is meant to stay stable. Everything else (this one included) can change between minor versions.

```rust
use rhue_parser::parse_chunk;

let chunk = parse_chunk("local function sq(x: number) return x * x end").unwrap();
assert_eq!(chunk.body.stmts.len(), 1);

let errors = parse_chunk("local = 1\nlocal y = )").unwrap_err();
assert_eq!(errors.len(), 2); // it recovers and keeps going
```

It takes all of Luau's type syntax (annotations, aliases with generics and defaults, type packs, `read`/`write` properties, `typeof`, `::`) and keeps it in the AST for `rhue-types`. The compiler just ignores it.

A few things worth knowing:

- every name occurrence gets a dense `NameId`, so later passes keep side tables in a `Vec` instead of hashing nodes
- nesting is capped at `MAX_DEPTH`, including long `1 + 1 + 1 ...` chains, so hostile input can't build a tree deep enough to blow the stack
- it stops collecting after `MAX_ERRORS`, the rest are usually cascades anyway
- `parse_expr` and `parse_type` parse just an expression or just a type (the REPL and host declarations use those)

## License

MIT or Apache-2.0, same as the rest of Rhue.
