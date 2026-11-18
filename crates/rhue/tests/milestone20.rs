//! Milestone 20: vectors. Luau's `vector` value (three `f32`s, stored in
//! the value itself), its arithmetic, its library, and its type.

use rhue::{Engine, Value};

fn run(src: &str) -> String {
    let mut e = Engine::new();
    let vals: Vec<Value> = e
        .eval_as::<rhue::Variadic<Value>>(src)
        .unwrap_or_else(|err| panic!("{src}\n{}", err.render(false)))
        .0;
    vals.iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join(" | ")
}

fn error(src: &str) -> String {
    Engine::new()
        .eval(src)
        .unwrap_err()
        .diagnostic()
        .message
        .clone()
}

fn check(src: &str) -> Result<(), String> {
    Engine::new()
        .check("t.rhue", &format!("--!strict\n{src}"))
        .map_err(|e| e.render(false))
}

#[test]
fn vectors_are_values() {
    assert_eq!(
        run("local v = vector.create(1, 2, 3)\nreturn v, type(v), typeof(v), vector.create(4, 5)"),
        "1, 2, 3 | vector | vector | 4, 5, 0"
    );
    // Equality by value; usable as table keys by value (-0 is 0).
    assert_eq!(
        run("local a, b = vector.create(1, 2, 3), vector.create(1, 2, 3)
local t = { [a] = 'found' }
return a == b, a ~= vector.zero, rawequal(a, b), t[b], t[vector.create(-0, 0, 0)] == t[vector.zero]"),
        "true | true | true | found | true"
    );
    assert!(error("local t = {}\nt[vector.create(0/0, 0, 0)] = 1").contains("NaN"));
    // Components, including Roblox's capitals; vectors can't be changed.
    assert_eq!(
        run("local v = vector.create(1, 2, 3)\nreturn v.x, v.y, v.z, v.X, v.Y, v.Z"),
        "1 | 2 | 3 | 1 | 2 | 3"
    );
    assert_eq!(
        error("return vector.create(1, 2, 3).w"),
        "attempt to index vector with 'w'"
    );
    assert_eq!(
        error("local v = vector.one\nv.x = 2"),
        "attempt to index vector with 'x'"
    );
    assert!(error("return #vector.one").contains("length"));
    // 32-bit floats: components round, `tostring` shows them at that precision.
    assert_eq!(
        run("local v = vector.create(0.1, 1e20, -0)\nreturn v, v.x"),
        "0.1, 1e+20, -0 | 0.10000000149011612"
    );
}

#[test]
fn vector_arithmetic() {
    let src = "local a, b = vector.create(1, 2, 3), vector.create(4, 5, 6)
return a + b, b - a, a * b, b / a, a * 2, 2 * a, b / 2, 12 / a, b // 4, -a";
    assert_eq!(
        run(src),
        "5, 7, 9 | 3, 3, 3 | 4, 10, 18 | 4, 2.5, 2 | 2, 4, 6 | 2, 4, 6 | 2, 2.5, 3 | 12, 6, 4 | 1, 1, 1 | -1, -2, -3"
    );
    for (expr, op) in [
        ("vector.one + 1", "add"),
        ("1 - vector.one", "sub"),
        ("vector.one % vector.one", "mod"),
        ("vector.one ^ 2", "pow"),
        ("2 // vector.one", "idiv"),
    ] {
        assert!(
            error(&format!("return {expr}")).contains(&format!("arithmetic ({op})")),
            "{expr}"
        );
    }
    assert_eq!(
        error("return vector.one < vector.zero"),
        "attempt to compare vector < vector"
    );
    // In compound assignment and loops, as any value.
    assert_eq!(
        run("local p = vector.zero\nfor i = 1, 10 do p += vector.create(i, 0, 1) end\nreturn p"),
        "55, 0, 10"
    );
}

#[test]
fn the_vector_library() {
    let cases = [
        ("vector.magnitude(vector.create(3, 4, 12))", "13"),
        ("vector.normalize(vector.create(0, 3, 4))", "0, 0.6, 0.8"),
        (
            "vector.dot(vector.create(1, 2, 3), vector.create(4, 5, 6))",
            "32",
        ),
        (
            "vector.cross(vector.create(1, 0, 0), vector.create(0, 1, 0))",
            "0, 0, 1",
        ),
        ("vector.floor(vector.create(1.5, -1.5, 2))", "1, -2, 2"),
        ("vector.ceil(vector.create(1.5, -1.5, 2))", "2, -1, 2"),
        ("vector.abs(vector.create(-1, 2, -0.5))", "1, 2, 0.5"),
        ("vector.sign(vector.create(-3, 0, 7))", "-1, 0, 1"),
        (
            "vector.clamp(vector.create(5, -5, 0.5), vector.zero, vector.one)",
            "1, 0, 0.5",
        ),
        (
            "vector.max(vector.create(1, 5, 2), vector.create(3, 1, 2), vector.create(0, 0, 9))",
            "3, 5, 9",
        ),
        (
            "vector.min(vector.create(1, 5, 2), vector.create(3, 1, 2))",
            "1, 1, 2",
        ),
        ("vector.zero, vector.one", "0, 0, 0 | 1, 1, 1"),
    ];
    for (expr, want) in cases {
        assert_eq!(run(&format!("return {expr}")), want, "{expr}");
    }
    // Angles: unsigned, or signed around an axis.
    let pi = std::f64::consts::FRAC_PI_2;
    let mut e = Engine::new();
    let a: f64 = e
        .eval_as("return vector.angle(vector.create(1, 0, 0), vector.create(0, 1, 0))")
        .unwrap();
    assert!((a - pi).abs() < 1e-6);
    let a: f64 = e
        .eval_as("return vector.angle(vector.create(0, 1, 0), vector.create(1, 0, 0), vector.create(0, 0, 1))")
        .unwrap();
    assert!((a + pi).abs() < 1e-6);
    // The zero vector normalizes to NaNs, as in Luau.
    assert_eq!(
        run("local n = vector.normalize(vector.zero)\nreturn n.x ~= n.x"),
        "true"
    );
    assert!(
        error("return vector.clamp(vector.zero, vector.one, vector.zero)")
            .contains("max.x must be greater")
    );
    assert!(error("return vector.dot(vector.one, 1)").contains("vector expected, got number"));
    assert!(error("return vector.create('x', 1)").contains("number expected"));
}

#[test]
fn vectors_are_typed() {
    check(
        "local a = vector.create(1, 2, 3)
local b: vector = a + vector.one
local c: vector = a * 2 + 3 * b / 4 - b // 2
local n: number = a.x + a.Y + vector.dot(a, b) + vector.magnitude(-a)
local function f(v: vector | number): number
  if type(v) == 'vector' then return v.z end
  return v
end
local m: vector = vector.max(a, b, c)",
    )
    .unwrap();
    for (bad, msg) in [
        (
            "local e = vector.one + 1",
            "can't be applied to 'vector' and 'number'",
        ),
        (
            "local e = 2 - vector.one",
            "can't be applied to 'number' and 'vector'",
        ),
        ("local e = vector.one % vector.one", "can't be applied"),
        (
            "local s: string = vector.one.x",
            "could not be converted into 'string'",
        ),
        ("local v = vector.one\nv.x = 1", "read-only"),
        ("local w = vector.one.w", "not found in class 'vector'"),
        ("local b = vector.one < vector.zero", "cannot be compared"),
        (
            "local v: vector = 5",
            "could not be converted into 'vector'",
        ),
    ] {
        let err = check(bad).unwrap_err();
        assert!(err.contains(msg), "{bad}: {err}");
    }
}

#[test]
fn hosts_pass_vectors() {
    let mut e = Engine::new();
    e.register("lerp", |a: [f32; 3], b: [f32; 3], t: f32| -> [f32; 3] {
        [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * t)
    });
    assert_eq!(
        e.eval_as::<[f32; 3]>("return lerp(vector.zero, vector.create(10, 20, 30), 0.5)")
            .unwrap(),
        [5.0, 10.0, 15.0]
    );
    e.set_global("spawn_point", Value::Vector([1.0, 2.0, 3.0]));
    assert_eq!(e.eval_as::<f64>("return spawn_point.y").unwrap(), 2.0);
    assert_eq!(
        e.global_type("lerp").as_deref(),
        Some("(vector, vector, number) -> vector")
    );
    let err = e.eval("lerp(1, 2, 3)").unwrap_err();
    assert!(
        err.diagnostic().message.contains("vector expected"),
        "{}",
        err.diagnostic().message
    );
}

#[cfg(feature = "serde")]
#[test]
fn vectors_read_as_three_numbers() {
    let mut e = Engine::new();
    let v = e.eval("return vector.create(1, 2, 3)").unwrap().remove(0);
    let xyz: [f32; 3] = rhue::serde::from_value(&mut e.context(), v.clone()).unwrap();
    assert_eq!(xyz, [1.0, 2.0, 3.0]);
    let t: (f64, f64, f64) = rhue::serde::from_value(&mut e.context(), v).unwrap();
    assert_eq!(t, (1.0, 2.0, 3.0));
}
