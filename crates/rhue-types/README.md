<div align="center">
  <img src="https://raw.githubusercontent.com/rhuelang/rhue/main/assets/Logo.svg" width="140" alt="Rhue logo">
  <h1>rhue-types</h1>
  <p>Rhue's type checker. Luau-style gradual typing, opted into per file with <code>--!strict</code> or <code>--!nonstrict</code>.</p>
  <p>
    <a href="https://crates.io/crates/rhue-types"><img src="https://img.shields.io/crates/v/rhue-types?style=for-the-badge&amp;color=e07b4f" alt="rhue-types on crates.io"></a>
    <a href="https://docs.rs/rhue-types"><img src="https://img.shields.io/docsrs/rhue-types?style=for-the-badge" alt="docs.rs"></a>
    <a href="https://github.com/rhuelang/rhue/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/rhuelang/rhue/ci.yml?branch=main&amp;style=for-the-badge&amp;label=ci" alt="CI status"></a>
    <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-1.88%2B-e07b4f?style=for-the-badge&amp;logo=rust" alt="Rust 1.88 or newer"></a>
    <a href="https://github.com/rhuelang/rhue#-license"><img src="https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-blue?style=for-the-badge" alt="MIT or Apache-2.0"></a>
  </p>
</div>

This is part of [Rhue](https://github.com/rhuelang/rhue)'s insides. If you just want to run scripts from Rust, use [`rhue`](https://crates.io/crates/rhue), it's the only crate whose API is meant to stay stable. Everything else (this one included) can change between minor versions.

```rust
use rhue_parser::{ast::TypeMode, parse_chunk};
use rhue_types::{Env, check_chunk};

let chunk = parse_chunk("local x: number = 'one'").unwrap();
let res = rhue_compiler::resolve::resolve(&chunk).unwrap();
let out = check_chunk(&chunk, &res, TypeMode::Strict, &Env::new());
assert_eq!(out.errors[0].message, "Type '\"one\"' could not be converted into 'number'");
```

It's one pass over the AST, statement by statement. What it handles:

- inference for locals and results, generics, type packs (`T...`) and alias defaults
- narrowing from conditions (`if x then`, `type(x) == "string"`, `assert(x)`, early returns), and flow typing for `local x` with no value, loops included
- structural tables with read-only and write-only properties
- metatables: `setmetatable` classes, `__index` chains, operator metamethods, and a typed `self` in methods
- `vector`, `buffer`, `thread` and host (userdata) types, declared from Rust through `Env`
- exporting a chunk's types as Luau syntax so saved chunks and modules keep them

Types never change what code does. The compiler throws them away, and a property test runs every random strict program the checker accepts to make sure none of them hits a type error at runtime.

`analyze_chunk` is the same check plus everything an editor wants (the type of every expression, where locals are used, members for completion). The language server is built on it.

## License

MIT or Apache-2.0, same as the rest of Rhue.
