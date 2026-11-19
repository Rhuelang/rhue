<div align="center">
  <img src="https://raw.githubusercontent.com/rhuelang/rhue/main/assets/Logo.svg" width="140" alt="Rhue logo">
  <h1>rhue-fmt</h1>
  <p>The formatter behind <code>rhue fmt</code> and the language server's formatting.</p>
  <p>
    <a href="https://crates.io/crates/rhue-fmt"><img src="https://img.shields.io/crates/v/rhue-fmt?style=for-the-badge&amp;color=e07b4f" alt="rhue-fmt on crates.io"></a>
    <a href="https://docs.rs/rhue-fmt"><img src="https://img.shields.io/docsrs/rhue-fmt?style=for-the-badge" alt="docs.rs"></a>
    <a href="https://github.com/rhuelang/rhue/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/rhuelang/rhue/ci.yml?branch=main&amp;style=for-the-badge&amp;label=ci" alt="CI status"></a>
    <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-1.88%2B-e07b4f?style=for-the-badge&amp;logo=rust" alt="Rust 1.88 or newer"></a>
    <a href="https://github.com/rhuelang/rhue#-license"><img src="https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-blue?style=for-the-badge" alt="MIT or Apache-2.0"></a>
  </p>
</div>

This is part of [Rhue](https://github.com/rhuelang/rhue)'s insides. If you just want to run scripts from Rust, use [`rhue`](https://crates.io/crates/rhue), it's the only crate whose API is meant to stay stable. Everything else (this one included) can change between minor versions.

```rust
use rhue_fmt::{FormatOptions, format};

let out = format("local  x={1,2}", &FormatOptions::default()).unwrap();
assert_eq!(out, "local x = { 1, 2 }\n");
```

It works on tokens, not a syntax tree, so your comments, blank lines and line breaks all survive. It only fixes indentation and spacing. Tabs by default, set `indent` to four spaces (or whatever) if you'd rather.

Before handing anything back it re-lexes the output and checks the tokens and comments match the input exactly. If they don't you get an error instead of a changed program. Code that doesn't parse doesn't get formatted at all.

## License

MIT or Apache-2.0, same as the rest of Rhue.
