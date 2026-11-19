//! Milestone 18: the standard library catches up with Luau's. `bit32`, buffers,
//! frozen and cloned tables, and `debug.traceback`/`debug.info`.

use rhue::{Engine, Value};

/// Runs `src`, returning what it returns as text (`a, b, c`).
fn run(src: &str) -> String {
    let mut e = Engine::new();
    let vals: Vec<Value> = e
        .eval_as::<rhue::Variadic<Value>>(src)
        .unwrap_or_else(|err| panic!("{src}\n{}", err.render(false)))
        .0;
    vals.iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The message of the error `src` raises.
fn error(src: &str) -> String {
    let mut e = Engine::new();
    match e.eval_as::<Value>(src) {
        Ok(v) => panic!("{src} returned {v}"),
        Err(err) => err.diagnostic().message.clone(),
    }
}

fn check(src: &str) -> Result<(), String> {
    Engine::new()
        .check("t.rhue", &format!("--!strict\n{src}"))
        .map_err(|e| e.render(false))
}

// ─ bit32 ─────────────────────────────────────────────────────────────────────

#[test]
fn bit32_matches_luau() {
    let cases = [
        ("bit32.band(0xFF, 0x0F, 0x3C)", "12"),
        ("bit32.band()", "4294967295"),
        ("bit32.bor(1, 2, 4)", "7"),
        ("bit32.bxor(0xFF, 0x0F)", "240"),
        ("bit32.btest(1, 2)", "false"),
        ("bit32.btest(3, 2)", "true"),
        ("bit32.bnot(0)", "4294967295"),
        // wrapping: negative numbers are their two's complement
        ("bit32.band(-1)", "4294967295"),
        ("bit32.band(2^32 + 5)", "5"),
        ("bit32.band(3.9)", "3"),
        ("bit32.lshift(1, 31)", "2147483648"),
        ("bit32.lshift(1, 32)", "0"),
        ("bit32.lshift(8, -2)", "2"),
        ("bit32.rshift(0x80000000, 31)", "1"),
        ("bit32.rshift(1, -3)", "8"),
        ("bit32.arshift(0x80000000, 4)", "4160749568"),
        ("bit32.arshift(0x80000000, 40)", "4294967295"),
        ("bit32.arshift(0x40000000, 4)", "67108864"),
        ("bit32.lrotate(0x80000001, 1)", "3"),
        ("bit32.rrotate(3, 1)", "2147483649"),
        ("bit32.lrotate(1, -1)", "2147483648"),
        ("bit32.extract(0xABCD, 4, 8)", "188"),
        ("bit32.extract(0x80000000, 31)", "1"),
        ("bit32.replace(0xFFFF, 0, 4, 8)", "61455"),
        ("bit32.replace(0, 0xFF, 28, 4)", "4026531840"),
        ("bit32.countlz(1)", "31"),
        ("bit32.countlz(0)", "32"),
        ("bit32.countrz(8)", "3"),
        ("bit32.byteswap(0x12345678)", "2018915346"),
    ];
    for (expr, want) in cases {
        assert_eq!(run(&format!("return {expr}")), want, "{expr}");
    }
    assert!(error("return bit32.extract(1, 30, 3)").contains("non-existent bits"));
    assert!(error("return bit32.extract(1, -1)").contains("field cannot be negative"));
    assert!(error("return bit32.extract(1, 0, 0)").contains("width must be positive"));
    assert!(error("return bit32.band('x')").contains("number expected, got string"));
    check("local n: number = bit32.band(1, 2)\nlocal b: boolean = bit32.btest(1)").unwrap();
    assert!(check("local s: string = bit32.bor(1)").is_err());
}

// ─ Buffers ───────────────────────────────────────────────────────────────────

#[test]
fn buffers_are_values() {
    assert_eq!(
        run(
            "local b = buffer.create(4)\nreturn type(b), typeof(b), buffer.len(b), tostring(b):sub(1, 7)"
        ),
        "buffer, buffer, 4, buffer:"
    );
    // identity, as table keys, through tables and closures
    assert_eq!(
        run(
            "local a, b = buffer.create(1), buffer.create(1)\nlocal t = { [a] = 'a' }\nreturn a == a, a == b, t[a], t[b]"
        ),
        "true, false, a, nil"
    );
    // zero-filled
    assert_eq!(
        run("local b = buffer.create(3)\nreturn buffer.readu8(b, 0), buffer.readu8(b, 2)"),
        "0, 0"
    );
}

#[test]
fn buffers_read_and_write_numbers() {
    let src = "local b = buffer.create(16)
buffer.writeu8(b, 0, 300)          -- wraps to 44
buffer.writei8(b, 1, -1)
buffer.writei16(b, 2, -2)
buffer.writeu32(b, 4, 0xDEADBEEF)
buffer.writef64(b, 8, math.pi)
return buffer.readu8(b, 0), buffer.readi8(b, 1), buffer.readu8(b, 1), buffer.readi16(b, 2),
  buffer.readu16(b, 2), buffer.readu32(b, 4), buffer.readi32(b, 4), buffer.readf64(b, 8) == math.pi,
  buffer.readu8(b, 4)";
    assert_eq!(
        run(src),
        "44, -1, 255, -2, 65534, 3735928559, -559038737, true, 239",
        "little-endian, wrapped"
    );
    // f32 rounds to single precision
    assert_eq!(
        run(
            "local b = buffer.create(4)\nbuffer.writef32(b, 0, 0.1)\nlocal x = buffer.readf32(b, 0)\nreturn x == 0.1, math.abs(x - 0.1) < 1e-7"
        ),
        "false, true"
    );
    for access in [
        "buffer.readu32(buffer.create(4), 1)",
        "buffer.readu8(buffer.create(4), -1)",
        "buffer.readu8(buffer.create(4), 4)",
        "buffer.writef64(buffer.create(7), 0, 1)",
        "buffer.readu8(buffer.create(0), 0)",
    ] {
        assert_eq!(
            error(&format!("return {access}")),
            "buffer access out of bounds",
            "{access}"
        );
    }
    assert!(error("return buffer.readu8({}, 0)").contains("buffer expected, got table"));
}

#[test]
fn buffers_hold_strings_and_copy() {
    let src = "local s = buffer.fromstring('héllo')
local n = buffer.len(s)
buffer.writestring(s, 0, 'Jx', 1)
local c = buffer.create(8)
buffer.copy(c, 2, s, 3)                -- 'llo'
buffer.copy(c, 0, c, 2, 3)             -- overlapping, within one buffer
buffer.fill(c, 5, 33)                  -- '!!!' to the end
return n, buffer.tostring(s), buffer.readstring(c, 0, 3), buffer.tostring(c)";
    assert_eq!(run(src), "6, Jéllo, llo, llolo!!!");
    // strings are UTF-8, so bytes that aren't don't become one
    let e = error("local b = buffer.fromstring('é')\nreturn buffer.readstring(b, 0, 1)");
    assert!(e.contains("valid UTF-8"), "{e}");
    let e =
        error("local b = buffer.create(1)\nbuffer.writeu8(b, 0, 255)\nreturn buffer.tostring(b)");
    assert!(e.contains("valid UTF-8"), "{e}");
    assert!(
        error("buffer.writestring(buffer.create(8), 0, 'ab', 3)")
            .contains("string length overflow")
    );
    assert_eq!(
        error("buffer.copy(buffer.create(2), 0, buffer.create(4))"),
        "buffer access out of bounds"
    );
}

#[test]
fn buffers_read_and_write_bits() {
    let src = "local b = buffer.create(4)
buffer.writebits(b, 3, 5, 0xFF)        -- only 5 bits fit
buffer.writebits(b, 10, 12, 0xABC)
buffer.writebits(b, 0, 0, 1)           -- no bits: no change
return buffer.readbits(b, 3, 5), buffer.readbits(b, 10, 12), buffer.readu8(b, 0), buffer.readbits(b, 0, 32) == buffer.readu32(b, 0)";
    assert_eq!(run(src), "31, 2748, 248, true");
    assert!(error("return buffer.readbits(buffer.create(1), 4, 5)").contains("out of bounds"));
    assert!(error("return buffer.readbits(buffer.create(8), 0, 33)").contains("[0, 32]"));
}

#[test]
fn buffers_count_against_memory_and_get_collected() {
    let mut e = Engine::new();
    e.set_memory_limit(Some(4 << 20));
    let err = e
        .eval_as::<Value>("return buffer.create(8 * 1024 * 1024)")
        .unwrap_err();
    assert!(
        err.render(false).contains("not enough memory"),
        "{}",
        err.render(false)
    );
    assert!(error("return buffer.create(-1)").contains("size must be between"));

    let mut e = Engine::new();
    e.set_gc_stress(true);
    let out = e
        .eval_as::<f64>(
            "local kept = buffer.create(8)\nbuffer.writeu32(kept, 0, 7)
for i = 1, 200 do local b = buffer.create(64); buffer.fill(b, 0, i) end
local t = { buffer.fromstring('abc') }
collectgarbage()
return buffer.readu32(kept, 0) + #buffer.tostring(t[1])",
        )
        .unwrap();
    assert_eq!(out, 10.0);
    let before = e.heap_stats().buffers;
    e.eval("collectgarbage()").unwrap();
    assert!(e.heap_stats().buffers < before.max(1) + 1);
}

#[test]
fn hosts_make_and_read_buffers() {
    let mut e = Engine::new();
    let b = e.context().create_buffer(&[1, 2, 3, 250]);
    e.set_global("data", b.clone());
    assert_eq!(
        e.eval_as::<f64>("return buffer.readu8(data, 3) + buffer.len(data)")
            .unwrap(),
        254.0
    );
    e.eval("buffer.writeu16(data, 0, 0x0A0B)").unwrap();
    let mut cx = e.context();
    assert_eq!(cx.buffer_bytes(&b), [0x0B, 0x0A, 3, 250]);
    cx.write_buffer(&b, 2, &[9, 9]).unwrap();
    assert!(cx.write_buffer(&b, 3, &[1, 1]).is_err());
    assert_eq!(cx.buffer_bytes(&b), [0x0B, 0x0A, 9, 9]);
    // natives take and return them
    e.register("sum", |cx: &mut rhue::Context<'_>, b: rhue::Buffer| {
        cx.buffer_bytes(&b)
            .iter()
            .map(|&x| f64::from(x))
            .sum::<f64>()
    });
    assert_eq!(
        e.eval_as::<f64>("return sum(buffer.fromstring('ab'))")
            .unwrap(),
        195.0
    );
}

#[test]
fn buffers_are_typed() {
    check(
        "local b: buffer = buffer.create(4)
buffer.writeu8(b, 0, 1)
local n: number = buffer.readu8(b, 0) + buffer.len(b)
local s: string = buffer.readstring(b, 0, 1)
local function size(x: buffer | string): number
  if type(x) == 'buffer' then return buffer.len(x) end
  return #x
end",
    )
    .unwrap();
    for bad in [
        "local s: string = buffer.readu8(buffer.create(1), 0)",
        "buffer.len('x')",
        "local b: buffer = {}",
        "local n: number = buffer.create(1)",
    ] {
        assert!(check(bad).is_err(), "{bad}");
    }
}

// ─ Tables ────────────────────────────────────────────────────────────────────

#[test]
fn frozen_tables_refuse_every_write() {
    let setup = "local t = table.freeze({ a = 1, 10, 20 })\n";
    for write in [
        "t.a = 2",
        "t.new = 1",
        "t[1] = 5",
        "t[3] = 30",
        "t.a = nil",
        "rawset(t, 'a', 3)",
        "table.insert(t, 4)",
        "table.insert(t, 1, 4)",
        "table.remove(t)",
        "table.sort(t)",
        "table.clear(t)",
        "setmetatable(t, {})",
    ] {
        assert_eq!(
            error(&format!("{setup}{write}")),
            "attempt to modify a readonly table",
            "{write}"
        );
    }
    // reads still work, and the table is unchanged
    assert_eq!(
        run(&format!(
            "{setup}pcall(function() t.a = 9 end)\nreturn t.a, #t, t[2], table.isfrozen(t), table.isfrozen({{}})"
        )),
        "1, 2, 20, true, false"
    );
    // a field written through `__newindex` isn't a write to the table
    assert_eq!(
        run(
            "local log = {}\nlocal t = table.freeze(setmetatable({}, { __newindex = function(_, k) table.insert(log, k) end }))\nt.x = 1\nreturn log[1], rawget(t, 'x')"
        ),
        "x, nil"
    );
    assert!(error("table.freeze(table.freeze({}))").contains("already frozen"));
    assert!(
        error("table.freeze(setmetatable({}, { __metatable = 'no' }))")
            .contains("protected metatable")
    );
}

#[test]
fn clones_are_shallow_and_writable() {
    let src = "local inner = {}
local mt = { __index = function() return 'default' end }
local t = table.freeze(setmetatable({ x = 1, inner = inner, 5, 6 }, mt))
local c = table.clone(t)
c.x = 2
c[3] = 7
return t.x, c.x, #t, #c, c.inner == inner, getmetatable(c) == mt, c.missing, table.isfrozen(c)";
    assert_eq!(run(src), "1, 2, 2, 3, true, true, default, false");
    assert!(
        error("table.clone(setmetatable({}, { __metatable = 'no' }))")
            .contains("protected metatable")
    );
    // `table.clear` keeps the metatable
    assert_eq!(
        run(
            "local mt = {}\nlocal t = setmetatable({ 1, 2, a = 3 }, mt)\ntable.clear(t)\nreturn #t, t.a, getmetatable(t) == mt"
        ),
        "0, nil, true"
    );
    check("local t = table.freeze({ x = 1 })\nlocal n: number = t.x\nlocal c = table.clone(t)\nlocal m: number = c.x\nlocal b: boolean = table.isfrozen(c)").unwrap();
}

// ─ Debug ─────────────────────────────────────────────────────────────────────

#[test]
fn debug_info_describes_levels_and_functions() {
    let src = "local function inner(a, b, ...)
  local s, l, n = debug.info(1, 'sln')
  local _, l2, n2 = debug.info(2, 'sln')
  local fn = debug.info(1, 'f')
  return s, l, n, l2, n2, fn == inner, select('#', debug.info(100, 'sln')), debug.info(0, 'n')
end
local function outer() local r = { inner(1, 2) } return r end
return table.unpack(outer())";
    assert_eq!(run(src), "<eval>, 2, inner, 7, outer, true, 0, info");
    let defs = "local function f(a, b, ...) end\nlocal function g(x) end\n";
    for (query, want) in [
        ("f, 'sln'", "<eval>, 1, f"),
        ("f, 'a'", "2, true"),
        ("g, 'la'", "2, 1, false"),
        ("print, 'sna'", "[C], print, 0, true"),
        ("f, 'f'", "function: "),
    ] {
        let got = run(&format!("{defs}return debug.info({query})"));
        assert!(got.starts_with(want), "{query}: {got}");
    }
    // the main chunk has no name
    assert_eq!(run("return debug.info(1, 'n')"), "nil");
    assert!(error("return debug.info(1, 'x')").contains("invalid option 'x'"));
    assert!(error("return debug.info(1, 'll')").contains("duplicate option"));
    assert!(error("return debug.info('x', 's')").contains("function or level expected"));
}

#[test]
fn debug_traceback_lists_the_stack() {
    let src = "local function c() return debug.traceback('here') end
local function b() return (c()) end
local function a() return (b()) end
local t = a()
return t";
    let t = run(src);
    let lines: Vec<&str> = t.lines().collect();
    assert_eq!(lines[0], "here");
    assert_eq!(lines[1], "stack traceback:");
    assert!(lines[2].contains("in `c` (<eval>:1:"), "{t}");
    assert!(lines[3].contains("called from `b` (<eval>:2:"), "{t}");
    assert!(lines[4].contains("called from `a` (<eval>:3:"), "{t}");
    assert!(lines[5].contains("called from <main> (<eval>:4:"), "{t}");
    // a level skips frames, no message gives just the trace
    let t = run("local function f() return debug.traceback(nil, 2) end\nreturn (f())");
    assert!(t.starts_with("stack traceback:\n  in <main>"), "{t}");
    // non-string messages come back as they are
    assert_eq!(run("local t = {}\nreturn debug.traceback(t) == t"), "true");
    // a suspended coroutine's stack
    let src = "local co = coroutine.create(function()
  local function deep() coroutine.yield() end
  deep()
end)
coroutine.resume(co)
return debug.traceback(co, 'co'), debug.info(co, 0, 'n')";
    let t = run(src);
    assert!(t.starts_with("co\nstack traceback:\n  in `deep`"), "{t}");
    assert!(t.ends_with(", deep"), "{t}");
    check("local s: string = debug.traceback('x', 1)\nlocal t: string = debug.traceback((coroutine.running()))").unwrap();
    assert!(check("local n: number = debug.traceback()").is_err());
}
