//! Milestone 8: metatables, tail calls, string interpolation, the memory limit,
//! and metatables in the type checker. Runtime tests also run with the
//! collector going at every safe point.

use rhue::{Context, Engine, ErrorKind, Function, ScriptError};

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

// ─ Metatables ────────────────────────────────────────────────────────────────

const VEC: &str = "
    local Vec = {}
    Vec.__index = Vec
    local function new(x, y) return setmetatable({x = x, y = y}, Vec) end
    Vec.new = new
    function Vec.__add(a, b) return new(a.x + b.x, a.y + b.y) end
    function Vec.__sub(a, b) return new(a.x - b.x, a.y - b.y) end
    function Vec.__mul(a, k) return new(a.x * k, a.y * k) end
    function Vec.__div(a, k) return new(a.x / k, a.y / k) end
    function Vec.__idiv(a, k) return new(a.x // k, a.y // k) end
    function Vec.__mod(a, k) return new(a.x % k, a.y % k) end
    function Vec.__pow(a, k) return new(a.x ^ k, a.y ^ k) end
    function Vec.__unm(a) return new(-a.x, -a.y) end
    function Vec.__eq(a, b) return a.x == b.x and a.y == b.y end
    function Vec.__lt(a, b) return a:len2() < b:len2() end
    function Vec.__le(a, b) return a:len2() <= b:len2() end
    function Vec.__len(a) return 2 end
    function Vec.__concat(a, b) return tostring(a) .. '|' .. tostring(b) end
    function Vec.__tostring(a) return '(' .. a.x .. ',' .. a.y .. ')' end
    function Vec.__call(a, k) return a.x * k + a.y end
    function Vec:len2() return self.x * self.x + self.y * self.y end
";

#[test]
fn every_metamethod_runs() {
    each(|e| {
        let src = format!(
            "{VEC}
            local a, b = new(1, 2), new(3, 4)
            return tostring(a + b), tostring(b - a), tostring(a * 3), tostring(b / 2),
                tostring(b // 2), tostring(b % 3), tostring(a ^ 2), tostring(-a),
                a == new(1, 2), a ~= b, a < b, b <= a, a > b, #a, a .. b, a .. 'x', 'x' .. a,
                a(10), `{{a}} and {{b}}`"
        );
        assert_eq!(
            run(e, &src),
            "(4,6) (2,2) (3,6) (1.5,2) (1,2) (0,1) (1,4) (-1,-2) true true true false false 2 \
             (1,2)|(3,4) (1,2)|x x|(1,2) 12 (1,2) and (3,4)"
        );
    });
}

#[test]
fn eq_is_only_asked_about_two_tables() {
    let mut e = Engine::new();
    let src = "
        local calls = 0
        local mt = {__eq = function() calls += 1 return true end}
        local a, b = setmetatable({}, mt), setmetatable({}, mt)
        local r = {a == b, a == a, a == 1, 1 == a, a == 'x', a ~= b}
        return calls, r[1], r[2], r[3], r[4], r[5], r[6]";
    // `a == a` is raw-equal, so `__eq` isn't asked, neither is it for a table
    // against a number or string
    assert_eq!(run(&mut e, src), "2 true true false false false false");
}

#[test]
fn index_and_newindex_tables_functions_and_chains() {
    each(|e| {
        let src = "
            local log = {}
            local base = {inherited = 'yes'}
            local mid = setmetatable({middle = 'm'}, {__index = base})
            local obj = setmetatable({}, {__index = mid})
            local lazy = setmetatable({}, {__index = function(t, k) return k .. '!' end})
            local store = {}
            local guarded = setmetatable({}, {__newindex = function(t, k, v)
                log[#log + 1] = k
                rawset(t, k, v * 10)
            end})
            guarded.a = 1          -- goes to __newindex
            guarded.a = 2          -- exists now: plain assignment
            local redirect = setmetatable({}, {__newindex = store})
            redirect.z = 5
            return obj.inherited, obj.middle, obj.missing, lazy.hi, guarded.a, #log,
                rawget(redirect, 'z'), store.z, rawget(obj, 'inherited')";
        assert_eq!(run(e, src), "yes m nil hi! 2 1 nil 5 nil");
    });
}

#[test]
fn a_nil_field_on_a_table_with_newindex_counts_as_missing() {
    let mut e = Engine::new();
    let src = "
        local hits = 0
        local t = setmetatable({x = 1}, {__newindex = function(t, k, v) hits += 1 rawset(t, k, v) end})
        t.x = nil   -- present: plain
        t.x = 2     -- now nil: __newindex again
        return hits, t.x";
    assert_eq!(run(&mut e, src), "1 2");
}

#[test]
fn metatable_errors_and_protection() {
    let mut e = Engine::new();
    assert_eq!(
        fails(&mut e, "local t = setmetatable({}, {}) return t + 1"),
        "attempt to perform arithmetic (add) on table"
    );
    assert_eq!(
        fails(&mut e, "return setmetatable({}, {}) < 1"),
        "attempt to compare table < number"
    );
    assert_eq!(
        fails(
            &mut e,
            "local t = {} setmetatable(t, {__index = t}) return t.x"
        ),
        "'__index' chain too long; possible loop"
    );
    assert_eq!(
        fails(&mut e, "return setmetatable({}, {__index = 5}).x"),
        "attempt to index number with 'x'"
    );
    assert_eq!(
        fails(
            &mut e,
            "return tostring(setmetatable({}, {__tostring = function() return 1 end}))"
        ),
        "'__tostring' must return a string"
    );
    assert_eq!(
        fails(&mut e, "local c = setmetatable({}, {}) c()"),
        "attempt to call a table value"
    );
    let src = "
        local p = setmetatable({}, {__metatable = 'locked'})
        local ok, err = pcall(setmetatable, p, {})
        return getmetatable(p), ok, err:find('cannot change a protected metatable') ~= nil";
    assert_eq!(run(&mut e, src), "locked false true");
    assert_eq!(
        run(
            &mut e,
            "return getmetatable('x').__index == string, getmetatable({}), getmetatable(1)"
        ),
        "true nil nil"
    );
    assert!(fails(&mut e, "setmetatable(1, {})").contains("invalid argument #1 to 'setmetatable'"),);
}

#[test]
fn metamethods_raise_and_unwind_like_any_call() {
    each(|e| {
        let src = "
            local mt = {__add = function(a, b) error('no adding') end,
                        __index = function(t, k) error('no ' .. k) end}
            local t = setmetatable({}, mt)
            local ok1, e1 = pcall(function() return t + 1 end)
            local ok2, e2 = pcall(function() return t.thing end)
            return ok1, e1:find('no adding') ~= nil, ok2, e2:find('no thing') ~= nil";
        assert_eq!(run(e, src), "false true false true");
    });
}

#[test]
fn call_and_iter_metamethods() {
    each(|e| {
        let src = "
            local counter = setmetatable({n = 0}, {__call = function(self, by)
                self.n += by
                return self.n
            end})
            counter(2) counter(3)
            local ok, v = pcall(counter, 5)
            local range = setmetatable({from = 1, to = 4}, {__iter = function(r)
                local i = r.from - 1
                return function()
                    i += 1
                    if i <= r.to then return i, i * i end
                end
            end})
            local s = 0
            for i, sq in range do s += sq end
            local plain = setmetatable({10, 20}, {})   -- no __iter: raw iteration
            local n = 0
            for _, v in plain do n += v end
            return counter.n, ok, v, s, n";
        assert_eq!(run(e, src), "10 true 10 30 30");
    });
}

#[test]
fn hosts_can_call_callable_tables() {
    let mut e = Engine::new();
    e.eval("callable = setmetatable({}, {__call = function(self, a, b) return a * b end})")
        .unwrap();
    let f = e.global("callable");
    let r: f64 = e.call_value(&f, (6, 7)).unwrap();
    assert_eq!(r, 42.0);
    e.register(
        "apply",
        |cx: &mut Context, f: rhue::Value, x: f64| -> Result<f64, ScriptError> {
            cx.call_value::<f64>(&f, (x, x))
        },
    );
    assert_eq!(run(&mut e, "return apply(callable, 3)"), "9");
}

#[test]
fn classes_with_inheritance() {
    each(|e| {
        let src = "
            local Animal = {}
            Animal.__index = Animal
            function Animal.new(name) return setmetatable({name = name}, Animal) end
            function Animal:speak() return self.name .. ' makes a sound' end
            function Animal:kind() return 'animal' end
            local Dog = setmetatable({}, {__index = Animal})
            Dog.__index = Dog
            function Dog.new(name) return setmetatable(Animal.new(name), Dog) end
            function Dog:speak() return self.name .. ' barks' end
            local d, a = Dog.new('Rex'), Animal.new('Cat')
            local out = {}
            for i = 1, 3 do out[#out + 1] = d:speak() .. '/' .. d:kind() .. '/' .. a:speak() end
            return out[3]";
        assert_eq!(run(e, src), "Rex barks/animal/Cat makes a sound");
    });
}

// ─ Tail Calls ────────────────────────────────────────────────────────────────

#[test]
fn tail_calls_run_in_constant_stack() {
    let mut e = Engine::new();
    let src = "
        local function count(n, acc) if n == 0 then return acc end return count(n - 1, acc + 1) end
        local function even(n) if n == 0 then return true end return odd(n - 1) end
        function odd(n) if n == 0 then return false end return even(n - 1) end
        return count(1000000, 0), even(100001)";
    assert_eq!(run(&mut e, src), "1000000 false");
    // without the tail call, the same depth overflows
    assert_eq!(
        fails(
            &mut e,
            "local function count(n) if n == 0 then return 0 end return (count(n - 1)) end return count(1000000)"
        ),
        "stack overflow"
    );
}

#[test]
fn tail_calls_keep_results_varargs_and_protection() {
    each(|e| {
        let src = "
            local function many(...) return ... end
            local function pass(...) return many(...) end
            local function boom() error('tail boom') end
            local function viaTail() return boom() end
            local ok, err = pcall(viaTail)
            local ok2, a, b = pcall(function() return many(1, 2) end)
            local c = setmetatable({}, {__call = function(self, x) return x + 1 end})
            local function callTable(x) return c(x) end
            local function native(x) return math.max(x, 3, 1) end
            return select('#', pass(1, nil, 3, nil)), ok, err:find('tail boom') ~= nil, ok2, a, b,
                callTable(41), native(2)";
        assert_eq!(run(e, src), "4 false true true 1 2 42 3");
    });
}

#[test]
fn tracebacks_name_the_tail_calls_that_led_there() {
    let mut e = Engine::new();
    let src = "local function c() return nil + 1 end\nlocal function b() return c() end\nlocal function a() return b() end\nlocal function z() return (a()) end\nreturn z()";
    let err = e.eval_named("tail.rhue", src).unwrap_err();
    let r = err.render(false);
    assert!(r.contains("in `c` (tail.rhue:1:"), "{r}");
    assert!(
        r.contains("called from `b` (tail call) (tail.rhue:2:"),
        "{r}"
    );
    assert!(
        r.contains("called from `a` (tail call) (tail.rhue:3:"),
        "{r}"
    );
    assert!(r.contains("called from `z` (tail.rhue:4:"), "{r}");
    // long chains keep the latest two and count the rest
    let src =
        "local function f(n) if n == 0 then return nil + 1 end return f(n - 1) end\nreturn (f(5))";
    let r = e.eval_named("chain.rhue", src).unwrap_err().render(false);
    assert!(r.contains("(after 3 more tail calls)"), "{r}");
}

#[test]
fn the_instruction_limit_stops_infinite_tail_recursion() {
    let mut e = Engine::new();
    e.set_instruction_limit(Some(100_000));
    let err = e
        .eval("local function f() return f() end return f()")
        .unwrap_err();
    assert!(err.to_string().contains("instruction limit"), "{err}");
}

// ─ String Interpolation ──────────────────────────────────────────────────────

#[test]
fn interpolated_strings() {
    each(|e| {
        let src = r#"
            local p = {name = "Aalto", hp = 7}
            local function two() return 1, 2 end
            local named = setmetatable({}, {__tostring = function() return "N" end})
            return `{p.name}: {p.hp}/{10} hp`, `plain`, `\{x\} \`q\``, `{nil}{true}{1.5}`,
                `nested {`in {p.hp * 2}`}`, `{two()}`, `{ {1}  ~= nil }`, `{"}"}`, `{named}!`,
                `-- {p.hp --[[ } ]]} --`"#;
        assert_eq!(
            run(e, src),
            "Aalto: 7/10 hp plain {x} `q` niltrue1.5 nested in 14 1 true } N! -- 7 --"
        );
    });
}

#[test]
fn interpolation_syntax_errors() {
    let mut e = Engine::new();
    for (src, want) in [
        ("return `{}`", "empty `{}` in an interpolated string"),
        (
            "return `{{x}}`",
            "double braces aren't allowed in an interpolated string",
        ),
        ("return `abc", "unterminated string"),
        ("return `{x`", "unterminated string"),
        ("return `a \\q`", "invalid escape sequence"),
        ("return `{1 +}`", "expected an expression"),
    ] {
        let err = e.eval(src).unwrap_err();
        assert_eq!(err.kind, ErrorKind::Syntax, "{src}");
        assert!(err.to_string().contains(want), "{src}: {err}");
    }
}

// ─ The Memory Limit ──────────────────────────────────────────────────────────

#[test]
fn the_memory_limit_stops_runaway_allocation() {
    let mut e = Engine::new();
    e.set_memory_limit(Some(8 << 20));
    for src in [
        "local s = 'x' while true do s = s .. s end",
        "local s = 'x' while true do s = `{s}{s}` end",
        "local t = {} local i = 0 while true do i += 1 t[i] = {i} end",
        "local t = {} for i = 1, 1e7 do t[i] = tostring(i) end",
        "return string.rep('ab', 1e7)",
        "return table.create(1e7, 0)",
        "local t = {} for i = 1, 1e6 do t[i] = 'abcdefgh' end return table.concat(t)",
        // `pcall` can't catch it
        "pcall(function() local s = 'x' while true do s = s .. s end end) return 'escaped'",
    ] {
        let err = e.eval(src).unwrap_err();
        assert_eq!(err.to_string(), "not enough memory", "{src}");
        e.gc();
        assert!(e.memory_used() < 8 << 20, "{src}: {}", e.memory_used());
    }
    // programs that churn through garbage but keep little are fine
    let src = "local keep for i = 1, 200000 do keep = {i, tostring(i)} end return keep[1]";
    assert_eq!(run(&mut e, src), "200000");
    e.set_memory_limit(None);
    assert_eq!(run(&mut e, "return #string.rep('ab', 1e7)"), "20000000");
}

#[test]
#[expect(clippy::cast_possible_truncation, reason = "test values are small")]
fn natives_can_check_the_memory_limit() {
    let mut e = Engine::new();
    e.set_memory_limit(Some(1 << 20));
    e.register(
        "big",
        |cx: &mut Context, n: f64| -> Result<String, ScriptError> {
            cx.reserve(n as usize)?;
            Ok("x".repeat(n as usize))
        },
    );
    assert_eq!(run(&mut e, "return #big(100)"), "100");
    assert_eq!(fails(&mut e, "return big(1e8)"), "not enough memory");
    assert_eq!(
        fails(&mut e, "local ok = pcall(big, 1e8) return ok"),
        "not enough memory"
    );
}

// ─ Types ─────────────────────────────────────────────────────────────────────

#[test]
fn the_class_pattern_type_checks() {
    let mut e = Engine::new();
    let class = "
        local Account = {}
        Account.__index = Account
        function Account.new(balance: number)
            local self = setmetatable({}, Account)
            self.balance = balance
            return self
        end
        function Account:deposit(n: number)
            self.balance += n
        end
        local a = Account.new(10)
    ";
    assert!(
        type_errors(
            &mut e,
            &format!("{class} a:deposit(5) local b: number = a.balance")
        )
        .is_empty()
    );
    assert_eq!(
        type_errors(
            &mut e,
            &format!("{class} a:deposit('5') a:withdraw(1) local s: string = a.balance")
        ),
        [
            "Type '\"5\"' could not be converted into 'number'",
            "Key 'withdraw' not found in table '{ balance: number } with metatable Account'",
            "Type 'number' could not be converted into 'string'",
        ]
    );
    // and it runs
    assert_eq!(
        run(&mut e, &format!("{class} a:deposit(5) return a.balance")),
        "15"
    );
}

#[test]
fn metatables_in_the_checker() {
    let mut e = Engine::new();
    let src = "
        local Vec = {}
        Vec.__index = Vec
        function Vec.new(x: number, y: number) return setmetatable({x = x, y = y}, Vec) end
        function Vec.__add(p, q) return Vec.new(p.x + q.x, p.y + q.y) end
        local v = Vec.new(1, 2) + Vec.new(3, 4)
        local n: number = v.x
        local Base = {}
        Base.__index = Base
        function Base:hello(): string return 'hi' end
        local Derived = setmetatable({}, Base)
        Derived.__index = Derived
        local d = setmetatable({}, Derived)
        local h: string = d:hello()
        local lazy = setmetatable({}, {__index = function(t, k): number return 1 end})
        local one: number = lazy.anything
        local callable = setmetatable({}, {__call = function(self, x: number): string return 'x' end})
        local s: string = callable(1)
        local str = `{n} and {h}`
        local len: number = #str
    ";
    assert_eq!(type_errors(&mut e, src), Vec::<String>::new());
    assert_eq!(
        type_errors(
            &mut e,
            "local v = setmetatable({x = 1}, {}) local s: string = v.x local w = setmetatable(5, {})"
        ),
        [
            "Type 'number' could not be converted into 'string'",
            "setmetatable needs a table, got 'number'",
        ]
    );
    assert_eq!(
        type_errors(
            &mut e,
            "local c = setmetatable({}, {__call = function(self, x: number) end}) c('no')"
        ),
        ["Type '\"no\"' could not be converted into 'number'"]
    );
}

#[test]
fn interpolation_is_a_string_and_checks_its_holes() {
    let mut e = Engine::new();
    assert_eq!(
        type_errors(
            &mut e,
            "local x = 1 local s: number = `{x}` local t = `{nope}`"
        ),
        [
            "Type 'string' could not be converted into 'number'",
            "Unknown global 'nope'",
        ]
    );
}

#[test]
fn a_native_calling_back_through_metamethods() {
    each(|e| {
        e.register(
            "sum_all",
            |cx: &mut Context, f: Function, n: i64| -> Result<f64, ScriptError> {
                let mut total = 0.0;
                for i in 1..=n {
                    total += cx.call::<f64>(&f, (i,))?;
                }
                Ok(total)
            },
        );
        let src = "
            local W = {}
            W.__index = W
            W.__add = function(a, b) return setmetatable({v = a.v + b.v}, W) end
            local acc = setmetatable({v = 0}, W)
            local total = sum_all(function(i)
                acc = acc + setmetatable({v = i}, W)
                return acc.v
            end, 300)
            return total, acc.v";
        assert_eq!(run(e, src), "4545100 45150");
    });
}
