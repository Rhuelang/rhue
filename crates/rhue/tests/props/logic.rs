//! Property test for milestone 2: random expressions over locals of mixed
//! types (numbers, booleans, nil, strings), using arithmetic, comparisons,
//! `and`/`or`/`not`, `..` and if-expressions. Each one gets checked against
//! a reference evaluator written straight from Luau's semantics, in five
//! positions that hit different codegen paths:
//!
//! 1. as a returned value (`return e`),
//! 2. assigned to an existing local (`v0 = e; return v0`),
//! 3. as an `if` condition (jump lists, no booleans made),
//! 4. inside a function, with the variables passed as parameters,
//! 5. inside a closure, with the variables captured as upvalues and the
//!    result assigned back to one of them through the closure.
//!
//! Errors have to match too, if the reference raises the VM has to raise.

use proptest::prelude::*;
use rhue::{CompileOptions, Engine, Value};
use rhue_bytecode::{ArithOp, arith, format_number};

#[derive(Clone, Debug, PartialEq)]
enum V {
    Nil,
    Bool(bool),
    Num(f64),
    Str(String),
}

impl V {
    fn truthy(&self) -> bool {
        !matches!(self, V::Nil | V::Bool(false))
    }

    fn source(&self) -> String {
        match self {
            V::Nil => "nil".into(),
            V::Bool(b) => b.to_string(),
            V::Num(n) => format!("{n:?}"),
            V::Str(s) => format!("{s:?}"),
        }
    }

    fn same(&self, v: &Value) -> bool {
        match (self, v) {
            (V::Nil, Value::Nil) => true,
            (V::Bool(a), Value::Bool(b)) => a == b,
            (V::Num(a), Value::Number(b)) => {
                a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan())
            }
            (V::Str(a), Value::String(b)) => **a == **b,
            _ => false,
        }
    }
}

#[derive(Clone, Debug)]
enum E {
    Lit(V),
    Var(usize),
    Neg(Box<E>),
    Not(Box<E>),
    Arith(ArithOp, Box<E>, Box<E>),
    Cmp(&'static str, Box<E>, Box<E>),
    And(Box<E>, Box<E>),
    Or(Box<E>, Box<E>),
    Concat(Box<E>, Box<E>),
    If(Box<E>, Box<E>, Box<E>),
}

const VARS: usize = 4;

type R = Result<V, ()>;

fn num(v: &V) -> Result<f64, ()> {
    match v {
        V::Num(n) => Ok(*n),
        _ => Err(()),
    }
}

impl E {
    fn eval(&self, env: &[V]) -> R {
        Ok(match self {
            E::Lit(v) => v.clone(),
            E::Var(i) => env[*i].clone(),
            E::Neg(e) => V::Num(-num(&e.eval(env)?)?),
            E::Not(e) => V::Bool(!e.eval(env)?.truthy()),
            E::Arith(op, l, r) => {
                let (a, b) = (l.eval(env)?, r.eval(env)?);
                V::Num(arith(*op, num(&a)?, num(&b)?))
            }
            E::Cmp(op, l, r) => {
                let (a, b) = (l.eval(env)?, r.eval(env)?);
                V::Bool(match *op {
                    "==" => a == b && !matches!((&a, &b), (V::Num(x), _) if x.is_nan()),
                    "~=" => !(a == b && !matches!((&a, &b), (V::Num(x), _) if x.is_nan())),
                    _ => {
                        // `a > b` is `b < a`, `a >= b` is `b <= a`
                        let (x, y, strict) = match *op {
                            "<" => (&a, &b, true),
                            "<=" => (&a, &b, false),
                            ">" => (&b, &a, true),
                            ">=" => (&b, &a, false),
                            _ => unreachable!(),
                        };
                        match (x, y) {
                            (V::Num(x), V::Num(y)) => {
                                if strict {
                                    x < y
                                } else {
                                    x <= y
                                }
                            }
                            (V::Str(x), V::Str(y)) => {
                                if strict {
                                    x < y
                                } else {
                                    x <= y
                                }
                            }
                            _ => return Err(()),
                        }
                    }
                })
            }
            E::And(l, r) => {
                let a = l.eval(env)?;
                if a.truthy() { r.eval(env)? } else { a }
            }
            E::Or(l, r) => {
                let a = l.eval(env)?;
                if a.truthy() { a } else { r.eval(env)? }
            }
            E::Concat(l, r) => {
                let piece = |v: V| match v {
                    V::Str(s) => Ok(s),
                    V::Num(n) => Ok(format_number(n)),
                    _ => Err(()),
                };
                let (a, b) = (l.eval(env)?, r.eval(env)?);
                V::Str(piece(a)? + &piece(b)?)
            }
            E::If(c, t, f) => {
                if c.eval(env)?.truthy() {
                    t.eval(env)?
                } else {
                    f.eval(env)?
                }
            }
        })
    }

    fn source(&self) -> String {
        match self {
            E::Lit(v) => v.source(),
            E::Var(i) => format!("v{i}"),
            E::Neg(e) => format!("(-{})", e.source()),
            E::Not(e) => format!("(not {})", e.source()),
            E::Arith(op, l, r) => {
                let sym = match op {
                    ArithOp::Add => "+",
                    ArithOp::Sub => "-",
                    ArithOp::Mul => "*",
                    ArithOp::Div => "/",
                    ArithOp::IDiv => "//",
                    ArithOp::Mod => "%",
                    ArithOp::Pow => "^",
                };
                format!("({} {sym} {})", l.source(), r.source())
            }
            E::Cmp(op, l, r) => format!("({} {op} {})", l.source(), r.source()),
            E::And(l, r) => format!("({} and {})", l.source(), r.source()),
            E::Or(l, r) => format!("({} or {})", l.source(), r.source()),
            E::Concat(l, r) => format!("({} .. {})", l.source(), r.source()),
            E::If(c, t, f) => format!(
                "(if {} then {} else {})",
                c.source(),
                t.source(),
                f.source()
            ),
        }
    }
}

fn value() -> impl Strategy<Value = V> {
    prop_oneof![
        Just(V::Nil),
        any::<bool>().prop_map(V::Bool),
        (0u32..20).prop_map(|n| V::Num(n as f64)),
        (1u32..16).prop_map(|n| V::Num(n as f64 / 4.0)),
        prop::sample::select(vec!["", "a", "b", "ab", "10"]).prop_map(|s| V::Str(s.into())),
    ]
}

fn expr() -> impl Strategy<Value = E> {
    let leaf = prop_oneof![
        value().prop_map(E::Lit),
        (0..VARS).prop_map(E::Var),
        (0..VARS).prop_map(E::Var)
    ];
    leaf.prop_recursive(6, 48, 3, |inner| {
        let b = |e: E| Box::new(e);
        prop_oneof![
            inner.clone().prop_map(move |e| E::Neg(b(e))),
            inner.clone().prop_map(move |e| E::Not(b(e))),
            (
                prop::sample::select(vec![ArithOp::Add, ArithOp::Sub, ArithOp::Mul, ArithOp::Mod]),
                inner.clone(),
                inner.clone()
            )
                .prop_map(move |(o, l, r)| E::Arith(o, b(l), b(r))),
            (
                prop::sample::select(vec!["==", "~=", "<", "<=", ">", ">="]),
                inner.clone(),
                inner.clone()
            )
                .prop_map(move |(o, l, r)| E::Cmp(o, b(l), b(r))),
            (inner.clone(), inner.clone()).prop_map(move |(l, r)| E::And(b(l), b(r))),
            (inner.clone(), inner.clone()).prop_map(move |(l, r)| E::Or(b(l), b(r))),
            (inner.clone(), inner.clone()).prop_map(move |(l, r)| E::Concat(b(l), b(r))),
            (inner.clone(), inner.clone(), inner).prop_map(move |(c, t, f)| E::If(
                b(c),
                b(t),
                b(f)
            )),
        ]
    })
}

fn prelude(env: &[V]) -> String {
    let names: Vec<String> = (0..VARS).map(|i| format!("v{i}")).collect();
    let vals: Vec<String> = env.iter().map(V::source).collect();
    format!("local {} = {}\n", names.join(", "), vals.join(", "))
}

fn check(src: &str, expected: &Result<V, ()>, fold: bool) -> Result<(), TestCaseError> {
    check_with(src, expected, fold, false)
}

fn check_with(
    src: &str,
    expected: &Result<V, ()>,
    fold: bool,
    gc_stress: bool,
) -> Result<(), TestCaseError> {
    let mut engine = Engine::with_options(CompileOptions::new().fold_constants(fold));
    engine.set_gc_stress(gc_stress);
    match (engine.eval(src), expected) {
        (Ok(vals), Ok(want)) => {
            prop_assert!(
                want.same(&vals[0]),
                "fold={}\n{}\n=> {:?}, expected {:?}",
                fold,
                src,
                vals[0],
                want
            );
        }
        (Err(e), Err(())) => {
            prop_assert_eq!(
                e.kind,
                rhue::ErrorKind::Runtime,
                "fold={}\n{}\n{}",
                fold,
                src,
                e.render(false)
            );
        }
        (Ok(vals), Err(())) => prop_assert!(
            false,
            "fold={fold}\n{src}\n=> {:?}, expected an error",
            vals[0]
        ),
        (Err(e), Ok(want)) => prop_assert!(
            false,
            "fold={fold}\n{src}\n=> error {}, expected {:?}",
            e,
            want
        ),
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1500))]

    #[test]
    fn returned_value(env in prop::collection::vec(value(), VARS), e in expr()) {
        let src = format!("{}return {}", prelude(&env), e.source());
        let expected = e.eval(&env);
        check(&src, &expected, true)?;
        check(&src, &expected, false)?;
    }

    #[test]
    fn assigned_to_a_live_local(env in prop::collection::vec(value(), VARS), e in expr()) {
        // assigning into v0 while `e` may read v0 is where register reuse bites
        let src = format!("{}v0 = {}\nreturn v0", prelude(&env), e.source());
        let expected = e.eval(&env);
        check(&src, &expected, true)?;
        check(&src, &expected, false)?;
    }

    #[test]
    fn used_as_a_condition(env in prop::collection::vec(value(), VARS), e in expr()) {
        let src = format!("{}if {} then return true else return false end", prelude(&env), e.source());
        let expected = e.eval(&env).map(|v| V::Bool(v.truthy()));
        check(&src, &expected, true)?;
        check(&src, &expected, false)?;
    }

    #[test]
    fn inside_a_function_with_parameters(env in prop::collection::vec(value(), VARS), e in expr()) {
        let args: Vec<String> = env.iter().map(V::source).collect();
        let src = format!(
            "local function f(v0, v1, v2, v3)\n  return {}\nend\nreturn f({})",
            e.source(),
            args.join(", ")
        );
        let expected = e.eval(&env);
        check(&src, &expected, true)?;
        check(&src, &expected, false)?;
    }

    #[test]
    fn inside_a_closure_over_upvalues(env in prop::collection::vec(value(), VARS), e in expr()) {
        // v0..v3 are upvalues of the inner closure, it writes the result into
        // v0 (SETUPVAL), and the outer function reads v0 back from its register
        let src = format!(
            "{}local function outer()\n  local set = function() v0 = {} end\n  set()\n  return v0\nend\nreturn outer()",
            prelude(&env),
            e.source()
        );
        let expected = e.eval(&env);
        check(&src, &expected, true)?;
        check(&src, &expected, false)?;
        // collect before every instruction: strings built by `..` and the
        // closure itself must survive
        check_with(&src, &expected, true, true)?;
    }
}
