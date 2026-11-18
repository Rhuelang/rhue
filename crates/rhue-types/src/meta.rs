//! Metatables in the checker: `setmetatable`, lookups through `__index`,
//! operators through metamethods, and the class pattern
//!
//! ```lua
//! local Account = {}
//! Account.__index = Account
//! function Account.new(balance: number)
//!     local self = setmetatable({}, Account)
//!     self.balance = balance
//!     return self
//! end
//! function Account:deposit(n: number) self.balance += n end
//! ```
//!
//! An instance's type remembers its metatable by the *local* that holds it
//! ([`Meta::Local`]), so methods added to the class after the instance was
//! made are still found. `T.__index = T` marks the class as its own
//! `__index` ([`TableType::index_self`]).
//!
//! Inside a `:` method of a class, `self` is an instance: the fields the
//! class's constructors give it (any function on the class that returns a
//! table with the class as its metatable, checked earlier in the chunk),
//! the class's methods through `__index`, and, since a method can't know
//! every field other methods add, any other field as `any`
//! ([`TableType::open`]). Before any constructor is seen, `self` is `any`.

use std::rc::Rc;

use rhue_common::Span;
use rhue_parser::ast::{Expr, ExprKind};

use rhue_compiler::resolve::LocalId;

use crate::check::Checker;
use crate::types::{Meta, Pack, TableType, Type};

/// How many `__index` tables a lookup follows.
const MAX_DEPTH: u32 = 8;

impl Checker<'_> {
    /// The table type of `t`'s metatable, if it has one the checker can see.
    /// `Err(())` when it has one that can't be resolved (treat as `any`).
    fn meta_table(&mut self, t: &TableType) -> Result<Option<Rc<TableType>>, ()> {
        let meta = match &t.meta {
            None => return Ok(None),
            Some(Meta::Local { id, .. }) => match self.locals.get(id).cloned() {
                Some(m) => m,
                None => return Err(()),
            },
            Some(Meta::Type(m)) => m.clone(),
        };
        match self.expand(&meta) {
            Type::Table(m) => Ok(Some(m)),
            _ => Err(()),
        }
    }

    /// `t.name` found through `t`'s metatable (`__index` tables, chains of
    /// them, or an `__index` function's result). `None`: not there.
    pub fn lookup_meta(&mut self, t: &TableType, name: &str) -> Option<Type> {
        self.lookup_meta_at(t, name, 0)
    }

    fn lookup_meta_at(&mut self, t: &TableType, name: &str, depth: u32) -> Option<Type> {
        if depth > MAX_DEPTH {
            return None;
        }
        let m = match self.meta_table(t) {
            Ok(Some(m)) => m,
            Ok(None) => return None,
            Err(()) => return Some(Type::Any),
        };
        if m.index_self {
            return match m.prop(name) {
                Some(p) => Some(p.clone()),
                None => self.lookup_meta_at(&m, name, depth + 1),
            };
        }
        let index = m.prop("__index")?.clone();
        match self.expand(&index) {
            t if t.is_any() => Some(Type::Any),
            Type::Table(i) => match i.prop(name) {
                Some(p) => Some(p.clone()),
                None => self.lookup_meta_at(&i, name, depth + 1),
            },
            Type::Function(f) => Some(f.rets.get(0)),
            _ => None,
        }
    }

    /// The type of `t`'s metamethod `event` (a raw field of its metatable).
    pub fn metamethod(&mut self, t: &Type, event: &str) -> Option<Type> {
        let Type::Table(table) = self.expand(t) else {
            return None;
        };
        match self.meta_table(&table) {
            Ok(Some(m)) => m.prop(event).cloned(),
            Ok(None) => None,
            Err(()) => Some(Type::Any),
        }
    }

    /// The result of an operator defined by a metamethod on either operand.
    pub fn meta_operator(&mut self, l: &Type, r: &Type, event: &str) -> Option<Type> {
        let mm = self
            .metamethod(l, event)
            .or_else(|| self.metamethod(r, event))?;
        Some(match self.expand(&mm) {
            Type::Function(f) => f.rets.get(0),
            _ => Type::Any,
        })
    }

    /// Whether assigning a missing field of `t` goes to a `__newindex`.
    pub fn has_newindex(&mut self, t: &TableType) -> bool {
        match self.meta_table(t) {
            Ok(Some(m)) => m.prop("__newindex").is_some(),
            Ok(None) => false,
            Err(()) => true,
        }
    }

    /// `setmetatable(t, mt)` (the built-in, not shadowed): `t`'s type with
    /// `mt` as its metatable.
    pub fn setmetatable_call(&mut self, args: &[Expr], span: Span) -> Pack {
        let (Some(t), Some(mt)) = (args.first(), args.get(1)) else {
            for a in args {
                self.expr(a);
            }
            self.error(span, "setmetatable needs a table and a metatable");
            return Pack::any();
        };
        let tt = self.expr(t);
        let mtt = self.expr(mt);
        for extra in &args[2..] {
            self.expr(extra);
        }
        let meta = match &mt.kind {
            ExprKind::Name(n) => match self.local_id(n) {
                Some(id) if matches!(self.locals.get(&id), Some(Type::Table(_))) => Meta::Local {
                    id,
                    name: n.text.clone(),
                },
                _ => Meta::Type(mtt.clone()),
            },
            _ => Meta::Type(mtt.clone()),
        };
        match self.expand(&mtt) {
            m if m.is_any() || matches!(m, Type::Table(_) | Type::Nil) => {}
            other => self.error(
                mt.span,
                format!("setmetatable's metatable must be a table, got '{other}'"),
            ),
        }
        match self.expand(&tt) {
            Type::Table(table) => {
                let mut with = (*table).clone();
                with.meta = if matches!(self.expand(&mtt), Type::Nil) {
                    None
                } else {
                    Some(meta)
                };
                Pack::one(Type::table(with))
            }
            t if t.is_any() => Pack::any(),
            other => {
                self.error(t.span, format!("setmetatable needs a table, got '{other}'"));
                Pack::any()
            }
        }
    }

    /// `T.__index = T` on a local holding a table literal's type: marks the
    /// table as a class. Returns `true` when it handled the assignment.
    pub fn class_index(&mut self, target: &Expr, value: &Expr) -> bool {
        let ExprKind::Index { object, key } = &target.kind else {
            return false;
        };
        let (ExprKind::Name(o), ExprKind::String(k), ExprKind::Name(v)) =
            (&object.kind, &key.kind, &value.kind)
        else {
            return false;
        };
        if &**k != "__index" {
            return false;
        }
        let (Some(oid), Some(vid)) = (self.local_id(o), self.local_id(v)) else {
            return false;
        };
        if oid != vid {
            return false;
        }
        let Some(Type::Table(t)) = self.locals.get(&oid).cloned() else {
            return false;
        };
        if !t.unsealed {
            return false;
        }
        let mut class = (*t).clone();
        class.index_self = true;
        self.locals.insert(oid, Type::table(class));
        self.invalidate(oid);
        true
    }

    /// Whether `callee` is the built-in global `name` (not a local, and not
    /// redefined by the script).
    pub fn is_builtin(&mut self, callee: &Expr, name: &str) -> bool {
        match &callee.kind {
            ExprKind::Name(n) => {
                &*n.text == name
                    && self.local_id(n).is_none()
                    && !self.chunk_globals.contains_key(&n.text)
                    && !self.assigned_globals.contains(&n.text)
                    && self.env.global(name).is_some()
            }
            _ => false,
        }
    }

    /// Remembers what a function on class `class` returned, if it is an
    /// instance of the class (a table whose metatable is that local).
    pub fn note_instance(&mut self, class: LocalId, ret: &Type) {
        let Type::Table(t) = self.expand(ret) else {
            return;
        };
        match &t.meta {
            Some(Meta::Local { id, .. }) if *id == class => {}
            _ => return,
        }
        let merged = match self.instances.get(&class) {
            // Another constructor: fields either one gives.
            Some(known) => {
                let mut m = (**known).clone();
                for (n, p) in &t.props {
                    if m.prop(n).is_none() {
                        m.props.push((n.clone(), p.clone()));
                    }
                }
                m
            }
            None => (*t).clone(),
        };
        self.instances.insert(class, Rc::new(merged));
    }

    /// `self` in a method of class `class` (named `name`).
    pub fn instance_type(&mut self, class: LocalId, name: &Rc<str>) -> Type {
        match self.instances.get(&class) {
            Some(known) => Type::table(TableType {
                props: known.props.clone(),
                indexer: known.indexer.clone(),
                unsealed: false,
                meta: Some(Meta::Local {
                    id: class,
                    name: name.clone(),
                }),
                index_self: false,
                open: true,
                access: known.access.clone(),
            }),
            None => Type::Any,
        }
    }

    /// What `value.` / `value:` can name on a value of type `t`: its
    /// fields, then what its metatable's `__index` adds (classes, chains),
    /// a host type's members, or the string library for strings.
    pub fn members_of(&mut self, t: &Type) -> Vec<(Rc<str>, Type)> {
        let mut out: Vec<(Rc<str>, Type)> = Vec::new();
        let add = |out: &mut Vec<(Rc<str>, Type)>, n: &Rc<str>, t: &Type| {
            if !out.iter().any(|(m, _)| m == n) {
                out.push((n.clone(), t.clone()));
            }
        };
        let t = self.expand(t);
        let t = if t.can_be_nil() { t.strip_nil() } else { t };
        match self.expand(&t) {
            Type::Table(mut table) => {
                for _ in 0..MAX_DEPTH {
                    for (n, p) in &table.props {
                        if !n.starts_with("__") {
                            add(&mut out, n, p);
                        }
                    }
                    let next = match self.meta_table(&table) {
                        Ok(Some(m)) if m.index_self => m,
                        Ok(Some(m)) => match m.prop("__index").map(|i| self.expand(i)) {
                            Some(Type::Table(i)) => i,
                            _ => break,
                        },
                        _ => break,
                    };
                    table = next;
                }
            }
            Type::Class(c) => {
                if let Some(class) = self.env.class(&c) {
                    for (n, p, _) in &class.props {
                        add(&mut out, n, p);
                    }
                    for (n, m) in &class.methods {
                        add(&mut out, n, m);
                    }
                }
            }
            Type::String | Type::StrLit(_) => {
                if let Some(Type::Table(lib)) = self.env.global("string") {
                    for (n, p) in &lib.props {
                        add(&mut out, n, p);
                    }
                }
            }
            _ => {}
        }
        out
    }

    /// Whether a table type is a class (`T.__index = T`).
    pub fn is_class(&mut self, t: &Type) -> bool {
        matches!(self.expand(t), Type::Table(t) if t.index_self)
    }
}
