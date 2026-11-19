<div align="center">
  <img src="https://raw.githubusercontent.com/rhuelang/rhue/main/assets/Logo.svg" width="140" alt="Rhue logo">
  <h1>rhue-common</h1>
  <p>Spans, diagnostics and source files, the bits every other Rhue crate shares.</p>
  <p>
    <a href="https://crates.io/crates/rhue-common"><img src="https://img.shields.io/crates/v/rhue-common?style=for-the-badge&amp;color=e07b4f" alt="rhue-common on crates.io"></a>
    <a href="https://docs.rs/rhue-common"><img src="https://img.shields.io/docsrs/rhue-common?style=for-the-badge" alt="docs.rs"></a>
    <a href="https://github.com/rhuelang/rhue/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/rhuelang/rhue/ci.yml?branch=main&amp;style=for-the-badge&amp;label=ci" alt="CI status"></a>
    <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-1.88%2B-e07b4f?style=for-the-badge&amp;logo=rust" alt="Rust 1.88 or newer"></a>
    <a href="https://github.com/rhuelang/rhue#-license"><img src="https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-blue?style=for-the-badge" alt="MIT or Apache-2.0"></a>
  </p>
</div>

This is part of [Rhue](https://github.com/rhuelang/rhue)'s insides. If you just want to run scripts from Rust, use [`rhue`](https://crates.io/crates/rhue), it's the only crate whose API is meant to stay stable. Everything else (this one included) can change between minor versions.

What's in here:

- `Span`, a `u32` byte range into a source
- `SourceFile`, a named source with a lazy line table (debug hooks ask for lines a lot)
- `Diagnostic`, the one error shape every phase reports through, rendered with [ariadne](https://crates.io/crates/ariadne). It can carry `Fix`es for editors to offer
- `closest`, the "did you mean ...?" helper

```rust
use rhue_common::closest;

assert_eq!(closest("pirnt", ["print", "pairs"]), Some("print"));
```

## License

MIT or Apache-2.0, same as the rest of Rhue.
