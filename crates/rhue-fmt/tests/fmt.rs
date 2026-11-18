//! The formatter: golden output, idempotence, tokens and comments kept,
//! and (property) inputs that differ only in spacing format the same.

use proptest::prelude::*;
use rhue_fmt::{FormatError, FormatOptions, format};

fn fmt(src: &str) -> String {
    format(src, &FormatOptions::default()).unwrap_or_else(|e| panic!("{e:?}\n{src}"))
}

/// Real programs to format: the repository's examples and benchmarks, and
/// a deliberately messy file.
const CORPUS: &[&str] = &[
    include_str!("messy.rhue"),
    include_str!("../../../examples/arith.rhue"),
    include_str!("../../../examples/closures.rhue"),
    include_str!("../../../examples/loops.rhue"),
    include_str!("../../../examples/tables.rhue"),
    include_str!("../../../examples/typed.rhue"),
    include_str!("../../../bench/classes.rhue"),
    include_str!("../../../bench/closures.rhue"),
    include_str!("../../../bench/coroutines.rhue"),
    include_str!("../../../bench/fib.rhue"),
    include_str!("../../../bench/host.rhue"),
    include_str!("../../../bench/loops.rhue"),
    include_str!("../../../bench/methods.rhue"),
    include_str!("../../../bench/natives.rhue"),
    include_str!("../../../bench/patterns.rhue"),
    include_str!("../../../bench/strings.rhue"),
    include_str!("../../../bench/tables.rhue"),
];

#[test]
fn golden() {
    assert_eq!(
        fmt(include_str!("messy.rhue")),
        include_str!("messy.expected.rhue")
    );
}

#[test]
fn idempotent_on_the_corpus() {
    for src in CORPUS {
        let once = fmt(src);
        assert_eq!(fmt(&once), once);
    }
}

#[test]
fn spacing_rules() {
    for (src, want) in [
        ("local x=1+2*3", "local x = 1 + 2 * 3\n"),
        ("f(a,b)", "f(a, b)\n"),
        ("local t={a=1,[k]=2}", "local t = { a = 1, [k] = 2 }\n"),
        ("local t = {  }", "local t = {}\n"),
        ("x=-y z=- -y w=a- -b", "x = -y z = - -y w = a - -b\n"),
        ("local n=#t", "local n = #t\n"),
        ("obj:method(1) obj:m 'x'", "obj:method(1) obj:m 'x'\n"),
        (
            "local function f(x:number):string return '' end",
            "local function f(x: number): string return '' end\n",
        ),
        (
            "local function id<T>(x:T):T return x end",
            "local function id<T>(x: T): T return x end\n",
        ),
        (
            "type Pair<A,B> ={a:A,b:B}",
            "type Pair<A, B> = { a: A, b: B }\n",
        ),
        ("if a<b and c>d then end", "if a < b and c > d then end\n"),
        ("local s=a..b", "local s = a .. b\n"),
        ("x+=1 y..=z", "x += 1 y ..= z\n"),
        ("local f: (number)->string", "local f: (number) -> string\n"),
        ("local u: string|number?", "local u: string | number?\n"),
        ("local v=(x::any)", "local v = (x :: any)\n"),
        (
            "print 'hi' print'hi' f{1} f {1}",
            "print 'hi' print'hi' f{ 1 } f { 1 }\n",
        ),
        (
            "local a , b = ... ; return a",
            "local a, b = ...; return a\n",
        ),
        ("x = t [1] [2] . y", "x = t[1][2].y\n"),
        ("local x = 1 -- note   ", "local x = 1 -- note\n"),
        ("local x = --[[inline]] 1", "local x = --[[inline]] 1\n"),
    ] {
        assert_eq!(fmt(src), want, "{src}");
    }
}

#[test]
fn indentation() {
    let src = "local function f()\nif x then\nfor i=1,3 do\nprint(i)\nend\nelseif y then\nwhile z do z=false end\nelse\nrepeat\nn+=1\nuntil n>3\nend\nend\n";
    assert_eq!(
        fmt(src),
        "local function f()\n\tif x then\n\t\tfor i = 1, 3 do\n\t\t\tprint(i)\n\t\tend\n\telseif y then\n\t\twhile z do z = false end\n\telse\n\t\trepeat\n\t\t\tn += 1\n\t\tuntil n > 3\n\tend\nend\n"
    );
    // Brackets opened on one line indent once; `end)` goes back.
    let src = "foo(function()\nbar({\na = 1,\n})\nend, function()\nbaz()\nend)\n";
    assert_eq!(
        fmt(src),
        "foo(function()\n\tbar({\n\t\ta = 1,\n\t})\nend, function()\n\tbaz()\nend)\n"
    );
    // Continuations and multi-line if-expressions.
    let src = "local s = a\n.. b\nlocal t = x +\ny\nlocal v = if c then 1\nelse 2\nprint(v)\n";
    assert_eq!(
        fmt(src),
        "local s = a\n\t.. b\nlocal t = x +\n\ty\nlocal v = if c then 1\n\telse 2\nprint(v)\n"
    );
    // Long strings and block comments are left exactly as written.
    let src = "local s = [[\n  a\n    b]]\n--[[\n   keep\n]]\nx = 1\n";
    assert_eq!(fmt(src), src);
    // Spaces instead of tabs.
    let four = FormatOptions {
        indent: "    ".into(),
    };
    assert_eq!(format("do\nx()\nend", &four).unwrap(), "do\n    x()\nend\n");
}

#[test]
fn blank_lines_and_crlf() {
    assert_eq!(fmt("\n\n\nx = 1\n\n\n\ny = 2\n\n\n"), "x = 1\n\ny = 2\n");
    assert_eq!(fmt("x = 1\r\ny = 2\r\n"), "x = 1\ny = 2\n");
    assert_eq!(fmt(""), "");
    assert_eq!(fmt("-- only a comment"), "-- only a comment\n");
}

#[test]
fn invalid_code_is_not_formatted() {
    match format("local = 1", &FormatOptions::default()) {
        Err(FormatError::Syntax(d)) => assert!(!d.is_empty()),
        other => panic!("{other:?}"),
    }
}

/// Re-spaces a program without changing its tokens: between two tokens on
/// one line, any amount of space (none where the tokens wouldn't merge);
/// elsewhere every run of spaces and tabs gets a random width. Call sugar
/// (`f "x"`, `f {..}`) keeps whether it had a space, which the formatter
/// also keeps.
fn perturb(src: &str, seeds: &[u8]) -> String {
    use rhue_lexer::Token;
    let mut k = 0;
    let mut next = || {
        k += 1;
        seeds[k % seeds.len()] as usize
    };
    let lexed = rhue_lexer::lex(src);
    let mut out = String::new();
    let mut prev_end = 0;
    let mut prev_tok: Option<(Token, &str)> = None;
    for t in &lexed.tokens {
        let (a, b) = (t.span.start as usize, t.span.end as usize);
        let gap = &src[prev_end..a];
        let text = &src[a..b];
        if gap.contains('\n') || gap.contains("--") || prev_tok.is_none() {
            // Keep line breaks and comments; vary the spaces around them.
            let mut i = 0;
            let mut in_ws = false;
            while i < gap.len() {
                let rest = &gap[i..];
                if let Some(body) = rest.strip_prefix("--") {
                    // A comment, copied as is.
                    let level = body
                        .strip_prefix('[')
                        .map(|r| (r.bytes().take_while(|&b| b == b'=').count(), r))
                        .filter(|(l, r)| r.as_bytes().get(*l) == Some(&b'['));
                    let len = match level {
                        Some((l, _)) => {
                            let close = format!("]{}]", "=".repeat(l));
                            rest.find(&close).map_or(rest.len(), |e| e + close.len())
                        }
                        None => rest.find('\n').unwrap_or(rest.len()),
                    };
                    out.push_str(&rest[..len]);
                    i += len;
                    in_ws = false;
                    continue;
                }
                let c = rest.chars().next().unwrap();
                if c == ' ' || c == '\t' {
                    if !in_ws {
                        for _ in 0..1 + next() % 3 {
                            out.push(if next() % 2 == 0 { ' ' } else { '\t' });
                        }
                        in_ws = true;
                    }
                } else {
                    in_ws = false;
                    out.push(c);
                }
                i += c.len_utf8();
            }
        } else if let Some((pt, ptext)) = prev_tok {
            let sugar = matches!(pt, Token::Ident | Token::RParen | Token::RBracket)
                && matches!(t.token, Token::String | Token::LBrace);
            let n = if sugar {
                usize::from(!gap.is_empty())
            } else {
                next() % 4
            };
            let glued = format!("{ptext}{text}");
            let safe = rhue_lexer::lex(&glued).tokens.len() == 3;
            let n = if n == 0 && !safe { 1 } else { n };
            out.push_str(&" ".repeat(n));
        }
        if t.token == Token::Eof {
            break;
        }
        out.push_str(text);
        prev_tok = Some((t.token, text));
        prev_end = b;
    }
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    #[test]
    fn spacing_doesnt_matter(which in 0..CORPUS.len(), seeds in prop::collection::vec(any::<u8>(), 1..64)) {
        let src = CORPUS[which];
        let messy = perturb(src, &seeds);
        prop_assert_eq!(fmt(&messy), fmt(src), "{}", messy);
    }
}
