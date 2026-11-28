//! Types written back out as Luau type syntax, for saving. A compiled
//! chunk carries the types of its globals and results this way, and
//! loading parses them again ([`Env::parse_type`](crate::Env::parse_type)).
//!
//! Only *detached* types are expected here (what a chunk exports, aliases
//! already expanded). Anything the syntax can't say gets simplified:
//!
//! - a table with a metatable becomes its fields plus the fields of its
//!   `__index` table (so a class instance keeps its methods, with `self`
//!   as `any` since the call supplies it). Operator metamethods get lost
//! - a table that might have unlisted fields (`self` in a method) lists
//!   the ones it has
//! - an alias reference or an error type becomes `any`

use crate::types::{FnType, Meta, NamedType, Pack, TableType, Type};

/// `t` as type syntax that parses back to (a simplified version of) it.
pub fn portable(t: &Type) -> String {
    let mut out = String::new();
    write_type(&mut out, t, 0, false);
    out
}

/// `t` as type syntax, keeping the names of aliases (`Signal<Shape>`), for
/// writing a definitions file that has the aliases too.
pub(crate) fn with_names(t: &Type) -> String {
    let mut out = String::new();
    write_type(&mut out, t, 0, true);
    out
}

/// A result pack as it goes after `->` (or `:` in a declaration), names kept.
pub(crate) fn pack_with_names(out: &mut String, p: &Pack) {
    write_pack(out, p, 0, true);
}

/// What goes after `...: ` in a declaration, `T` or `T...`, names kept.
pub(crate) fn variadic_with_names(out: &mut String, v: &Type) {
    match v.pack_var() {
        Some(g) => out.push_str(&g.display()),
        None => write_type(out, v, 1, true),
    }
}

/// How deep to follow types before giving up with `any` (detached types are
/// finite, but just in case).
const MAX_DEPTH: u32 = 32;

fn write_type(out: &mut String, t: &Type, depth: u32, names: bool) {
    if depth > MAX_DEPTH {
        out.push_str("any");
        return;
    }
    match t {
        Type::Named(n) if names => write_named(out, n, depth),
        Type::Any | Type::Error | Type::Named(_) => out.push_str("any"),
        Type::Unknown => out.push_str("unknown"),
        Type::Never => out.push_str("never"),
        Type::Nil => out.push_str("nil"),
        Type::Boolean => out.push_str("boolean"),
        Type::Number => out.push_str("number"),
        Type::String => out.push_str("string"),
        Type::BoolLit(b) => out.push_str(if *b { "true" } else { "false" }),
        Type::StrLit(s) => out.push_str(&quote(s)),
        Type::Class(name) => out.push_str(name),
        Type::Generic(g) => out.push_str(&g.display()),
        Type::Function(f) => write_fn(out, f, depth, names),
        Type::Table(tt) => write_table(out, tt, depth, names),
        Type::Union(ts) if ts.len() == 2 && ts.iter().any(|m| matches!(m, Type::Nil)) => {
            let other = ts
                .iter()
                .find(|m| !matches!(m, Type::Nil))
                .expect("two members");
            let wrap = matches!(
                other,
                Type::Function(_) | Type::Union(_) | Type::Intersection(_)
            );
            if wrap {
                out.push('(');
            }
            write_type(out, other, depth + 1, names);
            if wrap {
                out.push(')');
            }
            out.push('?');
        }
        Type::Union(ts) | Type::Intersection(ts) => {
            let sep = if matches!(t, Type::Union(_)) {
                " | "
            } else {
                " & "
            };
            for (i, m) in ts.iter().enumerate() {
                if i > 0 {
                    out.push_str(sep);
                }
                let wrap = matches!(
                    m,
                    Type::Function(_) | Type::Union(_) | Type::Intersection(_)
                );
                if wrap {
                    out.push('(');
                }
                write_type(out, m, depth + 1, names);
                if wrap {
                    out.push(')');
                }
            }
        }
    }
}

/// `Name<A, B>`, `Signal<Shape>` for an alias whose one parameter is a pack.
fn write_named(out: &mut String, n: &NamedType, depth: u32) {
    out.push_str(&n.alias.name);
    if n.args.is_empty() && n.packs.is_empty() {
        return;
    }
    out.push('<');
    let mut first = true;
    for a in &n.args {
        if !first {
            out.push_str(", ");
        }
        first = false;
        write_type(out, a, depth + 1, true);
    }
    let one_pack = n.alias.generics.len() == 1 && n.args.is_empty();
    for p in &n.packs {
        if !first {
            out.push_str(", ");
        }
        first = false;
        let bare = one_pack && p.variadic.is_none() && !p.types.is_empty();
        if !bare {
            out.push('(');
        }
        for (i, t) in p.types.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            write_type(out, t, depth + 1, true);
        }
        if let Some(v) = &p.variadic {
            if !p.types.is_empty() {
                out.push_str(", ");
            }
            write_variadic(out, v, depth, true);
        }
        if !bare {
            out.push(')');
        }
    }
    out.push('>');
}

fn write_fn(out: &mut String, f: &FnType, depth: u32, names: bool) {
    if !f.generics.is_empty() {
        let g: Vec<String> = f.generics.iter().map(|g| g.display()).collect();
        out.push('<');
        out.push_str(&g.join(", "));
        out.push('>');
    }
    out.push('(');
    for (i, p) in f.params.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        write_type(out, p, depth + 1, names);
    }
    if let Some(v) = &f.variadic {
        if !f.params.is_empty() {
            out.push_str(", ");
        }
        write_variadic(out, v, depth, names);
    }
    out.push_str(") -> ");
    write_pack(out, &f.rets, depth, names);
}

fn write_variadic(out: &mut String, v: &Type, depth: u32, names: bool) {
    match v.pack_var() {
        Some(g) => out.push_str(&g.display()),
        None => {
            out.push_str("...");
            write_type(out, v, depth + 1, names);
        }
    }
}

fn write_pack(out: &mut String, p: &Pack, depth: u32, names: bool) {
    match (&p.types[..], &p.variadic) {
        ([one], None) if !matches!(one, Type::Function(_)) => {
            write_type(out, one, depth + 1, names)
        }
        ([], Some(v)) => write_variadic(out, v, depth, names),
        _ => {
            out.push('(');
            for (i, t) in p.types.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_type(out, t, depth + 1, names);
            }
            if let Some(v) = &p.variadic {
                if !p.types.is_empty() {
                    out.push_str(", ");
                }
                write_variadic(out, v, depth, names);
            }
            out.push(')');
        }
    }
}

/// The fields a table can be read for: its own, then (through a metatable whose
/// `__index` is a table) the inherited ones, no metamethods. Inherited methods
/// take `self` as `any`, since a detached instance type only gets unrolled a
/// few levels and `self` (a parameter, so compared the other way round) would
/// otherwise demand the deeper unrolling.
fn fields(tt: &TableType, depth: u32) -> Vec<(String, Type)> {
    let mut out: Vec<(String, Type)> = Vec::new();
    for (n, t) in &tt.props {
        if tt.index_self && n.as_ref() == "__index" {
            continue;
        }
        out.push((n.to_string(), t.clone()));
    }
    if depth < MAX_DEPTH
        && let Some(Meta::Type(Type::Table(mt))) = &tt.meta
    {
        let index = if mt.index_self {
            Some((**mt).clone())
        } else {
            match mt.prop("__index") {
                Some(Type::Table(ix)) => Some((**ix).clone()),
                _ => None,
            }
        };
        if let Some(ix) = index {
            for (n, t) in fields(&ix, depth + 1) {
                if !n.starts_with("__") && !out.iter().any(|(m, _)| *m == n) {
                    let t = match &t {
                        // a method's `self`, an instance (open, or with a
                        // metatable)
                        Type::Function(f) if matches!(f.params.first(), Some(Type::Table(p)) if p.open || p.meta.is_some()) =>
                        {
                            let mut f = (**f).clone();
                            f.params[0] = Type::Any;
                            Type::function(f)
                        }
                        _ => t,
                    };
                    out.push((n, t));
                }
            }
        }
    }
    out
}

fn write_table(out: &mut String, tt: &TableType, depth: u32, names: bool) {
    let fields = fields(tt, depth);
    match (&fields[..], &tt.indexer) {
        ([], None) => {
            out.push_str("{}");
            return;
        }
        ([], Some((Type::Number, v))) => {
            out.push('{');
            write_type(out, v, depth + 1, names);
            out.push('}');
            return;
        }
        _ => {}
    }
    out.push_str("{ ");
    let mut first = true;
    for (n, t) in &fields {
        if !first {
            out.push_str(", ");
        }
        first = false;
        out.push_str(tt.access(n).prefix());
        if is_name(n) {
            out.push_str(n);
        } else {
            out.push('[');
            out.push_str(&quote(n));
            out.push(']');
        }
        out.push_str(": ");
        write_type(out, t, depth + 1, names);
    }
    if let Some((k, v)) = &tt.indexer {
        if !first {
            out.push_str(", ");
        }
        out.push('[');
        write_type(out, k, depth + 1, names);
        out.push_str("]: ");
        write_type(out, v, depth + 1, names);
    }
    out.push_str(" }");
}

const KEYWORDS: &[&str] = &[
    "and", "break", "continue", "do", "else", "elseif", "end", "export", "false", "for",
    "function", "if", "in", "local", "nil", "not", "or", "repeat", "return", "then", "true",
    "type", "typeof", "until", "while",
];

fn is_name(s: &str) -> bool {
    let mut cs = s.chars();
    cs.next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !KEYWORDS.contains(&s)
}

/// A string literal Rhue reads back as `s`.
fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::portable;
    use crate::Env;

    /// Type syntax that should print back exactly as written.
    #[test]
    fn written_types_round_trip() {
        let env = Env::new();
        for src in [
            "number",
            "string?",
            "nil | number | string",
            "\"on\" | \"off\"",
            "true",
            "{number}",
            "{}",
            "{ x: number, y: number }",
            "{ [string]: boolean }",
            "{ name: string, [number]: string }",
            "{ [\"two words\"]: number, [\"end\"]: string }",
            "{ read id: number, write log: string, name: string }",
            "(number, string) -> boolean",
            "() -> ()",
            "(...number) -> ...string",
            "(number, ...any) -> (string, number)",
            "<T>(T) -> T",
            "<T, U>({T}, (T) -> U) -> {U}",
            "<A..., R...>((A...) -> R..., A...) -> R...",
            "<T...>(number, T...) -> (string, T...)",
            "((number) -> string)?",
            "{ f: (number) -> ((string) -> boolean) }",
            "(number) -> (() -> number)",
            "\"quote\\\" back\\\\slash\\nnewline\"",
        ] {
            let t = env
                .parse_type(src)
                .unwrap_or_else(|| panic!("{src} parses"));
            let out = portable(&t);
            let again = env
                .parse_type(&out)
                .unwrap_or_else(|| panic!("{out} (from {src}) parses"));
            assert_eq!(portable(&again), out, "{src}");
            assert_eq!(out, src);
        }
    }

    #[test]
    fn what_syntax_cant_say_is_simplified() {
        use crate::types::{Meta, TableType, Type};
        // an instance, its fields and then its class's methods
        let class = Type::table(TableType {
            props: vec![
                ("__index".into(), Type::Any),
                ("bump".into(), Type::function(crate::FnType::any())),
                ("__add".into(), Type::function(crate::FnType::any())),
            ],
            index_self: true,
            ..TableType::default()
        });
        let instance = Type::table(TableType {
            props: vec![("n".into(), Type::Number)],
            meta: Some(Meta::Type(class.clone())),
            open: true,
            ..TableType::default()
        });
        assert_eq!(
            portable(&instance),
            "{ n: number, bump: (...any) -> ...any }"
        );
        assert_eq!(
            portable(&class),
            "{ bump: (...any) -> ...any, __add: (...any) -> ...any }"
        );
        assert_eq!(portable(&Type::Error), "any");
    }
}
