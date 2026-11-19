//! Milestone 10: Lua patterns (`find`, `match`, `gmatch`, `gsub`), the `utf8`
//! library and the sandbox-safe part of `os`, and `require` with host-provided
//! modules (typed, when the requiring chunk is checked). Runtime tests also run
//! with the collector going at every safe point.

use std::cell::RefCell;
use std::rc::Rc;

use rhue::{Engine, ErrorKind};

fn engines() -> Vec<Engine> {
    let plain = Engine::new();
    let mut stressed = Engine::new();
    stressed.set_gc_stress(true);
    vec![plain, stressed]
}

fn each(test: impl Fn(&mut Engine)) {
    for mut e in engines() {
        test(&mut e);
    }
}

fn run(e: &mut Engine, src: &str) -> String {
    e.eval(src)
        .unwrap_or_else(|err| panic!("{}\n{src}", err.render(false)))
        .iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

fn fails(e: &mut Engine, src: &str) -> String {
    e.eval(src).expect_err(src).to_string()
}

fn type_errors(e: &mut Engine, src: &str) -> Vec<String> {
    match e.check("t", src) {
        Ok(()) => Vec::new(),
        Err(err) => {
            assert_eq!(err.kind, ErrorKind::Type, "{err}");
            err.diagnostics.into_iter().map(|d| d.message).collect()
        }
    }
}

// ─ Patterns ──────────────────────────────────────────────────────────────────

#[test]
fn find_and_match() {
    each(|e| {
        let src = r##"
            local out = {}
            local function add(...) out[#out + 1] = table.concat(table.pack(...), ",", 1, select("#", ...)) end
            local function s(...) local t = table.pack(...) for i = 1, t.n do t[i] = tostring(t[i]) end return table.unpack(t, 1, t.n) end
            add(s(("hello world"):find("o w")))
            add(s(("hello world"):find("(o)%s(w)")))
            add(s(("hello"):find("l+")))
            add(s(("hello"):find("xyz")))
            add(s(("a.b"):find(".", 1, true)))
            add(s(("abc"):find("b", -1)))
            add(s(("abc"):find("", 10)))
            add(s(("key = value"):match("(%w+)%s*=%s*(%w+)")))
            add(s(("2026-11-17"):match("(%d+)-(%d+)-(%d+)")))
            add(s(("  trim me  "):match("^%s*(.-)%s*$")))
            add(s(("hello"):match("()ll()")))
            add(s(("f(a(b)c)d"):match("%b()")))
            add(s(("THE (quick) fox"):match("%f[%a]%a+%f[%A]", 5)))
            add(s(("abcabc"):match("(abc)%1")))
            add(s(("x=1"):match("^(%a)=(%d)$")))
            return table.concat(out, " | ")"##;
        assert_eq!(
            run(e, src),
            "5,7 | 5,7,o,w | 3,4 | nil | 2,2 | nil | nil | key,value | 2026,11,17 | trim me \
             | 3,5 | (a(b)c) | quick | abc | x,1"
        );
    });
}

#[test]
fn gmatch_iterates() {
    each(|e| {
        let src = r##"
            local pairs_, words, empties = {}, {}, 0
            for k, v in ("a=1, b=2, c=3"):gmatch("(%w+)=(%w+)") do pairs_[#pairs_ + 1] = k .. v end
            for w in ("The quick  brown fox"):gmatch("%a+") do words[#words + 1] = w end
            for _ in ("abc"):gmatch("x*") do empties += 1 end
            local after = {}
            for w in ("one two three"):gmatch("%a+", 5) do after[#after + 1] = w end
            return table.concat(pairs_, ","), table.concat(words, ","), empties, table.concat(after, ",")"##;
        assert_eq!(run(e, src), "a1,b2,c3 The,quick,brown,fox 4 two,three");
    });
}

#[test]
fn gsub_with_every_kind_of_replacement() {
    each(|e| {
        let src = r##"
            local out = {}
            local function add(s, n) out[#out + 1] = s .. "#" .. n end
            add(("hello world"):gsub("o", "0"))
            add(("hello world"):gsub("(%w+)", "<%1>"))
            add(("hello world"):gsub("%w+", "%0 %0", 1))
            add(("$name is $age"):gsub("%$(%w+)", {name = "Aalto", age = 3}))
            add(("$name $missing"):gsub("%$(%w+)", {name = "x"}))
            add(("abc"):gsub("%w", function(c) return c:upper() .. "." end))
            add(("abc"):gsub("%w", function(c) if c == "b" then return false end return 1 end))
            add(("abc"):gsub("", "-"))
            add(("hello"):gsub("l*", "x"))
            add(("abc"):gsub("^a", "X"))
            add(("50%"):gsub("%%", "%%%%"))
            add(("hello"):gsub("()l", "%1"))
            return table.concat(out, " | ")"##;
        assert_eq!(
            run(e, src),
            "hell0 w0rld#2 | <hello> <world>#2 | hello hello world#1 | Aalto is 3#2 \
             | x $missing#2 | A.B.C.#3 | 1b1#3 | -a-b-c-#4 | xhxexox#4 | Xbc#1 | 50%%#1 | he34o#2"
        );
    });
}

#[test]
fn gsub_callbacks_can_do_anything() {
    each(|e| {
        // a callback that allocates, raises, and is a closure over state
        let src = r##"
            local seen = {}
            local s, n = ("a b c d e"):gsub("%a", function(c)
                seen[#seen + 1] = {c}
                return string.rep(c, #seen)
            end)
            local ok, err = pcall(string.gsub, "abc", "b", function() error("no b", 0) end)
            return s, n, ok, err"##;
        assert_eq!(run(e, src), "a bb ccc dddd eeeee 5 false no b");
    });
}

#[test]
fn pattern_errors() {
    let mut e = Engine::new();
    for (src, want) in [
        ("return ('x'):find('[a')", "malformed pattern (missing ']')"),
        (
            "return ('x'):match('%')",
            "malformed pattern (ends with '%')",
        ),
        ("return ('x'):match('(x')", "unfinished capture"),
        ("return ('x'):match('x)')", "invalid pattern capture"),
        ("return ('x'):gsub('x', '%2')", "invalid capture index %2"),
        (
            "return ('x'):gsub('x', '%z')",
            "invalid use of '%' in replacement string",
        ),
        (
            "return ('x'):gsub('x', true)",
            "invalid argument #3 to 'gsub' (string/function/table expected, got boolean)",
        ),
        (
            "return ('x'):gsub('x', function() return {} end)",
            "invalid replacement value (a table)",
        ),
        (
            "return (string.rep('a', 400)):match(string.rep('a?', 400) .. string.rep('a', 400))",
            "pattern too complex",
        ),
    ] {
        let err = fails(&mut e, src);
        assert!(err.contains(want), "{src}: {err}");
    }
}

#[test]
fn patterns_work_on_bytes() {
    let mut e = Engine::new();
    // `.` is one byte, so `é` (two bytes) takes two, a lone half comes out as
    // U+FFFD, as with `string.sub`. Whole characters survive
    assert_eq!(run(&mut e, "return ('héllo'):gsub('.', '-')"), "------ 6");
    assert_eq!(run(&mut e, "return ('héllo'):match('h(.+)o')"), "éll");
    assert_eq!(run(&mut e, "return ('héllo'):find('l')"), "4 4");
    assert_eq!(run(&mut e, "return #('é'):match('.')"), "3");
}

// ─ utf8 and os ───────────────────────────────────────────────────────────────

#[test]
fn utf8_library() {
    each(|e| {
        let src = r##"
            local cps = {}
            for p, c in utf8.codes("aé😀") do cps[#cps + 1] = p .. ":" .. c end
            return utf8.char(72, 233, 0x1F600), utf8.len("héllo"), select(2, utf8.len("héllo", 3)),
                table.concat({utf8.codepoint("héllo", 1, -1)}, ","), table.concat(cps, " "),
                utf8.offset("aé😀b", 3), utf8.offset("aé😀b", -1), utf8.offset("aé😀b", 0, 3),
                utf8.offset("ab", 5)"##;
        assert_eq!(
            run(e, src),
            "Hé😀 5 3 104,233,108,108,111 1:97 2:233 4:128512 4 8 2 nil"
        );
    });
    let mut e = Engine::new();
    assert!(fails(&mut e, "return utf8.char(-1)").contains("value out of range"));
    assert!(fails(&mut e, "return utf8.codepoint('é', 2)").contains("invalid UTF-8 code"));
    assert!(fails(&mut e, "return utf8.offset('é', 1, 2)").contains("continuation byte"));
}

#[test]
fn os_dates_are_utc() {
    let mut e = Engine::new();
    let src = r##"
        local t = os.time({year = 2026, month = 11, day = 17, hour = 0})
        local d = os.date("*t", t)
        return t, os.date("!%Y-%m-%d %H:%M:%S %A %j %p", t + 6 * 86400 + 13 * 3600),
            d.year, d.month, d.day, d.wday, d.yday, d.isdst,
            os.date("%c", 0), os.date("%x %X %I %y %%", 86400 * 365 + 3661),
            os.time({year = 2026, month = 13, day = 1}) == os.time({year = 2027, month = 1, day = 1}),
            os.time({year = 2024, month = 3, day = 0, hour = 0}) == os.time({year = 2024, month = 2, day = 29, hour = 0}),
            os.difftime(10, 4), type(os.clock()), os.time() > 1.7e9"##;
    assert_eq!(
        run(&mut e, src),
        "1794873600 2026-11-23 13:00:00 Monday 327 PM 2026 11 17 3 321 false \
         Thu Jan  1 00:00:00 1970 01/01/71 01:01:01 01 71 % true true 6 number true"
    );
    assert!(fails(&mut e, "return os.time({year = 2026})").contains("field 'month' missing"));
    assert!(fails(&mut e, "return os.date('%Q')").contains("invalid conversion specifier '%Q'"));
    // nothing in `os` reaches outside the sandbox
    assert_eq!(
        run(
            &mut e,
            "return os.execute, os.getenv, os.remove, os.exit, io"
        ),
        "nil nil nil nil nil"
    );
}

// ─ require ───────────────────────────────────────────────────────────────────

#[test]
fn require_runs_a_module_once_and_caches_it() {
    each(|e| {
        e.add_module(
            "counter",
            "local name = ...
            loads = (loads or 0) + 1
            local M = {name = name, n = 0}
            function M.bump() M.n += 1 return M.n end
            return M",
        );
        e.add_module("flag", "x_loaded = true");
        let src = "
            local a = require('counter')
            local b = require('counter')
            a.bump() b.bump()
            return a == b, a.n, a.name, loads, require('flag'), x_loaded";
        assert_eq!(run(e, src), "true 2 counter 1 true true");
        // still cached in a later chunk, and after a collection
        e.gc();
        assert_eq!(run(e, "return require('counter').n, loads"), "2 1");
    });
}

#[test]
fn the_loader_finds_modules_by_name() {
    let mut e = Engine::new();
    let asked = Rc::new(RefCell::new(Vec::new()));
    let log = asked.clone();
    e.set_module_loader(move |name| {
        log.borrow_mut().push(name.to_string());
        match name {
            "util.math" => Some("return { double = function(x) return x * 2 end }".into()),
            "util.both" => Some("return require('util.math').double(21)".into()),
            _ => None,
        }
    });
    e.add_module("local", "return 'from add_module'");
    let src = "return require('util.both'), require('util.math').double(5), require('local')";
    assert_eq!(run(&mut e, src), "42 10 from add_module");
    assert_eq!(*asked.borrow(), ["util.both", "util.math"]);
    let err = fails(&mut e, "require('missing.thing')");
    assert!(err.contains("module 'missing.thing' not found"), "{err}");
}

#[test]
fn module_errors_and_cycles() {
    let mut e = Engine::new();
    e.add_module("a", "return require('b')");
    e.add_module("b", "return require('c')");
    e.add_module("c", "return require('a')");
    e.add_module("syntax", "return 1 +");
    e.add_module("typed", "--!strict\nlocal n: number = 'no'\nreturn n");
    e.add_module(
        "boom",
        "local function f() error('inside module') end\nf()\nreturn 1",
    );
    let err = fails(&mut e, "require('a')");
    assert!(
        err.contains("cyclic module dependency: a -> b -> c -> a"),
        "{err}"
    );
    let err = fails(&mut e, "require('syntax')");
    assert!(
        err.contains("error loading module 'syntax': syntax:1: expected an expression"),
        "{err}"
    );
    let err = fails(&mut e, "require('typed')");
    assert!(
        err.contains("error loading module 'typed': typed:2: Type '\"no\"'"),
        "{err}"
    );
    // a runtime error keeps its traceback through the module
    let err = e.eval_named("main.rhue", "require('boom')").unwrap_err();
    let r = err.render(false);
    assert!(r.contains("inside module"), "{r}");
    assert!(r.contains("boom:1:"), "{r}");
    assert!(r.contains("main.rhue:1:"), "{r}");
    // a failed module isn't cached: fixing it makes require work
    e.add_module("boom", "return 'fixed'");
    assert_eq!(run(&mut e, "return require('boom')"), "fixed");
}

#[test]
fn required_modules_are_typed() {
    let mut e = Engine::new();
    e.add_module(
        "vec",
        "--!strict
        local Vec = {}
        Vec.__index = Vec
        function Vec.new(x: number, y: number) return setmetatable({x = x, y = y}, Vec) end
        function Vec:len(): number return math.sqrt(self.x * self.x + self.y * self.y) end
        function Vec.__add(a, b) return Vec.new(a.x + b.x, a.y + b.y) end
        return Vec",
    );
    e.add_module(
        "config",
        "--!strict\nreturn { name = 'rhue', debug = false, retries = 3 }",
    );
    e.add_module("untyped", "return { anything = 1 }");
    let src = "--!strict
        local Vec = require('vec')
        local config = require('config')
        local v = Vec.new(3, 4) + Vec.new(1, 1)
        local n: number = v:len()
        local s: string = v.x
        local d: boolean = config.debug
        local r: string = config.retries
        local nope = config.missing
        local u: string = require('untyped').whatever
        Vec.new('3', 4)";
    assert_eq!(
        type_errors(&mut e, src),
        [
            "Type 'number' could not be converted into 'string'",
            "Type 'number' could not be converted into 'string'",
            "Key 'missing' not found in table '{ name: string, debug: boolean, retries: number }'",
            "Type '\"3\"' could not be converted into 'number'",
        ]
    );
    // the module was compiled while checking, and runs once when required
    let ok = "--!strict
        local Vec = require('vec')
        return Vec.new(3, 4):len(), (Vec.new(1, 2) + Vec.new(1, 1)).y";
    assert_eq!(run(&mut e, ok), "5 3");
}

#[test]
fn string_library_types() {
    let mut e = Engine::new();
    let src = "--!strict
        local a, b = string.match('k=v', '(%w+)=(%w+)')
        for w in ('a b'):gmatch('%a+') do local s: string = w end
        local out: string, n: number = ('x'):gsub('x', { x = 'y' })
        local _ = ('x'):gsub('x', function(c: string) return c end)
        local bad = ('x'):gsub('x', true)
        local t: number = os.time({ year = 2026, month = 1, day = 1 })
        local c: string = utf8.char(65)";
    assert_eq!(
        type_errors(&mut e, src),
        [
            "Type 'true' could not be converted into 'string | number | { [string]: any } | ((...string) -> any)'"
        ]
    );
}
