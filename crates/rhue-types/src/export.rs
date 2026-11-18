//! A chunk's exported types (what it returns, the globals it defines) as
//! Luau type syntax plus the aliases they need, written while the chunk's
//! checker is still alive, and read back with [`Env::import`].
//!
//! Unlike [`portable`](crate::portable), nothing is unrolled: every alias
//! the types mention, and every class, becomes a named alias, so recursive
//! types stay recursive. A class is two aliases: its instances,
//! `setmetatable<{ fields }, CounterClass>`, and its class table
//! `CounterClass` (methods, metamethods, `__index: CounterClass`), so
//! instances keep their exact shape and their operators.

use std::rc::Rc;

use rhue_compiler::resolve::LocalId;

use crate::check::Checker;
use crate::types::{AliasDef, FnType, GenericVar, Meta, NamedType, Pack, TableType, Type};
use crate::{HashMap, HashSet};

/// Exported types as text (see the module docs).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Exported {
    /// `type Name<T> = body` definitions the types below can name.
    pub aliases: Vec<String>,
    /// What the chunk returns.
    pub returns: String,
    /// The globals it defines.
    pub globals: Vec<(String, String)>,
}

enum Job {
    Alias(Rc<AliasDef>, String),
    Class(LocalId, String, String),
}

pub(crate) struct Exporter<'c, 'a> {
    c: &'c mut Checker<'a>,
    aliases: HashMap<*const AliasDef, String>,
    classes: HashMap<LocalId, (String, String)>,
    used: HashSet<String>,
    jobs: Vec<Job>,
    defs: Vec<String>,
}

/// Nesting (of types written inline) past which a type is written `any`.
const MAX_DEPTH: u32 = 48;

impl<'c, 'a> Exporter<'c, 'a> {
    pub(crate) fn new(c: &'c mut Checker<'a>) -> Self {
        Exporter {
            c,
            aliases: HashMap::default(),
            classes: HashMap::default(),
            used: HashSet::default(),
            jobs: Vec::new(),
            defs: Vec::new(),
        }
    }

    /// Writes `t`, queuing the definitions it needs.
    pub(crate) fn write(&mut self, t: &Type) -> String {
        let mut out = String::new();
        self.ty(&mut out, t, 0);
        out
    }

    /// Writes every queued definition (and those they need in turn).
    pub(crate) fn finish(mut self) -> Vec<String> {
        while let Some(job) = self.jobs.pop() {
            let def = match job {
                Job::Alias(alias, name) => self.alias_def(&alias, &name),
                Job::Class(id, instance, class) => self.class_defs(id, &instance, &class),
            };
            self.defs.push(def);
        }
        self.defs
    }

    fn fresh_name(&mut self, base: &str) -> String {
        let mut name = base.to_string();
        let mut n = 2;
        while !self.used.insert(name.clone()) {
            name = format!("{base}_{n}");
            n += 1;
        }
        name
    }

    fn alias_name(&mut self, alias: &Rc<AliasDef>) -> String {
        let key = Rc::as_ptr(alias);
        if let Some(n) = self.aliases.get(&key) {
            return n.clone();
        }
        let name = self.fresh_name(&alias.name);
        self.aliases.insert(key, name.clone());
        self.jobs.push(Job::Alias(alias.clone(), name.clone()));
        name
    }

    /// The instance and class-table aliases of class `id`.
    fn class_names(&mut self, id: LocalId, name: &str) -> (String, String) {
        if let Some(n) = self.classes.get(&id) {
            return n.clone();
        }
        let instance = self.fresh_name(name);
        let class = self.fresh_name(&format!("{name}Class"));
        self.classes.insert(id, (instance.clone(), class.clone()));
        self.jobs
            .push(Job::Class(id, instance.clone(), class.clone()));
        (instance, class)
    }

    /// `type Name<T, U...> = body`, the body expanded with the parameters
    /// standing for themselves.
    fn alias_def(&mut self, alias: &Rc<AliasDef>, name: &str) -> String {
        let mut args = Vec::new();
        let mut packs = Vec::new();
        let mut params = Vec::new();
        for g in &alias.generics {
            let var = Type::Generic(GenericVar::new(g.name.clone(), g.pack));
            params.push(if g.pack {
                format!("{}...", g.name)
            } else {
                g.name.to_string()
            });
            if g.pack {
                packs.push(Pack {
                    types: Vec::new(),
                    variadic: Some(var),
                });
            } else {
                args.push(var);
            }
        }
        let named = Type::Named(Rc::new(NamedType {
            alias: alias.clone(),
            args,
            packs,
            expanded: Default::default(),
        }));
        let body = self.c.expand(&named);
        let mut out = format!("type {name}");
        if !params.is_empty() {
            out.push('<');
            out.push_str(&params.join(", "));
            out.push('>');
        }
        out.push_str(" = ");
        self.ty(&mut out, &body, 0);
        out
    }

    /// `type Counter = setmetatable<{...}, CounterClass>` and
    /// `type CounterClass = {...}`.
    fn class_defs(&mut self, id: LocalId, instance: &str, class: &str) -> String {
        let fields = self.c.instances.get(&id).cloned();
        let mut out = format!("type {instance} = setmetatable<");
        match fields {
            Some(f) => self.table_fields(&mut out, &f, None, 0),
            None => out.push_str("{}"),
        }
        out.push_str(&format!(", {class}>\n"));
        out.push_str(&format!("type {class} = "));
        match self.c.locals.get(&id).cloned().map(|t| self.c.expand(&t)) {
            Some(Type::Table(t)) => self.table_fields(&mut out, &t, Some(class), 0),
            _ => out.push_str("{}"),
        }
        out
    }

    /// The class whose table `t` is (a `T.__index = T` table), if any.
    fn class_of_table(&mut self, t: &TableType) -> Option<(LocalId, Rc<str>)> {
        if !t.index_self {
            return None;
        }
        let names: Vec<&str> = t.props.iter().map(|(n, _)| &**n).collect();
        let mut ids: Vec<LocalId> = self.c.instances.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let Some(Type::Table(ct)) = self.c.locals.get(&id).cloned() else {
                continue;
            };
            if ct.index_self && names.iter().all(|n| ct.prop(n).is_some()) {
                let name = self.c.local_name(id).unwrap_or_else(|| Rc::from("Class"));
                return Some((id, name));
            }
        }
        None
    }

    fn ty(&mut self, out: &mut String, t: &Type, depth: u32) {
        if depth > MAX_DEPTH {
            out.push_str("any");
            return;
        }
        match t {
            Type::Named(n) => {
                let name = self.alias_name(&n.alias);
                out.push_str(&name);
                if !n.args.is_empty() || !n.packs.is_empty() {
                    out.push('<');
                    let mut first = true;
                    for a in &n.args {
                        if !first {
                            out.push_str(", ");
                        }
                        first = false;
                        self.ty(out, a, depth + 1);
                    }
                    for p in &n.packs {
                        if !first {
                            out.push_str(", ");
                        }
                        first = false;
                        match (&p.types[..], &p.variadic) {
                            ([], Some(v)) if v.pack_var().is_some() => self.ty(out, v, depth + 1),
                            _ => {
                                out.push('(');
                                self.pack_items(out, p, depth);
                                out.push(')');
                            }
                        }
                    }
                    out.push('>');
                }
            }
            Type::Table(tt) => match &tt.meta {
                Some(Meta::Local { id, name }) => {
                    let (instance, _) = self.class_names(*id, name);
                    out.push_str(&instance);
                }
                Some(Meta::Type(mt)) => {
                    out.push_str("setmetatable<");
                    self.table_fields(out, tt, None, depth);
                    out.push_str(", ");
                    let mt = mt.clone();
                    self.ty(out, &mt, depth + 1);
                    out.push('>');
                }
                None => match self.class_of_table(tt) {
                    Some((id, name)) => {
                        let (_, class) = self.class_names(id, &name);
                        out.push_str(&class);
                    }
                    None => self.table_fields(out, tt, None, depth),
                },
            },
            Type::Function(f) => self.function(out, f, depth),
            Type::Union(ts) if ts.len() == 2 && ts.iter().any(|m| matches!(m, Type::Nil)) => {
                let other = ts
                    .iter()
                    .find(|m| !matches!(m, Type::Nil))
                    .expect("two members");
                self.wrapped(out, other, depth);
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
                    self.wrapped(out, m, depth);
                }
            }
            // Everything else reads the same as written by `portable`.
            other => out.push_str(&crate::portable(other)),
        }
    }

    /// `t`, in parentheses if it's a function or a union.
    fn wrapped(&mut self, out: &mut String, t: &Type, depth: u32) {
        let wrap = matches!(
            t,
            Type::Function(_) | Type::Union(_) | Type::Intersection(_)
        );
        if wrap {
            out.push('(');
        }
        self.ty(out, t, depth + 1);
        if wrap {
            out.push(')');
        }
    }

    fn function(&mut self, out: &mut String, f: &FnType, depth: u32) {
        if !f.generics.is_empty() {
            let g: Vec<String> = f.generics.iter().map(|g| g.display()).collect();
            out.push('<');
            out.push_str(&g.join(", "));
            out.push('>');
        }
        out.push('(');
        self.pack_items(
            out,
            &Pack {
                types: f.params.clone(),
                variadic: f.variadic.clone(),
            },
            depth,
        );
        out.push_str(") -> ");
        match (&f.rets.types[..], &f.rets.variadic) {
            ([one], None) => self.wrapped_fn(out, one, depth),
            ([], Some(v)) => self.variadic(out, v, depth),
            _ => {
                out.push('(');
                self.pack_items(out, &f.rets, depth);
                out.push(')');
            }
        }
    }

    /// A single result: parenthesized if it's a function.
    fn wrapped_fn(&mut self, out: &mut String, t: &Type, depth: u32) {
        if matches!(t, Type::Function(_)) {
            out.push('(');
            self.ty(out, t, depth + 1);
            out.push(')');
        } else {
            self.ty(out, t, depth + 1);
        }
    }

    fn pack_items(&mut self, out: &mut String, p: &Pack, depth: u32) {
        for (i, t) in p.types.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            self.ty(out, t, depth + 1);
        }
        if let Some(v) = &p.variadic {
            if !p.types.is_empty() {
                out.push_str(", ");
            }
            self.variadic(out, v, depth);
        }
    }

    fn variadic(&mut self, out: &mut String, v: &Type, depth: u32) {
        match v.pack_var() {
            Some(g) => out.push_str(&g.display()),
            None => {
                out.push_str("...");
                self.ty(out, v, depth + 1);
            }
        }
    }

    /// `{ a: T, read b: U, [K]: V }`, and `__index: Class` first for a
    /// class table (`class` is its alias).
    fn table_fields(&mut self, out: &mut String, tt: &TableType, class: Option<&str>, depth: u32) {
        let mut parts: Vec<String> = Vec::new();
        if let Some(class) = class {
            parts.push(format!("__index: {class}"));
        }
        for (n, t) in &tt.props {
            if tt.index_self && &**n == "__index" {
                continue;
            }
            let mut part = String::from(tt.access(n).prefix());
            if is_name(n) {
                part.push_str(n);
            } else {
                part.push_str(&format!("[{}]", crate::portable(&Type::StrLit(n.clone()))));
            }
            part.push_str(": ");
            self.ty(&mut part, t, depth + 1);
            parts.push(part);
        }
        match (&tt.indexer, parts.is_empty()) {
            (Some((Type::Number, v)), true) => {
                out.push('{');
                self.ty(out, v, depth + 1);
                out.push('}');
                return;
            }
            (Some((k, v)), _) => {
                let mut part = String::from("[");
                self.ty(&mut part, k, depth + 1);
                part.push_str("]: ");
                self.ty(&mut part, v, depth + 1);
                parts.push(part);
            }
            (None, _) => {}
        }
        if parts.is_empty() {
            out.push_str("{}");
        } else {
            out.push_str("{ ");
            out.push_str(&parts.join(", "));
            out.push_str(" }");
        }
    }
}

fn is_name(s: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "and", "break", "continue", "do", "else", "elseif", "end", "export", "false", "for",
        "function", "if", "in", "local", "nil", "not", "or", "repeat", "return", "then", "true",
        "type", "typeof", "until", "while",
    ];
    let mut cs = s.chars();
    cs.next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !KEYWORDS.contains(&s)
}
