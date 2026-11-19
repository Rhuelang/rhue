<div align="center">
  <img src="https://raw.githubusercontent.com/rhuelang/rhue/main/assets/Logo.svg" width="140" alt="Rhue logo">
  <h1>rhue-lexer</h1>
  <p>Rhue's lexer, built on <a href="https://crates.io/crates/logos">logos</a>. It knows every Luau token.</p>
  <p>
    <a href="https://crates.io/crates/rhue-lexer"><img src="https://img.shields.io/crates/v/rhue-lexer?style=for-the-badge&amp;color=e07b4f" alt="rhue-lexer on crates.io"></a>
    <a href="https://docs.rs/rhue-lexer"><img src="https://img.shields.io/docsrs/rhue-lexer?style=for-the-badge" alt="docs.rs"></a>
    <a href="https://github.com/rhuelang/rhue/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/rhuelang/rhue/ci.yml?branch=main&amp;style=for-the-badge&amp;label=ci" alt="CI status"></a>
    <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/rust-1.88%2B-e07b4f?style=for-the-badge&amp;logo=rust" alt="Rust 1.88 or newer"></a>
    <a href="https://github.com/rhuelang/rhue#-license"><img src="https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-blue?style=for-the-badge" alt="MIT or Apache-2.0"></a>
  </p>
</div>

This is part of [Rhue](https://github.com/rhuelang/rhue)'s insides. If you just want to run scripts from Rust, use [`rhue`](https://crates.io/crates/rhue), it's the only crate whose API is meant to stay stable. Everything else (this one included) can change between minor versions.

```rust
use rhue_lexer::{Token, lex};

let lexed = lex("local x = 1 + 2");
let tokens: Vec<Token> = lexed.tokens.iter().map(|t| t.token).collect();
assert_eq!(tokens[0], Token::Local);
assert_eq!(*tokens.last().unwrap(), Token::Eof);
```

Tokens don't hold their text, only numbers get a payload. Names and strings get sliced out of the source by span when the parser wants them, which keeps `Token` `Copy`. Errors don't stop lexing either, bad characters get reported and skipped so the parser still sees the rest of the file.

Interpolated strings (`` `hp {p.hp}` ``) come out as one token. `interp::pieces` splits them into text and holes for the parser.

## License

MIT or Apache-2.0, same as the rest of Rhue.
