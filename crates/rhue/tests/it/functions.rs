//! Milestone 3: functions, calls, multiple returns, varargs, closures and
//! upvalues, the standard library, and runtime errors across frames.

use std::cell::RefCell;
use std::rc::Rc;

use rhue::{CompileOptions, Engine, ErrorKind, Value};

fn engines() -> [Engine; 2] {
    [
        Engine::with_options(CompileOptions::new().fold_constants(true)),
        Engine::with_options(CompileOptions::new().fold_constants(false)),
    ]
}

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
    // again with a collection before every instruction: a value the GC can't
    // see would be freed and break the result
    let mut stressed = Engine::new();
    stressed.set_gc_stress(true);
    let z = stressed
        .eval(src)
        .unwrap_or_else(|e| panic!("(GC stress) {}", e.render(false)));
    assert_eq!(
        format!("{x:?}").len(),
        format!("{z:?}").len(),
        "GC stress changed the results"
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

fn nums(src: &str) -> Vec<f64> {
    run(src)
        .iter()
        .map(|v| {
            v.as_number()
                .unwrap_or_else(|| panic!("{v:?} is not a number"))
        })
        .collect()
}

fn err(src: &str) -> rhue::Error {
    let [mut a, mut b] = engines();
    let e = a.eval(src).expect_err("expected an error");
    let e2 = b.eval(src).expect_err("expected an error (unfolded)");
    assert_eq!(e.to_string(), e2.to_string());
    e
}

/// An engine whose `print` output is collected.
fn capturing() -> (Engine, Rc<RefCell<Vec<String>>>) {
    let out = Rc::new(RefCell::new(Vec::new()));
    let mut e = Engine::new();
    let sink = out.clone();
    e.on_print(move |line| sink.borrow_mut().push(line.to_string()));
    (e, out)
}

#[test]
fn functions_and_calls() {
    assert_eq!(
        one("local function add(a, b) return a + b end return add(2, 3)"),
        n(5.0)
    );
    assert_eq!(
        one("function sq(x) return x * x end return sq(sq(3))"),
        n(81.0)
    );
    assert_eq!(
        one("local f = function(x) return x .. '!' end return f 'hi'"),
        s("hi!")
    );
    assert_eq!(
        one("local function f() end return (f())"),
        Value::Nil,
        "parens force exactly one value"
    );
    assert_eq!(
        one("local function f(a, b) return b end return f(1)"),
        Value::Nil,
        "missing args are nil"
    );
    assert_eq!(
        one("local function f(a) return a end return f(1, 2, 3)"),
        n(1.0),
        "extra args are dropped"
    );
    assert_eq!(one("return (function(x) return x * 2 end)(21)"), n(42.0));
    // calls as statements discard results
    assert_eq!(
        one("local c = 0 local function inc() c += 1 return c end inc() inc() return c"),
        n(2.0)
    );
}

#[test]
fn recursion() {
    assert_eq!(
        one(
            "local function fact(n) if n <= 1 then return 1 end return n * fact(n - 1) end return fact(10)"
        ),
        n(3628800.0)
    );
    assert_eq!(
        one(
            "local function fib(n) if n < 2 then return n end return fib(n-1) + fib(n-2) end return fib(20)"
        ),
        n(6765.0)
    );
    // mutual recursion through globals
    assert_eq!(
        one(
            "function even(n) if n == 0 then return true end return odd(n - 1) end
             function odd(n) if n == 0 then return false end return even(n - 1) end
             return even(100)"
        ),
        Value::Bool(true)
    );
}

#[test]
fn multiple_returns_are_adjusted() {
    let pre = "local function three() return 1, 2, 3 end ";
    assert_eq!(nums(&format!("{pre} return three()")), [1.0, 2.0, 3.0]);
    assert_eq!(
        nums(&format!("{pre} return three(), 10")),
        [1.0, 10.0],
        "only the last call expands"
    );
    assert_eq!(
        nums(&format!("{pre} return 0, three()")),
        [0.0, 1.0, 2.0, 3.0]
    );
    assert_eq!(
        nums(&format!("{pre} return (three())")),
        [1.0],
        "parentheses truncate to one"
    );
    assert_eq!(
        run(&format!(
            "{pre} local a, b, c, d = three() return a, b, c, d"
        )),
        [n(1.0), n(2.0), n(3.0), Value::Nil]
    );
    assert_eq!(
        nums(&format!("{pre} local a, b = three() return a, b")),
        [1.0, 2.0]
    );
    assert_eq!(
        nums(&format!("{pre} local x, y x, y = three() return x, y")),
        [1.0, 2.0]
    );
    assert_eq!(
        nums(&format!(
            "{pre} local function sum(a, b, c) return a + b + c end return sum(three())"
        )),
        [6.0]
    );
    assert_eq!(
        nums(&format!(
            "{pre} local function count(...) return select('#', ...) end return count(three(), three())"
        )),
        [4.0]
    );
    assert_eq!(
        run("local function none() end local a, b = none() return a, b"),
        [Value::Nil, Value::Nil]
    );
    assert_eq!(run("local function none() end return none()"), []);
}

#[test]
fn varargs() {
    assert_eq!(
        nums("local function f(...) return ... end return f(1, 2, 3)"),
        [1.0, 2.0, 3.0]
    );
    assert_eq!(
        nums("local function f(a, ...) return select('#', ...) end return f(1, 2, 3)"),
        [2.0]
    );
    assert_eq!(
        nums("local function f(...) local a, b = ... return b end return f(5, 6, 7)"),
        [6.0]
    );
    assert_eq!(
        nums("local function f(...) return select(2, ...) end return f(5, 6, 7)"),
        [6.0, 7.0]
    );
    assert_eq!(
        nums("local function f(...) return select(-1, ...) end return f(5, 6, 7)"),
        [7.0]
    );
    assert_eq!(
        one(
            "local function join(sep, ...) local out = '' for i = 1, select('#', ...) do out ..= (if i > 1 then sep else '') .. select(i, ...) end return out end return join(', ', 'a', 'b', 'c')"
        ),
        s("a, b, c")
    );
    // the main chunk is vararg too (empty here)
    assert_eq!(nums("return select('#', ...)"), [0.0]);
    assert_eq!(
        err("local function f(a) return ... end").kind,
        ErrorKind::Compile
    );
}

#[test]
fn closures_capture_variables_not_values() {
    assert_eq!(
        nums(
            "local function counter() local n = 0 return function() n += 1 return n end end
              local a, b = counter(), counter()
              a() a() b()
              return a(), b()"
        ),
        [3.0, 2.0],
        "each call to counter() gets its own n"
    );
    assert_eq!(
        nums("local x = 1 local function get() return x end x = 5 return get()"),
        [5.0],
        "a closure sees later writes to an open upvalue"
    );
    assert_eq!(
        nums(
            "local get, set
              do local v = 10 get = function() return v end set = function(x) v = x end end
              set(42)
              return get()"
        ),
        [42.0],
        "two closures share one variable after its scope ends"
    );
}

#[test]
fn nested_closures_thread_upvalues_through() {
    assert_eq!(
        one("local function outer() local x = 1
               return function() return function() x += 1 return x end end
             end
             local f = outer()()
             f() return f()"),
        n(3.0)
    );
    assert_eq!(
        one(
            "local a = 'a' local function f() local b = 'b' return function() local c = 'c' return function() return a .. b .. c end end end return f()()()"
        ),
        s("abc")
    );
}

#[test]
fn each_loop_iteration_gets_a_fresh_variable() {
    // numeric for: the loop variable itself
    assert_eq!(
        nums(
            "local f1, f2, f3
              for i = 1, 3 do
                local g = function() return i end
                if i == 1 then f1 = g elseif i == 2 then f2 = g else f3 = g end
              end
              return f1(), f2(), f3()"
        ),
        [1.0, 2.0, 3.0]
    );
    // while: a local declared in the body
    assert_eq!(
        nums(
            "local a, b local i = 0
              while i < 2 do
                i += 1
                local v = i * 10
                if i == 1 then a = function() return v end else b = function() return v end end
              end
              return a(), b()"
        ),
        [10.0, 20.0]
    );
    // repeat: body locals (the condition can read them too)
    assert_eq!(
        nums(
            "local a, b local i = 0
              repeat
                i += 1
                local v = i
                if i == 1 then a = function() return v end else b = function() return v end end
              until v >= 2
              return a(), b()"
        ),
        [1.0, 2.0]
    );
}

#[test]
fn break_and_continue_close_captured_variables() {
    // `continue` skips the end of the body, where the variable would normally
    // be closed
    assert_eq!(
        nums(
            "local a, b
              for i = 1, 4 do
                local v = i * 100
                if i == 1 then a = function() return v end continue end
                if i == 2 then b = function() return v end continue end
              end
              return a(), b()"
        ),
        [100.0, 200.0]
    );
    // `break` leaves the loop with the variable still open
    assert_eq!(
        nums(
            "local f local i = 0
              while true do
                i += 1
                local v = i
                f = function() return v end
                if i == 3 then break end
              end
              i = 99
              return f()"
        ),
        [3.0]
    );
    assert_eq!(
        nums(
            "local f for i = 1, 10 do f = function() return i end if i == 4 then break end end return f()"
        ),
        [4.0]
    );
}

#[test]
fn shadowed_and_reassigned_upvalues() {
    assert_eq!(
        nums(
            "local x = 1
              local function f() return x end
              local x = 2
              return f(), x"
        ),
        [1.0, 2.0],
        "the closure captured the first x"
    );
    assert_eq!(
        nums("local function make() local n = 0 local function inc() n += 1 end local function get() return n end return inc, get end
              local inc, get = make()
              inc() inc() inc()
              return get()"),
        [3.0]
    );
}

#[test]
fn functions_are_values() {
    assert_eq!(one("local function f() end return type(f)"), s("function"));
    assert_eq!(
        one("local function f() end local g = f return f == g"),
        Value::Bool(true)
    );
    assert_eq!(
        one("local function mk() return function() end end return mk() == mk()"),
        Value::Bool(false)
    );
    assert_eq!(one("return type(print)"), s("function"));
    assert!(
        one("local function f() end return tostring(f)")
            .as_str()
            .unwrap()
            .starts_with("function: 0x")
    );
    let mut e = Engine::new();
    let v = e.eval("return function(x) return x end").unwrap().remove(0);
    assert_eq!(v.type_name(), "function");
    // a function from one eval can be called from the next via a global
    e.set_global("double", v.clone());
    e.eval("function double(x) return x * 2 end").unwrap();
    assert_eq!(e.eval_expr("double(21)").unwrap(), n(42.0));
}

#[test]
fn higher_order_functions() {
    assert_eq!(
        one(
            "local function compose(f, g) return function(...) return f(g(...)) end end
             local function inc(x) return x + 1 end
             local function dbl(x) return x * 2 end
             return compose(inc, dbl)(5)"
        ),
        n(11.0)
    );
    assert_eq!(
        one(
            "local function apply_n(f, n, x) for i = 1, n do x = f(x) end return x end
             return apply_n(function(v) return v * 3 end, 4, 1)"
        ),
        n(81.0)
    );
}

#[test]
fn print_and_tostring() {
    let (mut e, out) = capturing();
    e.eval("print('hello', 1, nil, true, 2.5) print() print(1 .. '')")
        .unwrap();
    assert_eq!(*out.borrow(), ["hello\t1\tnil\ttrue\t2.5", "", "1"]);
    assert_eq!(one("return tostring(10 / 4) .. tostring(nil)"), s("2.5nil"));
}

#[test]
fn type_tonumber_select_assert() {
    assert_eq!(
        run("return type(1), type('s'), type(nil), type(true)"),
        [s("number"), s("string"), s("nil"), s("boolean")]
    );
    assert_eq!(
        run(
            "return tonumber('42'), tonumber(' 0x10 '), tonumber('1e2'), tonumber('z'), tonumber(7)"
        ),
        [n(42.0), n(16.0), n(100.0), Value::Nil, n(7.0)]
    );
    assert_eq!(
        run("return tonumber('ff', 16), tonumber('-101', 2), tonumber('nan'), tonumber('inf')"),
        [n(255.0), n(-5.0), Value::Nil, Value::Nil]
    );
    assert_eq!(
        run("return select('#'), select('#', nil, nil)"),
        [n(0.0), n(2.0)]
    );
    assert_eq!(run("return assert(1, 'unused')"), [n(1.0), s("unused")]);
    assert_eq!(err("assert(false)").to_string(), "assertion failed!");
    assert_eq!(
        err("assert(nil, 'custom message')").to_string(),
        "custom message"
    );
    assert_eq!(
        err("return select(0, 1)").to_string(),
        "invalid argument #1 to 'select' (index out of range)"
    );
    assert_eq!(
        err("return type()").to_string(),
        "missing argument #1 to 'type'"
    );
}

#[test]
fn error_function() {
    let e = err(
        "local function check(x) if x < 0 then error('negative input: ' .. x) end return x end return check(-3)",
    );
    assert_eq!(e.kind, ErrorKind::Runtime);
    assert_eq!(e.to_string(), "negative input: -3");
    assert_eq!(err("error(42)").to_string(), "42");
}

#[test]
fn calling_non_functions() {
    assert_eq!(
        err("local x = 5 x()").to_string(),
        "attempt to call a number value"
    );
    assert_eq!(
        err("undefined_function()").to_string(),
        "attempt to call a nil value"
    );
}

#[test]
fn stack_overflow_is_an_error_not_a_crash() {
    let e = err("local function f(n) return f(n + 1) + 1 end return f(1)");
    assert_eq!(e.to_string(), "stack overflow");
    let rendered = e.render(false);
    assert!(rendered.contains("stack traceback:"), "{rendered}");
    assert!(
        // `return f(1)` is a tail call, so <main> has no frame of its own
        rendered.contains("(repeated 9999 times)"),
        "recursion collapses into one line: {rendered}"
    );
    assert!(rendered.lines().count() < 20, "{rendered}");
}

#[test]
fn deep_recursion_within_the_limit_works_on_a_small_thread() {
    // calls don't recurse on the Rust stack, so 5000 Lua frames fit in a 256
    // KiB thread
    let r = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let mut e = Engine::new();
            e.eval("local function depth(n) if n == 0 then return 0 end return 1 + depth(n - 1) end return depth(5000)")
                .map(|v| v[0].as_number())
                .map_err(|e| e.to_string())
        })
        .unwrap()
        .join()
        .unwrap();
    assert_eq!(r, Ok(Some(5000.0)));
}

#[test]
fn errors_show_a_traceback() {
    let mut e = Engine::new();
    let src = "local function inner(x)\n  return x + nil\nend\nlocal function outer()\n  return inner(1)\nend\nreturn outer()";
    let err = e.eval_named("trace.rhue", src).unwrap_err();
    assert_eq!(
        err.to_string(),
        "attempt to perform arithmetic (add) on nil"
    );
    let rendered = err.render(false);
    assert!(
        rendered.contains("in `inner` (trace.rhue:2:10)"),
        "{rendered}"
    );
    assert!(
        // both calls are tail calls: their frames are gone, but the traceback
        // still names where they came from
        rendered.contains("called from `outer` (tail call) (trace.rhue:5:10)"),
        "{rendered}"
    );
    assert!(
        rendered.contains("called from <main> (tail call) (trace.rhue:7:8)"),
        "{rendered}"
    );
}

#[test]
fn errors_point_into_the_source_that_defined_the_function() {
    let mut e = Engine::new();
    e.eval_named("lib.rhue", "function risky(x)\n  return x .. nil\nend")
        .unwrap();
    let err = e.eval_named("main.rhue", "return risky('a')").unwrap_err();
    assert_eq!(err.source_name(), "lib.rhue");
    let rendered = err.render(false);
    assert!(rendered.contains("lib.rhue:2:"), "{rendered}");
    assert!(
        rendered.contains("called from <main> (tail call) (main.rhue:1:8)"),
        "{rendered}"
    );
}

#[test]
fn instruction_limit_applies_to_function_calls() {
    let mut e = Engine::new();
    e.set_instruction_limit(Some(50_000));
    let err = e
        .eval("local function spin() while true do end end spin()")
        .unwrap_err();
    assert!(
        err.to_string()
            .starts_with("script exceeded its instruction limit")
    );
}

#[test]
fn bare_engine_has_no_globals() {
    let mut e = Engine::bare();
    assert_eq!(e.eval_expr("print").unwrap(), Value::Nil);
}

#[test]
fn engine_survives_errors_inside_closures() {
    let mut e = Engine::new();
    e.eval("local n = 0 function bump() n += 1 if n == 2 then error('boom') end return n end")
        .unwrap();
    assert_eq!(e.eval_expr("bump()").unwrap(), n(1.0));
    assert!(e.eval_expr("bump()").is_err());
    // the upvalue was closed when the defining chunk returned, so it survives
    assert_eq!(e.eval_expr("bump()").unwrap(), n(3.0));
}
