//! End-to-end tests: source → lexer → parser → compiler → VM. Every case runs
//! with constant folding on and off, and the two have to agree bit-for-bit.

use rhue::{CompileOptions, Engine, ErrorKind, Value};

fn engines() -> [Engine; 2] {
    [
        Engine::with_options(CompileOptions::new().fold_constants(true)),
        Engine::with_options(CompileOptions::new().fold_constants(false)),
    ]
}

fn eval(src: &str) -> Value {
    let [mut folded, mut plain] = engines();
    let a = folded
        .eval_expr(src)
        .unwrap_or_else(|e| panic!("{src}: {}", e.render(false)));
    let b = plain
        .eval_expr(src)
        .unwrap_or_else(|e| panic!("{src} (unfolded): {}", e.render(false)));
    assert!(same(&a, &b), "{src}: folded {a:?} vs unfolded {b:?}");
    a
}

fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan())
        }
        _ => a == b,
    }
}

fn num(src: &str) -> f64 {
    eval(src)
        .as_number()
        .unwrap_or_else(|| panic!("{src} did not produce a number"))
}

fn shown(src: &str) -> String {
    eval(src).to_string()
}

fn runtime_error(src: &str) -> String {
    for mut engine in engines() {
        let err = engine.eval_expr(src).expect_err("expected an error");
        assert_eq!(err.kind, ErrorKind::Runtime, "{src}: {}", err);
    }
    engines()[0].eval_expr(src).unwrap_err().to_string()
}

#[test]
fn basic_arithmetic() {
    assert_eq!(num("1 + 2"), 3.0);
    assert_eq!(num("10 - 4 - 3"), 3.0);
    assert_eq!(num("2 * 3 + 4"), 10.0);
    assert_eq!(num("2 + 3 * 4"), 14.0);
    assert_eq!(num("(2 + 3) * 4"), 20.0);
    assert_eq!(num("7 / 2"), 3.5);
    assert_eq!(num("7 // 2"), 3.0);
    assert_eq!(num("-7 // 2"), -4.0);
    assert_eq!(num("7 % 3"), 1.0);
    assert_eq!(num("-7 % 3"), 2.0);
    assert_eq!(num("2 ^ 10"), 1024.0);
    assert_eq!(num("2 ^ 3 ^ 2"), 512.0);
    assert_eq!(num("-2 ^ 2"), -4.0);
    assert_eq!(num("2 ^ -1"), 0.5);
    assert_eq!(num("- - 5"), 5.0);
    assert_eq!(num("0x10 + 0b11 + 1_000"), 1019.0);
    assert_eq!(num("1e3 / .5"), 2000.0);
}

#[test]
fn big_and_fractional_constants() {
    assert_eq!(num("100000 * 3"), 300000.0);
    assert_eq!(num("0.1 + 0.2"), 0.1 + 0.2);
    assert_eq!(num("32767 + 1"), 32768.0);
    assert_eq!(num("-32768 - 1"), -32769.0);
}

#[test]
fn ieee_edge_cases() {
    assert_eq!(num("1 / 0"), f64::INFINITY);
    assert_eq!(num("-1 / 0"), f64::NEG_INFINITY);
    assert!(num("0 / 0").is_nan());
    assert!(num("-0").is_sign_negative());
    assert_eq!(shown("-0"), "-0");
}

#[test]
fn formatting_matches_luau() {
    assert_eq!(shown("1 + 2"), "3");
    assert_eq!(shown("7 / 2"), "3.5");
    assert_eq!(shown("0.1 + 0.2"), "0.30000000000000004");
    assert_eq!(shown("10 ^ 20"), "1e+20");
    assert_eq!(shown("1 / 0"), "inf");
    assert_eq!(shown("nil"), "nil");
    assert_eq!(shown("not nil"), "true");
}

#[test]
fn literals_and_not() {
    assert_eq!(eval("nil"), Value::Nil);
    assert_eq!(eval("true"), Value::Bool(true));
    assert_eq!(eval("not true"), Value::Bool(false));
    assert_eq!(eval("not 0"), Value::Bool(false));
    assert_eq!(eval("not not nil"), Value::Bool(false));
}

#[test]
fn chunks_return_multiple_values() {
    let [mut e, _] = engines();
    assert_eq!(
        e.eval("return 1, 2 * 3, nil").unwrap(),
        vec![Value::Number(1.0), Value::Number(6.0), Value::Nil]
    );
    assert_eq!(e.eval("return").unwrap(), vec![]);
    assert_eq!(e.eval("-- nothing to see\n").unwrap(), vec![]);
    assert_eq!(
        e.eval("--!strict\nreturn 2 ^ 8;").unwrap(),
        vec![Value::Number(256.0)]
    );
}

#[test]
fn runtime_errors_name_the_operation_and_type() {
    assert_eq!(
        runtime_error("1 + true"),
        "attempt to perform arithmetic (add) on boolean"
    );
    assert_eq!(
        runtime_error("nil * 2"),
        "attempt to perform arithmetic (mul) on nil"
    );
    assert_eq!(
        runtime_error("-true"),
        "attempt to perform arithmetic (unm) on boolean"
    );
    assert_eq!(
        runtime_error("nil // false"),
        "attempt to perform arithmetic (idiv) on nil and boolean"
    );
    assert_eq!(
        runtime_error("2 ^ (1 + nil)"),
        "attempt to perform arithmetic (add) on nil"
    );
}

#[test]
fn runtime_error_points_at_the_failing_expression() {
    let mut engine = Engine::new();
    let err = engine
        .eval_named("calc.rhue", "return 1 + (2 * true)")
        .unwrap_err();
    assert_eq!(err.diagnostic().span(), rhue::Span::new(12, 20));
    let rendered = err.render(false);
    assert!(rendered.contains("calc.rhue"), "{rendered}");
    assert!(
        rendered.contains("attempt to perform arithmetic (mul) on boolean"),
        "{rendered}"
    );
}

#[test]
fn syntax_and_compile_errors_are_classified() {
    let mut engine = Engine::new();
    let e = engine.eval_expr("1 +").unwrap_err();
    assert_eq!(e.kind, ErrorKind::Syntax);
    assert_eq!(e.to_string(), "expected an expression, found end of input");

    let e = engine.eval("break").unwrap_err();
    assert_eq!(e.kind, ErrorKind::Compile);

    let e = engine.eval_expr("\"unterminated").unwrap_err();
    assert_eq!(e.kind, ErrorKind::Syntax);
    assert!(e.render(false).contains("unterminated string"));
}

#[test]
fn errors_render_with_source_context() {
    let mut engine = Engine::new();
    let err = engine.eval_named("demo.rhue", "return (1 + 2").unwrap_err();
    let out = err.render(false);
    assert!(out.contains("expected `)` to close `(`"), "{out}");
    assert!(out.contains("unclosed `(` opened here"), "{out}");
    assert!(out.contains("return (1 + 2"), "{out}");
}

#[test]
fn deep_but_legal_nesting_runs() {
    // 249 right-nested levels need 250 registers: exactly at the limit
    let src = format!("{}1{}", "1 + (".repeat(249), ")".repeat(249));
    assert_eq!(num(&src), 250.0);
    // a long flat chain only needs two registers
    let chain = format!("1{}", " + 1".repeat(900));
    assert_eq!(num(&chain), 901.0);
}

#[test]
fn engine_can_be_reused() {
    let mut engine = Engine::new();
    for i in 0..100 {
        assert_eq!(
            engine.eval_expr(&format!("{i} * 2")).unwrap(),
            Value::Number(i as f64 * 2.0)
        );
    }
}
