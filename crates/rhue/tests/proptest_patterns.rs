//! Property test: random Lua patterns (classes, sets, `.`, all four
//! quantifiers, captures, anchors) run through `string.find` and compared
//! with the same pattern translated to a `regex` crate regex. Without
//! alternation, Lua's backtracking matcher and the regex crate's
//! leftmost-first semantics agree on where the first match is and what each
//! group captured, so any difference is a bug in one of them (ours).

use proptest::prelude::*;
use regex::bytes::Regex;
use rhue::{Engine, Value};

#[derive(Clone, Debug)]
enum Atom {
    Lit(u8),
    Any,
    Class(u8),
    Set { negated: bool, items: Vec<SetItem> },
}

#[derive(Clone, Debug)]
enum SetItem {
    Lit(u8),
    Range(u8, u8),
    Class(u8),
}

#[derive(Clone, Debug)]
enum Elem {
    One(Atom, Option<u8>),
    Group(Vec<(Atom, Option<u8>)>),
}

#[derive(Clone, Debug)]
struct Pat {
    anchor_start: bool,
    elems: Vec<Elem>,
    anchor_end: bool,
}

const ALPHABET: &[u8] = b"ab1 .c";

fn class_regex(c: u8) -> String {
    let body = match c.to_ascii_lowercase() {
        b'a' => "A-Za-z",
        b'd' => "0-9",
        b's' => r"\t\n\x0B\x0C\r ",
        b'w' => "0-9A-Za-z",
        b'l' => "a-z",
        b'p' => r"!-/:-@\[-`{-~",
        _ => unreachable!(),
    };
    if c.is_ascii_uppercase() {
        format!("[^{body}]")
    } else {
        format!("[{body}]")
    }
}

impl Atom {
    fn lua(&self) -> String {
        match self {
            Atom::Lit(c) if b"^$*+?.([%-]".contains(c) => format!("%{}", *c as char),
            Atom::Lit(c) => (*c as char).to_string(),
            Atom::Any => ".".into(),
            Atom::Class(c) => format!("%{}", *c as char),
            Atom::Set { negated, items } => {
                let mut s = String::from("[");
                if *negated {
                    s.push('^');
                }
                for i in items {
                    match i {
                        SetItem::Lit(c) if b"^]%-".contains(c) => {
                            s.push_str(&format!("%{}", *c as char))
                        }
                        SetItem::Lit(c) => s.push(*c as char),
                        SetItem::Range(a, b) => {
                            s.push_str(&format!("{}-{}", *a as char, *b as char))
                        }
                        SetItem::Class(c) => s.push_str(&format!("%{}", *c as char)),
                    }
                }
                s.push(']');
                s
            }
        }
    }

    fn regex(&self) -> String {
        match self {
            Atom::Lit(c) => regex::escape(&(*c as char).to_string()),
            Atom::Any => "(?s:.)".into(),
            Atom::Class(c) => class_regex(*c),
            Atom::Set { negated, items } => {
                // A union of nested classes: `[[a][0-9]]`, or `[^...]`.
                let parts: Vec<String> = items
                    .iter()
                    .map(|i| match i {
                        SetItem::Lit(c) => {
                            format!("[{}]", regex::escape(&(*c as char).to_string()))
                        }
                        SetItem::Range(a, b) => format!("[{}-{}]", *a as char, *b as char),
                        SetItem::Class(c) => class_regex(*c),
                    })
                    .collect();
                let union = parts
                    .iter()
                    .map(|p| p[1..p.len() - 1].to_string())
                    .map(|body| format!("[{body}]"))
                    .collect::<Vec<_>>()
                    .join("||");
                if *negated {
                    format!("[^{}]", strip_union(&union))
                } else {
                    format!("[{}]", strip_union(&union))
                }
            }
        }
    }
}

/// `[x]||[y]` → `[x][y]` inside a class: a union of nested classes.
fn strip_union(u: &str) -> String {
    u.replace("||", "")
}

fn quant_lua(q: Option<u8>) -> String {
    q.map(|c| (c as char).to_string()).unwrap_or_default()
}

fn quant_regex(q: Option<u8>) -> &'static str {
    match q {
        None => "",
        Some(b'*') => "*",
        Some(b'+') => "+",
        Some(b'-') => "*?",
        Some(b'?') => "?",
        _ => unreachable!(),
    }
}

impl Pat {
    fn lua(&self) -> String {
        let mut s = String::new();
        if self.anchor_start {
            s.push('^');
        }
        for e in &self.elems {
            match e {
                Elem::One(a, q) => s.push_str(&(a.lua() + &quant_lua(*q))),
                Elem::Group(items) => {
                    s.push('(');
                    for (a, q) in items {
                        s.push_str(&(a.lua() + &quant_lua(*q)));
                    }
                    s.push(')');
                }
            }
        }
        if self.anchor_end {
            s.push('$');
        }
        s
    }

    fn regex(&self) -> String {
        let mut s = String::from("(?-u)");
        if self.anchor_start {
            s.push_str(r"\A");
        }
        for e in &self.elems {
            match e {
                Elem::One(a, q) => s.push_str(&format!("(?:{}){}", a.regex(), quant_regex(*q))),
                Elem::Group(items) => {
                    s.push('(');
                    for (a, q) in items {
                        s.push_str(&format!("(?:{}){}", a.regex(), quant_regex(*q)));
                    }
                    s.push(')');
                }
            }
        }
        if self.anchor_end {
            s.push_str(r"\z");
        }
        s
    }
}

fn atom() -> impl Strategy<Value = Atom> {
    let lit = prop::sample::select(ALPHABET.to_vec());
    let class = prop::sample::select(b"adswlpADSWLP".to_vec());
    let item = prop_oneof![
        lit.clone().prop_map(SetItem::Lit),
        (
            prop::sample::select(b"ab1".to_vec()),
            prop::sample::select(b"bcz9".to_vec())
        )
            .prop_filter("ordered range", |(a, b)| a <= b)
            .prop_map(|(a, b)| SetItem::Range(a, b)),
        prop::sample::select(b"adsw".to_vec()).prop_map(SetItem::Class),
    ];
    prop_oneof![
        4 => lit.prop_map(Atom::Lit),
        1 => Just(Atom::Any),
        2 => class.prop_map(Atom::Class),
        2 => (any::<bool>(), prop::collection::vec(item, 1..4))
            .prop_map(|(negated, items)| Atom::Set { negated, items }),
    ]
}

fn quant() -> impl Strategy<Value = Option<u8>> {
    prop_oneof![
        3 => Just(None),
        1 => Just(Some(b'*')),
        1 => Just(Some(b'+')),
        1 => Just(Some(b'-')),
        1 => Just(Some(b'?')),
    ]
}

fn pat() -> impl Strategy<Value = Pat> {
    let elem = prop_oneof![
        3 => (atom(), quant()).prop_map(|(a, q)| Elem::One(a, q)),
        1 => prop::collection::vec((atom(), quant()), 1..3).prop_map(Elem::Group),
    ];
    (
        any::<bool>(),
        prop::collection::vec(elem, 1..5),
        any::<bool>(),
    )
        .prop_map(|(anchor_start, elems, anchor_end)| Pat {
            anchor_start,
            elems,
            anchor_end,
        })
}

fn subject() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(ALPHABET.to_vec()), 0..12)
        .prop_map(|b| String::from_utf8(b).unwrap())
}

fn show(v: &Value) -> String {
    match v {
        Value::String(s) => format!("{s:?}"),
        other => other.to_string(),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2500))]

    #[test]
    fn find_agrees_with_regex(p in pat(), s in subject()) {
        let lua = p.lua();
        let re = Regex::new(&p.regex()).unwrap_or_else(|e| panic!("{}: {e}", p.regex()));
        let expected: Vec<String> = match re.captures(s.as_bytes()) {
            None => vec!["nil".into()],
            Some(c) => {
                let whole = c.get(0).unwrap();
                let mut out = vec![(whole.start() + 1).to_string(), whole.end().to_string()];
                for g in c.iter().skip(1) {
                    let g = g.expect("groups always participate (no alternation)");
                    out.push(format!("{:?}", String::from_utf8_lossy(g.as_bytes())));
                }
                out
            }
        };
        let mut engine = Engine::new();
        engine.set_global("s", s.as_str());
        engine.set_global("p", lua.as_str());
        let got: Vec<String> = engine
            .eval("return string.find(s, p)")
            .map_err(|e| TestCaseError::fail(format!("{lua}: {}", e.render(false))))?
            .iter()
            .map(show)
            .collect();
        prop_assert_eq!(got, expected, "pattern {:?} (regex {}) on {:?}", lua, p.regex(), s);
    }
}
