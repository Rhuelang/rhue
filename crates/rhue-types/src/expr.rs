//! Typing expressions, calls, indexing, function bodies and conditions.

use std::rc::Rc;

use rhue_common::Span;
use rhue_parser::ast::*;

use crate::check::{Checker, FuncCtx, Refine, always_exits};
use crate::env::{ClassMember, any_function};
use crate::relate::Subst;
use crate::types::*;

impl Checker<'_> {
    /// The type of one value of `e` (the first one, for calls and `...`).
    pub fn expr(&mut self, e: &Expr) -> Type {
        let t = stacker::maybe_grow(64 * 1024, 1024 * 1024, || self.expr_inner(e, None));
        self.record(e.span, &t);
        t
    }

    /// Like [`Self::expr`], but with the type the context expects. Table and
    /// function literals get typed to fit it (fields keep literal types, a
    /// callback's parameters come from the expected function type).
    pub fn expr_hinted(&mut self, e: &Expr, hint: Option<&Type>) -> Type {
        let t = stacker::maybe_grow(64 * 1024, 1024 * 1024, || self.expr_inner(e, hint));
        self.record(e.span, &t);
        t
    }

    fn expr_inner(&mut self, e: &Expr, hint: Option<&Type>) -> Type {
        match &e.kind {
            ExprKind::Nil => Type::Nil,
            ExprKind::Bool(b) => Type::BoolLit(*b),
            ExprKind::Number(_) => Type::Number,
            ExprKind::String(s) => Type::StrLit(s.clone()),
            // any value can go in a hole (it goes through `tostring`)
            ExprKind::Interp(parts) => {
                for part in parts {
                    if let InterpPart::Expr(x) = part {
                        self.expr(x);
                    }
                }
                Type::String
            }
            ExprKind::Name(n) => match self.local_id(n) {
                Some(id) => self.local_type(id),
                None => self.global_type(n),
            },
            ExprKind::Group(inner) => self.expr_hinted(inner, hint),
            ExprKind::Unary {
                op,
                operand,
                op_span,
            } => {
                let t = self.expr(operand);
                match op {
                    UnOp::Neg if self.is_vector(&t) => t,
                    UnOp::Neg => match self.meta_operator(&t, &t, "__unm") {
                        Some(result) => result,
                        None => {
                            self.expect(operand.span, &t, &Type::Number);
                            Type::Number
                        }
                    },
                    UnOp::Not => Type::Boolean,
                    UnOp::Len => {
                        let e = self.expand(&t);
                        let ok = e.is_any()
                            || matches!(e, Type::String | Type::StrLit(_) | Type::Table(_))
                            || self.sub(&t, &Type::String);
                        if !ok {
                            self.error(
                                *op_span,
                                format!("Cannot get the length of a value of type '{t}'"),
                            );
                        }
                        Type::Number
                    }
                }
            }
            ExprKind::Binary {
                op,
                op_span,
                lhs,
                rhs,
            } => match op {
                BinOp::And | BinOp::Or => self.cond(e, hint).0,
                _ => {
                    let l = self.expr(lhs);
                    let r = self.expr(rhs);
                    self.binary_type(*op, &l, &r, lhs.span, rhs.span, *op_span)
                }
            },
            ExprKind::IfElse {
                branches,
                else_expr,
            } => {
                let mut types = Vec::new();
                let mut pushed = 0;
                for (cond, value) in branches {
                    let r = self.condition(cond);
                    types.push(self.with_layer(&r.truthy, |c| c.expr_hinted(value, hint)));
                    self.layers.push(crate::check::Layer {
                        depth: self.funcs.len(),
                        map: r.falsy.into_iter().collect(),
                    });
                    pushed += 1;
                }
                types.push(self.expr_hinted(else_expr, hint));
                for _ in 0..pushed {
                    self.layers.pop();
                }
                union(types)
            }
            ExprKind::Call { .. } | ExprKind::MethodCall { .. } | ExprKind::Vararg => {
                self.expr_pack(e).first()
            }
            ExprKind::Function(func) => {
                let hint_fn = hint.and_then(|h| match self.expand(h) {
                    Type::Function(f) => Some(f),
                    t => t.members().into_iter().find_map(|m| match m {
                        Type::Function(f) => Some(f),
                        _ => None,
                    }),
                });
                Type::function(self.function(func, hint_fn.as_deref(), None))
            }
            ExprKind::Index { object, key } => {
                let ot = self.expr(object);
                if let Some(t) = self.path_of(e).and_then(|p| self.narrowed(&p)) {
                    return t;
                }
                match &key.kind {
                    ExprKind::String(name) => {
                        let from = self.errors.len();
                        let t = self.index_name(&ot, name, e.span);
                        self.suggest_member(from, &ot, name, key.span);
                        self.suggest_assert(from, e.span, object.span);
                        t
                    }
                    _ => {
                        let kt = self.expr(key);
                        self.index_key(&ot, &kt, key.span, e.span)
                    }
                }
            }
            ExprKind::Table(items) => self.table_literal(items, hint),
            ExprKind::Cast { expr, ty } => {
                let t = self.expr(expr);
                let target = self.resolve_type(ty);
                if !self.sub(&t, &target) && !self.sub(&target, &t) {
                    self.error(
                        e.span,
                        format!(
                            "Cannot cast '{t}' into '{target}' because the types are unrelated"
                        ),
                    );
                }
                target
            }
        }
    }

    /// All the values of `e`, calls and `...` give several.
    pub fn expr_pack(&mut self, e: &Expr) -> Pack {
        let p = self.expr_pack_inner(e);
        if self.types.is_some()
            && matches!(e.kind, ExprKind::Call { .. } | ExprKind::MethodCall { .. })
        {
            self.record(e.span, &p.get(0));
        }
        p
    }

    fn expr_pack_inner(&mut self, e: &Expr) -> Pack {
        match &e.kind {
            ExprKind::Call { callee, args } if self.is_builtin(callee, "setmetatable") => {
                self.setmetatable_call(args, e.span)
            }
            // `require("name")`, the module's type once the engine has checked
            // it (it asks for the names it doesn't know yet)
            ExprKind::Call { callee, args }
                if args.len() == 1
                    && matches!(args[0].kind, ExprKind::String(_))
                    && self.is_builtin(callee, "require") =>
            {
                let ExprKind::String(name) = &args[0].kind else {
                    unreachable!()
                };
                match self.env.module_type(name) {
                    Some(t) => Pack::one(t.clone()),
                    None => {
                        if !self.requires.contains(name) {
                            self.requires.push(name.clone());
                        }
                        Pack::any()
                    }
                }
            }
            ExprKind::Call { callee, args } => {
                let ct = self.expr(callee);
                let name = callee_name(callee);
                let from = self.errors.len();
                let mut p = self.call(&ct, None, args, e.span, name.as_deref());
                self.suggest_assert(from, e.span, callee.span);
                // `assert(x)` returns `x`, which can't be nil if it returned
                if !p.types.is_empty() && self.is_builtin(callee, "assert") {
                    p.types[0] = p.types[0].strip_nil();
                }
                // `Instance.new("Box")` is a `Box`
                if let Some(c) = self.global_class_literal(callee, args)
                    && let Some(first) = p.types.first_mut()
                {
                    *first = self.as_class(first, &c);
                }
                p
            }
            ExprKind::MethodCall {
                object,
                method,
                method_span,
                args,
            } => {
                let ot = self.expr(object);
                let from = self.errors.len();
                let ft = self.method_type(&ot, method, *method_span);
                self.suggest_member(from, &ot, method, *method_span);
                self.suggest_assert(from, *method_span, object.span);
                let mut p = self.call(&ft, Some(ot.clone()), args, e.span, Some(method));
                // `FindFirstChildOfClass("Box")` is a `Box?`
                if let Some(c) = self.class_literal(&ot, method, args, true)
                    && let Some(first) = p.types.first_mut()
                {
                    *first = self.as_class(first, &c);
                }
                p
            }
            ExprKind::Vararg => {
                let v = self.funcs.last().map_or(Type::Any, |f| f.vararg.clone());
                Pack {
                    types: Vec::new(),
                    variadic: Some(v),
                }
            }
            _ => Pack::one(self.expr(e)),
        }
    }

    /// An expression list. Every value but the last is one value, the last one
    /// expands. `hints[i]` is the expected type of value `i`, if there is one.
    pub fn exprs_pack(&mut self, exprs: &[Expr], hints: &[Option<Type>]) -> Pack {
        let mut pack = Pack::default();
        for (i, e) in exprs.iter().enumerate() {
            if i + 1 == exprs.len() && e.kind.is_multi() {
                let p = self.expr_pack(e);
                pack.types.extend(p.types);
                pack.variadic = p.variadic;
            } else {
                let hint = hints.get(i).and_then(|h| h.as_ref());
                pack.types.push(self.expr_hinted(e, hint));
            }
        }
        pack
    }

    // ─ Operators ─────────────────────────────────────────────────────────────

    pub fn binary_type(
        &mut self,
        op: BinOp,
        l: &Type,
        r: &Type,
        lspan: Span,
        rspan: Span,
        op_span: Span,
    ) -> Type {
        use BinOp::*;
        let event = match op {
            Add => "__add",
            Sub => "__sub",
            Mul => "__mul",
            Div => "__div",
            IDiv => "__idiv",
            Mod => "__mod",
            Pow => "__pow",
            Concat => "__concat",
            Lt | Gt => "__lt",
            Le | Ge => "__le",
            Eq | Ne | And | Or => "",
        };
        if let Some(t) = self.vector_arith(op, l, r, op_span) {
            return t;
        }
        if !event.is_empty()
            && let Some(result) = self.meta_operator(l, r, event)
        {
            return if matches!(op, Lt | Gt | Le | Ge) {
                Type::Boolean
            } else {
                result
            };
        }
        match op {
            Add | Sub | Mul | Div | IDiv | Mod | Pow => {
                self.expect(lspan, l, &Type::Number);
                self.expect(rspan, r, &Type::Number);
                Type::Number
            }
            Concat => {
                let ok = union([Type::String, Type::Number]);
                self.expect(lspan, l, &ok);
                self.expect(rspan, r, &ok);
                Type::String
            }
            Eq | Ne => Type::Boolean,
            Lt | Le | Gt | Ge => {
                let both = |c: &mut Self, t: &Type| c.sub(l, t) && c.sub(r, t);
                if !(l.is_any()
                    || r.is_any()
                    || both(self, &Type::Number)
                    || both(self, &Type::String))
                {
                    self.error(
                        op_span,
                        format!(
                            "Types '{l}' and '{r}' cannot be compared with '{}'",
                            op.symbol()
                        ),
                    );
                }
                Type::Boolean
            }
            And | Or => unreachable!("handled with narrowing"),
        }
    }

    fn is_vector(&mut self, t: &Type) -> bool {
        matches!(self.expand(t), Type::Class(c) if &*c == "vector")
    }

    /// Arithmetic involving a vector, the way Luau allows it: `+ - * / //`
    /// between vectors, and `v * n`, `n * v`, `v / n`, `n / v`, `v // n`.
    /// `None` when neither side is a vector.
    fn vector_arith(&mut self, op: BinOp, l: &Type, r: &Type, span: Span) -> Option<Type> {
        use BinOp::*;
        let (lv, rv) = (self.is_vector(l), self.is_vector(r));
        if !(lv || rv) || !matches!(op, Add | Sub | Mul | Div | IDiv | Mod | Pow) {
            return None;
        }
        let number = |c: &mut Self, t: &Type| t.is_any() || c.sub(t, &Type::Number);
        let other_ok = match (lv, rv) {
            (true, true) => !matches!(op, Mod | Pow),
            (true, false) => r.is_any() || (matches!(op, Mul | Div | IDiv) && number(self, r)),
            (false, true) => l.is_any() || (matches!(op, Mul | Div) && number(self, l)),
            (false, false) => unreachable!("one is a vector"),
        };
        if !other_ok {
            self.error(
                span,
                format!(
                    "Operator '{}' can't be applied to '{l}' and '{r}'",
                    op.symbol()
                ),
            );
            return Some(Type::Error);
        }
        Some(Type::Class(Rc::from("vector")))
    }

    // ─ Calls ─────────────────────────────────────────────────────────────────

    /// Calls a value of type `callee` (with `self_ty` as the first argument for
    /// a method call) and returns the results.
    pub fn call(
        &mut self,
        callee: &Type,
        self_ty: Option<Type>,
        args: &[Expr],
        span: Span,
        name: Option<&str>,
    ) -> Pack {
        match self.expand(callee) {
            Type::Function(f) => self.call_fn(&f, self_ty, args, span, name),
            Type::Intersection(members) => {
                let tys = self.arg_types(self_ty, args, span);
                for m in members.iter() {
                    if let Type::Function(f) = self.expand(m) {
                        let before = self.errors.len();
                        let rets =
                            self.check_args(&f, &mut Subst::new(&f.generics), &tys, span, name);
                        if self.errors.len() == before {
                            return rets;
                        }
                        self.errors.truncate(before);
                    }
                }
                let shown: Vec<String> = tys.0.iter().map(|(t, _)| t.to_string()).collect();
                self.error(
                    span,
                    format!(
                        "No overload of '{}' accepts arguments of type ({})",
                        name.unwrap_or("function"),
                        shown.join(", ")
                    ),
                );
                Pack::any()
            }
            Type::Union(_) if callee.can_be_nil() || self.expand(callee).can_be_nil() => {
                let e = self.expand(callee);
                if self.strict {
                    self.error(span, format!("Value of type '{e}' could be nil"));
                }
                self.call(&e.strip_nil(), self_ty, args, span, name)
            }
            Type::Union(members) => {
                // any member could be the one getting called, check against
                // each
                let tys = self.arg_types(self_ty, args, span);
                let mut results = Vec::new();
                for m in members.iter() {
                    match self.expand(m) {
                        Type::Function(f) => results.push(self.check_args(
                            &f,
                            &mut Subst::new(&f.generics),
                            &tys,
                            span,
                            name,
                        )),
                        Type::Any | Type::Error => results.push(Pack::any()),
                        other => {
                            self.error(span, format!("Cannot call a value of type '{other}'"));
                            return Pack::any();
                        }
                    }
                }
                let n = results.iter().map(|p| p.types.len()).max().unwrap_or(0);
                Pack {
                    types: (0..n)
                        .map(|i| union(results.iter().map(|p| p.get(i)).collect::<Vec<_>>()))
                        .collect(),
                    variadic: results
                        .iter()
                        .filter_map(|p| p.variadic.clone())
                        .reduce(|a, b| union([a, b])),
                }
            }
            t if t.is_any() => {
                self.arg_types(self_ty, args, span);
                Pack::any()
            }
            Type::Never => {
                self.arg_types(self_ty, args, span);
                Pack::one(Type::Never)
            }
            other => {
                // a table with `__call` gets called with itself first
                if let Some(call) = self.metamethod(&other, "__call") {
                    return match (self.expand(&call), self_ty) {
                        (Type::Function(f), None) => {
                            self.call_fn(&f, Some(other.clone()), args, span, name)
                        }
                        (_, self_ty) => {
                            self.arg_types(self_ty, args, span);
                            Pack::any()
                        }
                    };
                }
                self.arg_types(self_ty, args, span);
                self.error(span, format!("Cannot call a value of type '{other}'"));
                Pack::any()
            }
        }
    }

    /// Argument types without expectations (for overloads and unions): the
    /// fixed values with their spans, then any variadic tail.
    fn arg_types(&mut self, self_ty: Option<Type>, args: &[Expr], span: Span) -> ArgTypes {
        let mut tys: Vec<(Type, Span)> = self_ty.into_iter().map(|t| (t, span)).collect();
        let mut tail = None;
        for (i, a) in args.iter().enumerate() {
            if i + 1 == args.len() && a.kind.is_multi() {
                let p = self.expr_pack(a);
                tys.extend(p.types.into_iter().map(|t| (t, a.span)));
                tail = p.variadic;
            } else {
                let t = self.expr(a);
                tys.push((t, a.span));
            }
        }
        ArgTypes(tys, tail)
    }

    fn call_fn(
        &mut self,
        f: &FnType,
        self_ty: Option<Type>,
        args: &[Expr],
        span: Span,
        name: Option<&str>,
    ) -> Pack {
        let mut subst = Subst::new(&f.generics);
        let mut tys: Vec<(Type, Span)> = self_ty.into_iter().map(|t| (t, span)).collect();
        let mut tail = None;
        // literals wait until other arguments have pinned down the generics, so
        // a callback can be typed by say the table it's sorting
        let mut deferred = Vec::new();
        for (i, a) in args.iter().enumerate() {
            if i + 1 == args.len() && a.kind.is_multi() {
                let p = self.expr_pack(a);
                tys.extend(p.types.into_iter().map(|t| (t, a.span)));
                tail = p.variadic;
            } else if matches!(a.kind, ExprKind::Function(_) | ExprKind::Table(_)) {
                deferred.push((tys.len(), i));
                tys.push((Type::Any, a.span));
            } else {
                let t = self.expr(a);
                tys.push((t, a.span));
            }
        }
        for (j, (t, _)) in tys.iter().enumerate() {
            if !deferred.iter().any(|&(d, _)| d == j) {
                let p = param_type(f, j);
                self.unify(&p, t, &mut subst);
            }
        }
        for (j, i) in deferred {
            let expected = subst.apply(&param_type(f, j));
            let t = self.expr_hinted(&args[i], Some(&expected));
            self.unify(&param_type(f, j), &t, &mut subst);
            tys[j].0 = t;
        }
        self.check_args(f, &mut subst, &ArgTypes(tys, tail), span, name)
    }

    /// Checks argument types against `f`'s parameters, returns its results with
    /// generics substituted.
    fn check_args(
        &mut self,
        f: &FnType,
        subst: &mut Subst,
        args: &ArgTypes,
        span: Span,
        name: Option<&str>,
    ) -> Pack {
        let ArgTypes(tys, tail) = args;
        for (j, (t, _)) in tys.iter().enumerate() {
            let p = param_type(f, j);
            self.unify(&p, t, subst);
        }
        let shown = name.map_or(String::new(), |n| format!(" '{n}'"));
        let fixed = f.params.len();
        // `...: T...`, the extra arguments are the pack (unless something else,
        // like a callback's parameters, already pinned it)
        if let Some(g) = f.variadic.as_ref().and_then(Type::pack_var) {
            let extra: Vec<Type> = tys.iter().skip(fixed).map(|(t, _)| t.clone()).collect();
            subst.bind_pack_once(
                g.id,
                Pack {
                    types: extra,
                    variadic: tail.clone(),
                },
            );
            if let Some(bound) = subst.pack(g.id) {
                // check the extras (and their tail) against what it is
                let args = ArgTypes(tys.iter().skip(fixed).cloned().collect(), tail.clone());
                let as_fn = FnType {
                    generics: Vec::new(),
                    params: bound.types,
                    variadic: bound.variadic,
                    rets: Pack::empty(),
                };
                let fixed_only = FnType {
                    params: f.params.clone(),
                    variadic: None,
                    ..as_fn.clone()
                };
                let head = ArgTypes(tys.iter().take(fixed).cloned().collect(), None);
                if tys.len() >= fixed {
                    self.check_args(&fixed_only, subst, &head, span, name);
                    self.check_args(&as_fn, &mut Subst::new(&[]), &args, span, name);
                    return subst.apply_pack(&f.rets);
                }
            }
        }
        for (j, (t, s)) in tys.iter().enumerate() {
            if j >= fixed && f.variadic.is_none() {
                self.error(
                    span,
                    format!(
                        "Argument count mismatch. Function{shown} expects {fixed} argument{}, but {} are specified",
                        if fixed == 1 { "" } else { "s" },
                        tys.len()
                    ),
                );
                break;
            }
            let p = subst.apply(&param_type(f, j));
            self.expect(*s, t, &p);
        }
        for j in tys.len()..fixed {
            let p = subst.apply(&f.params[j]);
            match tail {
                Some(t) => {
                    self.expect(span, t, &p);
                }
                None if !self.sub(&Type::Nil, &p) => {
                    let required = (0..fixed)
                        .rev()
                        .find(|&k| !self.sub(&Type::Nil, &f.params[k]))
                        .map_or(0, |k| k + 1);
                    self.error(
                        span,
                        format!(
                            "Argument count mismatch. Function{shown} expects {}{required} argument{}, but {} {} specified",
                            if required < fixed || f.variadic.is_some() { "at least " } else { "" },
                            if required == 1 { "" } else { "s" },
                            tys.len(),
                            if tys.len() == 1 { "is" } else { "are" }
                        ),
                    );
                    break;
                }
                None => {}
            }
        }
        if let (Some(t), Some(v)) = (tail, &f.variadic) {
            let v = subst.apply(v);
            self.expect(span, t, &v);
        }
        subst.apply_pack(&f.rets)
    }

    /// The function `obj:name(...)` calls.
    pub fn method_type(&mut self, ot: &Type, name: &str, span: Span) -> Type {
        match self.expand(ot) {
            Type::String | Type::StrLit(_) => match self.env.string_method(name) {
                Some(t) => t,
                None => {
                    self.error(span, format!("Key '{name}' not found in string"));
                    Type::Error
                }
            },
            Type::Class(c) => match self.env.class(&c) {
                Some(_) => match self.env.class_member(&c, name, true) {
                    Some(ClassMember::Method(t) | ClassMember::Prop(t, _)) => t.clone(),
                    None => match self.env.class_index(&c) {
                        Some(t) => t.clone(),
                        None => {
                            self.error(span, format!("Key '{name}' not found in class '{c}'"));
                            Type::Error
                        }
                    },
                },
                None => Type::Any,
            },
            _ => self.index_name(ot, name, span),
        }
    }

    // ─ Indexing ──────────────────────────────────────────────────────────────

    pub fn index_name(&mut self, ot: &Type, name: &str, span: Span) -> Type {
        match self.expand(ot) {
            t if t.is_any() => Type::Any,
            Type::Table(t) => {
                if let Some(p) = t.prop(name) {
                    if !t.access(name).can_read() {
                        self.error(span, format!("Property '{name}' is write-only"));
                        return Type::Error;
                    }
                    return p.clone();
                }
                if let Some((k, v)) = &t.indexer
                    && self.sub(&Type::StrLit(Rc::from(name)), k)
                {
                    return v.clone();
                }
                if let Some(found) = self.lookup_meta(&t, name) {
                    return found;
                }
                // a field some method (or other code) added to instances of
                // this class since this instance type was taken
                if let Some(found) = self.added_field(&t, name) {
                    return found;
                }
                if t.is_empty_literal() || t.open {
                    return Type::Any;
                }
                self.error(span, format!("Key '{name}' not found in table '{t}'"));
                Type::Error
            }
            Type::Class(c) => {
                let found = match self.env.class_member(&c, name, false) {
                    Some(ClassMember::Method(t) | ClassMember::Prop(t, _)) => Some(t.clone()),
                    // a name the host's index hook looks up
                    None => self.env.class_index(&c).cloned(),
                };
                found.unwrap_or_else(|| {
                    self.error(span, format!("Key '{name}' not found in class '{c}'"));
                    Type::Error
                })
            }
            Type::String | Type::StrLit(_) => self.env.string_method(name).unwrap_or_else(|| {
                self.error(span, format!("Key '{name}' not found in string"));
                Type::Error
            }),
            u @ Type::Union(_) => self.index_union(&u, span, |c, m| c.index_name(m, name, span)),
            Type::Intersection(ms) => {
                for m in ms.iter() {
                    let before = self.errors.len();
                    let t = self.index_name(m, name, span);
                    if self.errors.len() == before {
                        return t;
                    }
                    self.errors.truncate(before);
                }
                self.error(span, format!("Key '{name}' not found in '{ot}'"));
                Type::Error
            }
            other => {
                self.error(span, format!("Type '{other}' does not have key '{name}'"));
                Type::Error
            }
        }
    }

    pub fn index_key(&mut self, ot: &Type, kt: &Type, key_span: Span, span: Span) -> Type {
        if let Type::StrLit(s) = kt
            && matches!(self.expand(ot), Type::Table(_) | Type::Class(_))
        {
            return self.index_name(ot, &s.clone(), span);
        }
        match self.expand(ot) {
            t if t.is_any() => Type::Any,
            Type::Table(t) => match &t.indexer {
                Some((k, v)) => {
                    self.expect(key_span, kt, k);
                    v.clone()
                }
                None if t.is_empty_literal() || kt.is_any() => Type::Any,
                None => {
                    self.error(span, format!("Cannot index table '{t}' with '{kt}'"));
                    Type::Error
                }
            },
            u @ Type::Union(_) => {
                self.index_union(&u, span, |c, m| c.index_key(m, kt, key_span, span))
            }
            other => {
                self.error(
                    span,
                    format!("Cannot index a value of type '{other}' with '{kt}'"),
                );
                Type::Error
            }
        }
    }

    fn index_union(
        &mut self,
        u: &Type,
        span: Span,
        mut f: impl FnMut(&mut Self, &Type) -> Type,
    ) -> Type {
        let mut members = u.members();
        if u.can_be_nil() {
            if self.strict {
                self.error(span, format!("Value of type '{u}' could be nil"));
            }
            members.retain(|m| !matches!(m, Type::Nil));
        }
        union(members.iter().map(|m| f(self, m)).collect::<Vec<_>>())
    }

    /// A field added to instances of `t`'s class after `t` was taken.
    fn added_field(&self, t: &TableType, name: &str) -> Option<Type> {
        match &t.meta {
            Some(Meta::Local { id, .. }) => self.instances.get(id)?.prop(name).cloned(),
            _ => None,
        }
    }

    /// Records that instances of `t`'s class (if it's an instance) can have
    /// field `name`, holding `value`'s type or what it held before. It's
    /// optional, code might read it before whatever assigns it has run. Returns
    /// whether `t` was an instance.
    fn add_field(&mut self, t: &TableType, name: &str, value: &Type, before: Option<Type>) -> bool {
        let Some(Meta::Local { id, .. }) = &t.meta else {
            return false;
        };
        let Some(known) = self.instances.get(id) else {
            return false;
        };
        let value = value.widen();
        if matches!(value, Type::Nil) && before.is_none() {
            // `self.x = nil` doesn't say anything about what `x` will hold
            return false;
        }
        let mut grown = (**known).clone();
        let t = Type::optional(union([before.unwrap_or(Type::Never), value]));
        match grown.props.iter_mut().find(|(n, _)| &**n == name) {
            Some(slot) => slot.1 = t,
            None => grown.props.push((Rc::from(name), t)),
        }
        self.instances.insert(*id, Rc::new(grown));
        self.added_fields.insert((*id, Rc::from(name)));
        true
    }

    pub fn assign_prop(&mut self, object: &Expr, ot: &Type, name: &str, value: &Type, span: Span) {
        match self.expand(ot) {
            t if t.is_any() => {}
            Type::Table(t) => {
                let added = match &t.meta {
                    Some(Meta::Local { id, .. }) => {
                        self.added_fields.contains(&(*id, Rc::from(name)))
                    }
                    _ => false,
                };
                if added {
                    // a field added by assignment. methods (`self`, open) grow
                    // it, other code has to fit what they put there
                    let before = self.added_field(&t, name);
                    if t.open {
                        self.add_field(&t, name, value, before);
                    } else if let Some(field) = before {
                        self.expect(span, value, &field);
                    }
                } else if let Some(p) = t.prop(name) {
                    if !t.access(name).can_write() {
                        self.error(span, format!("Property '{name}' is read-only"));
                        return;
                    }
                    let p = p.clone();
                    self.expect(span, value, &p);
                } else if let Some((k, v)) = t
                    .indexer
                    .clone()
                    .filter(|(k, _)| self.sub(&Type::StrLit(Rc::from(name)), k))
                {
                    let _ = k;
                    self.expect(span, value, &v);
                } else if t.unsealed {
                    let mut grown = (*t).clone();
                    grown.props.push((Rc::from(name), value.widen()));
                    self.grow_table(object, Type::table(grown), &t, span, name);
                } else if let Some(field) = self.added_field(&t, name) {
                    // added before (see `add_field`)
                    if t.open {
                        self.add_field(&t, name, value, Some(field));
                    } else {
                        self.expect(span, value, &field);
                    }
                } else if t.open && !self.has_newindex(&t) && self.add_field(&t, name, value, None)
                {
                    // a new field on an instance (`self.cache = {}`)
                } else if t.open || self.has_newindex(&t) {
                    // unknown to the checker, or `__newindex` decides
                } else {
                    self.error(span, format!("Cannot add property '{name}' to table '{t}'"));
                }
            }
            Type::Class(c) => {
                let (prop, is_method) = match self.env.class_member(&c, name, false) {
                    Some(ClassMember::Prop(t, w)) => (Some((t.clone(), w)), false),
                    Some(ClassMember::Method(_)) => (None, true),
                    None => (None, false),
                };
                match prop {
                    Some((t, true)) => {
                        self.expect(span, value, &t);
                    }
                    Some((_, false)) => {
                        self.error(span, format!("Property '{name}' of '{c}' is read-only"))
                    }
                    None if is_method => self.error(
                        span,
                        format!("Cannot assign to '{name}': it is a method of '{c}'"),
                    ),
                    None => self.error(span, format!("Key '{name}' not found in class '{c}'")),
                }
            }
            u @ Type::Union(_) if u.can_be_nil() => {
                if self.strict {
                    self.error(span, format!("Value of type '{u}' could be nil"));
                }
                self.assign_prop(object, &u.strip_nil(), name, value, span);
            }
            other => self.error(span, format!("Type '{other}' does not have key '{name}'")),
        }
    }

    pub fn assign_indexed(
        &mut self,
        object: &Expr,
        ot: &Type,
        kt: &Type,
        value: &Type,
        span: Span,
    ) {
        if let Type::StrLit(s) = kt {
            return self.assign_prop(object, ot, &s.clone(), value, span);
        }
        match self.expand(ot) {
            t if t.is_any() => {}
            Type::Table(t) => match &t.indexer {
                Some((k, v)) => {
                    let (k, v) = (k.clone(), v.clone());
                    self.expect(span, kt, &k);
                    self.expect(span, value, &v);
                }
                None if t.unsealed => {
                    let mut grown = (*t).clone();
                    grown.indexer = Some((kt.widen(), value.widen()));
                    self.grow_table(object, Type::table(grown), &t, span, "[...]");
                }
                None if kt.is_any() => {}
                None => self.error(span, format!("Cannot index table '{t}' with '{kt}'")),
            },
            u @ Type::Union(_) if u.can_be_nil() => {
                if self.strict {
                    self.error(span, format!("Value of type '{u}' could be nil"));
                }
                self.assign_indexed(object, &u.strip_nil(), kt, value, span);
            }
            other => self.error(
                span,
                format!("Cannot index a value of type '{other}' with '{kt}'"),
            ),
        }
    }

    /// A table literal's type gains a field, only through the local (or global)
    /// holding it.
    fn grow_table(&mut self, object: &Expr, grown: Type, was: &TableType, span: Span, what: &str) {
        if let ExprKind::Name(n) = &object.kind {
            match self.local_id(n) {
                Some(id) => {
                    let base_is_this = matches!(self.locals.get(&id), Some(Type::Table(_)));
                    if base_is_this {
                        self.locals.insert(id, grown);
                        self.invalidate(id);
                        return;
                    }
                }
                None if self.chunk_globals.contains_key(&n.text) => {
                    self.chunk_globals.insert(n.text.clone(), grown);
                    return;
                }
                None => {}
            }
        }
        if !self.strict {
            return;
        }
        self.error(
            span,
            format!("Cannot add property '{what}' to table '{was}'"),
        );
    }

    // ─ Literals ──────────────────────────────────────────────────────────────

    pub fn table_literal(&mut self, items: &[TableItem], hint: Option<&Type>) -> Type {
        let hint = hint.and_then(|h| {
            let h = self.expand(h);
            h.members().into_iter().find_map(|m| match self.expand(&m) {
                Type::Table(t) => Some(t),
                _ => None,
            })
        });
        let keep = |t: Type, hinted: bool| if hinted { t } else { t.widen() };
        let mut props: Vec<(Rc<str>, Type)> = Vec::new();
        let mut keys = Vec::new();
        let mut values = Vec::new();
        for (i, item) in items.iter().enumerate() {
            match item {
                TableItem::Positional(v) => {
                    let h = hint
                        .as_ref()
                        .and_then(|t| t.indexer.as_ref().map(|ix| ix.1.clone()));
                    keys.push(Type::Number);
                    if i + 1 == items.len() && v.kind.is_multi() {
                        let p = self.expr_pack(v);
                        values.extend(p.types.into_iter().map(|t| keep(t, h.is_some())));
                        values.extend(p.variadic);
                    } else {
                        let t = self.expr_hinted(v, h.as_ref());
                        values.push(keep(t, h.is_some()));
                    }
                }
                TableItem::Keyed { key, value } => {
                    if let ExprKind::String(name) = &key.kind {
                        let h = hint.as_ref().and_then(|t| {
                            t.prop(name)
                                .cloned()
                                .or_else(|| t.indexer.as_ref().map(|ix| ix.1.clone()))
                        });
                        let t = self.expr_hinted(value, h.as_ref());
                        let t = keep(t, h.is_some());
                        props.retain(|(n, _)| n != name);
                        props.push((name.clone(), t));
                    } else {
                        let k = self.expr(key);
                        let h = hint
                            .as_ref()
                            .and_then(|t| t.indexer.as_ref().map(|ix| ix.1.clone()));
                        let v = self.expr_hinted(value, h.as_ref());
                        keys.push(k.widen());
                        values.push(keep(v, h.is_some()));
                    }
                }
            }
        }
        let indexer = if keys.is_empty() {
            // `{}` where a `{T}` or `{[K]: V}` is expected is an empty one
            hint.as_ref().and_then(|h| h.indexer.clone())
        } else {
            Some((union(keys), union(values)))
        };
        Type::table(TableType {
            props,
            indexer,
            unsealed: true,
            ..TableType::default()
        })
    }

    /// The type a function declaration has before its body gets checked:
    /// annotations, with `any` for the rest.
    pub fn signature(&mut self, func: &FunctionBody) -> FnType {
        let generics = self.open_generics(&func.generics);
        let params = func
            .param_types
            .iter()
            .map(|t| t.as_ref().map_or(Type::Any, |te| self.resolve_type(te)))
            .collect();
        let variadic = func.is_vararg.then(|| {
            func.vararg_type
                .as_ref()
                .map_or(Type::Any, |te| self.resolve_type(te))
        });
        let rets = func
            .return_type
            .as_ref()
            .map_or_else(Pack::any, |p| self.resolve_pack(p));
        self.generic_scopes.pop();
        FnType {
            generics,
            params,
            variadic,
            rets,
        }
    }

    /// Checks a function body and returns the function's type. `hint` types
    /// unannotated parameters (a callback passed where a function type is
    /// expected), `self_ty` is the object of a `function a:m()`.
    pub fn function(
        &mut self,
        func: &FunctionBody,
        hint: Option<&FnType>,
        self_ty: Option<Type>,
    ) -> FnType {
        let generics = self.open_generics(&func.generics);
        let mut params = Vec::new();
        for (i, p) in func.params.iter().enumerate() {
            let t = match &func.param_types[i] {
                Some(te) => self.resolve_type(te),
                None if i == 0 && func.is_method && self_ty.is_some() => self_ty.clone().unwrap(),
                None => match hint {
                    Some(h) => param_type(h, i),
                    None => Type::Any,
                },
            };
            let _ = p;
            params.push(t);
        }
        let variadic = func.is_vararg.then(|| match &func.vararg_type {
            Some(te) => self.resolve_type(te),
            None => hint.and_then(|h| h.variadic.clone()).unwrap_or(Type::Any),
        });
        let declared = func.return_type.as_ref().map(|p| self.resolve_pack(p));

        self.funcs.push(FuncCtx {
            declared: declared.clone(),
            returns: Vec::new(),
            vararg: variadic.clone().unwrap_or(Type::Any),
        });
        self.with_layer(&[], |c| {
            for (p, t) in func.params.iter().zip(&params) {
                c.declare(p, t.clone());
            }
            c.block(&func.body);
        });
        let ctx = self.funcs.pop().expect("pushed above");
        let falls_off = !always_exits(&func.body, false);

        let rets = match declared {
            Some(d) => {
                let needs_values = (0..d.types.len()).any(|i| !self.sub(&Type::Nil, &d.types[i]));
                if needs_values && falls_off {
                    self.error(
                        func.span,
                        format!("Not all codepaths in this function return '{d}'"),
                    );
                }
                d
            }
            None if !self.strict => Pack::any(),
            None => merge_returns(ctx.returns, falls_off),
        };
        self.generic_scopes.pop();
        let f = FnType {
            generics,
            params,
            variadic,
            rets,
        };
        if self.types.is_some() {
            // for tooling, return-type hints read it here
            self.record(func.span, &Type::function(f.clone()));
        }
        f
    }

    /// Opens a generic scope with fresh variables for `<T, U>`.
    fn open_generics(&mut self, generics: &[GenericParam]) -> Vec<Rc<GenericVar>> {
        let vars: Vec<Rc<GenericVar>> = generics
            .iter()
            .map(|g| GenericVar::new(g.name.clone(), g.pack))
            .collect();
        self.generic_scopes.push(
            vars.iter()
                .map(|v| (v.name.clone(), Type::Generic(v.clone())))
                .collect(),
        );
        vars
    }

    // ─ Conditions ────────────────────────────────────────────────────────────

    /// Types `e` as a condition and returns how it narrows locals when it's
    /// true and when it's false.
    pub fn condition(&mut self, e: &Expr) -> Refine {
        self.cond(e, None).1
    }

    /// A condition's type and narrowings together, so `a and b and c` only
    /// visits each operand once.
    fn cond(&mut self, e: &Expr, hint: Option<&Type>) -> (Type, Refine) {
        stacker::maybe_grow(64 * 1024, 1024 * 1024, || self.cond_inner(e, hint))
    }

    fn cond_inner(&mut self, e: &Expr, hint: Option<&Type>) -> (Type, Refine) {
        if let Some(key) = self.path_of(e) {
            let t = self.expr_hinted(e, hint);
            if t.is_any() {
                return (t, Refine::default());
            }
            let r = Refine {
                truthy: vec![(key.clone(), t.truthy())],
                falsy: vec![(key, t.falsy())],
            };
            return (t, r);
        }
        match &e.kind {
            ExprKind::Group(inner) => self.cond(inner, hint),
            ExprKind::Unary {
                op: UnOp::Not,
                operand,
                ..
            } => {
                let (_, r) = self.cond(operand, None);
                (
                    Type::Boolean,
                    Refine {
                        truthy: r.falsy,
                        falsy: r.truthy,
                    },
                )
            }
            ExprKind::Binary {
                op: BinOp::And,
                lhs,
                rhs,
                ..
            } => {
                let (at, a) = self.cond(lhs, None);
                let (bt, b) = self.with_layer(&a.truthy, |c| c.cond(rhs, hint));
                let t = union([at.falsy(), bt]);
                let r = Refine {
                    truthy: a.truthy.into_iter().chain(b.truthy).collect(),
                    falsy: Vec::new(),
                };
                (t, r)
            }
            ExprKind::Binary {
                op: BinOp::Or,
                lhs,
                rhs,
                ..
            } => {
                let (at, a) = self.cond(lhs, None);
                // `t or {}`, the fallback table is shaped like `t`
                let left = at.truthy();
                let hint = match (&rhs.kind, hint) {
                    (ExprKind::Table(_), None) if !left.is_any() => Some(&left),
                    _ => hint,
                };
                let (bt, b) = self.with_layer(&a.falsy, |c| c.cond(rhs, hint));
                let t = union([left.clone(), bt]);
                let r = Refine {
                    truthy: Vec::new(),
                    falsy: a.falsy.into_iter().chain(b.falsy).collect(),
                };
                (t, r)
            }
            // `n:IsA("Box")`
            ExprKind::MethodCall {
                object,
                method,
                args,
                ..
            } => {
                let t = self.expr_hinted(e, hint);
                let Some(key) = self.path_of(object) else {
                    return (t, Refine::default());
                };
                if !matches!(self.expand(&t), Type::Boolean) {
                    return (t, Refine::default());
                }
                let before = self.errors.len();
                let ot = self.expr(object);
                self.errors.truncate(before);
                let Some(c) = self.class_literal(&ot, method, args, false) else {
                    return (t, Refine::default());
                };
                let r = Refine {
                    truthy: vec![(key.clone(), self.filter_class(&ot, &c, true))],
                    falsy: vec![(key, self.filter_class(&ot, &c, false))],
                };
                (t, r)
            }
            ExprKind::Binary {
                op: op @ (BinOp::Eq | BinOp::Ne),
                lhs,
                rhs,
                ..
            } => {
                self.expr(lhs);
                self.expr(rhs);
                let r = self.equality_refinement(lhs, rhs).unwrap_or_default();
                let r = if *op == BinOp::Eq {
                    r
                } else {
                    Refine {
                        truthy: r.falsy,
                        falsy: r.truthy,
                    }
                };
                (Type::Boolean, r)
            }
            _ => (self.expr_hinted(e, hint), Refine::default()),
        }
    }

    /// Like [`Self::condition`] but without reporting errors (the expression's
    /// already been checked).
    pub fn condition_quiet(&mut self, e: &Expr) -> Refine {
        let before = self.errors.len();
        let r = self.condition(e);
        self.errors.truncate(before);
        r
    }

    /// `x == nil`, `type(x) == "string"`, `typeof(x) == "Player"`, either way
    /// round.
    fn equality_refinement(&mut self, lhs: &Expr, rhs: &Expr) -> Option<Refine> {
        let (subject, other) = match (&lhs.kind, &rhs.kind) {
            (_, ExprKind::Nil | ExprKind::String(_)) => (lhs, rhs),
            (ExprKind::Nil | ExprKind::String(_), _) => (rhs, lhs),
            _ => return None,
        };
        match (&subject.kind, &other.kind) {
            (_, ExprKind::Nil) => {
                let key = self.path_of(subject)?;
                let t = self.expr(subject);
                if t.is_any() {
                    return None;
                }
                Some(Refine {
                    truthy: vec![(key.clone(), Type::Nil)],
                    falsy: vec![(key, t.strip_nil())],
                })
            }
            (ExprKind::Call { callee, args }, ExprKind::String(tag)) => {
                let ExprKind::Name(f) = &callee.kind else {
                    return None;
                };
                let is_typeof = match f.text.as_ref() {
                    "type" => false,
                    "typeof" => true,
                    _ => return None,
                };
                if self.local_id(f).is_some() || args.len() != 1 {
                    return None;
                }
                let key = self.path_of(&args[0])?;
                let t = self.expr(&args[0]);
                Some(Refine {
                    truthy: vec![(key.clone(), self.filter_tag(&t, tag, is_typeof, true))],
                    falsy: vec![(key, self.filter_tag(&t, tag, is_typeof, false))],
                })
            }
            _ => None,
        }
    }

    /// The class a string literal names in `obj:method(...)`, when `obj` is a
    /// class whose `method` takes a class name there (see
    /// [`Env::declare_class_param`](crate::Env::declare_class_param)). A
    /// literal that isn't a class is an error in strict mode (if `report`).
    fn class_literal(
        &mut self,
        ot: &Type,
        method: &str,
        args: &[Expr],
        report: bool,
    ) -> Option<Rc<str>> {
        let Type::Class(class) = self.expand(ot) else {
            return None;
        };
        let i = self.env.class_param(&class, method)?;
        self.literal_class(args.get(i)?, report)
    }

    /// The same for a global function (`f(...)`, `Table.f(...)`) that
    /// takes a class name, see
    /// [`Env::declare_global_class_param`](crate::Env::declare_global_class_param).
    fn global_class_literal(&mut self, callee: &Expr, args: &[Expr]) -> Option<Rc<str>> {
        let host = |c: &Self, n: &rhue_parser::ast::Name| {
            c.local_id(n).is_none() && !c.chunk_globals.contains_key(&n.text)
        };
        let path = match &callee.kind {
            ExprKind::Name(n) if host(self, n) => n.text.to_string(),
            ExprKind::Index { object, key } => match (&object.kind, &key.kind) {
                (ExprKind::Name(o), ExprKind::String(k)) if host(self, o) => {
                    format!("{}.{k}", o.text)
                }
                _ => return None,
            },
            _ => return None,
        };
        let i = self.env.global_class_param(&path)?;
        self.literal_class(args.get(i)?, true)
    }

    /// The class string literal `arg` names. One that isn't a class is an
    /// error in strict mode (if `report`).
    fn literal_class(&mut self, arg: &Expr, report: bool) -> Option<Rc<str>> {
        let ExprKind::String(name) = &arg.kind else {
            return None;
        };
        if self.env.class(name).is_some() {
            return Some(name.clone());
        }
        if report && self.strict {
            self.error(arg.span, format!("'{name}' is not a class"));
        }
        None
    }

    /// `t` with any class `c` is under swapped for `c` (`Node?` → `Box?`).
    fn as_class(&mut self, t: &Type, c: &Rc<str>) -> Type {
        let members: Vec<Type> = self
            .expand(t)
            .members()
            .iter()
            .map(|m| self.expand(m))
            .collect();
        if !members
            .iter()
            .any(|m| matches!(m, Type::Class(x) if self.env.is_subclass(c, x)))
        {
            return t.clone();
        }
        union(
            members
                .into_iter()
                .map(|m| match m {
                    Type::Class(x) if self.env.is_subclass(c, &x) => Type::Class(c.clone()),
                    m => m,
                })
                .collect::<Vec<_>>(),
        )
    }

    /// The part of `t` that is (`keep`) or isn't a `c`: a `Node` that is a
    /// `Box` is a `Box`, one that isn't stays a `Node`.
    fn filter_class(&mut self, t: &Type, c: &Rc<str>, keep: bool) -> Type {
        let e = self.expand(t);
        if matches!(e, Type::Any | Type::Error | Type::Unknown) {
            return if keep { Type::Class(c.clone()) } else { e };
        }
        let members: Vec<Type> = e.members().iter().map(|m| self.expand(m)).collect();
        let env = self.env;
        union(
            members
                .into_iter()
                .filter_map(|m| match &m {
                    // already a `c`
                    Type::Class(x) if env.is_subclass(x, c) => keep.then_some(m),
                    // might be one
                    Type::Class(x) if env.is_subclass(c, x) => {
                        Some(if keep { Type::Class(c.clone()) } else { m })
                    }
                    _ => (!keep).then_some(m),
                })
                .collect::<Vec<_>>(),
        )
    }

    /// The part of `t` whose `type()` (or `typeof()`) is (`keep`) or isn't
    /// `tag`.
    fn filter_tag(&mut self, t: &Type, tag: &str, is_typeof: bool, keep: bool) -> Type {
        let e = self.expand(t);
        if matches!(e, Type::Any | Type::Error | Type::Unknown) {
            if !keep {
                return e;
            }
            return match tag {
                "nil" => Type::Nil,
                "boolean" => Type::Boolean,
                "number" => Type::Number,
                "string" => Type::String,
                "table" => Type::table(TableType {
                    props: Vec::new(),
                    indexer: Some((Type::Any, Type::Any)),
                    unsealed: false,
                    ..TableType::default()
                }),
                "function" => any_function(),
                "thread" | "buffer" | "vector" if self.env.class(tag).is_some() => {
                    Type::Class(Rc::from(tag))
                }
                _ if is_typeof && self.env.class(tag).is_some() => Type::Class(Rc::from(tag)),
                _ => e,
            };
        }
        let members: Vec<Type> = e.members().iter().map(|m| self.expand(m)).collect();
        union(
            members
                .into_iter()
                .filter(|m| match tag_of(m, is_typeof) {
                    Some(mt) => (mt == tag) == keep,
                    // generics and stuff like that could be anything, keep both
                    // ways
                    None => true,
                })
                .collect::<Vec<_>>(),
        )
    }

    // ─ Iteration ─────────────────────────────────────────────────────────────

    /// The types of the variables of `for a, b in exprs`.
    pub fn iteration_types(&mut self, exprs: &[Expr], n: usize, span: Span) -> Vec<Type> {
        // `pairs(t)` / `ipairs(t)`, straight from the table's type
        if let [
            Expr {
                kind: ExprKind::Call { callee, args },
                ..
            },
        ] = exprs
            && let (ExprKind::Name(f), [arg]) = (&callee.kind, args.as_slice())
        {
            let builtin = self.local_id(f).is_none() && !self.chunk_globals.contains_key(&f.text);
            if builtin && matches!(f.text.as_ref(), "pairs" | "ipairs") {
                let t = self.expr(arg);
                let (k, v) = self.table_kv(&t, arg.span, f.text.as_ref() == "ipairs");
                return pad(vec![k, v], n);
            }
        }
        let pack = self.exprs_pack(exprs, &[]);
        let first = pack.first();
        match self.expand(&first) {
            t if t.is_any() => vec![Type::Any; n],
            Type::Table(_) => {
                let (k, v) = self.table_kv(&first, span, false);
                pad(vec![k, v], n)
            }
            // `a or b` where both are tables, either one's keys and values
            u @ Type::Union(_) if self.all_tables(&u) => {
                let (k, v) = self.table_kv(&u, span, false);
                pad(vec![k, v], n)
            }
            Type::Function(f) => {
                let mut subst = Subst::new(&f.generics);
                for (j, t) in [pack.get(1), pack.get(2)].iter().enumerate() {
                    let p = param_type(&f, j);
                    self.unify(&p, t, &mut subst);
                }
                let rets = subst.apply_pack(&f.rets);
                let mut vars: Vec<Type> = (0..n).map(|i| rets.get(i)).collect();
                // the loop stops when the first value is nil
                if let Some(v) = vars.first_mut() {
                    *v = v.strip_nil();
                }
                vars
            }
            u @ Type::Union(_) if u.can_be_nil() => {
                if self.strict {
                    self.error(span, format!("Value of type '{u}' could be nil"));
                }
                vec![Type::Error; n]
            }
            other => {
                self.error(
                    span,
                    format!("Cannot iterate over a value of type '{other}'"),
                );
                vec![Type::Error; n]
            }
        }
    }

    /// Whether every member of `t` is a table.
    fn all_tables(&mut self, t: &Type) -> bool {
        t.members()
            .iter()
            .all(|m| matches!(self.expand(m), Type::Table(_)))
    }

    /// Key and value types when iterating a table.
    fn table_kv(&mut self, t: &Type, span: Span, ipairs: bool) -> (Type, Type) {
        match self.expand(t) {
            t if t.is_any() => (Type::Any, Type::Any),
            Type::Table(tt) => {
                if tt.is_empty_literal() {
                    return (if ipairs { Type::Number } else { Type::Any }, Type::Any);
                }
                if ipairs {
                    let v = match &tt.indexer {
                        Some((k, v)) if self.sub(&Type::Number, k) => v.clone(),
                        _ => Type::Never,
                    };
                    return (Type::Number, v);
                }
                let mut keys = Vec::new();
                let mut values = Vec::new();
                if let Some((k, v)) = &tt.indexer {
                    keys.push(k.clone());
                    values.push(v.clone());
                }
                if !tt.props.is_empty() {
                    keys.push(Type::String);
                    values.extend(tt.props.iter().map(|(_, t)| t.clone()));
                }
                (union(keys), union(values))
            }
            u @ Type::Union(_) if u.can_be_nil() => {
                if self.strict {
                    self.error(span, format!("Value of type '{u}' could be nil"));
                }
                self.table_kv(&u.strip_nil(), span, ipairs)
            }
            u @ Type::Union(_) if self.all_tables(&u) => {
                let (keys, values): (Vec<Type>, Vec<Type>) = u
                    .members()
                    .iter()
                    .map(|m| self.table_kv(m, span, ipairs))
                    .unzip();
                (union(keys), union(values))
            }
            other => {
                self.error(
                    span,
                    format!("Cannot iterate over a value of type '{other}'"),
                );
                (Type::Error, Type::Error)
            }
        }
    }
}

/// Argument types (with spans) and the variadic tail of a final call.
pub(crate) struct ArgTypes(Vec<(Type, Span)>, Option<Type>);

/// The type of parameter `i`. `any` past the end, since extra arguments are
/// only ignored when the function's variadic and otherwise the caller reports
/// the count.
pub(crate) fn param_type(f: &FnType, i: usize) -> Type {
    f.params
        .get(i)
        .cloned()
        .or_else(|| f.variadic.clone())
        .unwrap_or(Type::Any)
}

fn pad(mut v: Vec<Type>, n: usize) -> Vec<Type> {
    v.resize(n.max(v.len()), Type::Nil);
    v.truncate(n);
    v
}

/// The result types of a function without annotations. Each position is the
/// union of what its `return`s give (plus `nil` if it can fall off the end).
fn merge_returns(returns: Vec<Pack>, falls_off: bool) -> Pack {
    if returns.is_empty() {
        return Pack::empty();
    }
    let n = returns.iter().map(|p| p.types.len()).max().unwrap_or(0);
    let mut types: Vec<Type> = (0..n)
        .map(|i| union(returns.iter().map(|p| p.get(i)).collect::<Vec<_>>()))
        .collect();
    if falls_off {
        for t in &mut types {
            *t = union([t.clone(), Type::Nil]);
        }
    }
    let variadic = returns
        .iter()
        .filter_map(|p| p.variadic.clone())
        .reduce(|a, b| union([a, b]));
    Pack { types, variadic }
}

fn tag_of(t: &Type, is_typeof: bool) -> Option<String> {
    Some(
        match t {
            Type::Nil => "nil",
            Type::Boolean | Type::BoolLit(_) => "boolean",
            Type::Number => "number",
            Type::String | Type::StrLit(_) => "string",
            Type::Table(_) => "table",
            Type::Function(_) | Type::Intersection(_) => "function",
            Type::Class(c) if is_typeof || matches!(&**c, "thread" | "buffer" | "vector") => {
                return Some(c.to_string());
            }
            Type::Class(_) => "userdata",
            _ => return None,
        }
        .to_string(),
    )
}

fn callee_name(callee: &Expr) -> Option<String> {
    match &callee.kind {
        ExprKind::Name(n) => Some(n.text.to_string()),
        ExprKind::Index { object, key } => match (&object.kind, &key.kind) {
            (ExprKind::Name(o), ExprKind::String(k)) => Some(format!("{}.{k}", o.text)),
            (_, ExprKind::String(k)) => Some(k.to_string()),
            _ => None,
        },
        _ => None,
    }
}
