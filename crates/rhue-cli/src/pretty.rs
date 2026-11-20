//! Values the way the REPL (and test failures) show them: tables spelled out,
//! nested ones too, strings quoted inside them.

use std::fmt::Write as _;

use rhue::{Context, Table, Value};

/// Longest a line gets before a table spreads over several.
const WIDTH: usize = 80;
/// How deep nested tables get shown.
const DEPTH: usize = 3;
/// How many entries of a table get shown.
const ENTRIES: usize = 50;

/// `v` for display. Top-level strings show as-is, strings inside tables get
/// quoted.
pub fn show(cx: &mut Context<'_>, v: &Value) -> String {
    match v {
        Value::String(s) => s.to_string(),
        _ => Printer {
            cx,
            seen: Vec::new(),
        }
        .value(v, 0),
    }
}

struct Printer<'a, 'b> {
    cx: &'a mut Context<'b>,
    /// The tables being printed, outermost first (to catch cycles).
    seen: Vec<Table>,
}

/// A string as a Rhue literal.
fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{{{:x}}}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Whether `s` can be a key without brackets (`name = 1`).
fn is_name(s: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "if", "in",
        "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
    ];
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !KEYWORDS.contains(&s)
}

impl Printer<'_, '_> {
    fn value(&mut self, v: &Value, depth: usize) -> String {
        match v {
            Value::String(s) => quote(s),
            Value::Table(t) => self.table(t, depth),
            other => other.to_string(),
        }
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "a whole, non-negative number checked first"
    )]
    fn table(&mut self, t: &Table, depth: usize) -> String {
        // a `__tostring` says how it wants to look
        if let Ok(s) = self
            .cx
            .call_global::<String>("tostring", Value::Table(t.clone()))
            && !s.starts_with("table: ")
        {
            return s;
        }
        if self.seen.contains(t) {
            return "<cycle>".into();
        }
        let mut entries = self.cx.pairs(t);
        if entries.is_empty() {
            return "{}".into();
        }
        if depth >= DEPTH {
            return "{...}".into();
        }
        let n = self.cx.len(t);
        // array part in order, then names sorted, then the rest
        let index = |k: &Value| match k {
            Value::Number(x) if x.fract() == 0.0 && *x >= 1.0 && *x <= n as f64 => {
                Some(*x as usize)
            }
            _ => None,
        };
        entries.sort_by(|(a, _), (b, _)| {
            let rank = |k: &Value| match (index(k), k) {
                (Some(i), _) => (0, i, String::new()),
                (None, Value::String(s)) => (1, 0, s.to_string()),
                (None, other) => (2, 0, other.to_string()),
            };
            rank(a).cmp(&rank(b))
        });
        let more = entries.len().saturating_sub(ENTRIES);
        entries.truncate(ENTRIES);
        self.seen.push(t.clone());
        let mut items: Vec<String> = Vec::with_capacity(entries.len() + 1);
        for (k, v) in &entries {
            let value = self.value(v, depth + 1);
            items.push(match (index(k), k) {
                (Some(_), _) => value,
                (None, Value::String(s)) if is_name(s) => format!("{s} = {value}"),
                (None, k) => format!("[{}] = {value}", self.value(k, depth + 1)),
            });
        }
        self.seen.pop();
        if more > 0 {
            items.push(format!("... ({more} more)"));
        }
        let one_line = format!("{{ {} }}", items.join(", "));
        if one_line.len() <= WIDTH && !one_line.contains('\n') {
            return one_line;
        }
        let pad = "  ".repeat(depth + 1);
        let close = "  ".repeat(depth);
        let mut out = String::from("{\n");
        for item in items {
            let _ = writeln!(out, "{pad}{item},");
        }
        out.push_str(&close);
        out.push('}');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(src: &str) -> String {
        let mut e = rhue::Engine::new();
        let v = e.eval(src).unwrap().remove(0);
        show(&mut e.context(), &v)
    }

    #[test]
    fn tables() {
        assert_eq!(shown("return { 1, 2, 'three' }"), r#"{ 1, 2, "three" }"#);
        assert_eq!(
            shown("return { b = 2, a = 1, [true] = 'x', ['two words'] = 3 }"),
            r#"{ a = 1, b = 2, ["two words"] = 3, [true] = "x" }"#
        );
        assert_eq!(shown("return { 10, 20, n = 2 }"), "{ 10, 20, n = 2 }");
        assert_eq!(shown("return {}"), "{}");
        assert_eq!(shown("return 'top level'"), "top level");
        assert_eq!(shown("return { s = 'a\\n\"b' }"), r#"{ s = "a\n\"b" }"#);
        assert_eq!(
            shown("local t = {}\nt.self = t\nreturn t"),
            "{ self = <cycle> }"
        );
        assert_eq!(
            shown("return { a = { b = { c = { d = 1 } } } }"),
            "{ a = { b = { c = {...} } } }"
        );
        assert_eq!(
            shown("return setmetatable({}, { __tostring = function() return 'Point(1, 2)' end })"),
            "Point(1, 2)"
        );
        // long ones spread out
        let long =
            shown("local t = {} for i = 1, 30 do t['key' .. i] = i end return { inner = t }");
        assert!(
            long.starts_with("{\n  inner = {\n    key1 = 1,\n"),
            "{long}"
        );
        assert!(
            shown("local t = {} for i = 1, 60 do t[i] = i end return t")
                .ends_with("... (10 more),\n}")
        );
    }
}
