<div align="center">
  <img src="https://raw.githubusercontent.com/rhuelang/rhue/main/assets/Logo.svg" width="140" alt="Rhue logo">
  <h1>rhue-vm</h1>
  <p>The virtual machine that runs Rhue's bytecode, plus its heap, garbage collector and standard library.</p>
  <p>
    <a href="https://crates.io/crates/rhue-vm"><img src="https://img.shields.io/crates/v/rhue-vm?style=for-the-badge&amp;color=e07b4f" alt="rhue-vm on crates.io"></a>
    <a href="https://docs.rs/rhue-vm"><img src="https://img.shields.io/docsrs/rhue-vm?style=for-the-badge" alt="docs.rs"></a>
    <a href="https://github.com/rhuelang/rhue/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/rhuelang/rhue/ci.yml?branch=main&amp;style=for-the-badge&amp;label=ci" alt="CI status"></a>
    <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-1.88%2B-e07b4f?style=for-the-badge&amp;logo=rust" alt="Rust 1.88 or newer"></a>
    <a href="https://github.com/rhuelang/rhue#-license"><img src="https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-blue?style=for-the-badge" alt="MIT or Apache-2.0"></a>
  </p>
</div>

This is part of [Rhue](https://github.com/rhuelang/rhue)'s insides. If you just want to run scripts from Rust, use [`rhue`](https://crates.io/crates/rhue), it's the only crate whose API is meant to stay stable. Everything else (this one included) can change between minor versions.

What's in here:

- the interpreter, a register machine with inline caches for field and global lookups, fast paths for common builtins, and calls that never recurse on the Rust stack
- `Value`, `Copy` and 16 bytes. Heap objects are handles with a generation, so a stale one fails a check instead of aliasing whatever reused the slot
- a mark-and-sweep collector with an explicit worklist. It only runs at safe points, and there's a stress mode that collects before every instruction (the tests use it a lot)
- tables with an array part and an insertion-ordered hash part, so `pairs` order is stable and clearing fields while iterating works
- coroutines, metamethods that run in the dispatch loop (so they can yield), tail calls, debug hooks, hot swapping a module's functions in place
- the standard library: `table`, `string` with Lua patterns, `math`, `utf8`, a sandbox-safe `os`, `bit32`, `buffer`, `vector`, `debug`, `coroutine`
- instruction and memory limits, and interrupts from other threads

There's no `unsafe` anywhere in it, so bad bytecode can't do worse than panic, and the verifier in `rhue-bytecode` stops that too.

```rust
use std::rc::Rc;
use rhue_common::SourceFile;

let src = "return ('hello'):upper()";
let chunk = rhue_parser::parse_chunk(src).unwrap();
let source = Rc::new(SourceFile::new("main", src));
let proto = rhue_compiler::compile_chunk(&chunk, &source, Default::default()).unwrap();

let mut vm = rhue_vm::Vm::new();
let out = vm.execute(&Rc::new(proto)).unwrap();
assert_eq!(vm.display(out[0]), "HELLO");
```

## License

MIT or Apache-2.0, same as the rest of Rhue.
