//! Inlay hints: the types the checker inferred where the source doesn't
//! say, for editors to show inline (and offer to write in).

use std::collections::HashMap;

use rhue_parser::ast::{Block, Expr, ExprKind, FunctionBody, InterpPart, StmtKind, TableItem};
use rhue_types::Type;

use crate::Span;

/// A type shown after a name or a parameter list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlayHint {
    /// Where it goes: just after the name, or after a function's `)`.
    pub offset: u32,
    /// What's shown: `: number`.
    pub label: String,
    /// Whether it's a variable's type or a function's results.
    pub kind: HintKind,
    /// The annotation to insert at `offset` to make it explicit, when the
    /// type can be written as Luau syntax.
    pub insert: Option<String>,
}

/// What an [`InlayHint`] describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HintKind {
    /// A local's, loop variable's or parameter's type.
    Type,
    /// A function's results.
    Return,
}

/// Longest label shown; longer ones are cut (the annotation isn't).
const MAX_LABEL: usize = 60;

pub(crate) fn hints(body: &Block, source: &str, types: &[(Span, Type)]) -> Vec<InlayHint> {
    // The last type recorded for a span is the final one (flow locals are
    // updated in place; functions are recorded each time they're checked).
    let types: HashMap<Span, &Type> = types.iter().map(|(s, t)| (*s, t)).collect();
    let mut h = Hints {
        source,
        types,
        out: Vec::new(),
    };
    h.block(body);
    h.out.sort_by_key(|x| x.offset);
    h.out.dedup_by_key(|x| x.offset);
    h.out
}

struct Hints<'a> {
    source: &'a str,
    types: HashMap<Span, &'a Type>,
    out: Vec<InlayHint>,
}

/// Whether a type says nothing worth showing.
fn uninformative(t: &Type) -> bool {
    matches!(t, Type::Any | Type::Error | Type::Unknown)
}

impl Hints<'_> {
    fn push(&mut self, offset: u32, shown: String, kind: HintKind) {
        // Results are a pack, which parses as a function type's results.
        let as_type = match kind {
            HintKind::Type => shown.clone(),
            HintKind::Return => format!("() -> {shown}"),
        };
        let insert = rhue_parser::parse_type(&as_type)
            .is_ok()
            .then(|| format!(": {shown}"));
        let mut label = format!(": {shown}");
        if label.chars().count() > MAX_LABEL {
            label = label.chars().take(MAX_LABEL - 1).collect::<String>() + "…";
        }
        self.out.push(InlayHint {
            offset,
            label,
            kind,
            insert,
        });
    }

    /// A hint for the name declared at `span`, if its type is known.
    fn name(&mut self, span: Span) {
        if &self.source[span.start as usize..span.end as usize] == "_" {
            return;
        }
        if let Some(t) = self.types.get(&span).copied()
            && !uninformative(t)
        {
            self.push(span.end, t.to_string(), HintKind::Type);
        }
    }

    fn block(&mut self, b: &Block) {
        for s in &b.stmts {
            match &s.kind {
                StmtKind::Local {
                    names,
                    types,
                    values,
                } => {
                    for v in values {
                        self.expr(v);
                    }
                    for (i, n) in names.iter().enumerate() {
                        let obvious = matches!(
                            values.get(i).map(|v| &v.kind),
                            Some(
                                ExprKind::Function(_)
                                    | ExprKind::Number(_)
                                    | ExprKind::String(_)
                                    | ExprKind::Interp(_)
                                    | ExprKind::Bool(_)
                                    | ExprKind::Cast { .. }
                            )
                        );
                        if types[i].is_none() && !obvious {
                            self.name(n.span);
                        }
                    }
                }
                StmtKind::Assign { targets, values } => {
                    for e in targets.iter().chain(values) {
                        self.expr(e);
                    }
                }
                StmtKind::CompoundAssign { target, value, .. } => {
                    self.expr(target);
                    self.expr(value);
                }
                StmtKind::Do(b) => self.block(b),
                StmtKind::While { cond, body } | StmtKind::Repeat { body, cond } => {
                    self.expr(cond);
                    self.block(body);
                }
                StmtKind::If {
                    branches,
                    else_block,
                } => {
                    for (c, b) in branches {
                        self.expr(c);
                        self.block(b);
                    }
                    if let Some(b) = else_block {
                        self.block(b);
                    }
                }
                StmtKind::NumericFor {
                    start,
                    limit,
                    step,
                    body,
                    ..
                } => {
                    self.expr(start);
                    self.expr(limit);
                    if let Some(e) = step {
                        self.expr(e);
                    }
                    self.block(body);
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
                    for (n, t) in names.iter().zip(types) {
                        if t.is_none() {
                            self.name(n.span);
                        }
                    }
                    self.block(body);
                }
                StmtKind::Call(e) => self.expr(e),
                StmtKind::Function { func, .. } | StmtKind::LocalFunction { func, .. } => {
                    self.function(func);
                }
                StmtKind::Break | StmtKind::Continue | StmtKind::TypeAlias(_) => {}
            }
        }
        if let Some(r) = &b.ret {
            for v in &r.values {
                self.expr(v);
            }
        }
    }

    fn function(&mut self, f: &FunctionBody) {
        for (i, (p, t)) in f.params.iter().zip(&f.param_types).enumerate() {
            if t.is_none() && !(f.is_method && i == 0) {
                self.name(p.span);
            }
        }
        if f.return_type.is_none()
            && let Some(Type::Function(ft)) = self.types.get(&f.span).copied()
            && !(ft.rets.types.is_empty() && ft.rets.variadic.is_none())
            && !(ft.rets.types.is_empty() && ft.rets.variadic.as_ref().is_some_and(uninformative))
            && !ft.rets.types.iter().all(uninformative)
            && let Some(close) = self.params_end(f.span)
        {
            self.push(close, ft.rets.to_string(), HintKind::Return);
        }
        self.block(&f.body);
    }

    /// Just after the `)` closing a function's parameters.
    fn params_end(&self, span: Span) -> Option<u32> {
        let text = &self.source[span.start as usize..span.end as usize];
        let mut depth = 0u32;
        for t in rhue_lexer::lex(text).tokens {
            match t.token {
                rhue_lexer::Token::LParen => depth += 1,
                rhue_lexer::Token::RParen => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return Some(span.start + t.span.end);
                    }
                }
                _ => {}
            }
        }
        None
    }

    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Function(f) => self.function(f),
            ExprKind::Interp(parts) => {
                for p in parts {
                    if let InterpPart::Expr(x) = p {
                        self.expr(x);
                    }
                }
            }
            ExprKind::Group(x)
            | ExprKind::Unary { operand: x, .. }
            | ExprKind::Cast { expr: x, .. } => self.expr(x),
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
            ExprKind::MethodCall { object, args, .. } => {
                self.expr(object);
                for a in args {
                    self.expr(a);
                }
            }
            ExprKind::Index { object, key } => {
                self.expr(object);
                self.expr(key);
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
            ExprKind::Nil
            | ExprKind::Bool(_)
            | ExprKind::Number(_)
            | ExprKind::String(_)
            | ExprKind::Vararg
            | ExprKind::Name(_) => {}
        }
    }
}
