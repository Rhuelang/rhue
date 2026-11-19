use super::*;
use Token::*;

fn kinds(src: &str) -> Vec<Token> {
    let lexed = lex(src);
    assert!(
        lexed.errors.is_empty(),
        "unexpected errors: {:?}",
        lexed.errors
    );
    lexed.tokens.into_iter().map(|t| t.token).collect()
}

fn number(src: &str) -> f64 {
    match kinds(src).as_slice() {
        [Number(n), Eof] => *n,
        other => panic!("expected one number, got {other:?}"),
    }
}

fn error_kinds(src: &str) -> Vec<std::string::String> {
    lex(src).errors.into_iter().map(|d| d.message).collect()
}

#[test]
fn arithmetic_tokens() {
    assert_eq!(
        kinds("1 + 2 * (3 - 4) / 5 // 6 % 7 ^ 8"),
        vec![
            Number(1.0),
            Plus,
            Number(2.0),
            Star,
            LParen,
            Number(3.0),
            Minus,
            Number(4.0),
            RParen,
            Slash,
            Number(5.0),
            SlashSlash,
            Number(6.0),
            Percent,
            Number(7.0),
            Caret,
            Number(8.0),
            Eof
        ]
    );
}

#[test]
fn longest_match_on_operators() {
    assert_eq!(
        kinds("//= // / ..= ... .. . :: : -> ~= =="),
        vec![
            SlashSlashEq,
            SlashSlash,
            Slash,
            DotDotEq,
            Ellipsis,
            DotDot,
            Dot,
            ColonColon,
            Colon,
            Arrow,
            NotEq,
            EqEq,
            Eof
        ]
    );
}

#[test]
fn numbers() {
    assert_eq!(number("42"), 42.0);
    assert_eq!(number("3.25"), 3.25);
    assert_eq!(number("1."), 1.0);
    assert_eq!(number(".5"), 0.5);
    assert_eq!(number("1e3"), 1000.0);
    assert_eq!(number("2.5E-2"), 0.025);
    assert_eq!(number("1_000_000"), 1_000_000.0);
    assert_eq!(number("0xff"), 255.0);
    assert_eq!(number("0XDEAD_BEEF"), 3735928559.0);
    assert_eq!(number("0b1010"), 10.0);
    assert_eq!(number("0xffffffffffffffff"), u64::MAX as f64);
}

#[test]
fn malformed_numbers() {
    assert_eq!(error_kinds("3abc"), vec!["malformed number `3abc`"]);
    assert_eq!(error_kinds("1..2"), vec!["malformed number `1..2`"]);
    assert_eq!(error_kinds("0x"), vec!["malformed number `0x`"]);
    assert_eq!(
        error_kinds("0x1_0000_0000_0000_0000"),
        vec!["malformed number `0x1_0000_0000_0000_0000`"]
    );
}

#[test]
fn keywords_vs_identifiers() {
    assert_eq!(kinds("local x = nil"), vec![Local, Ident, Eq, Nil, Eof]);
    assert_eq!(
        kinds("ending endx _end type continue"),
        vec![Ident, Ident, Ident, Ident, Ident, Eof]
    );
    assert_eq!(
        kinds("true false not and or"),
        vec![True, False, Not, And, Or, Eof]
    );
}

#[test]
fn comments_are_skipped() {
    assert_eq!(
        kinds("1 -- one\n+ 2"),
        vec![Number(1.0), Plus, Number(2.0), Eof]
    );
    assert_eq!(kinds("--!strict\n1"), vec![Number(1.0), Eof]);
    assert_eq!(
        kinds("1 --[[ block\n comment ]] + 2"),
        vec![Number(1.0), Plus, Number(2.0), Eof]
    );
    assert_eq!(kinds("--[==[ has ]] inside ]==] 7"), vec![Number(7.0), Eof]);
    assert_eq!(kinds("-- trailing comment"), vec![Eof]);
    // `--[` without a second bracket is just a line comment
    assert_eq!(kinds("--[ not block\n3"), vec![Number(3.0), Eof]);
}

#[test]
fn strings() {
    assert_eq!(
        kinds(r#""hi" 'there' "esc\"aped""#),
        vec![String, String, String, Eof]
    );
    assert_eq!(
        kinds("[[long\nstring]] [==[with ]] inside]==]"),
        vec![String, String, Eof]
    );
    let lexed = lex(r#"x = "a\"b""#);
    assert_eq!(lexed.tokens[2].span.text(r#"x = "a\"b""#), r#""a\"b""#);
}

#[test]
fn string_errors() {
    assert_eq!(error_kinds("\"abc\n"), vec!["unterminated string"]);
    assert_eq!(
        error_kinds("[[never closed"),
        vec!["unterminated long string"]
    );
    assert_eq!(
        error_kinds("--[[never closed"),
        vec!["unterminated block comment"]
    );
}

#[test]
fn unexpected_character_is_reported_and_lexing_continues() {
    let lexed = lex("1 @ 2");
    assert_eq!(lexed.errors.len(), 1);
    assert_eq!(lexed.errors[0].message, "unexpected character `@`");
    assert_eq!(lexed.errors[0].span(), rhue_common::Span::new(2, 3));
    let toks: Vec<_> = lexed.tokens.iter().map(|t| t.token).collect();
    assert_eq!(toks, vec![Number(1.0), Number(2.0), Eof]);
}

#[test]
fn spans_are_byte_offsets() {
    let lexed = lex("  12 +\n  3");
    let spans: Vec<_> = lexed
        .tokens
        .iter()
        .map(|t| (t.span.start, t.span.end))
        .collect();
    assert_eq!(spans, vec![(2, 4), (5, 6), (9, 10), (10, 10)]);
}
