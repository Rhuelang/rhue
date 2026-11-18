//! Rhue's lexer, built on [`logos`].
//!
//! Part of [Rhue](https://docs.rs/rhue)'s implementation. To embed Rhue, use
//! the `rhue` crate: its API is the one kept stable.
//!
//! Tokens carry no payload except numbers: identifiers and strings are sliced
//! out of the source by span when the parser needs them. That keeps [`Token`]
//! `Copy` and lifetime-free.

pub mod interp;
mod token;

pub use token::{LexErrorKind, Token};

use logos::Logos;
use rhue_common::{Diagnostic, Span};

/// A token together with where it came from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spanned {
    pub token: Token,
    pub span: Span,
}

/// The result of lexing a whole source file.
#[derive(Debug, Default)]
pub struct Lexed {
    /// Every token, always terminated by a single [`Token::Eof`].
    pub tokens: Vec<Spanned>,
    pub errors: Vec<Diagnostic>,
}

/// Lexes `source` completely. Errors don't stop lexing: the bad characters are
/// skipped and reported, so the parser still sees the rest of the file.
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
