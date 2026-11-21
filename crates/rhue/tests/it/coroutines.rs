//! Milestone 9: coroutines, metamethods running in the dispatch loop (so they
//! can yield), and a typed `self` in class methods. Runtime tests also run with
//! the collector going at every safe point.

use rhue::{Context, Engine, ErrorKind, Function, ScriptError, Thread, Value};

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

// ─ Coroutines ────────────────────────────────────────────────────────────────

#[test]
fn resume_and_yield_pass_values_both_ways() {
    each(|e| {
        let src = "
            local log = {}
            local co = coroutine.create(function(a, b)
                log[#log + 1] = `start {a} {b}`
                local c = coroutine.yield(a + b)
                log[#log + 1] = `got {c}`
                local d, e = coroutine.yield(c * 2)
                return 'done', d + e
            end)
            local s0 = coroutine.status(co)
            local r1 = {coroutine.resume(co, 1, 2)}
            local s1 = coroutine.status(co)
            local r2 = {coroutine.resume(co, 10)}
            local r3 = {coroutine.resume(co, 3, 4)}
            local s3 = coroutine.status(co)
            local r4 = {coroutine.resume(co)}
            return s0, r1[1], r1[2], s1, r2[2], r3[2], r3[3], s3, r4[1], r4[2], table.concat(log, ',')";
        assert_eq!(
            run(e, src),
            "suspended true 3 suspended 20 done 7 dead false cannot resume dead coroutine \
             start 1 2,got 10"
        );
    });
}

#[test]
fn wrap_makes_generators() {
    each(|e| {
        let src = "
            local function range(n, step)
                return coroutine.wrap(function()
                    for i = 1, n, step do coroutine.yield(i, i * i) end
                end)
            end
            local s, sq = 0, 0
            for i, q in range(100, 1) do s += i sq += q end
            local evens = {}
            for i in range(10, 2) do evens[#evens + 1] = i end
            return s, sq, table.concat(evens, ' ')";
        assert_eq!(run(e, src), "5050 338350 1 3 5 7 9");
    });
}

#[test]
fn varargs_tail_yields_and_open_results() {
    each(|e| {
        let src = "
            local echo = coroutine.wrap(function(...)
                local n = select('#', ...)
                while true do
                    -- the second resume's arguments come back out of `yield`, and are returned
                    return coroutine.yield(n, ...)
                end
            end)
            local a = table.pack(echo(1, nil, 3))
            local b = table.pack(echo('x', 'y'))
            local co = coroutine.create(function(...) return select('#', coroutine.yield(...)) end)
            local _, x, y = coroutine.resume(co, 'p', 'q')
            local _, count = coroutine.resume(co, 1, 2, 3, nil)
            return a.n, a[1], a[4], b.n, b[1], b[2], x, y, count";
        assert_eq!(run(e, src), "4 3 3 2 x y p q 4");
    });
}

#[test]
fn errors_kill_the_coroutine_and_come_back_from_resume() {
    each(|e| {
        let src = "
            local bad = coroutine.create(function(x)
                coroutine.yield(x)
                error({code = 42})
            end)
            local _, first = coroutine.resume(bad, 'ok')
            local ok, err = coroutine.resume(bad)
            local msg = coroutine.wrap(function() error('boom') end)
            local ok2, err2 = pcall(msg)
            local ok3, err3 = pcall(msg)
            return first, ok, err.code, coroutine.status(bad), ok2,
                err2:find('boom', 1, true) ~= nil, err3:find('dead', 1, true) ~= nil";
        assert_eq!(run(e, src), "ok false 42 dead false true true");
    });
}

#[test]
fn status_running_and_nesting() {
    each(|e| {
        let src = "
            local main, ismain = coroutine.running()
            local outer
            local seen = {}
            outer = coroutine.create(function()
                local me, mine = coroutine.running()
                seen[#seen + 1] = coroutine.status(outer)
                seen[#seen + 1] = tostring(me == outer) .. tostring(mine)
                local inner = coroutine.create(function()
                    seen[#seen + 1] = coroutine.status(outer)
                    -- the resumer can't be resumed, nor can the running one
                    seen[#seen + 1] = select(2, coroutine.resume(outer))
                    coroutine.yield('from inner')
                end)
                local _, v = coroutine.resume(inner)
                coroutine.yield(v)
            end)
            local _, v = coroutine.resume(outer)
            return type(main), ismain, coroutine.isyieldable(), v, table.concat(seen, '|'),
                select(2, coroutine.resume(coroutine.running()))";
        assert_eq!(
            run(e, src),
            "thread true false from inner running|truefalse|normal|cannot resume non-suspended coroutine \
             cannot resume non-suspended coroutine"
        );
    });
}

#[test]
fn yield_crosses_pcall_but_not_native_callbacks() {
    each(|e| {
        let src = "
            local p = coroutine.wrap(function()
                local ok, v = pcall(function()
                    local got = coroutine.yield('in pcall')
                    error('after ' .. got, 0)
                end)
                coroutine.yield(ok, v)
                return 'end'
            end)
            local a = p()
            local b, c = p('resume')
            local d = p()
            local q = coroutine.create(function()
                table.sort({3, 1, 2}, function(x, y) coroutine.yield() return x < y end)
            end)
            local ok, err = coroutine.resume(q)
            local r = coroutine.create(function() return pcall(coroutine.yield, 1) end)
            local _, y = coroutine.resume(r)
            local _, rok, rv = coroutine.resume(r, 'back')
            return a, b, c, d, ok, err:find('C-call boundary', 1, true) ~= nil, y, rok, rv";
        assert_eq!(
            run(e, src),
            "in pcall false after resume end false true 1 true back"
        );
        assert!(
            fails(e, "coroutine.yield(1)").contains("attempt to yield from outside a coroutine")
        );
    });
}

#[test]
fn upvalues_are_shared_across_threads() {
    each(|e| {
        let src = "
            local gen = coroutine.wrap(function()
                local x = 0
                local function bump() x += 1 return x end
                coroutine.yield(bump)          -- x is still open on the coroutine's stack
                coroutine.yield(x)
                x += 100
                coroutine.yield(x)
            end)
            local bump = gen()
            bump() bump()
            local seen_inside = gen()
            local after = gen()
            local last = bump()                -- the coroutine is still suspended
            local outer = 0
            local co = coroutine.wrap(function() outer += 5 coroutine.yield() outer += 5 end)
            co() local mid = outer co()
            return seen_inside, after, last, mid, outer";
        assert_eq!(run(e, src), "2 102 103 5 10");
    });
}

#[test]
fn close_finishes_a_suspended_coroutine() {
    each(|e| {
        let src = "
            local get
            local co = coroutine.create(function()
                local v = 'kept'
                get = function() return v end
                coroutine.yield()
                v = 'never'
            end)
            coroutine.resume(co)
            local closed = coroutine.close(co)
            local ok, err = pcall(coroutine.close, coroutine.running())
            return closed, coroutine.status(co), get(), ok, err:find('running', 1, true) ~= nil,
                select(2, coroutine.resume(co))";
        assert_eq!(
            run(e, src),
            "true dead kept false true cannot resume dead coroutine"
        );
    });
}

#[test]
fn metamethods_can_yield() {
    each(|e| {
        let src = "
            local mt = {}
            mt.__index = function(t, k)
                local v = coroutine.yield('need ' .. k)
                rawset(t, k, v)
                return v
            end
            mt.__newindex = function(t, k, v) coroutine.yield('set ' .. k) rawset(t, k, v) end
            mt.__add = function(a, b) return coroutine.yield('add') end
            mt.__eq = function(a, b) return coroutine.yield('eq') end
            mt.__lt = function(a, b) return coroutine.yield('lt') end
            mt.__le = function(a, b) return coroutine.yield('le') end
            mt.__len = function() return coroutine.yield('len') end
            mt.__concat = function() return coroutine.yield('cat') end
            mt.__unm = function() return coroutine.yield('unm') end
            mt.__call = function(self, x) return coroutine.yield('call ' .. x) end
            local t, u = setmetatable({}, mt), setmetatable({}, mt)
            local co = coroutine.wrap(function()
                local a = t.x
                local b = t + 1
                local c = if t == u then 'eq' else 'ne'
                local d = t < u
                local d2 = t <= u
                t.y = 5
                local n = #t
                local s = t .. 's'
                local m = -t
                local called = t('arg')
                return a, b, c, d, d2, rawget(t, 'y'), n, s, m, called
            end)
            local asks = {co()}
            for _, answer in {10, 20, false, true, false, 0, 7, 'cc', 'neg', 'res'} do
                asks = {co(answer)}
                if #asks > 1 then break end
            end
            return table.unpack(asks)";
        assert_eq!(run(e, src), "10 20 ne true false 5 7 cc neg res");
    });
}

#[test]
fn coroutines_are_collected_when_unreachable() {
    let mut e = Engine::new();
    e.eval(
        "for i = 1, 2000 do
            local co = coroutine.create(function(x) coroutine.yield(x) end)
            coroutine.resume(co, {i})
        end",
    )
    .unwrap();
    e.gc();
    assert!(e.heap_stats().threads <= 2, "{:?}", e.heap_stats());
    // a closure over a suspended coroutine's local keeps it alive
    e.eval(
        "local co = coroutine.wrap(function()
            local secret = {value = 'alive'}
            peek = function() return secret.value end
            coroutine.yield()
        end)
        co()",
    )
    .unwrap();
    e.gc();
    assert_eq!(run(&mut e, "collectgarbage() return peek()"), "alive");
}

#[test]
fn limits_stop_runaway_coroutines() {
    let mut e = Engine::new();
    e.set_instruction_limit(Some(200_000));
    let err = e
        .eval("local co = coroutine.wrap(function() while true do end end) local ok = pcall(co)")
        .unwrap_err();
    assert!(err.to_string().contains("instruction limit"), "{err}");
    let err = e
        .eval("local co = coroutine.create(function() while true do end end) return coroutine.resume(co)")
        .unwrap_err();
    assert!(err.to_string().contains("instruction limit"), "{err}");
    // the engine is fine afterwards
    assert_eq!(
        run(&mut e, "return coroutine.wrap(function() return 1 end)()"),
        "1"
    );
    e.set_instruction_limit(None);
    e.set_memory_limit(Some(8 << 20));
    let err = e
        .eval("coroutine.wrap(function() local s = 'x' while true do s = s .. s end end)()")
        .unwrap_err();
    assert_eq!(err.to_string(), "not enough memory");
    // deeply nested resumes stop with an error, not a crash
    e.set_memory_limit(None);
    let src = "
        local function nest(n)
            if n == 0 then return 0 end
            return coroutine.wrap(function() return 1 + nest(n - 1) end)()
        end
        local ok, err = pcall(nest, 1000)
        return ok, tostring(err):find('too deeply', 1, true) ~= nil, nest(50)";
    assert_eq!(run(&mut e, src), "false true 50");
}

#[test]
fn hosts_resume_coroutines() {
    let mut e = Engine::new();
    e.eval(
        "function worker(name)
            local got = coroutine.yield('ready ' .. name)
            local more = coroutine.yield(got * 2)
            return 'done', more
        end",
    )
    .unwrap();
    let f: Function = e.get("worker").unwrap();
    let mut cx = e.context();
    let co: Thread = cx.create_thread(&f);
    assert_eq!(cx.thread_status(&co), "suspended");
    let first: String = cx.resume(&co, "w").unwrap();
    let second: f64 = cx.resume(&co, 21).unwrap();
    let (tag, more): (String, bool) = cx.resume(&co, true).unwrap();
    assert_eq!(
        (first.as_str(), second, tag.as_str(), more),
        ("ready w", 42.0, "done", true)
    );
    assert_eq!(cx.thread_status(&co), "dead");
    let err = cx.resume::<()>(&co, ()).unwrap_err();
    assert!(err.to_string().contains("cannot resume dead coroutine"));
    // threads cross the boundary as values
    let t: Thread = e
        .eval_as("return coroutine.create(function() return 7 end)")
        .unwrap();
    let mut cx = e.context();
    let n: f64 = cx.resume(&t, ()).unwrap();
    assert_eq!(n, 7.0);
    e.set_global("t", t);
    assert_eq!(
        run(&mut e, "return type(t), typeof(t), coroutine.status(t)"),
        "thread thread dead"
    );
    // a native can resume what a script gave it
    e.register(
        "drain",
        |cx: &mut Context, co: Thread| -> Result<f64, ScriptError> {
            let mut total = 0.0;
            while cx.thread_status(&co) != "dead" {
                total += cx.resume::<Option<f64>>(&co, ())?.unwrap_or(0.0);
            }
            Ok(total)
        },
    );
    assert_eq!(
        run(
            &mut e,
            "return drain(coroutine.create(function() for i = 1, 4 do coroutine.yield(i) end end))"
        ),
        "10"
    );
    assert!(matches!(e.global("t"), Value::Thread(_)));
}

#[test]
fn tracebacks_inside_coroutines() {
    let mut e = Engine::new();
    let src = "local function deep() return nil + 1 end\nlocal co = coroutine.wrap(function()\n  coroutine.yield()\n  deep()\nend)\nco()\nco()";
    let r = e.eval_named("co.rhue", src).unwrap_err().render(false);
    assert!(r.contains("attempt to perform arithmetic"), "{r}");
    assert!(r.contains("in `deep` (co.rhue:1:"), "{r}");
}

// ─ Metamethods in the Dispatch Loop ──────────────────────────────────────────

#[test]
fn deep_metamethod_recursion_is_a_stack_overflow_not_a_crash() {
    let mut e = Engine::new();
    let src = "
        local mt = {}
        mt.__index = function(t, k) return setmetatable({}, mt)[k] end
        return setmetatable({}, mt).x";
    assert!(fails(&mut e, src).contains("stack overflow"));
    // chains much deeper than the native nesting limit (200) are fine now
    let src = "
        local mt = {}
        mt.__index = function(t, k)
            if k == 0 then return 'bottom' end
            return setmetatable({}, mt)[k - 1]
        end
        return setmetatable({}, mt)[5000]";
    assert_eq!(run(&mut e, src), "bottom");
}

#[test]
fn comparison_metamethods_pick_the_right_branch() {
    each(|e| {
        let src = "
            local mt = {
                __eq = function(a, b) return a.v == b.v end,
                __lt = function(a, b) return a.v < b.v end,
                __le = function(a, b) return a.v <= b.v end,
            }
            local function n(v) return setmetatable({v = v}, mt) end
            local out = {}
            for i = 1, 3 do
                local a, b = n(i), n(2)
                out[#out + 1] = table.concat({
                    tostring(a == b), tostring(a ~= b), tostring(a < b), tostring(a <= b),
                    tostring(a > b), tostring(a >= b),
                    if a < b then 'lt' elseif a == b then 'eq' else 'gt',
                    tostring(not (a < b)),
                }, ',')
            end
            -- loops whose condition is a metamethod
            local i, steps = n(0), 0
            while i < n(5) do i = n(i.v + 1) steps += 1 end
            repeat i = n(i.v - 1) steps += 1 until i <= n(2)
            return table.concat(out, ' / '), steps";
        assert_eq!(
            run(e, src),
            "false,true,true,true,false,false,lt,false / true,false,false,true,false,true,eq,true \
             / false,true,false,false,true,true,gt,true 8"
        );
    });
}

#[test]
fn metamethods_that_are_natives_or_callable_tables() {
    each(|e| {
        let src = "
            local callable = setmetatable({}, {__call = function(self, t, k) return k .. '!' end})
            local a = setmetatable({}, {__index = callable})
            local b = setmetatable({}, {__index = rawget, __len = rawlen})
            rawset(b, 'k', 'raw')
            local c = setmetatable({}, {__add = math.max})
            return a.hey, b.k, #b, c + 5";
        // `math.max(c, 5)` fails: natives as metamethods get the operands as is
        let err = e.eval(src).unwrap_err().to_string();
        assert!(err.contains("invalid argument #1 to 'max'"), "{err}");
        let src = "
            local callable = setmetatable({}, {__call = function(self, t, k) return k .. '!' end})
            -- an `__index` table is indexed, even when it's callable
            local a = setmetatable({}, {__index = callable})
            local b = setmetatable({}, {__index = rawget, __len = rawlen})
            rawset(b, 'k', 'raw')
            -- other metamethods are called, through `__call` if need be
            local c = setmetatable({}, {__add = callable, __concat = callable})
            return a.hey, b.k, b.missing, #b, c + 5, c .. 'x'";
        assert_eq!(run(e, src), "nil raw nil 0 5! x!");
    });
}

#[test]
fn metamethod_frames_show_in_tracebacks() {
    let mut e = Engine::new();
    let src = "local mt = {}\nfunction mt.__add(a, b)\n  return a.v + nil\nend\nlocal t = setmetatable({v = 1}, mt)\nreturn t + 1";
    let r = e.eval_named("mm.rhue", src).unwrap_err().render(false);
    assert!(
        r.contains("in `mt.__add` (mm.rhue:3:") || r.contains("in `__add` (mm.rhue:3:"),
        "{r}"
    );
    assert!(r.contains("called from <main> (mm.rhue:6:"), "{r}");
}

// ─ Typed Self ────────────────────────────────────────────────────────────────

const ACCOUNT: &str = "
    --!strict
    local Account = {}
    Account.__index = Account
    function Account.new(owner: string, balance: number)
        local self = setmetatable({}, Account)
        self.owner = owner
        self.balance = balance
        return self
    end
    function Account:withdraw(n: number): boolean
        if n > self.balance then return false end
        self.balance -= n
        return true
    end
";

#[test]
fn self_in_class_methods_is_an_instance() {
    let mut e = Engine::new();
    let ok = format!(
        "{ACCOUNT}
        function Account:deposit(n: number): number
            self.balance += n
            self.last = n                 -- a field the constructor didn't set
            local ok: boolean = self:withdraw(0)
            return self.balance
        end
        function Account:label(): string
            return self.owner:upper() .. tostring(self.last)
        end"
    );
    assert_eq!(type_errors(&mut e, &ok), Vec::<String>::new());
    let bad = format!(
        "{ACCOUNT}
        function Account:deposit(n: number)
            local s: string = self.balance
            self.owner = 5
            self:withdraw('all')
            local b: string = self:withdraw(1)
        end"
    );
    assert_eq!(
        type_errors(&mut e, &bad),
        [
            "Type 'number' could not be converted into 'string'",
            "Type 'number' could not be converted into 'string'",
            "Type '\"all\"' could not be converted into 'number'",
            "Type 'boolean' could not be converted into 'string'",
        ]
    );
    // before any constructor, `self` is `any`
    let early = "--!strict
        local C = {}
        C.__index = C
        function C:m() local x: string = self.anything return x end
        function C.new() return setmetatable({n = 1}, C) end";
    assert_eq!(type_errors(&mut e, early), Vec::<String>::new());
}

#[test]
fn inherited_instances_keep_their_fields() {
    let mut e = Engine::new();
    let src = "--!strict
        local Animal = {}
        Animal.__index = Animal
        function Animal.new(name: string) return setmetatable({name = name, legs = 4}, Animal) end
        function Animal:describe(): string return `{self.name} ({self.legs})` end
        local Bird = setmetatable({}, Animal)
        Bird.__index = Bird
        function Bird.new(name: string)
            local b = setmetatable(Animal.new(name), Bird)
            b.legs = 2
            return b
        end
        function Bird:fly(): string
            local n: number = self.name     -- error: string
            return self:describe() .. ' flies'
        end";
    assert_eq!(
        type_errors(&mut e, src),
        ["Type 'string' could not be converted into 'number'"]
    );
}

#[test]
fn coroutine_library_types() {
    let mut e = Engine::new();
    let src = "--!strict
        local co = coroutine.create(function(a: number) return a end)
        local ok: boolean, v = coroutine.resume(co, 1)
        local s: string = coroutine.status(co)
        local t: thread = co
        local n: number = co
        local function f(x: thread | number): string
            if type(x) == 'thread' then
                local t2: thread = x
                return coroutine.status(x)
            end
            return tostring(x + 1)
        end
        coroutine.resume(5)";
    assert_eq!(
        type_errors(&mut e, src),
        [
            "Type 'thread' could not be converted into 'number'",
            "Type 'number' could not be converted into 'thread'",
        ]
    );
}
