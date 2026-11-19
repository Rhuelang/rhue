//! Milestone 4: tables, indexing, methods, generic `for`, `pcall`/`error`, the
//! standard library and the garbage collector.
//!
//! Every program runs three ways: folded, unfolded, and with the GC collecting
//! before every instruction (so any object the collector fails to see as live
//! gets freed right away and breaks the test).

use std::cell::RefCell;
use std::rc::Rc;

use rhue::{CompileOptions, Engine, ErrorKind, Value};

fn engines() -> [Engine; 3] {
    let mut stressed = Engine::new();
    stressed.set_gc_stress(true);
    [
        Engine::with_options(CompileOptions::new().fold_constants(true)),
        Engine::with_options(CompileOptions::new().fold_constants(false)),
        stressed,
    ]
}

/// Results rendered as text, so tables and functions compare by shape.
fn render(vals: &[Value]) -> Vec<String> {
    vals.iter()
        .map(|v| match v {
            Value::Table(_) => "<table>".to_string(),
            Value::Function(_) => "<function>".to_string(),
            Value::String(s) => format!("{s:?}"),
            other => other.to_string(),
        })
        .collect()
}

fn run(src: &str) -> Vec<String> {
    let [mut a, mut b, mut c] = engines();
    let x = a
        .eval(src)
        .unwrap_or_else(|e| panic!("{}", e.render(false)));
    let y = b
        .eval(src)
        .unwrap_or_else(|e| panic!("(unfolded) {}", e.render(false)));
    let z = c
        .eval(src)
        .unwrap_or_else(|e| panic!("(GC stress) {}", e.render(false)));
    let (x, y, z) = (render(&x), render(&y), render(&z));
    assert_eq!(x, y, "folded and unfolded disagree");
    assert_eq!(x, z, "GC stress changed the result");
    x
}

fn one(src: &str) -> String {
    run(src).remove(0)
}

fn err(src: &str) -> rhue::Error {
    let [mut a, _, mut c] = engines();
    let e = a.eval(src).expect_err("expected an error");
    let e2 = c.eval(src).expect_err("expected an error (GC stress)");
    assert_eq!(e.to_string(), e2.to_string());
    e
}

#[test]
fn constructors_and_indexing() {
    assert_eq!(
        run("local t = {10, 20, 30} return t[1], t[3], t[4], #t"),
        ["10", "30", "nil", "3"]
    );
    assert_eq!(
        run("local t = {x = 1, ['y z'] = 2, [3] = 'three'} return t.x, t['y z'], t[3]"),
        ["1", "2", "\"three\""]
    );
    assert_eq!(
        run("local t = {} t.a = 1 t['b'] = 2 t[1] = 'one' return t.a + t.b, t[1]"),
        ["3", "\"one\""]
    );
    assert_eq!(
        run("local t = {1, 2, [2] = 'x', 3} return t[2]"),
        ["2"],
        "positional wins over an earlier [2]"
    );
    assert_eq!(one("local t = {{1, 2}, {3, 4}} return t[2][1]"), "3");
    assert_eq!(
        one("local t = {n = {m = {k = 'deep'}}} return t.n.m.k"),
        "\"deep\""
    );
    assert_eq!(one("local t = {} return t.missing"), "nil");
    // number keys are normalised: 1 and 1.0 are the same key, -0 and 0 too
    assert_eq!(
        one("local t = {} t[1.0] = 'a' t[-0] = 'z' return t[1] .. t[0]"),
        "\"az\""
    );
}

#[test]
fn constructors_expand_trailing_calls_and_varargs() {
    assert_eq!(
        run("local function f() return 1, 2, 3 end local t = {f()} return #t"),
        ["3"]
    );
    assert_eq!(
        run("local function f() return 1, 2, 3 end local t = {f(), f()} return #t, t[4]"),
        ["4", "3"]
    );
    assert_eq!(
        run("local function f(...) return {...} end local t = f('a', 'b') return #t, t[2]"),
        ["2", "\"b\""]
    );
    assert_eq!(
        run("local t = {(function() return 1, 2 end)()} return #t"),
        ["2"]
    );
}

#[test]
fn big_constructors_flush_in_batches() {
    let items: Vec<String> = (1..=130).map(|i| i.to_string()).collect();
    let src = format!(
        "local t = {{{}}} return #t, t[1], t[50], t[51], t[130]",
        items.join(", ")
    );
    assert_eq!(run(&src), ["130", "1", "50", "51", "130"]);
}

#[test]
fn length_and_holes() {
    assert_eq!(
        one("local t = {} for i = 1, 100 do t[i] = i end return #t"),
        "100"
    );
    assert_eq!(one("local t = {1, 2, 3} t[3] = nil return #t"), "2");
    assert_eq!(
        one("local t = {} t[3] = 'x' t[2] = 'y' t[1] = 'z' return #t"),
        "3"
    );
}

#[test]
fn assignment_to_fields() {
    assert_eq!(one("local t = {n = 1} t.n += 10 t.n *= 2 return t.n"), "22");
    assert_eq!(one("local t = {'a'} t[1] ..= 'b' return t[1]"), "\"ab\"");
    assert_eq!(
        run("local t = {} t.x, t.y = 1, 2 return t.x, t.y"),
        ["1", "2"]
    );
    // `i, t[i] = i + 1, 20` indexes with the old `i` (Lua manual §3.3.3)
    assert_eq!(
        run("local t = {} local i = 3 i, t[i] = i + 1, 20 return i, t[3], t[4]"),
        ["4", "20", "nil"]
    );
    // `t[f()] += 1` evaluates f once
    assert_eq!(
        run(
            "local calls = 0 local t = {0, 0} local function k() calls += 1 return 2 end t[k()] += 5 return calls, t[2]"
        ),
        ["1", "5"]
    );
    // globals holding tables
    assert_eq!(
        one("config = {} config.debug = true return config.debug"),
        "true"
    );
}

#[test]
fn methods_and_self() {
    assert_eq!(
        one("local Counter = {count = 0}
             function Counter:inc(by) self.count += by or 1 return self end
             Counter:inc():inc(5)
             return Counter.count"),
        "6"
    );
    assert_eq!(
        one("local obj = {name = 'aalto'}
             function obj.greet(self, greeting) return greeting .. ', ' .. self.name end
             return obj:greet('hi')"),
        "\"hi, aalto\""
    );
    assert_eq!(
        one("local a = {b = {}} function a.b.c() return 'nested' end return a.b.c()"),
        "\"nested\""
    );
}

#[test]
fn class_pattern_with_closures() {
    assert_eq!(
        run("local function Stack()
               local items, n = {}, 0
               local s = {}
               function s.push(v) n += 1 items[n] = v end
               function s.pop() local v = items[n] items[n] = nil n -= 1 return v end
               function s.size() return n end
               return s
             end
             local s = Stack()
             s.push('a') s.push('b') s.push('c')
             local top = s.pop()
             return top, s.size()"),
        ["\"c\"", "2"]
    );
}

#[test]
fn string_methods() {
    assert_eq!(one("return ('hello'):upper()"), "\"HELLO\"");
    assert_eq!(
        one("local s = 'Hello' return s:lower() .. s:len()"),
        "\"hello5\""
    );
    assert_eq!(one("local s = 'abcdef' return s:sub(2, 4)"), "\"bcd\"");
    assert_eq!(
        run("return ('abc'):sub(-2), ('abc'):byte(1), string.char(72, 105)"),
        ["\"bc\"", "97", "\"Hi\""]
    );
    assert_eq!(
        run("return ('a,b,,c'):split(',')[3], #('a,b,,c'):split(',')"),
        ["\"\"", "4"]
    );
    assert_eq!(
        run("return ('hello world'):find('o w', 1, true)"),
        ["5", "7"]
    );
    assert_eq!(one("return ('ab'):rep(3, '-')"), "\"ab-ab-ab\"");
    assert_eq!(one("return ('stressed'):reverse()"), "\"desserts\"");
    assert_eq!(
        one("return string.format('%5.2f|%-4d|%03d|%x|%s|%q', 3.14159, 7, 5, 255, nil, 'a\"b')"),
        "\" 3.14|7   |005|ff|nil|\\\"a\\\\\\\"b\\\"\""
    );
    assert_eq!(one("return ('%d items'):format(3)"), "\"3 items\"");
    // (Patterns arrived in milestone 10.)
    assert_eq!(one("return ('xa.b'):find('a.b')"), "2");
    assert_eq!(
        err("return ('x'):find('[a')").to_string(),
        "malformed pattern (missing ']')"
    );
}

#[test]
fn generic_for() {
    assert_eq!(
        one("local s = 0 for i, v in ipairs({10, 20, 30}) do s += i * v end return s"),
        "140"
    );
    assert_eq!(
        one("local n = 0 for k, v in pairs({a = 1, b = 2, 3}) do n += v end return n"),
        "6"
    );
    // luau's generalized iteration: iterate a table directly
    assert_eq!(
        one("local n = 0 for k, v in {x = 5, y = 6} do n += v end return n"),
        "11"
    );
    // insertion order is preserved for the hash part, array part first
    assert_eq!(
        one("local out = '' for k in {1, 2, z = 0, a = 0} do out ..= tostring(k) end return out"),
        "\"12za\""
    );
    // ipairs stops at the first nil
    assert_eq!(
        one("local c = 0 for _ in ipairs({1, 2, nil, 4}) do c += 1 end return c"),
        "2"
    );
    // a custom iterator function
    assert_eq!(
        one("local function range(n) local i = 0 return function() i += 1 if i <= n then return i end end end
             local s = 0 for i in range(5) do s += i end return s"),
        "15"
    );
    // stateless iterator with explicit state and control
    assert_eq!(
        one(
            "local function iter(t, i) i += 1 if t[i] then return i, t[i] end end
             local s = '' for i, v in iter, {'a', 'b'}, 0 do s ..= i .. v end return s"
        ),
        "\"1a2b\""
    );
    assert_eq!(
        err("for x in 5 do end").to_string(),
        "attempt to iterate over a number value"
    );
}

#[test]
fn clearing_fields_while_iterating() {
    assert_eq!(
        one("local t = {a = 1, b = 2, c = 3, d = 4}
             for k in pairs(t) do t[k] = nil end
             return next(t) == nil"),
        "true"
    );
}

#[test]
fn generic_for_captures_fresh_variables() {
    assert_eq!(
        run("local fs = {}
             for i, v in ipairs({'a', 'b', 'c'}) do fs[i] = function() return v end end
             return fs[1](), fs[3]()"),
        ["\"a\"", "\"c\""]
    );
    assert_eq!(
        one(
            "local f for k, v in pairs({x = 1}) do f = function() return k end break end return f()"
        ),
        "\"x\""
    );
}

#[test]
fn table_library() {
    assert_eq!(
        one(
            "local t = {} table.insert(t, 'a') table.insert(t, 'c') table.insert(t, 2, 'b') return table.concat(t, ',')"
        ),
        "\"a,b,c\""
    );
    assert_eq!(
        run("local t = {1, 2, 3} local x = table.remove(t, 1) return x, table.concat(t, ' '), #t"),
        ["1", "\"2 3\"", "2"]
    );
    assert_eq!(
        run("local t = {1, 2, 3} return table.remove(t), #t"),
        ["3", "2"]
    );
    assert_eq!(run("return table.unpack({1, 2, 3})"), ["1", "2", "3"]);
    assert_eq!(
        run("local p = table.pack(1, nil, 3) return p.n, p[3]"),
        ["3", "3"]
    );
    assert_eq!(
        one("local t = {5, 2, 8, 1} table.sort(t) return table.concat(t, ' ')"),
        "\"1 2 5 8\""
    );
    assert_eq!(
        one("local t = {'pear', 'apple', 'fig'} table.sort(t) return table.concat(t, ' ')"),
        "\"apple fig pear\""
    );
    assert_eq!(
        run("return table.find({'a', 'b', 'c'}, 'b'), table.find({'a'}, 'z')"),
        ["2", "nil"]
    );
    assert_eq!(
        one("local t = table.create(3, 0) return table.concat(t, '')"),
        "\"000\""
    );
    assert_eq!(
        one("local t = {1, 2, x = 3} table.clear(t) return next(t) == nil"),
        "true"
    );
    assert_eq!(
        one(
            "local t = {3, 1, 2} table.sort(t, function(a, b) return a > b end) return table.concat(t, ' ')"
        ),
        "\"3 2 1\""
    );
}

#[test]
fn math_library() {
    assert_eq!(
        run(
            "return math.floor(3.7), math.ceil(3.2), math.abs(-4), math.max(1, 9, 3), math.min(4, -2)"
        ),
        ["3", "4", "4", "9", "-2"]
    );
    assert_eq!(
        run(
            "return math.sqrt(16), math.clamp(15, 0, 10), math.sign(-3), math.round(2.5), math.huge"
        ),
        ["4", "10", "-1", "3", "inf"]
    );
    assert_eq!(one("return math.pi > 3.14 and math.pi < 3.15"), "true");
    assert_eq!(
        one(
            "math.randomseed(7) local a = math.random(1, 6) math.randomseed(7) return a == math.random(1, 6)"
        ),
        "true"
    );
    assert_eq!(
        one(
            "for i = 1, 200 do local r = math.random(3, 5) if r < 3 or r > 5 then return false end end return true"
        ),
        "true"
    );
    assert_eq!(one("return math.log(8, 2)"), "3");
}

#[test]
fn pcall_and_error() {
    assert_eq!(
        run("return pcall(function() return 1, 2 end)"),
        ["true", "1", "2"]
    );
    assert_eq!(
        run("local ok, e = pcall(function() error('boom') end) return ok, e"),
        ["false", "\"<eval>:1: boom\""]
    );
    assert_eq!(
        run("local ok, e = pcall(function() error('boom', 0) end) return e"),
        ["\"boom\""],
        "level 0: no position"
    );
    assert_eq!(
        run("local ok, e = pcall(function() local x = nil + 1 end) return ok, e"),
        [
            "false",
            "\"<eval>:1: attempt to perform arithmetic (add) on nil\""
        ]
    );
    // non-string error values pass through untouched
    assert_eq!(
        run("local ok, e = pcall(error, {code = 42}) return e.code"),
        ["42"]
    );
    assert_eq!(run("return pcall(error)"), ["false", "nil"]);
    // pcall a native directly
    assert_eq!(run("return pcall(tonumber, '12')"), ["true", "12"]);
    assert_eq!(
        run("return pcall(nil)"),
        ["false", "\"attempt to call a nil value\""]
    );
    // errors unwind through several frames and close their upvalues
    assert_eq!(
        run("local get
             local ok = pcall(function()
               local captured = 'kept'
               get = function() return captured end
               local function deeper() error('fail') end
               deeper()
             end)
             return ok, get()"),
        ["false", "\"kept\""]
    );
    // execution continues normally after a caught error
    assert_eq!(
        one(
            "local n = 0 for i = 1, 5 do local ok = pcall(function() if i % 2 == 0 then error('even') end end) if ok then n += 1 end end return n"
        ),
        "3"
    );
    // nested pcalls
    assert_eq!(
        run(
            "return pcall(function() local ok, e = pcall(error, 'inner', 0) error(e .. '+outer', 0) end)"
        ),
        ["false", "\"inner+outer\""]
    );
}

#[test]
fn error_levels_point_at_the_caller() {
    let mut e = Engine::new();
    let src = "local function check(x)\n  if type(x) ~= 'number' then error('expected a number', 2) end\nend\ncheck('oops')";
    let err = e.eval_named("lvl.rhue", src).unwrap_err();
    assert_eq!(err.to_string(), "expected a number");
    assert_eq!(
        err.diagnostic().span().text(src),
        "check('oops')",
        "level 2 blames the call site"
    );
}

#[test]
fn uncaught_error_objects() {
    assert_eq!(
        err("error({})").to_string(),
        "(error object is a table value)"
    );
    assert_eq!(err("error('plain message')").to_string(), "plain message");
}

#[test]
fn stack_overflow_can_be_caught_but_the_instruction_limit_cannot() {
    assert_eq!(
        run(
            "local function f() return f() + 1 end local ok, e = pcall(f) return ok, e:find('stack overflow', 1, true) ~= nil"
        ),
        ["false", "true"]
    );
    let mut e = Engine::new();
    e.set_instruction_limit(Some(100_000));
    let err = e
        .eval("return pcall(function() while true do end end)")
        .unwrap_err();
    assert!(err.to_string().contains("instruction limit"));
}

#[test]
fn indexing_errors() {
    assert_eq!(
        err("local t = nil return t.x").to_string(),
        "attempt to index nil with 'x'"
    );
    assert_eq!(
        err("local n = 5 n.x = 1").to_string(),
        "attempt to index number with 'x'"
    );
    assert_eq!(
        err("local t = {} t[nil] = 1").to_string(),
        "table index is nil"
    );
    assert_eq!(
        err("local t = {} t[0/0] = 1").to_string(),
        "table index is NaN"
    );
    assert_eq!(
        err("local t = {} t.x.y = 1").to_string(),
        "attempt to index nil with 'y'"
    );
    assert_eq!(
        err("local t = {} t:nope()").to_string(),
        "attempt to call a nil value"
    );
}

#[test]
fn raw_functions_and_next() {
    assert_eq!(
        run(
            "local t = {} rawset(t, 'k', 'v') return rawget(t, 'k'), rawlen({1, 2}), rawequal(t, t)"
        ),
        ["\"v\"", "2", "true"]
    );
    assert_eq!(run("local t = {7} return next(t)"), ["1", "7"]);
    assert_eq!(
        err("next({}, 'missing')").to_string(),
        "invalid key to 'next'"
    );
}

#[test]
fn tables_compare_by_identity_and_print_as_addresses() {
    assert_eq!(
        run("local a, b = {}, {} return a == b, a == a"),
        ["false", "true"]
    );
    let mut e = Engine::new();
    let out = Rc::new(RefCell::new(Vec::new()));
    let sink = out.clone();
    e.on_print(move |l| sink.borrow_mut().push(l.to_string()));
    e.eval("print({})").unwrap();
    assert!(out.borrow()[0].starts_with("table: 0x"));
}

// ─ Garbage Collection ────────────────────────────────────────────────────────

#[test]
fn garbage_is_collected_and_memory_stays_bounded() {
    let mut e = Engine::new();
    e.eval("for i = 1, 200000 do local t = {i, tostring(i), {nested = i}} end")
        .unwrap();
    let stats = e.heap_stats();
    assert!(
        stats.collections > 0,
        "the loop should have triggered collections"
    );
    e.gc();
    let after = e.heap_stats();
    assert!(after.tables < 50, "garbage tables were freed: {after:?}");
    assert!(after.bytes < 2_000_000, "memory stays bounded: {after:?}");
}

#[test]
fn closures_and_strings_are_collected_too() {
    let mut e = Engine::new();
    e.eval("for i = 1, 50000 do local f = function() return i .. '!' end f() end")
        .unwrap();
    e.gc();
    let s = e.heap_stats();
    assert!(s.functions < 200 && s.strings < 2000, "{s:?}");
}

#[test]
fn reachable_data_survives_collection() {
    let mut e = Engine::new();
    e.eval("keep = {} for i = 1, 1000 do keep[i] = {id = i, name = 'item' .. i} end")
        .unwrap();
    e.gc();
    e.eval("for i = 1, 100000 do local junk = {i} end").unwrap();
    e.gc();
    assert_eq!(
        e.eval("return keep[500].name, #keep").unwrap(),
        [Value::from("item500"), Value::Number(1000.0)]
    );
}

#[test]
fn cycles_are_collected() {
    let mut e = Engine::new();
    e.gc();
    let before = e.heap_stats().tables;
    e.eval("for i = 1, 1000 do local a, b = {}, {} a.other = b b.other = a end")
        .unwrap();
    e.gc();
    assert_eq!(e.heap_stats().tables, before);
}

#[test]
fn collectgarbage_from_scripts() {
    assert_eq!(
        one("local before = collectgarbage('count') return type(before)"),
        "\"number\""
    );
    assert_eq!(
        one("for i = 1, 1000 do local t = {} end collectgarbage() return 1"),
        "1"
    );
    assert_eq!(err("collectgarbage('step')").kind, ErrorKind::Runtime);
}

#[test]
fn values_held_by_the_host_survive_collection() {
    let mut e = Engine::new();
    let t = e.eval("return {answer = 42}").unwrap().remove(0);
    let f = e
        .eval("return function(x) return x * 2 end")
        .unwrap()
        .remove(0);
    for _ in 0..3 {
        e.eval("for i = 1, 50000 do local junk = {i} end").unwrap();
        e.gc();
    }
    e.set_global("t", t.clone());
    e.set_global("double", f.clone());
    assert_eq!(
        e.eval_expr("double(t.answer)").unwrap(),
        Value::Number(84.0)
    );
    // once the host drops them (and the globals are cleared), they can go
    e.set_global("t", Value::Nil);
    e.set_global("double", Value::Nil);
    drop((t, f));
    e.gc();
}

#[test]
#[should_panic(expected = "from one Engine was passed to another")]
fn handles_cannot_cross_engines() {
    let mut a = Engine::new();
    let mut b = Engine::new();
    let t = a.eval("return {}").unwrap().remove(0);
    b.set_global("t", t);
}

#[test]
fn deep_structures_do_not_overflow_the_collector() {
    let mut e = Engine::new();
    e.eval("local list = nil for i = 1, 200000 do list = {next = list} end keep_list = list")
        .unwrap();
    e.gc();
    assert!(e.heap_stats().tables >= 200000);
    e.eval("keep_list = nil").unwrap();
    e.gc();
    assert!(e.heap_stats().tables < 100);
}
