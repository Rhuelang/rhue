//! Property test for milestone 6: the type checker is *sound* on the language
//! it generates programs from. Random `--!strict` programs mix annotated and
//! inferred locals, optionals, unions, a record table with an optional field,
//! early returns, loops and narrowing conditions. Most are ill-typed and get
//! rejected, and every one the checker accepts gets run and can't fail with a
//! type error at runtime (arithmetic on nil, indexing a number, comparing a
//! string with a number, ...).
//!
//! Left out on purpose, since they're `any` by design (the escape hatches of
//! gradual typing): `::` casts (`x :: any` means "trust me") and unannotated
//! locals initialised with `nil`.
//!
//! Every program, accepted or not, also gets checked in non-strict mode, so the
//! checker sees plenty of ill-typed input without panicking.

use std::cell::Cell;

use proptest::prelude::*;
use proptest::test_runner::{Config, TestRunner};
use rhue::{Engine, ErrorKind, TypeMode};

#[derive(Clone, Debug)]
enum E {
    Num(i32),
    Str(&'static str),
    Bool(bool),
    Nil,
    Var(&'static str),
    Field(&'static str),
    Neg(Box<E>),
    Not(Box<E>),
    Len(Box<E>),
    Bin(&'static str, Box<E>, Box<E>),
    Inc(Box<E>),
    ToStr(Box<E>),
    Floor(Box<E>),
    Upper(Box<E>),
    IfElse(Box<C>, Box<E>, Box<E>),
}

#[derive(Clone, Debug)]
enum C {
    Expr(E),
    IsNil(&'static str, bool),
    TypeIs(&'static str, &'static str),
}

#[derive(Clone, Debug)]
enum S {
    Assign(&'static str, E),
    SetField(&'static str, E),
    If(C, Vec<S>, Option<Vec<S>>),
    ReturnIf(C, E),
    While(C, Vec<S>),
    /// Runs its body twice, so narrowing has to survive a second iteration.
    Twice(Vec<S>),
}

/// Every variable the programs use, with how it's declared.
const VARS: &[&str] = &["n", "s", "b", "on", "os", "u", "inferred"];

impl E {
    fn src(&self) -> String {
        match self {
            E::Num(n) => n.to_string(),
            E::Str(s) => format!("{s:?}"),
            E::Bool(b) => b.to_string(),
            E::Nil => "nil".into(),
            E::Var(v) => v.to_string(),
            E::Field(f) => format!("t.{f}"),
            E::Neg(e) => format!("-({})", e.src()),
            E::Not(e) => format!("not ({})", e.src()),
            E::Len(e) => format!("#({})", e.src()),
            E::Bin(op, a, b) => format!("({} {op} {})", a.src(), b.src()),
            E::Inc(e) => format!("inc({})", e.src()),
            E::ToStr(e) => format!("tostring({})", e.src()),
            E::Floor(e) => format!("math.floor({})", e.src()),
            E::Upper(e) => format!("({}):upper()", e.src()),
            E::IfElse(c, a, b) => format!("(if {} then {} else {})", c.src(), a.src(), b.src()),
        }
    }
}

impl C {
    fn src(&self) -> String {
        match self {
            C::Expr(e) => e.src(),
            C::IsNil(v, true) => format!("{v} == nil"),
            C::IsNil(v, false) => format!("{v} ~= nil"),
            C::TypeIs(v, tag) => format!("type({v}) == {tag:?}"),
        }
    }
}

fn block_src(stmts: &[S], indent: usize, out: &mut String) {
    let pad = "  ".repeat(indent);
    for s in stmts {
        match s {
            S::Assign(v, e) => out.push_str(&format!("{pad}{v} = {}\n", e.src())),
            S::SetField(f, e) => out.push_str(&format!("{pad}t.{f} = {}\n", e.src())),
            S::If(c, a, b) => {
                out.push_str(&format!("{pad}if {} then\n", c.src()));
                block_src(a, indent + 1, out);
                if let Some(b) = b {
                    out.push_str(&format!("{pad}else\n"));
                    block_src(b, indent + 1, out);
                }
                out.push_str(&format!("{pad}end\n"));
            }
            S::ReturnIf(c, e) => out.push_str(&format!(
                "{pad}if {} then return {} end\n",
                c.src(),
                e.src()
            )),
            S::While(c, body) => {
                out.push_str(&format!("{pad}while {} do\n", c.src()));
                block_src(body, indent + 1, out);
                out.push_str(&format!("{pad}  break\n{pad}end\n"));
            }
            S::Twice(body) => {
                out.push_str(&format!("{pad}for _ = 1, 2 do\n"));
                block_src(body, indent + 1, out);
                out.push_str(&format!("{pad}end\n"));
            }
        }
    }
}

fn leaf() -> impl Strategy<Value = E> {
    prop_oneof![
        (-3i32..10).prop_map(E::Num),
        prop::sample::select(vec!["", "a", "hi"]).prop_map(E::Str),
        any::<bool>().prop_map(E::Bool),
        Just(E::Nil),
        prop::sample::select(VARS.to_vec()).prop_map(E::Var),
        prop::sample::select(VARS.to_vec()).prop_map(E::Var),
        prop::sample::select(vec!["x", "y"]).prop_map(E::Field),
    ]
}

fn expr() -> impl Strategy<Value = E> {
    leaf().prop_recursive(3, 24, 3, |inner| {
        prop_oneof![
            inner.clone().prop_map(|e| E::Neg(Box::new(e))),
            inner.clone().prop_map(|e| E::Not(Box::new(e))),
            inner.clone().prop_map(|e| E::Len(Box::new(e))),
            (
                prop::sample::select(vec![
                    "+", "-", "*", "..", "<", "<=", "==", "~=", "and", "or"
                ]),
                inner.clone(),
                inner.clone()
            )
                .prop_map(|(op, a, b)| E::Bin(op, Box::new(a), Box::new(b))),
            inner.clone().prop_map(|e| E::Inc(Box::new(e))),
            inner.clone().prop_map(|e| E::ToStr(Box::new(e))),
            inner.clone().prop_map(|e| E::Floor(Box::new(e))),
            inner.clone().prop_map(|e| E::Upper(Box::new(e))),
            (cond_leaf(), inner.clone(), inner).prop_map(|(c, a, b)| E::IfElse(
                Box::new(c),
                Box::new(a),
                Box::new(b)
            )),
        ]
    })
}

fn cond_leaf() -> impl Strategy<Value = C> {
    prop_oneof![
        leaf().prop_map(C::Expr),
        (prop::sample::select(vec!["on", "os", "u"]), any::<bool>())
            .prop_map(|(v, eq)| C::IsNil(v, eq)),
        (
            prop::sample::select(vec!["u", "on", "os", "inferred"]),
            prop::sample::select(vec!["number", "string", "nil"])
        )
            .prop_map(|(v, t)| C::TypeIs(v, t)),
    ]
}

fn cond() -> impl Strategy<Value = C> {
    prop_oneof![cond_leaf(), expr().prop_map(C::Expr)]
}

fn stmt() -> impl Strategy<Value = S> {
    let simple = prop_oneof![
        (prop::sample::select(VARS.to_vec()), expr()).prop_map(|(v, e)| S::Assign(v, e)),
        (prop::sample::select(vec!["x", "y"]), expr()).prop_map(|(f, e)| S::SetField(f, e)),
        (cond(), expr()).prop_map(|(c, e)| S::ReturnIf(c, e)),
    ];
    simple.prop_recursive(2, 8, 3, |inner| {
        prop_oneof![
            (
                cond(),
                prop::collection::vec(inner.clone(), 0..3),
                prop::option::of(prop::collection::vec(inner.clone(), 0..2))
            )
                .prop_map(|(c, a, b)| S::If(c, a, b)),
            (cond(), prop::collection::vec(inner.clone(), 0..3)).prop_map(|(c, b)| S::While(c, b)),
            prop::collection::vec(inner, 1..4).prop_map(S::Twice),
        ]
    })
}

#[derive(Clone, Debug)]
struct Program {
    on: Option<i32>,
    os: Option<&'static str>,
    u_is_num: bool,
    y: Option<&'static str>,
    inferred: E,
    stmts: Vec<S>,
    ret: E,
}

impl Program {
    fn source(&self) -> String {
        let mut out = String::from("--!strict\n");
        out.push_str("local n: number = 4\nlocal s: string = \"str\"\nlocal b: boolean = true\n");
        out.push_str(&format!(
            "local on: number? = {}\n",
            self.on.map_or("nil".into(), |n| n.to_string())
        ));
        out.push_str(&format!(
            "local os: string? = {}\n",
            self.os.map_or("nil".into(), |s| format!("{s:?}"))
        ));
        out.push_str(&format!(
            "local u: number | string = {}\n",
            if self.u_is_num { "7" } else { "\"seven\"" }
        ));
        out.push_str(&format!(
            "local t: {{x: number, y: string?}} = {{x = 1, y = {}}}\n",
            self.y.map_or("nil".into(), |s| format!("{s:?}"))
        ));
        out.push_str("local function inc(v: number): number return v + 1 end\n");
        out.push_str(&format!("local inferred = {}\n", self.inferred.src()));
        block_src(&self.stmts, 0, &mut out);
        out.push_str(&format!("return {}\n", self.ret.src()));
        out
    }
}

fn program() -> impl Strategy<Value = Program> {
    (
        prop::option::of(0i32..5),
        prop::option::of(prop::sample::select(vec!["", "q"])),
        any::<bool>(),
        prop::option::of(prop::sample::select(vec!["", "w"])),
        leaf().prop_filter("nil-initialised locals are `any`", |e| !matches!(e, E::Nil)),
        prop::collection::vec(stmt(), 0..4),
        expr(),
    )
        .prop_map(|(on, os, u_is_num, y, inferred, stmts, ret)| Program {
            on,
            os,
            u_is_num,
            y,
            inferred,
            stmts,
            ret,
        })
}

/// Messages of runtime errors a well-typed program should never raise.
const TYPE_ERRORS: &[&str] = &[
    "attempt to perform arithmetic",
    "attempt to concatenate",
    "attempt to compare",
    "attempt to call",
    "attempt to index",
    "attempt to get length",
    "invalid argument",
];

#[test]
fn accepted_strict_programs_never_raise_type_errors() {
    let accepted = Cell::new(0u32);
    let total = Cell::new(0u32);
    let mut runner = TestRunner::new(Config {
        cases: 6000,
        failure_persistence: None,
        ..Config::default()
    });
    runner
        .run(&program(), |p| {
            let src = p.source();
            total.set(total.get() + 1);
            // never panics, in either mode
            let mut lenient = Engine::new();
            let nonstrict = src.replacen("--!strict", "--!nonstrict", 1);
            let _ = lenient.compile("p", &nonstrict);

            let mut e = Engine::new();
            e.set_instruction_limit(Some(100_000));
            match e.compile("p", &src) {
                Err(err) => {
                    prop_assert_eq!(err.kind, ErrorKind::Type, "{}\n{}", src, err);
                }
                Ok(proto) => {
                    accepted.set(accepted.get() + 1);
                    if let Err(err) = e.run(&proto) {
                        let msg = err.to_string();
                        prop_assert!(
                            !TYPE_ERRORS.iter().any(|t| msg.starts_with(t)),
                            "the checker accepted a program that failed with a type error:\n{}\n=> {}",
                            src,
                            msg
                        );
                    }
                }
            }
            Ok(())
        })
        .unwrap();
    // the property means something only if the checker accepts a fair share
    let (a, t) = (accepted.get(), total.get());
    assert!(a * 20 >= t, "only {a} of {t} programs were accepted");
    println!("accepted {a} of {t}");
}

#[test]
fn checking_never_changes_what_a_program_does() {
    // the same accepted program gives the same result with checking on and off
    let mut runner = TestRunner::new(Config {
        cases: 800,
        failure_persistence: None,
        ..Config::default()
    });
    runner
        .run(&program(), |p| {
            let src = p.source();
            let mut checked = Engine::new();
            let Ok(proto) = checked.compile("p", &src) else {
                return Ok(());
            };
            let mut unchecked = Engine::new();
            unchecked.set_default_type_mode(TypeMode::NoCheck);
            let plain = src.replacen("--!strict", "--!nocheck", 1);
            let a = checked.run(&proto).map_err(|e| e.to_string());
            let b = unchecked.eval(&plain).map_err(|e| e.to_string());
            prop_assert_eq!(a, b);
            Ok(())
        })
        .unwrap();
}

/// Pieces of code that use, clear and re-check an optional number `x`.
const STEPS: &[&str] = &[
    "local u = x + 1",
    "x = nil",
    "x = 3",
    "if x then local w = x + 1 end",
    "if x == nil then x = 1 end",
    "if not x then return 0 end",
    "x = x and x + 1",
    "f = function() return x + 1 end",
    "f = function() if x then return x + 1 end return 0 end",
    "f()",
    "local v = (x or 0) + 1",
];

const LOOPS: &[(&str, &str)] = &[
    ("for _ = 1, 2 do", "end"),
    ("local k = 0 while k < 2 do k += 1", "end"),
    ("local k = 0 repeat k += 1", "until k >= 2"),
    ("for _, _ in {1, 2} do", "end"),
    ("do", "end"),
];

fn narrowing_program() -> impl Strategy<Value = String> {
    (
        any::<bool>(),
        prop::sample::select(vec!["", "if x == nil then return 0 end", "if not x then return 0 end", "assert(x)"]),
        prop::collection::vec(prop::sample::select(STEPS.to_vec()), 0..3),
        prop::sample::select(LOOPS.to_vec()),
        prop::collection::vec(prop::sample::select(STEPS.to_vec()), 1..5),
        prop::collection::vec(prop::sample::select(STEPS.to_vec()), 0..3),
    )
        .prop_map(|(start_nil, guard, before, (open, close), body, after)| {
            format!(
                "--!strict\nlocal x: number? = {}\nlocal f = function() return 0 end\n{guard}\n{}\n{open}\n{}\n{close}\n{}\nreturn 0\n",
                if start_nil { "nil" } else { "1" },
                before.join("\n"),
                body.join("\n"),
                after.join("\n"),
            )
        })
}

#[test]
fn narrowing_is_only_trusted_where_it_holds() {
    let accepted = Cell::new(0u32);
    let mut runner = TestRunner::new(Config {
        cases: 10000,
        failure_persistence: None,
        ..Config::default()
    });
    runner
        .run(&narrowing_program(), |src| {
            let mut e = Engine::new();
            e.set_instruction_limit(Some(100_000));
            if let Ok(proto) = e.compile("p", &src) {
                accepted.set(accepted.get() + 1);
                if let Err(err) = e.run(&proto) {
                    let msg = err.to_string();
                    prop_assert!(
                        !TYPE_ERRORS.iter().any(|t| msg.starts_with(t)),
                        "the checker accepted a program that failed with a type error:\n{}\n=> {}",
                        src,
                        msg
                    );
                }
            }
            Ok(())
        })
        .unwrap();
    assert!(accepted.get() >= 300, "only {} accepted", accepted.get());
}
