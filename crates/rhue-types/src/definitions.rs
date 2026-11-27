//! Definitions files (`.d.rhue`): what a host has, in Luau's declaration
//! syntax, so the standalone tools can check scripts written for it. See
//! [`rhue_parser::parse_definitions`] for the syntax.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::rc::Rc;

use rhue_common::Diagnostic;
use rhue_parser::ast::{Access, ClassItem, Declaration, Definitions, TypeExpr};

use crate::check::Checker;
use crate::env::Env;
use crate::portable::{pack_with_names, variadic_with_names, with_names};
use crate::types::{AliasDef, ClosedScope, FnType, GenericVar, NamedType, Pack, Type};
use crate::{HashMap, intersection};

/// What one declaration turns into, worked out before anything changes.
enum Change {
    Prop(Rc<str>, Rc<str>, Type, bool),
    Method(Rc<str>, Rc<str>, Type),
    Index(Rc<str>, Type),
    Global(Rc<str>, Type),
}

impl Env {
    /// Adds what a definitions file declares: its classes, their members, its
    /// globals, and its aliases (which scripts can name too). `source` is the
    /// file's text, for writing the aliases back out. All or nothing: with
    /// any error nothing changes.
    ///
    /// A member or global the file declares replaces what's there. A
    /// function declared more than once is overloaded (an intersection).
    pub fn load_definitions(
        &mut self,
        defs: &Definitions,
        source: &str,
    ) -> Result<(), Vec<Diagnostic>> {
        let mut env = self.clone();
        let mut errors = Vec::new();
        // every class first, so anything can name any of them
        for item in &defs.items {
            if let Declaration::Class(c) = item {
                env.declare_class(&c.name, None);
            }
        }
        for item in &defs.items {
            let Declaration::Class(c) = item else {
                continue;
            };
            let Some((parent, span)) = &c.parent else {
                continue;
            };
            if env.class(parent).is_none() {
                errors.push(Diagnostic::error(
                    format!("Unknown class '{parent}'"),
                    *span,
                ));
            } else if env.is_subclass(parent, &c.name) {
                errors.push(Diagnostic::error(
                    format!("'{}' can't extend '{parent}', which is under it", c.name),
                    *span,
                ));
            } else {
                env.declare_class(&c.name, Some(parent));
            }
        }

        let scope: Rc<ClosedScope> = Rc::default();
        let mut texts = Vec::new();
        for item in &defs.items {
            let Declaration::Alias(a) = item else {
                continue;
            };
            if scope.borrow().contains_key(&a.name) {
                errors.push(Diagnostic::error(
                    format!("Type '{}' is already defined", a.name),
                    a.name_span,
                ));
                continue;
            }
            let alias = Rc::new(AliasDef {
                name: a.name.clone(),
                generics: a.generics.clone(),
                body: a.ty.clone(),
                scope_depth: 0,
                generic_depth: 0,
                closed: Some(Rc::downgrade(&scope)),
            });
            scope.borrow_mut().insert(a.name.clone(), alias);
            let span = a.name_span.to(a.ty.span);
            let start = source[..span.start as usize]
                .rfind("type")
                .unwrap_or(span.start as usize);
            let text = &source[start..span.end as usize];
            let text = text.strip_prefix("export ").unwrap_or(text);
            texts.push((a.name.clone(), text.to_string()));
        }
        let aliases = scope.borrow().clone();

        let mut changes = Vec::new();
        {
            let mut c = Checker::for_declarations(&env);
            check_aliases(&mut c, &aliases);
            let resolve =
                |c: &mut Checker, te: &TypeExpr| c.resolve_with_aliases(aliases.clone(), te);
            for item in &defs.items {
                match item {
                    Declaration::Alias(_) => {}
                    Declaration::Global { name, ty, .. } => {
                        changes.push(Change::Global(name.clone(), resolve(&mut c, ty)));
                    }
                    Declaration::Class(class) => {
                        for m in &class.members {
                            let name = class.name.clone();
                            match m {
                                ClassItem::Prop {
                                    name: prop,
                                    span,
                                    ty,
                                    access,
                                } => {
                                    if *access == Access::Write {
                                        c.errors.push(Diagnostic::error(
                                            "a class property can be `read`, not `write`",
                                            *span,
                                        ));
                                        continue;
                                    }
                                    let t = resolve(&mut c, ty);
                                    changes.push(Change::Prop(
                                        name,
                                        prop.clone(),
                                        t,
                                        *access == Access::ReadWrite,
                                    ));
                                }
                                ClassItem::Method { name: m, ty, .. } => {
                                    changes.push(Change::Method(
                                        name,
                                        m.clone(),
                                        resolve(&mut c, ty),
                                    ));
                                }
                                ClassItem::Index { key, value, .. } => {
                                    if !matches!(resolve(&mut c, key), Type::String) {
                                        c.errors.push(Diagnostic::error(
                                            "a class can only have a `[string]: T` indexer",
                                            key.span,
                                        ));
                                        continue;
                                    }
                                    changes.push(Change::Index(name, resolve(&mut c, value)));
                                }
                            }
                        }
                    }
                }
            }
            errors.extend(c.errors);
        }
        if !errors.is_empty() {
            errors.sort_by_key(|d| d.primary.span.start);
            return Err(errors);
        }

        // a function declared twice in the file is overloaded
        let mut methods: BTreeMap<(Rc<str>, Rc<str>), Vec<Type>> = BTreeMap::new();
        let mut globals: BTreeMap<Rc<str>, Vec<Type>> = BTreeMap::new();
        for change in changes {
            match change {
                Change::Prop(class, name, t, writable) => {
                    let c = env.classes.get_mut(&class).expect("declared above");
                    c.props.retain(|(n, _, _)| *n != name);
                    c.props.push((name, t, writable));
                }
                Change::Method(class, name, t) => methods.entry((class, name)).or_default().push(t),
                Change::Index(class, t) => {
                    env.classes.get_mut(&class).expect("declared above").index = Some(t);
                }
                Change::Global(name, t) => globals.entry(name).or_default().push(t),
            }
        }
        for ((class, name), ts) in methods {
            let c = env.classes.get_mut(&class).expect("declared above");
            c.methods.retain(|(n, _)| *n != name);
            c.methods.push((name, intersection(ts)));
        }
        for (name, ts) in globals {
            env.declare_global_type(&name, intersection(ts));
        }
        env.aliases.extend(aliases);
        env.alias_text.extend(texts);
        env.imported.push(scope);
        *self = env;
        Ok(())
    }

    /// Everything declared, as a definitions file: aliases from definitions
    /// files, then classes (parents before children), then globals, each
    /// sorted by name so it diffs cleanly. What every engine has (the
    /// standard library) is left out.
    pub fn definitions(&self) -> String {
        let base = Env::new();
        let mut out = String::new();
        for text in self.alias_text.values() {
            out.push_str(text);
            out.push('\n');
        }
        if !self.alias_text.is_empty() {
            out.push('\n');
        }
        let mut names: Vec<&Rc<str>> = self
            .classes
            .keys()
            .filter(|n| base.class(n).is_none())
            .collect();
        names.sort();
        let mut written: Vec<Rc<str>> = Vec::new();
        for name in names {
            self.write_class(&mut out, name, &base, &mut written);
        }
        let mut globals: Vec<(&Rc<str>, &Type)> = self
            .globals
            .iter()
            .filter(|(n, _)| self.host.contains(*n) && base.global(n).is_none())
            .collect();
        globals.sort_by(|a, b| a.0.cmp(b.0));
        for (name, t) in globals {
            match functions(t) {
                Some(fs) => {
                    for f in fs {
                        out.push_str("declare function ");
                        out.push_str(name);
                        write_signature(&mut out, f, false);
                        out.push('\n');
                    }
                }
                None => {
                    let _ = writeln!(out, "declare {name}: {}", with_names(t));
                }
            }
        }
        while out.ends_with("\n\n") {
            out.pop();
        }
        out
    }

    fn write_class(
        &self,
        out: &mut String,
        name: &Rc<str>,
        base: &Env,
        written: &mut Vec<Rc<str>>,
    ) {
        if written.contains(name) {
            return;
        }
        written.push(name.clone());
        let Some(c) = self.class(name) else {
            return;
        };
        if let Some(p) = &c.parent
            && base.class(p).is_none()
        {
            self.write_class(out, p, base, written);
        }
        out.push_str("declare class ");
        out.push_str(name);
        if let Some(p) = &c.parent {
            out.push_str(" extends ");
            out.push_str(p);
        }
        out.push('\n');
        let mut props: Vec<_> = c.props.iter().collect();
        props.sort_by(|a, b| a.0.cmp(&b.0));
        for (n, t, writable) in props {
            let read = if *writable { "" } else { "read " };
            let _ = writeln!(out, "    {read}{n}: {}", with_names(t));
        }
        let mut methods: Vec<_> = c.methods.iter().collect();
        methods.sort_by(|a, b| a.0.cmp(&b.0));
        for (n, t) in methods {
            let fs = functions(t).filter(|fs| fs.iter().all(|f| !f.params.is_empty()));
            match fs {
                Some(fs) => {
                    for f in fs {
                        let _ = write!(out, "    function {n}");
                        write_signature(out, f, true);
                        out.push('\n');
                    }
                }
                None => {
                    let _ = writeln!(out, "    function {n}(self, ...: any): ...any");
                }
            }
        }
        if let Some(t) = &c.index {
            let _ = writeln!(out, "    [string]: {}", with_names(t));
        }
        out.push_str("end\n\n");
    }

    /// What a definitions file declares that this env doesn't have: classes,
    /// parents, members, properties scripts can set, index types and
    /// globals, as `Node`, `Shape extends Node`, `Node.Name`, `Node.Name can
    /// be set`, `Node:Destroy`, `Node[string]` and `world`. Only names get
    /// compared, not types.
    pub fn check_definitions(
        &self,
        defs: &Definitions,
        source: &str,
    ) -> Result<Vec<String>, Vec<Diagnostic>> {
        let mut file = Env::new();
        file.load_definitions(defs, source)?;
        let base = Env::new();
        let mut missing = Vec::new();
        let mut classes: Vec<_> = file
            .classes
            .iter()
            .filter(|(n, _)| base.class(n).is_none())
            .collect();
        classes.sort_by(|a, b| a.0.cmp(b.0));
        for (name, fc) in classes {
            let Some(mine) = self.class(name) else {
                missing.push(name.to_string());
                continue;
            };
            if let Some(p) = &fc.parent
                && mine.parent.as_ref() != Some(p)
            {
                missing.push(format!("{name} extends {p}"));
            }
            for (n, _, writable) in &fc.props {
                match self.class_member(name, n, false) {
                    Some(crate::ClassMember::Prop(_, w)) => {
                        if *writable && !w {
                            missing.push(format!("{name}.{n} can be set"));
                        }
                    }
                    _ => missing.push(format!("{name}.{n}")),
                }
            }
            for (n, _) in &fc.methods {
                if !matches!(
                    self.class_member(name, n, true),
                    Some(crate::ClassMember::Method(_))
                ) {
                    missing.push(format!("{name}:{n}"));
                }
            }
            if fc.index.is_some() && self.class_index(name).is_none() {
                missing.push(format!("{name}[string]"));
            }
        }
        let mut globals: Vec<_> = file
            .globals
            .keys()
            .filter(|n| base.global(n).is_none() && self.global(n).is_none())
            .map(|n| n.to_string())
            .collect();
        globals.sort();
        missing.extend(globals);
        Ok(missing)
    }
}

/// Every alias once, so a broken one fails here (like [`Env::import`]).
fn check_aliases(c: &mut Checker, aliases: &HashMap<Rc<str>, Rc<AliasDef>>) {
    for alias in aliases.values() {
        let mut args = Vec::new();
        let mut packs = Vec::new();
        for g in &alias.generics {
            let var = Type::Generic(GenericVar::new(g.name.clone(), g.pack));
            if g.pack {
                packs.push(Pack {
                    types: Vec::new(),
                    variadic: Some(var),
                });
            } else {
                args.push(var);
            }
        }
        c.expand(&Type::Named(Rc::new(NamedType {
            alias: alias.clone(),
            args,
            packs,
            expanded: Default::default(),
        })));
    }
}

/// A function type's overloads, if it's a function (or an intersection of
/// them).
fn functions(t: &Type) -> Option<Vec<&FnType>> {
    match t {
        Type::Function(f) => Some(vec![f]),
        Type::Intersection(ts) => ts
            .iter()
            .map(|t| match t {
                Type::Function(f) => Some(&**f),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

/// `<T>(self, arg1: T, ...: U): R` after a declared function's name. Types
/// don't keep parameter names, so they're numbered.
fn write_signature(out: &mut String, f: &FnType, method: bool) {
    if !f.generics.is_empty() {
        let g: Vec<String> = f.generics.iter().map(|g| g.display()).collect();
        let _ = write!(out, "<{}>", g.join(", "));
    }
    out.push('(');
    let mut params = f.params.iter();
    let mut first = true;
    if method {
        params.next();
        out.push_str("self");
        first = false;
    }
    for (i, p) in params.enumerate() {
        if !first {
            out.push_str(", ");
        }
        first = false;
        let _ = write!(out, "arg{}: {}", i + 1, with_names(p));
    }
    if let Some(v) = &f.variadic {
        if !first {
            out.push_str(", ");
        }
        out.push_str("...: ");
        variadic_with_names(out, v);
    }
    out.push_str("): ");
    pack_with_names(out, &f.rets);
}
