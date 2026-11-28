//! Milestone 23: performance IV, calls. Cheaper Lua calls and returns, fast
//! paths for common builtins (only when the callee *is* the builtin, so
//! shadowing doesn't need special handling), allocation-free yields and
//! in-place coroutine switches. None of it should be visible, so these tests
//! compare against the slow paths and poke at the edges.

use rhue::{Engine, Value};

// unfrozen so the tests can swap a builtin out from under its fast path
fn run(src: &str) -> String {
    let mut e = Engine::unfrozen();
    let vals: Vec<Value> = e
        .eval_as::<rhue::Variadic<Value>>(src)
        .unwrap_or_else(|err| panic!("{src}\n{}", err.render(false)))
        .0;
    vals.iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Every builtin with a fast path, called both ways on lots of arguments:
/// `f(...)` (the fast path when it applies, the full native when it doesn't)
/// and `pcall(f, ...)` (always the full native). Results and error messages
/// have to match.
#[test]
fn fast_paths_match_the_full_natives() {
    let functions = [
        "math.abs",
        "math.floor",
        "math.ceil",
        "math.sqrt",
        "math.round",
        "math.sign",
        "math.min",
        "math.max",
        "math.clamp",
        "type",
        "rawequal",
        "rawget",
        "rawlen",
        "getmetatable",
        "string.len",
        "string.byte",
        "string.sub",
        "next",
    ];
    let args = [
        "",
        "nil",
        "1",
        "-2.5",
        "0/0",
        "1/0",
        "-0",
        "3, 1",
        "1, 2, 3",
        "2, 3, 1",
        "1, 0/0",
        "'abc'",
        "'abc', 2",
        "'abc', -1",
        "'abc', 0",
        "'abc', 9",
        "'abc', 1.5",
        "'abc', 2, 3",
        "'abc', -2, -1",
        "'h\u{e9}llo', 2, 2",
        "'', 1",
        "'abc', nil, 2",
        "5, 'x'",
        "true",
        "{}",
        "{1, 2}",
        "{1, 2}, 1",
        "{x = 1}, 'x'",
        "{x = 1}, 'y'",
        "{1, 2}, 0/0",
        "{x = 1}, 'zz'",
        "setmetatable({}, { __metatable = 'locked' })",
        "setmetatable({}, { __index = function() return 1 end }), 'k'",
        "{}, {}",
        "'a', 'a'",
        "print",
        "print, print",
        "{1, nil, 3}, 2",
    ];
    let mut src = String::from("local out = {}\n");
    for f in functions {
        for a in args {
            src.push_str(&format!(
                "do\n\
                 local fast = table.pack(pcall(function() return {f}({a}) end))\n\
                 local slow = table.pack(pcall({f}{comma}{a}))\n\
                 local function norm(t) local o = {{}} for i = 1, t.n do local v = t[i] if type(v) == 'string' then v = v:gsub('^.-:%d+: ', '') end o[i] = tostring(v) end return table.concat(o, ',') end\n\
                 local x, y = norm(fast), norm(slow)\n\
                 if x ~= y then table.insert(out, [==[{f}({a})]==] .. ': ' .. x .. ' vs ' .. y) end\n\
                 end\n",
                comma = if a.is_empty() { "" } else { ", " },
            ));
        }
    }
    src.push_str("return table.concat(out, '\\n')");
    let diffs = run(&src);
    // NaN prints as `nan` both ways, but a table's identity differs between
    // calls that build one, neither is a real difference
    let real: Vec<&str> = diffs
        .lines()
        .filter(|l| !l.is_empty() && !l.contains("table: "))
        .collect();
    assert!(
        real.is_empty(),
        "fast and full paths disagree:\n{}",
        real.join("\n")
    );
}

#[test]
fn shadowed_builtins_are_called_as_written() {
    assert_eq!(
        run(
            "local real = math.floor\nmath.floor = function(x) return 'mine ' .. x end\nlocal a = math.floor(2.5)\nmath.floor = real\nreturn a, math.floor(2.5)"
        ),
        "mine 2.5 | 2"
    );
    assert_eq!(
        run("local type = function() return 'shadowed' end\nreturn type(1)"),
        "shadowed"
    );
    // a host can replace a builtin too
    let mut e = Engine::new();
    e.register("type", |_: Value| "host".to_string());
    assert_eq!(e.eval_as::<String>("return type(1)").unwrap(), "host");
    // methods on strings reach the same functions
    assert_eq!(
        run("local s = 'hello' return s:len(), s:byte(2), s:sub(2, 3), s:sub(1), s:sub(-3)"),
        "5 | 101 | el | hello | llo"
    );
}

/// The default engine freezes the libraries, so a script can't swap a builtin
/// out from under its fast path at all. The write fails and both ways of
/// calling it still give the real thing.
#[test]
fn frozen_builtins_keep_their_fast_paths() {
    let mut e = Engine::new();
    for write in [
        "math.floor = function(x) return 'mine ' .. x end",
        "rawset(string, 'len', function() return -1 end)",
    ] {
        let err = e.eval(write).expect_err(write);
        assert!(err.to_string().contains("readonly table"), "{write}: {err}");
    }
    assert_eq!(
        e.eval("return math.floor(2.5), select(2, pcall(math.floor, 2.5)), ('abc'):len(), string.len('abc')")
            .unwrap(),
        [
            Value::Number(2.0),
            Value::Number(2.0),
            Value::Number(3.0),
            Value::Number(3.0)
        ]
    );
    // a local with the same name is the script's own, frozen or not
    assert_eq!(
        e.eval_as::<String>(
            "local type = function() return 'shadowed' end
return type(1)"
        )
        .unwrap(),
        "shadowed"
    );
}

#[test]
fn setmetatable_fast_path_keeps_its_rules() {
    assert_eq!(
        run(
            "local t = setmetatable({}, { __metatable = 'no' })\nlocal ok, err = pcall(setmetatable, t, {})\nlocal ok2, err2 = pcall(function() return setmetatable(t, {}) end)\nreturn ok, ok2, (err:gsub('^.-:%d+: ', '')) == (err2:gsub('^.-:%d+: ', '')), getmetatable(t)"
        ),
        "false | false | true | no"
    );
    assert_eq!(
        run(
            "local f = table.freeze({})\nlocal ok, err = pcall(function() setmetatable(f, {}) end)\nreturn ok, err:match('readonly') ~= nil"
        ),
        "false | true"
    );
    assert_eq!(
        run(
            "local mt = {}\nlocal t = setmetatable({}, mt)\nlocal same = getmetatable(t) == mt\nsetmetatable(t, nil)\nreturn same, getmetatable(t)"
        ),
        "true | nil"
    );
}

#[test]
fn iteration_through_next_and_ipairs() {
    assert_eq!(
        run(
            "local t = { 10, 20, 30, x = 1 }\nlocal s, n = 0, 0\nfor i, v in ipairs(t) do s += i * v end\nfor k, v in pairs(t) do n += 1 end\nreturn s, n"
        ),
        "140 | 4"
    );
    // ipairs stops at the first hole, clearing fields during pairs works
    assert_eq!(
        run(
            "local t = { 1, 2, nil, 4 }\nlocal c = 0\nfor _ in ipairs(t) do c += 1 end\nlocal h = { a = 1, b = 2, c = 3 }\nfor k in pairs(h) do h[k] = nil end\nreturn c, next(h)"
        ),
        "2 | nil"
    );
    // errors still come from the full natives
    assert!(
        Engine::new()
            .eval("next({}, 'missing')")
            .unwrap_err()
            .render(false)
            .contains("invalid key to 'next'")
    );
    assert!(
        Engine::new()
            .eval("for _ in ipairs(nil) do end")
            .unwrap_err()
            .render(false)
            .contains("table expected")
    );
    // a metatable-driven iteration (`__iter`) is unaffected
    assert_eq!(
        run(
            "local t = setmetatable({}, { __iter = function() local i = 0 return function() i += 1 if i <= 3 then return i end end end })\nlocal s = 0 for v in t do s += v end return s"
        ),
        "6"
    );
}

#[test]
fn calls_and_returns() {
    // results counted every way a caller can want them
    assert_eq!(
        run(
            "local function none() end\nlocal function three() return 1, 2, 3 end\nlocal function five() return 1, 2, 3, 4, 5 end\nlocal a, b, c, d = three()\nlocal t = { five() }\nlocal u = { none() }\nreturn a, b, c, d, #t, #u, select('#', five()), (five())"
        ),
        "1 | 2 | 3 | nil | 5 | 0 | 5 | 1"
    );
    // varargs, missing and extra arguments
    assert_eq!(
        run(
            "local function f(a, b, ...) return a, b, select('#', ...) end\nlocal x, y, n = f(1)\nlocal p, q, m = f(1, 2, 3, 4, 5)\nreturn x, y, n, p, q, m"
        ),
        "1 | nil | 0 | 1 | 2 | 3"
    );
    // closures over locals still see them after the frame returns
    assert_eq!(
        run(
            "local function counter() local n = 0 return function() n += 1 return n end end\nlocal c = counter() c() c()\nreturn c()"
        ),
        "3"
    );
    // deep recursion still reports a stack overflow
    assert!(
        Engine::new()
            .eval("local function f(n) return 1 + f(n + 1) end return f(1)")
            .unwrap_err()
            .render(false)
            .contains("stack overflow")
    );
}

#[test]
fn coroutines_switch_cleanly() {
    // many yields from a wrapped generator, and a ping-pong
    assert_eq!(
        run(
            "local gen = coroutine.wrap(function() for i = 1, 1000 do coroutine.yield(i) end end)\nlocal s = 0 for _ = 1, 1000 do s += gen() end\nlocal co = coroutine.create(function(v) while true do v = coroutine.yield(v * 2) end end)\nlocal v = 1 for _ = 1, 10 do local _, nv = coroutine.resume(co, v) v = nv end\nreturn s, v"
        ),
        "500500 | 1024"
    );
    // nested coroutines (the outer one is `normal` while the inner runs),
    // errors after yields
    assert_eq!(
        run(
            "local outer\nlocal inner = coroutine.create(function() coroutine.yield(coroutine.status(outer)) error('boom') end)\nouter = coroutine.create(function() local _, s = coroutine.resume(inner) coroutine.yield(s) local ok, err = coroutine.resume(inner) return ok, err:match('boom') end)\nlocal _, a = coroutine.resume(outer)\nlocal _, ok, msg = coroutine.resume(outer)\nreturn a, ok, msg, coroutine.status(inner), coroutine.status(outer)"
        ),
        "normal | false | boom | dead | dead"
    );
    // upvalues opened inside a coroutine are closed when it finishes
    assert_eq!(
        run(
            "local get\nlocal co = coroutine.create(function() local x = 1 get = function() return x end coroutine.yield() x = 2 end)\ncoroutine.resume(co) local a = get() coroutine.resume(co)\nreturn a, get(), coroutine.close(coroutine.create(function() end))"
        ),
        "1 | 2 | true"
    );
    // yielding across a pcall boundary inside the coroutine
    assert_eq!(
        run(
            "local co = coroutine.wrap(function() local ok, v = pcall(function() return coroutine.yield(1) + 1 end) coroutine.yield(v) end)\nreturn co(), co(41)"
        ),
        "1 | 42"
    );
    // the memory limit still sees coroutine stacks
    let mut e = Engine::new();
    e.set_memory_limit(Some(200_000));
    assert!(
        e.eval("local cos = {} for i = 1, 100000 do cos[i] = coroutine.create(function() coroutine.yield() end) coroutine.resume(cos[i]) end")
            .is_err()
    );
}

// ─ The Call Depth Cap ────────────────────────────────────────────────────────

/// How deep `depth()` got before it overflowed, through `pcall`.
const DEPTH: &str = "local deepest = 0
local function down(n)
  deepest = n
  return down(n + 1) + 1
end
local ok, err = pcall(down, 1)
return ok, err, deepest";

#[test]
fn a_host_can_lower_the_call_depth_cap() {
    let mut e = Engine::new();
    e.set_max_call_depth(64);
    let got = e.eval(DEPTH).unwrap();
    assert_eq!(got[0], Value::Bool(false));
    assert!(got[1].to_string().contains("stack overflow"), "{}", got[1]);
    // the main chunk and pcall's frame count too
    let Value::Number(deepest) = got[2] else {
        panic!("{:?}", got[2])
    };
    assert!((60.0..64.0).contains(&deepest), "{deepest}");
    // 63 levels deep is still fine
    e.set_max_call_depth(64);
    let src = "local function down(n) if n == 0 then return 0 end return down(n - 1) + 1 end
return down(50)";
    assert_eq!(e.eval(src).unwrap(), [Value::Number(50.0)]);
}

#[test]
fn the_call_depth_cap_stays_within_its_range() {
    let mut e = Engine::new();
    let deepest = |e: &mut Engine| match e.eval(DEPTH).unwrap()[2] {
        Value::Number(n) => n,
        ref other => panic!("{other:?}"),
    };
    let default = deepest(&mut e);
    assert!(default > 9_000.0, "{default}");
    // higher than the default clamps to it
    e.set_max_call_depth(1_000_000);
    assert_eq!(deepest(&mut e), default);
    // and zero gets raised to 2: the chunk and one call under it
    e.set_max_call_depth(0);
    assert_eq!(
        e.eval("local function f() return 1 end return f()")
            .unwrap(),
        [Value::Number(1.0)]
    );
    let err = e
        .eval("local function g() end local function f() g() end f()")
        .unwrap_err();
    assert!(err.to_string().contains("stack overflow"), "{err}");
}

#[test]
fn each_coroutine_gets_its_own_depth() {
    let mut e = Engine::new();
    e.set_max_call_depth(40);
    // 30 deep on the main thread, then 30 more inside a coroutine
    let src = "local function down(n, k) if n == 0 then return k() end return down(n - 1, k) end
return down(30, function()
  local co = coroutine.wrap(function() return down(30, function() return 'ok' end) end)
  return co()
end)";
    assert_eq!(e.eval(src).unwrap(), [Value::String("ok".into())]);
}
