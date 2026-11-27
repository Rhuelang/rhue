//! Turning written types into [`Type`]s, and relating types: alias expansion,
//! subtyping, and inferring generic arguments.

use crate::HashMap;
use std::rc::Rc;

use rhue_common::Span;
use rhue_parser::ast::{Access, GenericParam, Singleton, TypeExpr, TypeKind, TypePack};

use crate::check::Checker;
use crate::types::*;

/// How deep alias expansion can nest before giving up.
const MAX_EXPANSION: usize = 64;

impl Checker<'_> {
    // ─ Resolving Written Types ───────────────────────────────────────────────

    pub fn resolve_type(&mut self, te: &TypeExpr) -> Type {
        stacker::maybe_grow(64 * 1024, 1024 * 1024, || self.resolve_inner(te))
    }

    fn resolve_inner(&mut self, te: &TypeExpr) -> Type {
        match &te.kind {
            TypeKind::Name { name, args } => self.resolve_name(name, args, te.span),
            TypeKind::Singleton(Singleton::Bool(b)) => Type::BoolLit(*b),
            TypeKind::Singleton(Singleton::String(s)) => Type::StrLit(s.clone()),
            TypeKind::Typeof(e) => {
                if self.res.is_none() {
                    self.error(te.span, "`typeof` can't be used in a host declaration");
                    return Type::Error;
                }
                // typing the expression can't change anything it mentions
                let errors = self.errors.len();
                let t = self.expr(e);
                self.errors.truncate(errors);
                t
            }
            TypeKind::Optional(t) => Type::optional(self.resolve_type(t)),
            TypeKind::GenericPack(name) => self.resolve_generic_pack(name, te.span),
            TypeKind::Pack(p) => match (&p.types[..], &p.variadic) {
                // `(T)` where a type goes is just `T`
                ([one], None) => self.resolve_type(one),
                _ => {
                    self.error(te.span, "A type pack can't be used as a type");
                    Type::Error
                }
            },
            TypeKind::Union(ts) => {
                union(ts.iter().map(|t| self.resolve_type(t)).collect::<Vec<_>>())
            }
            TypeKind::Intersection(ts) => {
                intersection(ts.iter().map(|t| self.resolve_type(t)).collect::<Vec<_>>())
            }
            TypeKind::Table { props, indexer } => {
                let mut out: Vec<(Rc<str>, Type)> = Vec::new();
                let mut access = Vec::new();
                for p in props {
                    if out.iter().any(|(n, _)| *n == p.name) {
                        self.error(p.span, format!("Duplicate field '{}'", p.name));
                        continue;
                    }
                    let t = self.resolve_type(&p.ty);
                    out.push((p.name.clone(), t));
                    if p.access != Access::ReadWrite {
                        access.push((p.name.clone(), p.access));
                    }
                }
                let indexer = indexer
                    .as_ref()
                    .map(|ix| (self.resolve_type(&ix.0), self.resolve_type(&ix.1)));
                Type::table(TableType {
                    props: out,
                    indexer,
                    unsealed: false,
                    access,
                    ..TableType::default()
                })
            }
            TypeKind::Function {
                generics,
                params,
                variadic,
                returns,
            } => {
                let vars: Vec<Rc<GenericVar>> = generics
                    .iter()
                    .map(|g| GenericVar::new(g.name.clone(), g.pack))
                    .collect();
                self.generic_scopes.push(
                    vars.iter()
                        .map(|v| (v.name.clone(), Type::Generic(v.clone())))
                        .collect(),
                );
                let params = params.iter().map(|p| self.resolve_type(&p.ty)).collect();
                let variadic = variadic.as_ref().map(|v| self.resolve_type(v));
                let rets = self.resolve_pack(returns);
                self.generic_scopes.pop();
                Type::function(FnType {
                    generics: vars,
                    params,
                    variadic,
                    rets,
                })
            }
        }
    }

    pub fn resolve_pack(&mut self, p: &TypePack) -> Pack {
        Pack {
            types: p.types.iter().map(|t| self.resolve_type(t)).collect(),
            variadic: p.variadic.as_ref().map(|v| self.resolve_type(v)),
        }
    }

    /// `T...` in a variadic slot.
    fn resolve_generic_pack(&mut self, name: &Rc<str>, span: Span) -> Type {
        let generic_limit = self.generic_view.unwrap_or(self.generic_scopes.len());
        let found = self.generic_scopes[..generic_limit]
            .iter()
            .rev()
            .find_map(|s| s.get(name).cloned());
        match found {
            Some(t) if t.pack_var().is_some() => t,
            Some(_) => {
                self.error(
                    span,
                    format!(
                        "'{name}' is a type, not a type pack (drop the `...`, or write `...{name}`)"
                    ),
                );
                Type::Error
            }
            None => {
                self.error(span, format!("Unknown type pack '{name}...'"));
                Type::Error
            }
        }
    }

    /// A pack argument to an alias: `(A, B)`, `()`, `...T`, `T...`, or a lone
    /// type (a pack of one).
    fn resolve_pack_arg(&mut self, te: &TypeExpr) -> Pack {
        match &te.kind {
            TypeKind::Pack(p) => self.resolve_pack(p),
            TypeKind::GenericPack(name) => Pack {
                types: Vec::new(),
                variadic: Some(self.resolve_generic_pack(name, te.span)),
            },
            _ => Pack::one(self.resolve_type(te)),
        }
    }

    /// Matches a use's type arguments to an alias's parameters, filling in
    /// defaults. `None` (after reporting it) if they don't fit.
    fn alias_args(
        &mut self,
        alias: &Rc<AliasDef>,
        args: &[TypeExpr],
        span: Span,
    ) -> Option<(Vec<Type>, Vec<Pack>)> {
        let name = &alias.name;
        let type_params: Vec<&GenericParam> = alias.generics.iter().filter(|g| !g.pack).collect();
        let pack_params: Vec<&GenericParam> = alias.generics.iter().filter(|g| g.pack).collect();
        let is_pack_expr = |a: &TypeExpr| {
            matches!(a.kind, TypeKind::GenericPack(_))
                || matches!(&a.kind, TypeKind::Pack(p) if p.types.len() != 1 || p.variadic.is_some())
        };
        // `F<A, B>` for `type F<T...> = ...`, the arguments are the pack
        if type_params.is_empty()
            && pack_params.len() == 1
            && !(args.len() == 1 && is_pack_expr(&args[0]))
            && !args
                .iter()
                .any(|a| matches!(a.kind, TypeKind::Pack(_) | TypeKind::GenericPack(_)))
            && !(args.is_empty() && pack_params[0].default.is_some())
        {
            let types = args.iter().map(|a| self.resolve_type(a)).collect();
            return Some((
                Vec::new(),
                vec![Pack {
                    types,
                    variadic: None,
                }],
            ));
        }
        let required = alias
            .generics
            .iter()
            .filter(|g| g.default.is_none())
            .count();
        let total = alias.generics.len();
        if args.len() < required || args.len() > total {
            let expected = if required == total {
                format!("{total}")
            } else if args.len() > total {
                format!("at most {total}")
            } else {
                format!("at least {required}")
            };
            self.error(
                span,
                format!(
                    "Generic type '{name}' expects {expected} type argument{}, but {} {} specified",
                    if total == 1 && required == total {
                        ""
                    } else {
                        "s"
                    },
                    args.len(),
                    if args.len() == 1 { "is" } else { "are" }
                ),
            );
            return None;
        }
        let mut types = Vec::new();
        let mut packs = Vec::new();
        let mut bound: HashMap<Rc<str>, Type> = HashMap::default();
        for (i, g) in alias.generics.iter().enumerate() {
            match (args.get(i), g.pack) {
                (Some(a), false) => {
                    if is_pack_expr(a) {
                        self.error(
                            a.span,
                            format!("'{}' of '{name}' is a type, not a type pack", g.name),
                        );
                        return None;
                    }
                    let t = self.resolve_type(a);
                    bound.insert(g.name.clone(), t.clone());
                    types.push(t);
                }
                (Some(a), true) => packs.push(self.resolve_pack_arg(a)),
                (None, pack) => {
                    let d = g.default.clone().expect("checked against `required`");
                    if pack {
                        let p = self.in_alias_scope(alias, &bound, |c| c.resolve_pack_arg(&d));
                        packs.push(p);
                    } else {
                        let t = self.in_alias_scope(alias, &bound, |c| c.resolve_type(&d));
                        bound.insert(g.name.clone(), t.clone());
                        types.push(t);
                    }
                }
            }
        }
        Some((types, packs))
    }

    /// Runs `f` with the scopes visible where `alias` was declared, plus
    /// `bound` (earlier parameters, for defaults that mention them).
    fn in_alias_scope<R>(
        &mut self,
        alias: &AliasDef,
        bound: &HashMap<Rc<str>, Type>,
        f: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let saved = (self.alias_view, self.generic_view);
        self.alias_view = Some(alias.scope_depth.min(self.alias_scopes.len()));
        let generic_depth = alias.generic_depth.min(self.generic_scopes.len());
        let tail = self.generic_scopes.split_off(generic_depth);
        self.generic_scopes.push(bound.clone());
        self.generic_view = None;
        let out = f(self);
        self.generic_scopes.pop();
        self.generic_scopes.extend(tail);
        (self.alias_view, self.generic_view) = saved;
        out
    }

    /// Checks an alias's defaults where it's declared (so a bad default gets
    /// reported even if no use needs it).
    pub(crate) fn check_alias_defaults(&mut self, alias: &Rc<AliasDef>) {
        let mut bound: HashMap<Rc<str>, Type> = HashMap::default();
        for g in &alias.generics {
            match (&g.default, g.pack) {
                (Some(d), false) => {
                    let t = self.in_alias_scope(alias, &bound, |c| c.resolve_type(d));
                    bound.insert(g.name.clone(), t);
                }
                (Some(d), true) => {
                    self.in_alias_scope(alias, &bound, |c| c.resolve_pack_arg(d));
                }
                (None, false) => {
                    bound.insert(
                        g.name.clone(),
                        Type::Generic(GenericVar::fresh(g.name.clone())),
                    );
                }
                (None, true) => {}
            }
        }
    }

    fn resolve_name(&mut self, name: &Rc<str>, args: &[TypeExpr], span: Span) -> Type {
        let primitive = match name.as_ref() {
            "any" => Some(Type::Any),
            "unknown" => Some(Type::Unknown),
            "never" => Some(Type::Never),
            "nil" => Some(Type::Nil),
            "boolean" => Some(Type::Boolean),
            "number" => Some(Type::Number),
            "string" => Some(Type::String),
            _ => None,
        };
        if let Some(p) = primitive {
            if !args.is_empty() {
                self.error(span, format!("Type '{name}' does not take type arguments"));
            }
            return p;
        }
        let generic_limit = self.generic_view.unwrap_or(self.generic_scopes.len());
        let found = self.generic_scopes[..generic_limit]
            .iter()
            .rev()
            .find_map(|s| s.get(name).cloned());
        if let Some(g) = found {
            if let Some(var) = g.pack_var() {
                self.error(
                    span,
                    format!(
                        "Type pack '{}' used where a type is expected (write `{}...` where a variadic goes)",
                        var.name, var.name
                    ),
                );
                return Type::Error;
            }
            if !args.is_empty() {
                self.error(span, format!("Type '{name}' does not take type arguments"));
            }
            return g;
        }
        let alias_limit = self.alias_view.unwrap_or(self.alias_scopes.len());
        let alias = self.alias_scopes[..alias_limit]
            .iter()
            .rev()
            .find_map(|s| s.get(name).cloned())
            .or_else(|| self.env.aliases.get(name).cloned());
        if let Some(alias) = alias {
            return match self.alias_args(&alias, args, span) {
                Some((args, packs)) => Type::Named(Rc::new(NamedType {
                    alias,
                    args,
                    packs,
                    expanded: Default::default(),
                })),
                None => Type::Error,
            };
        }
        if self.env.class(name).is_some() {
            if !args.is_empty() {
                self.error(span, format!("Type '{name}' does not take type arguments"));
            }
            return Type::Class(name.clone());
        }
        // `setmetatable<T, MT>`, table type `T` with metatable `MT` (like Luau,
        // it's how exported class instances get written)
        if &**name == "setmetatable" {
            let [t, mt] = args else {
                self.error(span, "setmetatable<T, MT> takes two type arguments");
                return Type::Error;
            };
            let (t_ty, mt_ty) = (self.resolve_type(t), self.resolve_type(mt));
            return match self.expand(&t_ty) {
                Type::Table(tt) => {
                    let mut tt = (*tt).clone();
                    tt.meta = Some(Meta::Type(mt_ty));
                    tt.unsealed = false;
                    Type::table(tt)
                }
                other => {
                    self.error(
                        t.span,
                        format!("setmetatable<T, MT> needs a table type, not '{other}'"),
                    );
                    Type::Error
                }
            };
        }
        self.error(span, format!("Unknown type '{name}'"));
        Type::Error
    }

    /// Expands an imported alias. Its body gets resolved in its own scope (the
    /// aliases imported with it), whatever this checker has in scope.
    fn expand_closed(&mut self, n: &Rc<NamedType>, closed: &std::rc::Weak<ClosedScope>) -> Type {
        let Some(scope) = closed.upgrade() else {
            return Type::Any;
        };
        let aliases = scope.borrow().clone();
        let mut params: HashMap<Rc<str>, Type> = HashMap::default();
        let mut packs = Subst::new(&[]);
        let mut args = n.args.iter();
        let mut pack_args = n.packs.iter();
        for g in &n.alias.generics {
            if g.pack {
                let var = GenericVar::new(g.name.clone(), true);
                if let Some(p) = pack_args.next() {
                    packs.add_pack(var.id, p.clone());
                }
                params.insert(g.name.clone(), Type::Generic(var));
            } else if let Some(a) = args.next() {
                params.insert(g.name.clone(), a.clone());
            }
        }
        let saved_aliases = std::mem::replace(&mut self.alias_scopes, vec![aliases]);
        let saved_generics = std::mem::replace(&mut self.generic_scopes, vec![params]);
        let saved_views = (self.alias_view, self.generic_view);
        (self.alias_view, self.generic_view) = (None, None);
        let body = n.alias.body.clone();
        let t = packs.apply(&self.resolve_type(&body));
        self.alias_scopes = saved_aliases;
        self.generic_scopes = saved_generics;
        (self.alias_view, self.generic_view) = saved_views;
        t
    }

    /// Resolves `te` with exactly `aliases` in scope (and host types).
    pub(crate) fn resolve_with_aliases(
        &mut self,
        aliases: HashMap<Rc<str>, Rc<AliasDef>>,
        te: &TypeExpr,
    ) -> Type {
        let saved_aliases = std::mem::replace(&mut self.alias_scopes, vec![aliases]);
        let saved_generics = std::mem::take(&mut self.generic_scopes);
        let t = self.resolve_type(te);
        self.alias_scopes = saved_aliases;
        self.generic_scopes = saved_generics;
        t
    }

    /// Looks through aliases (over and over) to the type they stand for.
    pub fn expand(&mut self, t: &Type) -> Type {
        let mut t = t.clone();
        let mut chain: Vec<Rc<NamedType>> = Vec::new();
        while let Type::Named(n) = &t {
            // `type A = B, type B = A` goes in circles without ever reaching a
            // real type
            if chain.iter().any(|c| Rc::ptr_eq(&c.alias, &n.alias)) {
                let name = n.alias.name.clone();
                let span = n.alias.body.span;
                self.error(
                    span,
                    format!("Type alias '{name}' refers to itself without a table or function in between"),
                );
                for c in &chain {
                    let _ = c.expanded.set(Type::Error);
                }
                return Type::Error;
            }
            chain.push(n.clone());
            t = self.expand_once(n);
        }
        t
    }

    fn expand_once(&mut self, n: &Rc<NamedType>) -> Type {
        if let Some(t) = n.expanded.get() {
            return t.clone();
        }
        let key = Rc::as_ptr(n);
        if self.expanding.contains(&key) || self.expanding.len() >= MAX_EXPANSION {
            self.error(
                n.alias.body.span,
                format!(
                    "Type alias '{}' refers to itself without a table or function in between",
                    n.alias.name
                ),
            );
            let _ = n.expanded.set(Type::Error);
            return Type::Error;
        }
        self.expanding.push(key);
        if let Some(closed) = &n.alias.closed {
            let t = self.expand_closed(n, closed);
            self.expanding.pop();
            let _ = n.expanded.set(t.clone());
            return t;
        }
        // resolve the body as it was written, the alias's own scopes with its
        // generic parameters bound to the arguments
        let saved = (self.alias_view, self.generic_view);
        self.alias_view = Some(n.alias.scope_depth.min(self.alias_scopes.len()));
        let generic_depth = n.alias.generic_depth.min(self.generic_scopes.len());
        // type parameters get bound to their arguments, pack parameters to
        // fresh pack variables that get substituted by the pack arguments after
        let mut scope: HashMap<Rc<str>, Type> = HashMap::default();
        let mut packs = Subst::new(&[]);
        let mut args = n.args.iter();
        let mut pack_args = n.packs.iter();
        for g in &n.alias.generics {
            if g.pack {
                let var = GenericVar::new(g.name.clone(), true);
                if let Some(p) = pack_args.next() {
                    packs.add_pack(var.id, p.clone());
                }
                scope.insert(g.name.clone(), Type::Generic(var));
            } else if let Some(a) = args.next() {
                scope.insert(g.name.clone(), a.clone());
            }
        }
        // the parameters go right above the alias's visible generic scopes
        let tail = self.generic_scopes.split_off(generic_depth);
        self.generic_scopes.push(scope);
        self.generic_view = None;
        let body = n.alias.body.clone();
        let t = packs.apply(&self.resolve_type(&body));
        self.generic_scopes.pop();
        self.generic_scopes.extend(tail);
        (self.alias_view, self.generic_view) = saved;
        self.expanding.pop();
        let t = match t {
            // `type A = B` where B expands back to A
            Type::Named(ref inner) if Rc::ptr_eq(&inner.alias, &n.alias) => {
                self.error(
                    n.alias.body.span,
                    format!(
                        "Type alias '{}' refers to itself without a table or function in between",
                        n.alias.name
                    ),
                );
                Type::Error
            }
            t => t,
        };
        let _ = n.expanded.set(t.clone());
        t
    }

    // ─ Subtyping ─────────────────────────────────────────────────────────────

    /// Whether a value of type `a` can be used where a `b` is expected.
    pub fn sub(&mut self, a: &Type, b: &Type) -> bool {
        let mut assumed = Vec::new();
        self.sub_in(a, b, &mut assumed)
    }

    fn sub_in(&mut self, a: &Type, b: &Type, assumed: &mut Vec<(Type, Type)>) -> bool {
        use Type::*;
        match (a, b) {
            (Any | Error, _) | (_, Any | Error | Unknown) | (Never, _) => true,
            // generic aliases whose arguments grow every level (`type T<A> =
            // {x: T<{A}>}`) never repeat a pair, so past this depth just assume
            // they're related
            (Named(_), _) | (_, Named(_)) if assumed.len() > 64 => true,
            (Named(_), Named(_)) => {
                if a.same(b) || assumed.iter().any(|(x, y)| x.same(a) && y.same(b)) {
                    return true;
                }
                // recursive aliases, assume the pair's related while checking
                // their expansions (coinduction)
                assumed.push((a.clone(), b.clone()));
                let (ea, eb) = (self.expand(a), self.expand(b));
                let r = self.sub_in(&ea, &eb, assumed);
                assumed.pop();
                r
            }
            (Named(_), _) | (_, Named(_)) => {
                // same coinduction when only one side is an alias (it might
                // meet the other inside a union)
                if assumed.iter().any(|(x, y)| x.same(a) && y.same(b)) {
                    return true;
                }
                assumed.push((a.clone(), b.clone()));
                let (ea, eb) = (self.expand(a), self.expand(b));
                let r = self.sub_in(&ea, &eb, assumed);
                assumed.pop();
                r
            }
            (Union(xs), _) => xs.clone().iter().all(|x| self.sub_in(x, b, assumed)),
            (_, Union(ys)) => {
                if matches!(a, Boolean)
                    && ys.iter().any(|y| matches!(y, BoolLit(true)))
                    && ys.iter().any(|y| matches!(y, BoolLit(false)))
                {
                    return true;
                }
                ys.clone().iter().any(|y| self.sub_in(a, y, assumed))
            }
            (Intersection(xs), _) => xs.clone().iter().any(|x| self.sub_in(x, b, assumed)),
            (_, Intersection(ys)) => ys.clone().iter().all(|y| self.sub_in(a, y, assumed)),
            (Unknown, _) => false,
            (Nil, Nil) | (Boolean, Boolean) | (Number, Number) | (String, String) => true,
            (BoolLit(_), Boolean) | (StrLit(_), String) => true,
            (BoolLit(x), BoolLit(y)) => x == y,
            (StrLit(x), StrLit(y)) => x == y,
            (Generic(x), Generic(y)) => x.id == y.id,
            (Class(x), Class(y)) => self.env.is_subclass(x, y),
            (Function(f), Function(g)) => {
                let (f, g) = (f.clone(), g.clone());
                self.fn_sub(&f, &g, assumed)
            }
            (Table(t), Table(u)) => {
                let (t, u) = (t.clone(), u.clone());
                self.table_sub(&t, &u, assumed)
            }
            _ => false,
        }
    }

    fn fn_sub(&mut self, f: &FnType, g: &FnType, assumed: &mut Vec<(Type, Type)>) -> bool {
        // generic functions get compared with their parameters as `any`
        let erase = |func: &FnType| {
            if func.generics.is_empty() {
                func.clone()
            } else {
                let s = Subst::new(&func.generics);
                FnType {
                    generics: Vec::new(),
                    params: func.params.iter().map(|p| s.apply(p)).collect(),
                    variadic: func.variadic.as_ref().map(|v| s.apply(v)),
                    rets: s.apply_pack(&func.rets),
                }
            }
        };
        let (f, g) = (erase(f), erase(g));
        let n = f.params.len().max(g.params.len());
        for i in 0..n {
            // what callers of `g` pass (nil past its parameters), which `f` has
            // to accept (it ignores extras when it has fewer)
            let passed = g
                .params
                .get(i)
                .cloned()
                .or_else(|| g.variadic.clone())
                .unwrap_or(Type::Nil);
            let accepted = f.params.get(i).cloned().or_else(|| f.variadic.clone());
            if let Some(acc) = accepted
                && !self.sub_in(&passed, &acc, assumed)
            {
                return false;
            }
        }
        if let (Some(gv), Some(fv)) = (&g.variadic, &f.variadic)
            && !self.sub_in(gv, fv, assumed)
        {
            return false;
        }
        self.pack_sub_in(&f.rets, &g.rets, assumed)
    }

    pub fn pack_sub(&mut self, p: &Pack, q: &Pack) -> bool {
        let mut assumed = Vec::new();
        self.pack_sub_in(p, q, &mut assumed)
    }

    fn pack_sub_in(&mut self, p: &Pack, q: &Pack, assumed: &mut Vec<(Type, Type)>) -> bool {
        let n = p.types.len().max(q.types.len());
        for i in 0..n {
            let Some(want) = q.types.get(i).cloned().or_else(|| q.variadic.clone()) else {
                // values past what's expected get dropped
                continue;
            };
            if !self.sub_in(&p.get(i), &want, assumed) {
                return false;
            }
        }
        match (&p.variadic, &q.variadic) {
            (Some(pv), Some(qv)) => self.sub_in(pv, qv, assumed),
            (Some(pv), None) if q.types.len() > p.types.len() => {
                // any of the missing positions could come from the variadic
                (p.types.len()..q.types.len()).all(|i| {
                    let want = q.types[i].clone();
                    self.sub_in(pv, &want, assumed)
                })
            }
            _ => true,
        }
    }

    /// Width subtyping with covariant fields (simpler than Luau's read/write
    /// variance, see the design notes).
    fn table_sub(&mut self, t: &TableType, u: &TableType, assumed: &mut Vec<(Type, Type)>) -> bool {
        if t.is_empty_literal() {
            return true;
        }
        for (name, want) in &u.props {
            let (has, got_access) = (t.prop(name).is_some(), t.access(name));
            let got = match t.prop(name) {
                Some(g) => g.clone(),
                None => match &t.indexer {
                    Some((k, v)) if self.sub_in(&Type::StrLit(name.clone()), k, assumed) => {
                        Type::optional(v.clone())
                    }
                    // a missing field reads as nil
                    _ => Type::Nil,
                },
            };
            // whatever `u` allows through it `t` has to allow too. a read-only
            // property is covariant, a write-only one contravariant
            let fits = match u.access(name) {
                Access::Read => got_access.can_read() && self.sub_in(&got, want, assumed),
                Access::Write => {
                    let writable = got_access.can_write()
                        && (has || t.indexer.is_some() || t.unsealed || t.open);
                    let got = got.strip_nil();
                    writable
                        && self.sub_in(
                            want,
                            if has { t.prop(name).unwrap() } else { &got },
                            assumed,
                        )
                }
                Access::ReadWrite => {
                    got_access == Access::ReadWrite && self.sub_in(&got, want, assumed)
                }
            };
            if !fits {
                return false;
            }
        }
        if let Some((uk, uv)) = &u.indexer {
            match &t.indexer {
                Some((tk, tv)) => {
                    let (tk, tv) = (tk.clone(), tv.clone());
                    if !self.sub_in(uk, &tk, assumed) || !self.sub_in(&tv, uv, assumed) {
                        return false;
                    }
                }
                None => {
                    // the fields become the entries
                    for (name, got) in &t.props {
                        if !self.sub_in(&Type::StrLit(name.clone()), uk, assumed)
                            || !self.sub_in(got, uv, assumed)
                        {
                            return false;
                        }
                    }
                }
            }
        }
        true
    }

    // ─ Generics ──────────────────────────────────────────────────────────────

    /// Infers generic arguments by matching a parameter type against an
    /// argument type. The first match for each generic wins, and the argument
    /// check after reports any conflict.
    pub fn unify(&mut self, param: &Type, arg: &Type, subst: &mut Subst) {
        self.unify_in(param, arg, subst, 0);
    }

    fn unify_in(&mut self, param: &Type, arg: &Type, subst: &mut Subst, depth: u32) {
        if depth > 16 || subst.is_empty() {
            return;
        }
        match param {
            Type::Generic(g) if g.pack => {}
            Type::Generic(g) if subst.owns(g.id) => {
                if !subst.is_bound(g.id) && !matches!(arg, Type::Never | Type::Error) {
                    subst.bind(g.id, arg.widen());
                }
            }
            Type::Named(_) => {
                let e = self.expand(param);
                self.unify_in(&e, arg, subst, depth + 1);
            }
            Type::Union(ms) => {
                let generic: Vec<&Type> = ms.iter().filter(|m| subst.mentions(m)).collect();
                if let [only] = generic.as_slice() {
                    let a = if param.can_be_nil() {
                        arg.strip_nil()
                    } else {
                        arg.clone()
                    };
                    let only = (*only).clone();
                    self.unify_in(&only, &a, subst, depth + 1);
                }
            }
            Type::Table(pt) => {
                if let Type::Table(at) = self.expand(arg) {
                    if let Some((pk, pv)) = &pt.indexer {
                        match &at.indexer {
                            Some((ak, av)) => {
                                self.unify_in(pk, ak, subst, depth + 1);
                                self.unify_in(pv, av, subst, depth + 1);
                            }
                            None if !at.props.is_empty() => {
                                self.unify_in(pk, &Type::String, subst, depth + 1);
                                let v = union(
                                    at.props.iter().map(|(_, t)| t.clone()).collect::<Vec<_>>(),
                                );
                                self.unify_in(pv, &v, subst, depth + 1);
                            }
                            None => {}
                        }
                    }
                    for (name, p) in &pt.props {
                        if let Some(a) = at.prop(name) {
                            let a = a.clone();
                            self.unify_in(p, &a, subst, depth + 1);
                        }
                    }
                }
            }
            Type::Function(pf) => {
                if let Type::Function(af) = self.expand(arg) {
                    for (p, a) in pf.params.iter().zip(&af.params) {
                        self.unify_in(p, a, subst, depth + 1);
                    }
                    for (p, a) in pf.rets.types.iter().zip(&af.rets.types) {
                        self.unify_in(p, a, subst, depth + 1);
                    }
                    // `(A...) -> R...` takes the rest of the function's
                    // parameters and results
                    if let Some(g) = pf.variadic.as_ref().and_then(Type::pack_var) {
                        let rest = af.params.get(pf.params.len()..).unwrap_or_default();
                        subst.bind_pack_once(
                            g.id,
                            Pack {
                                types: rest.to_vec(),
                                variadic: af.variadic.clone(),
                            },
                        );
                    }
                    if let Some(g) = pf.rets.variadic.as_ref().and_then(Type::pack_var) {
                        let rest = af.rets.types.get(pf.rets.types.len()..).unwrap_or_default();
                        subst.bind_pack_once(
                            g.id,
                            Pack {
                                types: rest.to_vec(),
                                variadic: af.rets.variadic.clone(),
                            },
                        );
                    }
                }
            }
            _ => {}
        }
    }
}

/// Generic arguments being inferred for one call.
pub struct Subst {
    map: HashMap<u32, Option<Type>>,
    /// Generic packs (`T...`) and the packs they stand for.
    packs: HashMap<u32, Option<Pack>>,
}

impl Subst {
    pub fn new(generics: &[Rc<GenericVar>]) -> Self {
        Subst {
            map: generics
                .iter()
                .filter(|g| !g.pack)
                .map(|g| (g.id, None))
                .collect(),
            packs: generics
                .iter()
                .filter(|g| g.pack)
                .map(|g| (g.id, None))
                .collect(),
        }
    }

    fn is_empty(&self) -> bool {
        self.map.is_empty() && self.packs.is_empty()
    }

    fn owns(&self, id: u32) -> bool {
        self.map.contains_key(&id) || self.packs.contains_key(&id)
    }

    fn is_bound(&self, id: u32) -> bool {
        matches!(self.map.get(&id), Some(Some(_)))
    }

    fn bind(&mut self, id: u32, t: Type) {
        self.map.insert(id, Some(t));
    }

    /// Adds a pack variable that's already bound (for expanding aliases).
    pub(crate) fn add_pack(&mut self, id: u32, p: Pack) {
        self.packs.insert(id, Some(p));
    }

    /// Binds pack `id` if it's ours and still free, first match wins.
    pub(crate) fn bind_pack_once(&mut self, id: u32, p: Pack) {
        if let Some(slot @ None) = self.packs.get_mut(&id) {
            let widen = |t: &Type| t.widen();
            *slot = Some(Pack {
                types: p.types.iter().map(widen).collect(),
                variadic: p.variadic.as_ref().map(widen),
            });
        }
    }

    /// What pack `id` stands for, if it's ours (`...any` while it's unbound).
    pub(crate) fn pack(&self, id: u32) -> Option<Pack> {
        self.packs
            .get(&id)
            .map(|p| p.clone().unwrap_or_else(Pack::any))
    }

    /// Whether `t` mentions one of these generics.
    fn mentions(&self, t: &Type) -> bool {
        match t {
            Type::Generic(g) => self.owns(g.id),
            Type::Function(f) => {
                f.params.iter().any(|p| self.mentions(p))
                    || f.variadic.as_ref().is_some_and(|v| self.mentions(v))
                    || self.mentions_pack(&f.rets)
            }
            Type::Table(t) => {
                t.props.iter().any(|(_, p)| self.mentions(p))
                    || t.indexer
                        .as_ref()
                        .is_some_and(|(k, v)| self.mentions(k) || self.mentions(v))
            }
            Type::Union(ts) | Type::Intersection(ts) => ts.iter().any(|m| self.mentions(m)),
            Type::Named(n) => {
                n.args.iter().any(|a| self.mentions(a))
                    || n.packs.iter().any(|p| self.mentions_pack(p))
            }
            _ => false,
        }
    }

    fn mentions_pack(&self, p: &Pack) -> bool {
        p.types.iter().any(|r| self.mentions(r))
            || p.variadic.as_ref().is_some_and(|v| self.mentions(v))
    }

    /// `t` with these generics replaced (unbound ones become `any`).
    pub fn apply(&self, t: &Type) -> Type {
        if self.is_empty() || !self.mentions(t) {
            return t.clone();
        }
        match t {
            // a pack where one value goes (`local a = ...`), so any
            Type::Generic(g) if g.pack => Type::Any,
            Type::Generic(g) => match self.map.get(&g.id) {
                Some(Some(b)) => b.clone(),
                Some(None) => Type::Any,
                None => t.clone(),
            },
            Type::Function(f) => {
                let params = self.apply_pack(&Pack {
                    types: f.params.clone(),
                    variadic: f.variadic.clone(),
                });
                Type::function(FnType {
                    generics: f
                        .generics
                        .iter()
                        .filter(|g| !self.owns(g.id))
                        .cloned()
                        .collect(),
                    params: params.types,
                    variadic: params.variadic,
                    rets: self.apply_pack(&f.rets),
                })
            }
            Type::Table(tt) => Type::table(TableType {
                props: tt
                    .props
                    .iter()
                    .map(|(n, p)| (n.clone(), self.apply(p)))
                    .collect(),
                indexer: tt
                    .indexer
                    .as_ref()
                    .map(|(k, v)| (self.apply(k), self.apply(v))),
                unsealed: tt.unsealed,
                meta: tt.meta.clone(),
                index_self: tt.index_self,
                open: tt.open,
                access: tt.access.clone(),
            }),
            Type::Union(ts) => union(ts.iter().map(|m| self.apply(m)).collect::<Vec<_>>()),
            Type::Intersection(ts) => {
                intersection(ts.iter().map(|m| self.apply(m)).collect::<Vec<_>>())
            }
            Type::Named(n) => Type::Named(Rc::new(NamedType {
                alias: n.alias.clone(),
                args: n.args.iter().map(|a| self.apply(a)).collect(),
                packs: n.packs.iter().map(|p| self.apply_pack(p)).collect(),
                expanded: Default::default(),
            })),
            t => t.clone(),
        }
    }

    /// A pack with these generics replaced. A generic pack in its tail gets
    /// spliced in (`(number, T...)` with `T... = (string, boolean)` is
    /// `(number, string, boolean)`).
    pub fn apply_pack(&self, p: &Pack) -> Pack {
        let mut types: Vec<Type> = p.types.iter().map(|t| self.apply(t)).collect();
        let variadic = match &p.variadic {
            Some(v) => match v.pack_var().and_then(|g| self.pack(g.id)) {
                Some(bound) => {
                    types.extend(bound.types);
                    bound.variadic
                }
                None => Some(self.apply(v)),
            },
            None => None,
        };
        Pack { types, variadic }
    }
}
