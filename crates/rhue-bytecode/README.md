<div align="center">
  <img src="https://raw.githubusercontent.com/rhuelang/rhue/main/assets/Logo.svg" width="140" alt="Rhue logo">
  <h1>rhue-bytecode</h1>
  <p>Rhue's bytecode: 32-bit register instructions, function prototypes, the verifier, and the format compiled chunks get saved in.</p>
  <p>
    <a href="https://crates.io/crates/rhue-bytecode"><img src="https://img.shields.io/crates/v/rhue-bytecode?style=for-the-badge&amp;color=e07b4f" alt="rhue-bytecode on crates.io"></a>
    <a href="https://docs.rs/rhue-bytecode"><img src="https://img.shields.io/docsrs/rhue-bytecode?style=for-the-badge" alt="docs.rs"></a>
    <a href="https://github.com/rhuelang/rhue/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/rhuelang/rhue/ci.yml?branch=main&amp;style=for-the-badge&amp;label=ci" alt="CI status"></a>
    <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-1.88%2B-e07b4f?style=for-the-badge&amp;logo=rust" alt="Rust 1.88 or newer"></a>
    <a href="https://github.com/rhuelang/rhue#-license"><img src="https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-blue?style=for-the-badge" alt="MIT or Apache-2.0"></a>
  </p>
</div>

This is part of [Rhue](https://github.com/rhuelang/rhue)'s insides. If you just want to run scripts from Rust, use [`rhue`](https://crates.io/crates/rhue), it's the only crate whose API is meant to stay stable. Everything else (this one included) can change between minor versions.

What's in here:

- **`Instr` and `Op`** — the instruction encoding. The opcode's in the low byte like Luau's, then `A`, `B`, `C` (or `Bx`/`sBx`, or a 24-bit jump)
- **`Proto`** — one compiled function: code, constants, spans for every instruction, nested prototypes, debug info, table templates. `disassemble()` gives you a listing
- **`verify`** — checks a prototype before the VM runs it: registers in bounds, constants the right type, jumps landing on real instructions, the `top` protocol, captures that exist. Bad bytecode gets refused instead of panicking the VM
- **`to_bytes` / `from_bytes`** — the saved-chunk format (with the source text, so errors still point at lines, and optionally the chunk's types). Loading only checks the shape of the bytes, run `verify` on whatever you load
- **`arith`** — the arithmetic, shared by the constant folder and the VM so a folded `2 ^ 0.5` and a computed one are bit-for-bit the same

A listing looks like this:

```text
function sq (3 instructions, 0 constants, 2 registers, 1 params)
     0  MUL       R1 R0 R0
     1  RETURN    R1 1
     2  RETURN    R0 0
```

## License

MIT or Apache-2.0, same as the rest of Rhue.
