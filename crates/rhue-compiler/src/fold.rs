use rhue_bytecode::{ArithOp, arith};
use rhue_parser::ast::{BinOp, Expr, ExprKind, InterpPart, TableItem, UnOp};

/// Constant folding as one bottom-up pass: returns a copy of `expr` where every
/// subtree made only of number literals and arithmetic is replaced by a single
/// `Number` node carrying the subtree's span.
///
/// Anything that could fail at runtime (`1 + true`) is left alone, so the
/// error still surfaces, with its span, when the code runs. Folding uses the
/// same [`arith`] function as the VM, so results are bit-identical.
pub fn fold(expr: &Expr) -> Expr {
    stacker::maybe_grow(64 * 1024, 1024 * 1024, || fold_inner(expr))
}

fn fold_inner(expr: &Expr) -> Expr {
    let number = |n: f64| Expr {
        kind: ExprKind::Number(n),
        span: expr.span,
    };
    let same = |kind| Expr {
        kind,
        span: expr.span,
    };
    match &expr.kind {
        ExprKind::Nil
        | ExprKind::Bool(_)
        | ExprKind::Number(_)
        | ExprKind::String(_)
        | ExprKind::Name(_)
        | ExprKind::Vararg
        // Function bodies are folded when they are compiled, statement by statement.
        | ExprKind::Function(_) => expr.clone(),
        ExprKind::Interp(parts) => same(ExprKind::Interp(
            parts
                .iter()
                .map(|p| match p {
                    InterpPart::Text(t) => InterpPart::Text(t.clone()),
                    InterpPart::Expr(e) => InterpPart::Expr(fold(e)),
                })
                .collect(),
        )),
        ExprKind::Index { object, key } => same(ExprKind::Index {
            object: Box::new(fold(object)),
            key: Box::new(fold(key)),
        }),
        ExprKind::MethodCall { object, method, method_span, args } => same(ExprKind::MethodCall {
            object: Box::new(fold(object)),
            method: method.clone(),
            method_span: *method_span,
            args: args.iter().map(fold).collect(),
        }),
        ExprKind::Table(items) => same(ExprKind::Table(
            items
                .iter()
                .map(|item| match item {
                    TableItem::Positional(v) => TableItem::Positional(fold(v)),
                    TableItem::Keyed { key, value } => TableItem::Keyed { key: fold(key), value: fold(value) },
                })
                .collect(),
        )),
        ExprKind::Call { callee, args } => same(ExprKind::Call {
            callee: Box::new(fold(callee)),
            args: args.iter().map(fold).collect(),
        }),
        // A type assertion is a group at runtime: one value, no type check.
        ExprKind::Cast { expr: inner, .. } => same(ExprKind::Group(Box::new(fold(inner)))),
        ExprKind::Group(inner) => {
            let inner = fold(inner);
            match inner.kind {
                // Parentheses around a number can't truncate anything.
                ExprKind::Number(n) => number(n),
                _ => same(ExprKind::Group(Box::new(inner))),
            }
        }
        ExprKind::Unary {
            op,
            op_span,
            operand,
        } => {
            let operand = fold(operand);
            match (op, &operand.kind) {
                (UnOp::Neg, ExprKind::Number(n)) => number(-n),
                _ => same(ExprKind::Unary {
                    op: *op,
                    op_span: *op_span,
                    operand: Box::new(operand),
                }),
            }
        }
        ExprKind::Binary {
            op,
            op_span,
            lhs,
            rhs,
        } => {
            let (lhs, rhs) = (fold(lhs), fold(rhs));
            if let (Some(aop), ExprKind::Number(a), ExprKind::Number(b)) =
                (arith_op(*op), &lhs.kind, &rhs.kind)
            {
                return number(arith(aop, *a, *b));
            }
            same(ExprKind::Binary {
                op: *op,
                op_span: *op_span,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            })
        }
        ExprKind::IfElse {
            branches,
            else_expr,
        } => same(ExprKind::IfElse {
            branches: branches.iter().map(|(c, v)| (fold(c), fold(v))).collect(),
            else_expr: Box::new(fold(else_expr)),
        }),
    }
}

/// Folds `expr` and returns its value if the whole thing is a constant number.
pub fn fold_number(expr: &Expr) -> Option<f64> {
    match fold(expr).kind {
        ExprKind::Number(n) => Some(n),
        _ => None,
    }
}

pub(crate) fn arith_op(op: BinOp) -> Option<ArithOp> {
    Some(match op {
        BinOp::Add => ArithOp::Add,
        BinOp::Sub => ArithOp::Sub,
        BinOp::Mul => ArithOp::Mul,
        BinOp::Div => ArithOp::Div,
        BinOp::IDiv => ArithOp::IDiv,
        BinOp::Mod => ArithOp::Mod,
        BinOp::Pow => ArithOp::Pow,
        _ => return None,
    })
}
