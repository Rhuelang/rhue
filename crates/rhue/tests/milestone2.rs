//! Milestone 2: locals, assignment, comparisons, `and`/`or`, control flow,
//! strings and globals. Every program runs with folding on and off.

use rhue::{CompileOptions, Engine, ErrorKind, Value};

fn engines() -> [Engine; 2] {
    [
        Engine::with_options(CompileOptions {
            fold_constants: true,
        }),
        Engine::with_options(CompileOptions {
            fold_constants: false,
        }),
    ]
}

/// Runs `src` in both modes and returns its return values.
fn run(src: &str) -> Vec<Value> {
    let [mut a, mut b] = engines();
    let x = a
        .eval(src)
        .unwrap_or_else(|e| panic!("{}", e.render(false)));
    let y = b
        .eval(src)
        .unwrap_or_else(|e| panic!("(unfolded) {}", e.render(false)));
    assert_eq!(
        format!("{x:?}"),
        format!("{y:?}"),
        "folded and unfolded disagree"
    );
    x
}

fn one(src: &str) -> Value {
    run(src).remove(0)
}

fn n(x: f64) -> Value {
    Value::Number(x)
}

fn s(x: &str) -> Value {
    Value::from(x)
}

fn err(src: &str) -> (ErrorKind, String) {
    let [mut a, mut b] = engines();
    let e = a.eval(src).expect_err("expected an error");
    let e2 = b.eval(src).expect_err("expected an error (unfolded)");
    assert_eq!(e.to_string(), e2.to_string());
    (e.kind, e.to_string())
}

#[test]
fn locals_and_scopes() {
    assert_eq!(run("local a, b = 1, 2 return a, b"), [n(1.0), n(2.0)]);
    assert_eq!(run("local a, b = 1 return a, b"), [n(1.0), Value::Nil]);
    assert_eq!(run("local a = 1, 2 return a"), [n(1.0)]);
    assert_eq!(one("local x = 1 do local x = 2 end return x"), n(1.0));
    assert_eq!(one("local x = 1 local x = x + 1 return x"), n(2.0));
    assert_eq!(
        one("local x = 10 do local y = x * 2 x = y end return x"),
        n(20.0)
    );
}

#[test]
fn assignment() {
    assert_eq!(
        run("local a, b = 1, 2 a, b = b, a return a, b"),
        [n(2.0), n(1.0)]
    );
    assert_eq!(
        run("local a, b, c = 1, 2, 3 a, b, c = c return a, b, c"),
        [n(3.0), Value::Nil, Value::Nil]
    );
    assert_eq!(one("local x = 5 x = x * x - x return x"), n(20.0));
    assert_eq!(one("local x, y = nil, 7 x = x or y return x"), n(7.0));
    assert_eq!(one("local x = 3 x = x > 2 and x or 0 return x"), n(3.0));
}

#[test]
fn compound_assignment() {
    assert_eq!(
        run(
            "local a, b, c, d, e, f, g = 10, 10, 10, 10, 10, 10, 2 a += 5 b -= 5 c *= 5 d /= 4 e //= 4 f %= 4 g ^= 10 return a, b, c, d, e, f, g"
        ),
        [n(15.0), n(5.0), n(50.0), n(2.5), n(2.0), n(2.0), n(1024.0)]
    );
    assert_eq!(one("local s = 'a' s ..= 'b' s ..= 1 return s"), s("ab1"));
    assert_eq!(
        one("local x = 2 x *= x + 1 return x"),
        n(6.0),
        "rhs is parenthesised"
    );
}

#[test]
fn and_or_return_operands() {
    assert_eq!(one("return nil or 5"), n(5.0));
    assert_eq!(one("return false or nil"), Value::Nil);
    assert_eq!(one("return 1 and 2"), n(2.0));
    assert_eq!(one("return nil and 2"), Value::Nil);
    assert_eq!(
        one("return false and error_if_evaluated"),
        Value::Bool(false)
    );
    assert_eq!(one("return 0 or 1"), n(0.0), "0 is truthy");
    assert_eq!(
        one("return '' and 'yes'"),
        s("yes"),
        "empty string is truthy"
    );
    assert_eq!(
        one("local a, b = 1, nil return a and b or 'fallback'"),
        s("fallback")
    );
}

#[test]
fn short_circuit_skips_errors() {
    assert_eq!(one("return true or (1 + nil)"), Value::Bool(true));
    assert_eq!(one("return false and (1 + nil)"), Value::Bool(false));
    assert_eq!(
        one("local x if x and x > 1 then return 'big' end return 'small'"),
        s("small")
    );
}

#[test]
fn comparisons() {
    let cases = [
        ("1 < 2", true),
        ("2 <= 2", true),
        ("3 > 4", false),
        ("4 >= 4", true),
        ("1 == 1", true),
        ("1 ~= 1", false),
        ("'a' < 'b'", true),
        ("'abc' < 'abd'", true),
        ("'Z' < 'a'", true),
        ("'' < 'a'", true),
        ("'a' == 'a'", true),
        ("'a' .. 'b' == 'ab'", true),
        ("1 == '1'", false),
        ("nil == false", false),
        ("nil == nil", true),
        ("true ~= false", true),
        ("0/0 == 0/0", false),
        ("0/0 ~= 0/0", true),
        ("not (0/0 < 1)", true),
        ("-0 == 0", true),
    ];
    for (src, want) in cases {
        assert_eq!(one(&format!("return {src}")), Value::Bool(want), "{src}");
    }
}

#[test]
fn comparison_errors() {
    assert_eq!(
        err("return 1 < 'a'").1,
        "attempt to compare number < string"
    );
    assert_eq!(err("return nil <= 1").1, "attempt to compare nil <= number");
    // `a > b` is evaluated as `b < a`, so the operands appear swapped, as in
    // Lua
    assert_eq!(err("return 1 > nil").1, "attempt to compare nil < number");
    assert_eq!(err("return {} < {}").1, "attempt to compare table < table");
}

#[test]
fn if_statements() {
    let classify = |x: i32| {
        one(&format!(
            "local x = {x}
             if x < 0 then return 'negative'
             elseif x == 0 then return 'zero'
             elseif x < 10 then return 'small'
             else return 'large' end"
        ))
    };
    assert_eq!(classify(-5), s("negative"));
    assert_eq!(classify(0), s("zero"));
    assert_eq!(classify(3), s("small"));
    assert_eq!(classify(99), s("large"));
    assert_eq!(one("local r = 1 if nil then r = 2 end return r"), n(1.0));
}

#[test]
fn if_expressions() {
    assert_eq!(
        one("local x = 5 return if x > 3 then 'hi' else 'lo'"),
        s("hi")
    );
    assert_eq!(
        one("local x = 1 return if x == 0 then 'a' elseif x == 1 then 'b' else 'c'"),
        s("b")
    );
    assert_eq!(
        one("local x = 1 x = if x == 1 then x + 10 else x return x"),
        n(11.0)
    );
    assert_eq!(one("return 1 + if true then 2 else 3"), n(3.0));
}

#[test]
fn while_and_repeat() {
    assert_eq!(
        one("local i, s = 0, 0 while i < 10 do i += 1 s += i end return s"),
        n(55.0)
    );
    assert_eq!(
        one("local i = 0 while true do i += 1 if i >= 7 then break end end return i"),
        n(7.0)
    );
    assert_eq!(
        one("local i = 0 repeat i += 1 until i == 4 return i"),
        n(4.0)
    );
    // `until` sees the body's locals
    assert_eq!(
        one("local n = 0 repeat local done = n >= 3 n += 1 until done return n"),
        n(4.0)
    );
    // a repeat body always runs at least once
    assert_eq!(one("local n = 0 repeat n += 1 until true return n"), n(1.0));
}

#[test]
fn continue_and_break() {
    assert_eq!(
        one("local s = 0 for i = 1, 10 do if i % 2 == 0 then continue end s += i end return s"),
        n(25.0)
    );
    assert_eq!(
        one(
            "local s, i = 0, 0 while i < 10 do i += 1 if i == 3 then continue end if i == 6 then break end s += i end return s"
        ),
        n(12.0)
    );
    assert_eq!(
        one("local n = 0 repeat n += 1 if n < 5 then continue end until n >= 5 return n"),
        n(5.0)
    );
    // `break` leaves only the innermost loop
    assert_eq!(
        one(
            "local c = 0 for i = 1, 3 do for j = 1, 3 do if j == 2 then break end c += 1 end end return c"
        ),
        n(3.0)
    );
}

#[test]
fn numeric_for() {
    assert_eq!(
        one("local s = 0 for i = 1, 5 do s += i end return s"),
        n(15.0)
    );
    assert_eq!(
        one("local s = 0 for i = 10, 1, -3 do s = s * 100 + i end return s"),
        n(10070401.0)
    );
    assert_eq!(
        one("local c = 0 for i = 0, 1, 0.25 do c += 1 end return c"),
        n(5.0)
    );
    assert_eq!(
        one("local c = 0 for i = 5, 1 do c += 1 end return c"),
        n(0.0)
    );
    // the loop variable is a copy: assigning it doesn't change iteration
    assert_eq!(
        one("local c = 0 for i = 1, 3 do c += 1 i = 100 end return c"),
        n(3.0)
    );
    // start, limit and step are evaluated once
    assert_eq!(
        one("local lim = 3 local c = 0 for i = 1, lim do lim = 100 c += 1 end return c"),
        n(3.0)
    );
    // the loop variable isn't visible afterwards
    assert_eq!(one("for i = 1, 2 do end return i"), Value::Nil);
}

#[test]
fn numeric_for_errors() {
    assert_eq!(err("for i = 1, 10, 0 do end").1, "'for' step is zero");
    assert_eq!(
        err("for i = nil, 10 do end").1,
        "invalid 'for' initial value (number expected, got nil)"
    );
    assert_eq!(
        err("for i = 1, 'x' do end").1,
        "invalid 'for' limit (number expected, got string)"
    );
    assert_eq!(
        err("for i = 1, 2, true do end").1,
        "invalid 'for' step (number expected, got boolean)"
    );
}

#[test]
fn fibonacci() {
    let src = "local a, b = 0, 1 for i = 1, 80 do a, b = b, a + b end return a";
    assert_eq!(one(src), n(23416728348467685.0));
}

#[test]
fn strings() {
    assert_eq!(one("return 'a' .. 'b' .. 'c'"), s("abc"));
    assert_eq!(one("return 1 .. 2"), s("12"));
    assert_eq!(one("return 'x' .. 1.5 .. 'y' .. 10 ^ 20"), s("x1.5y1e+20"));
    assert_eq!(one("return #'hello'"), n(5.0));
    assert_eq!(one("return #'héllo'"), n(6.0), "length is in bytes");
    assert_eq!(one(r#"return "tab\there\u{21}""#), s("tab\there!"));
    assert_eq!(one("return [[long\nstring]]"), s("long\nstring"));
    assert_eq!(
        one("local s = '' for i = 1, 5 do s ..= i end return s"),
        s("12345")
    );
    assert_eq!(
        err("return 'a' .. nil").1,
        "attempt to concatenate string with nil"
    );
    assert_eq!(
        err("return 'a' .. true").1,
        "attempt to concatenate string with boolean"
    );
    assert_eq!(
        err("return #5").1,
        "attempt to get length of a number value"
    );
}

#[test]
fn globals() {
    let mut e = Engine::new();
    e.eval("counter = 1").unwrap();
    e.eval("counter += 1").unwrap();
    assert_eq!(e.eval_expr("counter").unwrap(), n(2.0));
    assert_eq!(e.global("counter"), n(2.0));
    assert_eq!(e.eval_expr("undefined_thing").unwrap(), Value::Nil);

    e.set_global("greeting", "hi");
    e.set_global("count", 3);
    assert_eq!(
        e.eval_expr("greeting .. ' x' .. count").unwrap(),
        s("hi x3")
    );

    // locals don't leak into the next chunk
    e.eval("local temp = 1").unwrap();
    assert_eq!(e.eval_expr("temp").unwrap(), Value::Nil);

    // assigning nil removes a global
    e.eval("counter = nil").unwrap();
    assert_eq!(e.global("counter"), Value::Nil);
}

#[test]
fn local_shadows_global() {
    let mut e = Engine::new();
    e.set_global("x", 1);
    assert_eq!(e.eval("local x = 2 x = 3 return x").unwrap(), [n(3.0)]);
    assert_eq!(e.global("x"), n(1.0));
}

#[test]
fn instruction_limit() {
    let mut e = Engine::new();
    e.set_instruction_limit(Some(10_000));
    let err = e.eval("while true do end").unwrap_err();
    assert_eq!(err.kind, ErrorKind::Runtime);
    assert_eq!(
        err.to_string(),
        "script exceeded its instruction limit (10000)"
    );
    // a normal script fits
    assert_eq!(
        e.eval("local s = 0 for i = 1, 100 do s += i end return s")
            .unwrap(),
        [n(5050.0)]
    );
}

#[test]
fn several_syntax_errors_are_reported_together() {
    let mut e = Engine::new();
    let err = e
        .eval("local a = \nlocal b = )\nc = * 2\nreturn a")
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Syntax);
    assert_eq!(err.diagnostics.len(), 3);
    let rendered = err.render(false);
    assert_eq!(rendered.matches("Error:").count(), 3, "{rendered}");
}

#[test]
fn static_errors() {
    assert_eq!(
        err("break"),
        (ErrorKind::Compile, "`break` outside a loop".into())
    );
    assert_eq!(
        err("repeat if x then continue end local y = 1 until y").1,
        "local `y` is used in the `until` condition but may be skipped by `continue`"
    );
}

#[test]
fn runtime_error_spans_point_at_the_operation() {
    let mut e = Engine::new();
    let src = "local name = nil\nlocal greeting = 'hi ' .. name";
    let err = e.eval_named("greet.rhue", src).unwrap_err();
    assert_eq!(err.diagnostic().span().text(src), "'hi ' .. name");
    assert!(
        err.render(false).contains("greet.rhue:2:"),
        "{}",
        err.render(false)
    );
}
