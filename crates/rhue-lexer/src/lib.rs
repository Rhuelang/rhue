//! Rhue's lexer, built on [`logos`].
//!
//! This is part of [Rhue](https://docs.rs/rhue)'s guts. If you're embedding
//! Rhue, use the `rhue` crate, that's the API that stays stable.
//!
//! Tokens don't carry anything except numbers. Identifiers and strings get
//! sliced out of the source by span when the parser wants them, which keeps
//! [`Token`] `Copy` and lifetime-free.

pub mod interp;
mod token;

pub use token::{LexErrorKind, Token};

use logos::Logos;
use rhue_common::{Diagnostic, Span};

/// A token and where it came from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spanned {
    pub token: Token,
    pub span: Span,
}

/// Everything from lexing one source file.
#[derive(Debug, Default)]
pub struct Lexed {
    /// Every token, always ending in exactly one [`Token::Eof`].
    pub tokens: Vec<Spanned>,
    pub errors: Vec<Diagnostic>,
}

/// Lexes all of `source`. Errors don't stop it, bad characters get skipped and
/// reported so the parser still sees the rest of the file.
pub fn lex(source: &str) -> Lexed {
    let mut out = Lexed::default();
    if source.len() > u32::MAX as usize {
        out.errors.push(Diagnostic::error(
            "source file is larger than 4 GiB",
            Span::new(0, 0),
        ));
        out.tokens.push(Spanned {
            token: Token::Eof,
            span: Span::new(0, 0),
        });
        return out;
    }

    let mut lexer = Token::lexer(source);
    while let Some(result) = lexer.next() {
        let span = Span::from(lexer.span());
        match result {
            Ok(token) => out.tokens.push(Spanned { token, span }),
            Err(kind) => out.errors.push(kind.to_diagnostic(span, source)),
        }
    }
    let end = source.len() as u32;
    out.tokens.push(Spanned {
        token: Token::Eof,
        span: Span::new(end, end),
    });
    out
}

#[cfg(test)]
mod tests;
