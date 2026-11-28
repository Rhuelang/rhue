use std::fmt;

use logos::{FilterResult, Lexer, Logos};
use rhue_common::{Diagnostic, Span};

#[derive(Clone, Debug, Default, PartialEq)]
pub enum LexErrorKind {
    #[default]
    UnexpectedChar,
    UnterminatedString,
    UnterminatedLongString,
    UnterminatedComment,
    MalformedNumber,
}

impl LexErrorKind {
    pub fn to_diagnostic(&self, span: Span, source: &str) -> Diagnostic {
        match self {
            LexErrorKind::UnexpectedChar => {
                let ch = span.text(source).chars().next().unwrap_or('?');
                Diagnostic::error(
                    format!("unexpected character `{}`", ch.escape_debug()),
                    span,
                )
                .with_label("not valid here")
            }
            LexErrorKind::UnterminatedString => Diagnostic::error("unterminated string", span)
                .with_label("string starts here")
                .with_help(
                    "strings can't span lines; use `\\n`, `\\z`, or a long string `[[...]]`",
                ),
            LexErrorKind::UnterminatedLongString => {
                Diagnostic::error("unterminated long string", span)
                    .with_label("long string starts here")
            }
            LexErrorKind::UnterminatedComment => {
                Diagnostic::error("unterminated block comment", span)
                    .with_label("comment starts here")
            }
            LexErrorKind::MalformedNumber => {
                Diagnostic::error(format!("malformed number `{}`", span.text(source)), span)
            }
        }
    }
}

/// Every token in Rhue's (Luau-shaped) grammar.
///
/// `continue`, `type`, `export` and `typeof` are only keywords in context in
/// Luau, so they lex as [`Token::Ident`].
#[derive(Logos, Clone, Copy, Debug, PartialEq)]
// newer clippy reads two patterns sharing a callback as a duplicate attribute
#[allow(clippy::duplicated_attributes)]
#[logos(error = LexErrorKind)]
#[logos(skip r"[ \t\r\n\f\v]+")]
pub enum Token {
    // ─ Literals ──────────────────────────────────────────────────────────────
    #[regex(r"[0-9][0-9_]*(\.[0-9_]*)?([eE][+-]?[0-9_]+)?", decimal)]
    #[regex(r"\.[0-9][0-9_]*([eE][+-]?[0-9_]+)?", decimal)]
    #[regex(r"0[xX][0-9a-fA-F_]+", |lex| radix(lex, 16))]
    #[regex(r"0[bB][01_]+", |lex| radix(lex, 2))]
    Number(f64),

    #[regex(r#""([^"\\\n]|\\(.|\n))*""#)]
    #[regex(r#"'([^'\\\n]|\\(.|\n))*'"#)]
    #[regex(r#""([^"\\\n]|\\(.|\n))*"#, unterminated_string)]
    #[regex(r#"'([^'\\\n]|\\(.|\n))*"#, unterminated_string)]
    #[regex(r"\[=*\[", long_string)]
    String,

    /// `` `text {expr} text` `` (see [`crate::interp`]).
    #[token("`", interp_string)]
    InterpString,

    #[regex(r"[A-Za-z_][A-Za-z0-9_]*")]
    Ident,

    // ─ Comments (skipped) ────────────────────────────────────────────────────
    #[token("--", comment)]
    Comment,

    // ─ Keywords ──────────────────────────────────────────────────────────────
    #[token("and")]
    And,
    #[token("break")]
    Break,
    #[token("do")]
    Do,
    #[token("else")]
    Else,
    #[token("elseif")]
    Elseif,
    #[token("end")]
    End,
    #[token("false")]
    False,
    #[token("for")]
    For,
    #[token("function")]
    Function,
    #[token("if")]
    If,
    #[token("in")]
    In,
    #[token("local")]
    Local,
    #[token("nil")]
    Nil,
    #[token("not")]
    Not,
    #[token("or")]
    Or,
    #[token("repeat")]
    Repeat,
    #[token("return")]
    Return,
    #[token("then")]
    Then,
    #[token("true")]
    True,
    #[token("until")]
    Until,
    #[token("while")]
    While,

    // ─ Operators & Punctuation ───────────────────────────────────────────────
    #[token("+")]
    Plus,
    #[token("-")]
    Minus,
    #[token("*")]
    Star,
    #[token("/")]
    Slash,
    #[token("//")]
    SlashSlash,
    #[token("%")]
    Percent,
    #[token("^")]
    Caret,
    #[token("#")]
    Hash,
    /// Only for `@class(param)` in definitions files, scripts have no use
    /// for it.
    #[token("@")]
    At,
    #[token("..")]
    DotDot,
    #[token("==")]
    EqEq,
    #[token("~=")]
    NotEq,
    #[token("<")]
    Lt,
    #[token("<=")]
    LtEq,
    #[token(">")]
    Gt,
    #[token(">=")]
    GtEq,
    #[token("=")]
    Eq,
    #[token("+=")]
    PlusEq,
    #[token("-=")]
    MinusEq,
    #[token("*=")]
    StarEq,
    #[token("/=")]
    SlashEq,
    #[token("//=")]
    SlashSlashEq,
    #[token("%=")]
    PercentEq,
    #[token("^=")]
    CaretEq,
    #[token("..=")]
    DotDotEq,
    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token("{")]
    LBrace,
    #[token("}")]
    RBrace,
    #[token("[")]
    LBracket,
    #[token("]")]
    RBracket,
    #[token(";")]
    Semi,
    #[token(":")]
    Colon,
    #[token("::")]
    ColonColon,
    #[token(",")]
    Comma,
    #[token(".")]
    Dot,
    #[token("...")]
    Ellipsis,
    #[token("->")]
    Arrow,
    #[token("?")]
    Question,
    #[token("|")]
    Pipe,
    #[token("&")]
    Amp,

    /// Tacked on once by [`crate::lex`], logos never makes one.
    Eof,
}

impl Token {
    /// How the token shows up in error messages.
    pub fn describe(self) -> &'static str {
        use Token::*;
        match self {
            Number(_) => "number",
            String => "string",
            InterpString => "interpolated string",
            Ident => "identifier",
            Comment => "comment",
            And => "`and`",
            Break => "`break`",
            Do => "`do`",
            Else => "`else`",
            Elseif => "`elseif`",
            End => "`end`",
            False => "`false`",
            For => "`for`",
            Function => "`function`",
            If => "`if`",
            In => "`in`",
            Local => "`local`",
            Nil => "`nil`",
            Not => "`not`",
            Or => "`or`",
            Repeat => "`repeat`",
            Return => "`return`",
            Then => "`then`",
            True => "`true`",
            Until => "`until`",
            While => "`while`",
            Plus => "`+`",
            Minus => "`-`",
            Star => "`*`",
            Slash => "`/`",
            SlashSlash => "`//`",
            Percent => "`%`",
            Caret => "`^`",
            Hash => "`#`",
            At => "`@`",
            DotDot => "`..`",
            EqEq => "`==`",
            NotEq => "`~=`",
            Lt => "`<`",
            LtEq => "`<=`",
            Gt => "`>`",
            GtEq => "`>=`",
            Eq => "`=`",
            PlusEq => "`+=`",
            MinusEq => "`-=`",
            StarEq => "`*=`",
            SlashEq => "`/=`",
            SlashSlashEq => "`//=`",
            PercentEq => "`%=`",
            CaretEq => "`^=`",
            DotDotEq => "`..=`",
            LParen => "`(`",
            RParen => "`)`",
            LBrace => "`{`",
            RBrace => "`}`",
            LBracket => "`[`",
            RBracket => "`]`",
            Semi => "`;`",
            Colon => "`:`",
            ColonColon => "`::`",
            Comma => "`,`",
            Dot => "`.`",
            Ellipsis => "`...`",
            Arrow => "`->`",
            Question => "`?`",
            Pipe => "`|`",
            Amp => "`&`",
            Eof => "end of input",
        }
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.describe())
    }
}

fn interp_string(lex: &mut Lexer<'_, Token>) -> Result<(), LexErrorKind> {
    match crate::interp::scan(lex.remainder().as_bytes()) {
        Ok(n) => {
            lex.bump(n);
            Ok(())
        }
        Err(crate::interp::ScanError::Unterminated(n)) => {
            lex.bump(n.min(lex.remainder().len()));
            Err(LexErrorKind::UnterminatedString)
        }
    }
}

fn unterminated_string(_: &mut Lexer<'_, Token>) -> Result<(), LexErrorKind> {
    Err(LexErrorKind::UnterminatedString)
}

/// Luau rejects numbers glued to identifier characters (`3abc`, `1..2`), so
/// swallow the whole run and the error covers all of it.
fn reject_trailing(lex: &mut Lexer<'_, Token>) -> Result<(), LexErrorKind> {
    let extra = lex
        .remainder()
        .bytes()
        .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'.')
        .count();
    if extra > 0 {
        lex.bump(extra);
        return Err(LexErrorKind::MalformedNumber);
    }
    Ok(())
}

fn decimal(lex: &mut Lexer<'_, Token>) -> Result<f64, LexErrorKind> {
    reject_trailing(lex)?;
    let cleaned: std::string::String = lex.slice().chars().filter(|&c| c != '_').collect();
    cleaned
        .parse::<f64>()
        .map_err(|_| LexErrorKind::MalformedNumber)
}

fn radix(lex: &mut Lexer<'_, Token>, radix: u32) -> Result<f64, LexErrorKind> {
    reject_trailing(lex)?;
    let digits: std::string::String = lex.slice()[2..].chars().filter(|&c| c != '_').collect();
    if digits.is_empty() {
        return Err(LexErrorKind::MalformedNumber);
    }
    // same as Luau, parse literals as u64 then widen
    u64::from_str_radix(&digits, radix)
        .map(|v| v as f64)
        .map_err(|_| LexErrorKind::MalformedNumber)
}

/// After an opening `[`, `[=`, `[==`, ... count the `=`s and find the matching
/// close bracket. Returns how many bytes past the opener the close ends.
fn find_long_close(rest: &str, level: usize) -> Option<usize> {
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b']' {
            let eqs = bytes[i + 1..].iter().take_while(|&&b| b == b'=').count();
            if eqs == level && bytes.get(i + 1 + eqs) == Some(&b']') {
                return Some(i + eqs + 2);
            }
        }
        i += 1;
    }
    None
}

fn long_string(lex: &mut Lexer<'_, Token>) -> Result<(), LexErrorKind> {
    let level = lex.slice().len() - 2;
    match find_long_close(lex.remainder(), level) {
        Some(n) => {
            lex.bump(n);
            Ok(())
        }
        None => Err(LexErrorKind::UnterminatedLongString),
    }
}

fn comment(lex: &mut Lexer<'_, Token>) -> FilterResult<(), LexErrorKind> {
    let rest = lex.remainder();
    // block comment, `--[[ ... ]]` or `--[==[ ... ]==]`
    if let Some(after) = rest.strip_prefix('[') {
        let level = after.bytes().take_while(|&b| b == b'=').count();
        if after.as_bytes().get(level) == Some(&b'[') {
            let opener = 2 + level;
            return match find_long_close(&rest[opener..], level) {
                Some(n) => {
                    lex.bump(opener + n);
                    FilterResult::Skip
                }
                None => {
                    lex.bump(rest.len());
                    FilterResult::Error(LexErrorKind::UnterminatedComment)
                }
            };
        }
    }
    // line comment (`--!strict` directives land here too)
    let len = rest.find('\n').unwrap_or(rest.len());
    lex.bump(len);
    FilterResult::Skip
}
