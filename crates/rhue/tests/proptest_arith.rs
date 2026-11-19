//! Property test: random arithmetic trees get rendered to source, run through
//! the whole pipeline (folded and unfolded), and compared with a reference
//! evaluator that works right on the generated tree. That checks the parser
//! (precedence through full parenthesisation), the compiler's register
//! allocation, K-forms and folding, and the VM, all against one oracle.

use proptest::prelude::*;
use rhue::{CompileOptions, Engine, Value};
use rhue_bytecode::{ArithOp, arith};

#[derive(Clone, Debug)]
enum Tree {
    Num(f64),
    Neg(Box<Tree>),
    Bin(ArithOp, Box<Tree>, Box<Tree>),
}

impl Tree {
    fn eval(&self) -> f64 {
        match self {
            Tree::Num(n) => *n,
            Tree::Neg(t) => -t.eval(),
            Tree::Bin(op, l, r) => arith(*op, l.eval(), r.eval()),
        }
    }

    fn source(&self) -> String {
        match self {
            Tree::Num(n) => format!("{n:?}"),
            Tree::Neg(t) => format!("(-{})", t.source()),
            Tree::Bin(op, l, r) => format!("({} {} {})", l.source(), symbol(*op), r.source()),
        }
    }
}

fn symbol(op: ArithOp) -> &'static str {
    match op {
        ArithOp::Add => "+",
        ArithOp::Sub => "-",
        ArithOp::Mul => "*",
        ArithOp::Div => "/",
        ArithOp::IDiv => "//",
        ArithOp::Mod => "%",
        ArithOp::Pow => "^",
    }
}

fn leaf() -> impl Strategy<Value = Tree> {
    prop_oneof![
        (0u32..50).prop_map(|n| Tree::Num(n as f64)),
        (0u32..200_000).prop_map(|n| Tree::Num(n as f64)), // forces LOADK and K-forms past 255
        (1u32..64).prop_map(|n| Tree::Num(n as f64 / 8.0)),
    ]
}

fn tree() -> impl Strategy<Value = Tree> {
    let op = prop_oneof![
        Just(ArithOp::Add),
        Just(ArithOp::Sub),
        Just(ArithOp::Mul),
        Just(ArithOp::Div),
        Just(ArithOp::IDiv),
        Just(ArithOp::Mod),
        Just(ArithOp::Pow),
    ];
    leaf().prop_recursive(8, 64, 2, move |inner| {
        prop_oneof![
            inner.clone().prop_map(|t| Tree::Neg(Box::new(t))),
            (op.clone(), inner.clone(), inner).prop_map(|(o, l, r)| Tree::Bin(
                o,
                Box::new(l),
                Box::new(r)
            )),
        ]
    })
}

fn same(a: f64, b: f64) -> bool {
    a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn pipeline_matches_reference(t in tree()) {
        let src = t.source();
        let expected = t.eval();
        for fold in [true, false] {
            let mut engine = Engine::with_options(CompileOptions { fold_constants: fold });
            let got = engine.eval_expr(&src).map_err(|e| TestCaseError::fail(e.render(false)))?;
            let Value::Number(got) = got else { return Err(TestCaseError::fail("not a number")) };
            prop_assert!(same(got, expected), "{} (fold={}) gave {:?}, expected {:?}", src, fold, got, expected);
        }
    }
}
