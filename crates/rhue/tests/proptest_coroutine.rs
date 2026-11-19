//! Property test: the arithmetic trees from `proptest_meta.rs`, evaluated
//! inside a coroutine where every leaf and every metamethod *yields* its value
//! to the main thread, which sends it straight back. Each yield suspends
//! somewhere different (a plain call, an `__add` frame, a comparison's
//! continuation...), so this checks that resuming always puts the values where
//! the interrupted instruction wanted them. The answer still has to match plain
//! arithmetic bit for bit, under GC stress too.

use proptest::prelude::*;
use rhue::{Engine, Value};
use rhue_bytecode::{ArithOp, arith};

#[derive(Clone, Debug)]
enum Tree {
    Num(f64, bool),
    Neg(Box<Tree>),
    Bin(ArithOp, Box<Tree>, Box<Tree>),
}

impl Tree {
    fn eval(&self) -> f64 {
        match self {
            Tree::Num(n, _) => *n,
            Tree::Neg(t) => -t.eval(),
            Tree::Bin(op, l, r) => arith(*op, l.eval(), r.eval()),
        }
    }

    fn source(&self) -> String {
        match self {
            Tree::Num(n, true) => format!("W(Y({n:?}))"),
            Tree::Num(n, false) => format!("Y({n:?})"),
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
    let leaf = (0u32..2000, any::<bool>()).prop_map(|(n, boxed)| Tree::Num(n as f64 / 8.0, boxed));
    leaf.prop_recursive(6, 40, 2, move |inner| {
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

const PRELUDE: &str = "
    local Y = coroutine.yield
    local M = {}
    local function W(v) return setmetatable({v = v}, M) end
    local function V(x) if type(x) == 'table' then return x.v end return x end
    M.__add = function(a, b) return W(Y(V(a) + V(b))) end
    M.__sub = function(a, b) return W(Y(V(a) - V(b))) end
    M.__mul = function(a, b) return W(Y(V(a) * V(b))) end
    M.__div = function(a, b) return W(Y(V(a) / V(b))) end
    M.__idiv = function(a, b) return W(Y(V(a) // V(b))) end
    M.__mod = function(a, b) return W(Y(V(a) % V(b))) end
    M.__pow = function(a, b) return W(Y(V(a) ^ V(b))) end
    M.__unm = function(a) return W(Y(-a.v)) end
    M.__lt = function(a, b) return Y(V(a) < V(b)) end
";

fn same(a: f64, b: f64) -> bool {
    a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    #[test]
    fn yields_everywhere_match_plain_arithmetic(t in tree(), stress in any::<bool>()) {
        // the driver echoes every yielded value back, counting the yields
        let src = format!(
            "{PRELUDE}
            local co = coroutine.create(function()
                local r = V({})
                local _ = W(r) < W(0)   -- one comparison continuation too
                return r
            end)
            local ok, v = coroutine.resume(co)
            local yields = 0
            while coroutine.status(co) ~= 'dead' do
                yields += 1
                ok, v = coroutine.resume(co, v)
                assert(ok, v)
            end
            return v, yields",
            t.source()
        );
        let expected = t.eval();
        let mut engine = Engine::new();
        engine.set_gc_stress(stress);
        let got = engine.eval(&src).map_err(|e| TestCaseError::fail(e.render(false)))?;
        let Some(Value::Number(got)) = got.first().cloned() else {
            return Err(TestCaseError::fail("not a number"));
        };
        prop_assert!(same(got, expected), "{} gave {:?}, expected {:?}", src, got, expected);
    }
}
