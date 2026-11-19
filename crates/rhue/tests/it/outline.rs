//! Milestone 17: the stuff editor features are built from. Outlines (symbols,
//! classified tokens, module fields used and defined), inlay hints, and the
//! fixes the checker attaches to its errors.

use rhue::{Base, Engine, HintKind, SymbolKind, TokenKind};

fn slice(src: &str, s: rhue::Span) -> &str {
    &src[s.start as usize..s.end as usize]
}

/// Applies a fix's edits (from the end, so spans stay put).
fn apply(src: &str, fix: &rhue::Diagnostic, i: usize) -> String {
    let mut edits = fix.fixes[i].edits.clone();
    edits.sort_by_key(|(s, _)| std::cmp::Reverse(s.start));
    let mut out = src.to_string();
    for (s, t) in edits {
        out.replace_range(s.start as usize..s.end as usize, &t);
    }
    out
}

fn errors(src: &str) -> Vec<rhue::Diagnostic> {
    Engine::new().analyze(src, None).diagnostics
}

#[test]
fn misspellings_get_fixes() {
    let cases = [
        // (source, the fix's help, the fixed line)
        (
            "local p = { name = 'a' }\nprint(p.nmae)",
            "did you mean 'name'?",
            "print(p.name)",
        ),
        ("pirnt(1)", "did you mean 'print'?", "print(1)"),
        (
            "local s = 'x'\nprint(s:uppr())",
            "did you mean 'upper'?",
            "print(s:upper())",
        ),
        (
            "print(math.flor(1.5))",
            "did you mean 'floor'?",
            "print(math.floor(1.5))",
        ),
        (
            "local counter = 1\nprint(countr + 1)",
            "did you mean 'counter'?",
            "print(counter + 1)",
        ),
        (
            "type P = { x: number }\nlocal p: P = { x = 1 }\np.xx = 2",
            "did you mean 'x'?",
            "p.x = 2",
        ),
    ];
    for (body, help, fixed) in cases {
        let src = format!("--!strict\n{body}\n");
        let errs = errors(&src);
        let e = errs
            .iter()
            .find(|e| !e.fixes.is_empty())
            .unwrap_or_else(|| panic!("{body}: {errs:?}"));
        assert_eq!(e.help.as_deref(), Some(help), "{body}");
        let out = apply(&src, e, 0);
        assert!(out.contains(fixed), "{body} -> {out}");
        assert!(errors(&out).is_empty(), "{out}: {:?}", errors(&out));
    }
    // `t["x"]` isn't a name to replace, nothing close is no suggestion
    for body in [
        "local p = { name = 'a' }\nprint(p['nmae'])",
        "print(zzzqqq)",
    ] {
        let errs = errors(&format!("--!strict\n{body}\n"));
        assert!(
            !errs.is_empty() && errs.iter().all(|e| e.fixes.is_empty()),
            "{body}"
        );
    }
    // the CLI shows the help
    let err = Engine::new()
        .check("t.rhue", "--!strict\npirnt(1)")
        .unwrap_err();
    assert!(err.render(false).contains("did you mean 'print'?"));
}

#[test]
fn nil_errors_get_an_assert() {
    let src = "--!strict
type Node = { next: Node?, value: number }
local function first(): Node? return nil end
local function call(): (() -> number)? return nil end
local n = first()
print(n.value, call()(), n:value())
";
    let errs = errors(src);
    let fixed: Vec<String> = errs
        .iter()
        .filter(|e| e.message.contains("could be nil"))
        .map(|e| {
            assert_eq!(e.fixes[0].title, "Assert that it isn't nil");
            let out = apply(src, e, 0);
            out.lines().last().unwrap().to_string()
        })
        .collect();
    assert!(
        fixed.contains(&"print(assert(n).value, call()(), n:value())".to_string()),
        "{fixed:?}"
    );
    assert!(
        fixed.contains(&"print(n.value, assert(call())(), n:value())".to_string()),
        "{fixed:?}"
    );
    assert!(
        fixed.contains(&"print(n.value, call()(), assert(n):value())".to_string()),
        "{fixed:?}"
    );
}

#[test]
fn assert_returns_its_argument_without_nil() {
    let ok = |src: &str| Engine::new().check("t.rhue", &format!("--!strict\n{src}"));
    ok("local function f(): number? return 1 end\nlocal n: number = assert(f())").unwrap();
    ok("local function f(): number? return 1 end\nlocal n: number = assert(f(), 'no number')")
        .unwrap();
    assert!(ok("local function f(): number? return 1 end\nlocal s: string = assert(f())").is_err());
    // a local named `assert` is just a function
    assert!(ok("local function assert<T>(x: T): T return x end\nlocal function f(): number? return 1 end\nlocal n: number = assert(f())").is_err());
}

#[test]
fn hints_are_the_inferred_types() {
    let src = "--!strict
local xs = { 1, 2 }
local function count(t: {number}) return #t end
for i, x in xs do end
local c = count(xs)
local _ = c
table.sort(xs, function(a, b) return a > b end)
local named = { label = 'x' }
";
    let a = Engine::new().analyze(src, None);
    let shown: Vec<(String, &str, HintKind, bool)> = a
        .hints
        .iter()
        .map(|h| {
            let before = &src[..h.offset as usize];
            let word = before
                .rfind(|c: char| !(c.is_alphanumeric() || c == '_' || c == ')'))
                .map_or(before, |i| &before[i + 1..]);
            (
                word.to_string(),
                h.label.as_str(),
                h.kind,
                h.insert.is_some(),
            )
        })
        .collect();
    let expect = [
        ("xs", ": {number}", HintKind::Type),
        (")", ": number", HintKind::Return),
        ("i", ": number", HintKind::Type),
        ("x", ": number", HintKind::Type),
        ("c", ": number", HintKind::Type),
        ("a", ": number", HintKind::Type),
        ("b)", ": boolean", HintKind::Return),
        ("named", ": { label: string }", HintKind::Type),
    ];
    for (w, l, k) in expect {
        assert!(
            shown.iter().any(|s| s.0 == w && s.1 == l && s.2 == k),
            "{w} {l} in {shown:?}"
        );
    }
    assert!(!shown.iter().any(|s| s.0 == "_"), "{shown:?}");
    assert!(shown.iter().all(|s| s.3));
    // inserting every hint's annotation still checks
    let mut out = src.to_string();
    for h in a.hints.iter().rev() {
        out.insert_str(h.offset as usize, h.insert.as_ref().unwrap());
    }
    Engine::new()
        .check("t.rhue", &out)
        .unwrap_or_else(|e| panic!("{out}\n{}", e.render(false)));
}

#[test]
fn outlines_list_symbols() {
    let src = "local shapes = require('shapes')
type Pt = { x: number }
local M = { version = 1, helper = function() end }
function M.new(x) return { x = x } end
function M:describe() end
function M.sub.deep() end
M.count = 0
Global = {}
function Global.run() end
if true then
  local function nested() end
end
return M
";
    let o = rhue::outline(src);
    let flat: Vec<String> = o
        .symbols
        .iter()
        .flat_map(|s| {
            std::iter::once(format!("{:?} {}", s.kind, s.name)).chain(
                s.children
                    .iter()
                    .map(move |c| format!("{} > {:?} {}", s.name, c.kind, c.name)),
            )
        })
        .collect();
    assert_eq!(
        flat,
        [
            "Module shapes",
            "Type Pt",
            "Variable M",
            "M > Field version",
            "M > Method helper",
            "M > Method new",
            "M > Method describe",
            "M > Method sub.deep",
            "M > Field count",
            "Variable Global",
            "Global > Method run",
            "Function nested",
        ]
    );
    for s in &o.symbols {
        assert!(s.span.start <= s.name_span.start && s.name_span.end <= s.span.end);
        assert_eq!(slice(src, s.name_span), s.name);
    }
}

#[test]
fn outlines_track_module_fields() {
    let src = "local lib = require('lib')
local M = { a = 1 }
function M.b() return lib.f(lib.g) end
M.c = require('other').h
local function d() end
M.d = d
print(lib:m(), M.a)
return M
";
    let o = rhue::outline(src);
    assert_eq!(o.requires.len(), 1);
    assert_eq!(slice(src, o.requires[0].0), "lib");
    assert_eq!(o.requires[0].1, "lib");
    let uses: Vec<String> = o
        .field_uses
        .iter()
        .map(|u| {
            let base = match &u.base {
                Base::Local(s) => slice(src, *s).to_string(),
                Base::Module(m) => format!("require({m})"),
                _ => unreachable!("a new kind of base"),
            };
            assert_eq!(slice(src, u.span), u.name);
            format!("{base}.{}", u.name)
        })
        .collect();
    assert_eq!(
        uses,
        [
            "M.b",
            "lib.f",
            "lib.g",
            "M.c",
            "require(other).h",
            "M.d",
            "lib.m",
            "M.a"
        ]
    );
    let e = &o.exports;
    assert_eq!(e.local.map(|s| slice(src, s)), Some("M"));
    let fields: Vec<(&str, &str)> = e
        .fields
        .iter()
        .map(|(n, s)| (n.as_str(), slice(src, *s)))
        .collect();
    assert_eq!(fields, [("a", "a"), ("b", "b"), ("c", "c"), ("d", "d")]);

    // a returned table literal: a local's field points at the local
    let src = "local function ring() end\nreturn { ring = ring, size = 3 }\n";
    let o = rhue::outline(src);
    let fields: Vec<(&str, rhue::Span)> = o
        .exports
        .fields
        .iter()
        .map(|(n, s)| (n.as_str(), *s))
        .collect();
    assert_eq!(fields[0].0, "ring");
    assert_eq!(fields[0].1.start, src.find("ring").unwrap() as u32);
    assert_eq!(slice(src, fields[1].1), "size");
}

#[test]
fn outline_tokens_are_ordered_and_classified() {
    let src = "local t = { f = function(self, n) return n end }\nlocal r = t:f(1) + t.f(t, 2)\n";
    let o = rhue::outline(src);
    let mut end = 0;
    for t in &o.tokens {
        assert!(t.span.start >= end && t.span.end > t.span.start, "{t:?}");
        end = t.span.end;
    }
    let kinds: Vec<(&str, TokenKind)> = o
        .tokens
        .iter()
        .map(|t| (slice(src, t.span), t.kind))
        .collect();
    assert!(kinds.contains(&("f", TokenKind::Method)));
    assert!(kinds.contains(&("self", TokenKind::Parameter)));
    assert!(kinds.contains(&("1", TokenKind::Number)));
    assert!(kinds.contains(&("return", TokenKind::Keyword)));
    assert_eq!(
        o.symbols.iter().map(|s| s.kind).collect::<Vec<_>>(),
        [SymbolKind::Variable, SymbolKind::Variable]
    );
}
