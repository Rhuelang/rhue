<div align="center">
  <img src="https://raw.githubusercontent.com/rhuelang/rhue/main/assets/Logo.svg" width="140" alt="Rhue logo">
  <h1>rhue-compiler</h1>
  <p>Turns Rhue's AST into register bytecode. One pass, after a resolver that works out which names are locals, upvalues or globals.</p>
  <p>
    <a href="https://crates.io/crates/rhue-compiler"><img src="https://img.shields.io/crates/v/rhue-compiler?style=for-the-badge&amp;color=e07b4f" alt="rhue-compiler on crates.io"></a>
    <a href="https://docs.rs/rhue-compiler"><img src="https://img.shields.io/docsrs/rhue-compiler?style=for-the-badge" alt="docs.rs"></a>
    <a href="https://github.com/rhuelang/rhue/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/rhuelang/rhue/ci.yml?branch=main&amp;style=for-the-badge&amp;label=ci" alt="CI status"></a>
    <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-1.88%2B-e07b4f?style=for-the-badge&amp;logo=rust" alt="Rust 1.88 or newer"></a>
    <a href="https://github.com/rhuelang/rhue#-license"><img src="https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-blue?style=for-the-badge" alt="MIT or Apache-2.0"></a>
  </p>
</div>

This is part of [Rhue](https://github.com/rhuelang/rhue)'s insides. If you just want to run scripts from Rust, use [`rhue`](https://crates.io/crates/rhue), it's the only crate whose API is meant to stay stable. Everything else (this one included) can change between minor versions.

The whole pipeline by hand, without the `rhue` crate:

```rust
use std::rc::Rc;
use rhue_common::SourceFile;
use rhue_compiler::{CompileOptions, compile_chunk};

let src = "local function sq(x) return x * x end return sq(7)";
let source = Rc::new(SourceFile::new("main", src));
let chunk = rhue_parser::parse_chunk(src).unwrap();
let proto = compile_chunk(&chunk, &source, CompileOptions::default()).unwrap();

let mut vm = rhue_vm::Vm::new();
let out = vm.execute(&Rc::new(proto)).unwrap();
assert_eq!(vm.display(out[0]), "49");
```

What it does along the way:

- `resolve` binds every name, marks the locals closures capture (only those need closing), and checks `break`/`continue` placement and Luau's rule about `until` and `continue`
- `fold` folds constant arithmetic, using the same `arith` as the VM so results match exactly
- conditions compile to jump lists, so `a and b or c` in an `if` never makes a boolean
- comparisons and arithmetic against constants get their own opcodes (`ADDK`, `EQK`, ...)
- table constructors with constant keys get a template (`DUPTABLE`), so the keys are already in place
- the output always goes through the verifier, a failure there is a compiler bug

## License

MIT or Apache-2.0, same as the rest of Rhue.
