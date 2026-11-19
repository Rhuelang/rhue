//! The resolver binds every name to a local or a global before codegen.
//!
//! It walks the AST once with a scope stack and builds a [`Resolution`], a side
//! table indexed by [`NameId`]. Keeping it separate from the compiler means
//! codegen never does scope lookups, and the compiler knows up front which
//! locals get *captured* by an inner function (only those need upvalue
//! closing).
//!
//! It also checks the rules that need scope info: `break` and `continue`
//! outside a loop (loops don't cross function boundaries), `...` outside a
//! vararg function, and Luau's rule that an `until` condition can't read a
//! local declared after a `continue`.

use std::rc::Rc;

use rhue_common::{Diagnostic, Span};
use rhue_parser::ast::*;

pub type LocalId = u32;

/// What one name occurrence points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Binding {
    /// Declares or uses the local with this id. It might belong to an enclosing
    /// function, then the compiler reaches it through an upvalue.
    Local(LocalId),
    /// A global, looked up by name at runtime.
    Global,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LocalInfo {
    pub name: Rc<str>,
    pub span: Span,
    /// Which function declared it (0 is the main chunk, then functions are
    /// numbered in source order).
    pub function: u32,
    /// Set when an inner function refers to this local. Only captured locals
    /// need a `CLOSE` when their scope ends.
    pub captured: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Resolution {
    /// Indexed by `NameId`.
    pub names: Vec<Binding>,
    /// Indexed by `LocalId`, in declaration order.
    pub locals: Vec<LocalInfo>,
}

impl Resolution {
    pub fn binding(&self, name: &Name) -> Binding {
        self.names[name.id as usize]
    }

    /// Whether `name` refers to a captured local.
    pub fn is_captured(&self, id: LocalId) -> bool {
        self.locals[id as usize].captured
    }
}

pub fn resolve(chunk: &Chunk) -> Result<Resolution, Vec<Diagnostic>> {
    let mut r = Resolver {
        out: Resolution {
            names: vec![Binding::Global; chunk.name_count as usize],
            locals: Vec::new(),
        },
        scopes: vec![Vec::new()],
        functions: vec![FunctionCtx {
            id: 0,
            is_vararg: true,
            loops: Vec::new(),
        }],
        next_function: 1,
        errors: Vec::new(),
        in_type: 0,
    };
    r.block_body(&chunk.body);
    if r.errors.is_empty() {
        Ok(r.out)
    } else {
        Err(r.errors)
    }
}

struct LoopCtx {
    is_repeat: bool,
    /// First `continue` in this loop: its span and how many locals existed at
    /// that point. Locals with a higher id might get skipped by it.
    first_continue: Option<(Span, LocalId)>,
}

struct FunctionCtx {
    id: u32,
    is_vararg: bool,
    loops: Vec<LoopCtx>,
}

struct Resolver {
    out: Resolution,
    scopes: Vec<Vec<(Rc<str>, LocalId)>>,
    functions: Vec<FunctionCtx>,
    next_function: u32,
    errors: Vec<Diagnostic>,
    /// Inside `typeof(...)` in a type. Names get bound but never evaluated, so
    /// they don't make a local captured.
    in_type: u32,
}

impl Resolver {
    fn func(&mut self) -> &mut FunctionCtx {
        self.functions
            .last_mut()
            .expect("the main chunk is always on the stack")
    }

    fn declare(&mut self, name: &Name) {
        let id = self.out.locals.len() as LocalId;
        let function = self.func().id;
        self.out.locals.push(LocalInfo {
            name: name.text.clone(),
            span: name.span,
            function,
            captured: false,
        });
        self.out.names[name.id as usize] = Binding::Local(id);
        self.scopes
            .last_mut()
            .expect("scope stack is never empty")
            .push((name.text.clone(), id));
    }

    fn lookup(&self, text: &str) -> Option<LocalId> {
        self.scopes
            .iter()
            .rev()
            .flat_map(|s| s.iter().rev())
            .find(|(n, _)| &**n == text)
            .map(|&(_, id)| id)
    }

    fn use_name(&mut self, name: &Name) {
        if let Some(id) = self.lookup(&name.text) {
            self.out.names[name.id as usize] = Binding::Local(id);
            let current = self.func().id;
            let in_type = self.in_type > 0;
            let local = &mut self.out.locals[id as usize];
            if local.function != current && !in_type {
                local.captured = true;
            }
        }
    }

    fn scoped(&mut self, f: impl FnOnce(&mut Self)) {
        self.scopes.push(Vec::new());
        f(self);
        self.scopes.pop();
    }

    fn block(&mut self, block: &Block) {
        self.scoped(|r| r.block_body(block));
    }

    /// A block's statements, in the current scope.
    fn block_body(&mut self, block: &Block) {
        for stmt in &block.stmts {
            self.stmt(stmt);
        }
        if let Some(ret) = &block.ret {
            for v in &ret.values {
                self.expr(v);
            }
        }
    }

    fn function(&mut self, func: &FunctionBody) {
        let id = self.next_function;
        self.next_function += 1;
        self.functions.push(FunctionCtx {
            id,
            is_vararg: func.is_vararg,
            loops: Vec::new(),
        });
        self.scoped(|r| {
            for p in &func.params {
                r.declare(p);
            }
            // annotations can see the parameters (`typeof(a)` in a later one)
            for t in func.param_types.iter().flatten().chain(&func.vararg_type) {
                r.type_expr(t);
            }
            if let Some(ret) = &func.return_type {
                r.type_pack(ret);
            }
            r.block_body(&func.body);
        });
        self.functions.pop();
    }

    /// Binds the names inside `typeof(...)`. Everything else in a type is a
    /// type name, the type checker handles those.
    fn type_expr(&mut self, t: &TypeExpr) {
        match &t.kind {
            TypeKind::Name { args, .. } => args.iter().for_each(|a| self.type_expr(a)),
            TypeKind::Singleton(_) | TypeKind::GenericPack(_) => {}
            TypeKind::Pack(p) => self.type_pack(p),
            TypeKind::Typeof(e) => {
                self.in_type += 1;
                self.expr(e);
                self.in_type -= 1;
            }
            TypeKind::Optional(t) => self.type_expr(t),
            TypeKind::Union(ts) | TypeKind::Intersection(ts) => {
                ts.iter().for_each(|t| self.type_expr(t))
            }
            TypeKind::Table { props, indexer } => {
                for p in props {
                    self.type_expr(&p.ty);
                }
                if let Some(ix) = indexer {
                    self.type_expr(&ix.0);
                    self.type_expr(&ix.1);
                }
            }
            TypeKind::Function {
                params,
                variadic,
                returns,
                ..
            } => {
                for p in params {
                    self.type_expr(&p.ty);
                }
                if let Some(v) = variadic {
                    self.type_expr(v);
                }
                self.type_pack(returns);
            }
        }
    }

    fn type_pack(&mut self, p: &TypePack) {
        for t in &p.types {
            self.type_expr(t);
        }
        if let Some(v) = &p.variadic {
            self.type_expr(v);
        }
    }

    fn stmt(&mut self, stmt: &Stmt) {
        stacker::maybe_grow(64 * 1024, 1024 * 1024, || self.stmt_inner(stmt))
    }

    fn stmt_inner(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Local {
                names,
                types,
                values,
            } => {
                // `local x = x` reads the outer `x`, so resolve values first
                for v in values {
                    self.expr(v);
                }
                for t in types.iter().flatten() {
                    self.type_expr(t);
                }
                for n in names {
                    self.declare(n);
                }
            }
            StmtKind::Assign { targets, values } => {
                for v in values {
                    self.expr(v);
                }
                for t in targets {
                    self.expr(t);
                }
            }
            StmtKind::CompoundAssign { target, value, .. } => {
                self.expr(value);
                self.expr(target);
            }
            StmtKind::Call(call) => self.expr(call),
            StmtKind::Function { target, func } => {
                self.expr(target);
                self.function(func);
            }
            StmtKind::LocalFunction { name, func } => {
                // declared first so the body can call itself
                self.declare(name);
                self.function(func);
            }
            StmtKind::Do(body) => self.block(body),
            StmtKind::While { cond, body } => {
                self.expr(cond);
                self.in_loop(false, |r| r.block(body));
            }
            StmtKind::Repeat { body, cond } => {
                self.scoped(|r| {
                    let first_local = r.out.locals.len() as LocalId;
                    let ctx = r.in_loop(true, |r| r.block_body(body));
                    // the condition can see the body's locals
                    r.expr(cond);
                    if let Some((continue_span, locals_at_continue)) = ctx.first_continue {
                        r.check_until(cond, first_local.max(locals_at_continue), continue_span);
                    }
                });
            }
            StmtKind::If {
                branches,
                else_block,
            } => {
                for (cond, body) in branches {
                    self.expr(cond);
                    self.block(body);
                }
                if let Some(b) = else_block {
                    self.block(b);
                }
            }
            StmtKind::GenericFor {
                names,
                types,
                exprs,
                body,
            } => {
                for e in exprs {
                    self.expr(e);
                }
                for t in types.iter().flatten() {
                    self.type_expr(t);
                }
                self.scoped(|r| {
                    for n in names {
                        r.declare(n);
                    }
                    r.in_loop(false, |r| r.block(body));
                });
            }
            StmtKind::NumericFor {
                var,
                var_type,
                start,
                limit,
                step,
                body,
            } => {
                if let Some(t) = var_type {
                    self.type_expr(t);
                }
                self.expr(start);
                self.expr(limit);
                if let Some(s) = step {
                    self.expr(s);
                }
                self.scoped(|r| {
                    r.declare(var);
                    r.in_loop(false, |r| r.block(body));
                });
            }
            StmtKind::TypeAlias(alias) => {
                for g in &alias.generics {
                    if let Some(d) = &g.default {
                        self.type_expr(d);
                    }
                }
                self.type_expr(&alias.ty)
            }
            StmtKind::Break => {
                if self.func().loops.is_empty() {
                    self.errors.push(
                        Diagnostic::error("`break` outside a loop", stmt.span)
                            .with_label("there is no loop to break out of"),
                    );
                }
            }
            StmtKind::Continue => {
                let locals_now = self.out.locals.len() as LocalId;
                match self.func().loops.last_mut() {
                    None => self.errors.push(
                        Diagnostic::error("`continue` outside a loop", stmt.span)
                            .with_label("there is no loop to continue"),
                    ),
                    Some(ctx) => {
                        if ctx.is_repeat && ctx.first_continue.is_none() {
                            ctx.first_continue = Some((stmt.span, locals_now));
                        }
                    }
                }
            }
        }
    }

    fn in_loop(&mut self, is_repeat: bool, f: impl FnOnce(&mut Self)) -> LoopCtx {
        self.func().loops.push(LoopCtx {
            is_repeat,
            first_continue: None,
        });
        f(self);
        self.func().loops.pop().expect("pushed above")
    }

    /// Luau rejects `repeat ... continue ... local x ... until x`, jumping to
    /// the condition would skip `x`'s initialisation.
    fn check_until(&mut self, cond: &Expr, first_unsafe: LocalId, continue_span: Span) {
        let mut bad = None;
        visit_names(cond, &mut |name| {
            if bad.is_none()
                && let Binding::Local(id) = self.out.binding(name)
                && id >= first_unsafe
            {
                bad = Some((name.clone(), id));
            }
        });
        if let Some((name, id)) = bad {
            let decl = self.out.locals[id as usize].span;
            self.errors.push(
                Diagnostic::error(
                    format!("local `{}` is used in the `until` condition but may be skipped by `continue`", name.text),
                    name.span,
                )
                .with_label("used here")
                .with_secondary(continue_span, "`continue` jumps straight to the condition")
                .with_secondary(decl, "declared after the `continue`"),
            );
        }
    }

    fn expr(&mut self, expr: &Expr) {
        // the parser bounds AST depth, but deep trees still need a big stack in
        // debug builds
        stacker::maybe_grow(64 * 1024, 1024 * 1024, || match &expr.kind {
            ExprKind::Nil | ExprKind::Bool(_) | ExprKind::Number(_) | ExprKind::String(_) => {}
            ExprKind::Name(n) => self.use_name(n),
            ExprKind::Interp(parts) => {
                for part in parts {
                    if let InterpPart::Expr(e) = part {
                        self.expr(e);
                    }
                }
            }
            ExprKind::Group(e) => self.expr(e),
            ExprKind::Unary { operand, .. } => self.expr(operand),
            ExprKind::Binary { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            ExprKind::IfElse {
                branches,
                else_expr,
            } => {
                for (c, v) in branches {
                    self.expr(c);
                    self.expr(v);
                }
                self.expr(else_expr);
            }
            ExprKind::Call { callee, args } => {
                self.expr(callee);
                for a in args {
                    self.expr(a);
                }
            }
            ExprKind::Vararg => {
                if !self.func().is_vararg {
                    self.errors.push(
                        Diagnostic::error("`...` used outside a vararg function", expr.span)
                            .with_label("this function doesn't take `...`")
                            .with_help("add `...` to the end of the parameter list"),
                    );
                }
            }
            ExprKind::Function(func) => self.function(func),
            ExprKind::Cast { expr, ty } => {
                self.expr(expr);
                self.type_expr(ty);
            }
            ExprKind::Index { object, key } => {
                self.expr(object);
                self.expr(key);
            }
            ExprKind::MethodCall { object, args, .. } => {
                self.expr(object);
                for a in args {
                    self.expr(a);
                }
            }
            ExprKind::Table(items) => {
                for item in items {
                    match item {
                        TableItem::Positional(v) => self.expr(v),
                        TableItem::Keyed { key, value } => {
                            self.expr(key);
                            self.expr(value);
                        }
                    }
                }
            }
        })
    }
}

/// Visits the names an expression reads directly (not inside nested function
/// bodies, those run later).
fn visit_names(expr: &Expr, f: &mut impl FnMut(&Name)) {
    match &expr.kind {
        ExprKind::Name(n) => f(n),
        ExprKind::Interp(parts) => {
            for part in parts {
                if let InterpPart::Expr(e) = part {
                    visit_names(e, f);
                }
            }
        }
        ExprKind::Group(e)
        | ExprKind::Unary { operand: e, .. }
        | ExprKind::Cast { expr: e, .. } => visit_names(e, f),
        ExprKind::Binary { lhs, rhs, .. } => {
            visit_names(lhs, f);
            visit_names(rhs, f);
        }
        ExprKind::IfElse {
            branches,
            else_expr,
        } => {
            for (c, v) in branches {
                visit_names(c, f);
                visit_names(v, f);
            }
            visit_names(else_expr, f);
        }
        ExprKind::Call { callee, args } => {
            visit_names(callee, f);
            for a in args {
                visit_names(a, f);
            }
        }
        ExprKind::Index { object, key } => {
            visit_names(object, f);
            visit_names(key, f);
        }
        ExprKind::MethodCall { object, args, .. } => {
            visit_names(object, f);
            for a in args {
                visit_names(a, f);
            }
        }
        ExprKind::Table(items) => {
            for item in items {
                match item {
                    TableItem::Positional(v) => visit_names(v, f),
                    TableItem::Keyed { key, value } => {
                        visit_names(key, f);
                        visit_names(value, f);
                    }
                }
            }
        }
        ExprKind::Nil
        | ExprKind::Bool(_)
        | ExprKind::Number(_)
        | ExprKind::String(_)
        | ExprKind::Vararg
        | ExprKind::Function(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rhue_parser::parse_chunk;

    fn resolved(src: &str) -> (Chunk, Resolution) {
        let chunk = parse_chunk(src).unwrap();
        let res = resolve(&chunk).unwrap_or_else(|e| panic!("{}", e[0].message));
        (chunk, res)
    }

    fn errors(src: &str) -> Vec<String> {
        resolve(&parse_chunk(src).unwrap())
            .unwrap_err()
            .into_iter()
            .map(|d| d.message)
            .collect()
    }

    /// Bindings of every *use* in `return a, b, ...`.
    fn returned(src: &str) -> Vec<Binding> {
        let (chunk, res) = resolved(src);
        chunk
            .body
            .ret
            .unwrap()
            .values
            .iter()
            .map(|e| match &e.kind {
                ExprKind::Name(n) => res.binding(n),
                _ => panic!("return names only"),
            })
            .collect()
    }

    #[test]
    fn locals_and_globals() {
        assert_eq!(
            returned("local a = 1 return a, b"),
            [Binding::Local(0), Binding::Global]
        );
    }

    #[test]
    fn shadowing_picks_the_innermost() {
        assert_eq!(returned("local x local x return x"), [Binding::Local(1)]);
        assert_eq!(
            returned("local x do local x end return x"),
            [Binding::Local(0)]
        );
    }

    #[test]
    fn local_initialiser_sees_the_outer_name() {
        let (chunk, res) = resolved("local x = 1 local x = x");
        let StmtKind::Local { values, .. } = &chunk.body.stmts[1].kind else {
            panic!()
        };
        let ExprKind::Name(n) = &values[0].kind else {
            panic!()
        };
        assert_eq!(res.binding(n), Binding::Local(0));
    }

    #[test]
    fn scopes_end() {
        assert_eq!(returned("do local y end return y"), [Binding::Global]);
        assert_eq!(returned("for i = 1, 2 do end return i"), [Binding::Global]);
        assert_eq!(
            returned("while true do local w break end return w"),
            [Binding::Global]
        );
    }

    #[test]
    fn repeat_condition_sees_body_locals() {
        let (chunk, res) = resolved("repeat local done = true until done");
        let StmtKind::Repeat { cond, .. } = &chunk.body.stmts[0].kind else {
            panic!()
        };
        let ExprKind::Name(n) = &cond.kind else {
            panic!()
        };
        assert_eq!(res.binding(n), Binding::Local(0));
    }

    #[test]
    fn break_and_continue_need_a_loop() {
        assert_eq!(errors("break"), ["`break` outside a loop"]);
        assert_eq!(
            errors("if x then continue end"),
            ["`continue` outside a loop"]
        );
        assert!(resolve(&parse_chunk("while x do if y then break end end").unwrap()).is_ok());
    }

    #[test]
    fn captures_are_marked() {
        let (_, res) = resolved("local a, b = 1, 2 local function f() return a end return b");
        assert!(res.locals[0].captured, "a is used inside f");
        assert!(
            !res.locals[1].captured,
            "b is only used in its own function"
        );
        assert!(
            !res.locals[2].captured,
            "f itself is only used by the main chunk"
        );
        assert_eq!(res.locals[0].function, 0);
    }

    #[test]
    fn local_function_can_call_itself() {
        let (chunk, res) = resolved("local function f() return f end");
        let StmtKind::LocalFunction { func, .. } = &chunk.body.stmts[0].kind else {
            panic!()
        };
        let ExprKind::Name(n) = &func.body.ret.as_ref().unwrap().values[0].kind else {
            panic!()
        };
        assert_eq!(res.binding(n), Binding::Local(0));
        assert!(res.locals[0].captured);
    }

    #[test]
    fn params_are_locals_of_the_function() {
        let (_, res) = resolved("local x function g(x) return x end");
        assert_eq!(res.locals.len(), 2);
        assert_eq!(res.locals[1].function, 1);
        assert!(!res.locals[0].captured, "the parameter shadows the outer x");
    }

    #[test]
    fn loops_do_not_cross_functions() {
        assert_eq!(
            errors("while true do local f = function() break end end"),
            ["`break` outside a loop"]
        );
    }

    #[test]
    fn vararg_rules() {
        assert!(
            resolve(&parse_chunk("return ...").unwrap()).is_ok(),
            "the main chunk is vararg"
        );
        assert!(resolve(&parse_chunk("local f = function(...) return ... end").unwrap()).is_ok());
        assert_eq!(
            errors("local f = function(a) return ... end"),
            ["`...` used outside a vararg function"]
        );
    }

    #[test]
    fn until_cannot_read_locals_skipped_by_continue() {
        assert_eq!(
            errors("repeat if a then continue end local x = 1 until x"),
            ["local `x` is used in the `until` condition but may be skipped by `continue`"]
        );
        // declared before the `continue`, fine
        assert!(
            resolve(&parse_chunk("repeat local x = 1 if a then continue end until x").unwrap())
                .is_ok()
        );
        // the `continue` belongs to an inner loop, fine
        assert!(
            resolve(&parse_chunk("repeat while a do continue end local x = 1 until x").unwrap())
                .is_ok()
        );
    }
}
