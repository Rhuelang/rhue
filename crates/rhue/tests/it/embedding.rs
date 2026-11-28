//! Milestone 5: the embedding API. Rust functions registered with typed
//! signatures, scripts called from Rust, natives that call back into scripts,
//! and userdata.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use rhue::{
    AnyUserData, Context, Engine, ErrorKind, Function, ScriptError, Table, Ud, UserData, Value,
    Variadic,
};

/// Every engine-level test runs twice, normally and with the collector running
/// before every instruction, which frees anything that isn't rooted.
fn engines() -> [Engine; 2] {
    let mut stressed = Engine::new();
    stressed.set_gc_stress(true);
    [Engine::new(), stressed]
}

fn each(test: impl Fn(&mut Engine)) {
    for mut e in engines() {
        test(&mut e);
    }
}

fn eval_err(e: &mut Engine, src: &str) -> String {
    e.eval(src).expect_err("expected an error").to_string()
}

// ─ Registering Functions ─────────────────────────────────────────────────────

#[test]
fn typed_parameters_and_results() {
    each(|e| {
        e.register("add", |a: f64, b: f64| a + b);
        e.register("repeat_str", |s: String, n: usize| s.repeat(n));
        e.register("negate", |b: bool| !b);
        e.register("nothing", || ());
        e.register("divmod", |a: i64, b: i64| (a / b, a % b));
        e.register("maybe", |x: Option<f64>| x.map(|v| v * 2.0));
        e.register("sum", |xs: Variadic<f64>| xs.iter().sum::<f64>());
        e.register("first_and_rest", |first: String, rest: Variadic<Value>| {
            (first, rest.len())
        });
        e.register("echo", |v: Value| v);

        assert_eq!(e.eval_as::<f64>("return add(1, 2.5)").unwrap(), 3.5);
        assert_eq!(
            e.eval_as::<String>("return repeat_str('ab', 3)").unwrap(),
            "ababab"
        );
        assert!(e.eval_as::<bool>("return negate(false)").unwrap());
        assert_eq!(
            e.eval_as::<f64>("return select('#', nothing())").unwrap(),
            0.0
        );
        assert_eq!(
            e.eval_as::<(i64, i64)>("return divmod(17, 5)").unwrap(),
            (3, 2)
        );
        assert_eq!(e.eval_as::<Option<f64>>("return maybe(nil)").unwrap(), None);
        assert_eq!(e.eval_as::<Option<f64>>("return maybe()").unwrap(), None);
        assert_eq!(e.eval_as::<f64>("return maybe(4)").unwrap(), 8.0);
        assert_eq!(e.eval_as::<f64>("return sum()").unwrap(), 0.0);
        assert_eq!(e.eval_as::<f64>("return sum(1, 2, 3, 4)").unwrap(), 10.0);
        assert_eq!(
            e.eval_as::<(String, f64)>("return first_and_rest('x', 1, nil, {})")
                .unwrap(),
            ("x".to_string(), 3.0)
        );
        assert_eq!(e.eval_as::<String>("return echo('same')").unwrap(), "same");
        // tables and functions pass through unchanged (same identity)
        assert!(
            e.eval_as::<bool>("local t = {} return echo(t) == t")
                .unwrap()
        );
        assert!(e.eval_as::<bool>("return echo(print) == print").unwrap());
    });
}

#[test]
fn plain_fn_items_register_too() {
    fn hypot(a: f64, b: f64) -> f64 {
        (a * a + b * b).sqrt()
    }
    let mut e = Engine::new();
    e.register("hypot", hypot);
    assert_eq!(e.eval_as::<f64>("return hypot(3, 4)").unwrap(), 5.0);
}

#[test]
fn collections_convert_to_and_from_tables() {
    each(|e| {
        e.register("range", |n: usize| (1..=n).collect::<Vec<usize>>());
        e.register("total", |xs: Vec<f64>| xs.iter().sum::<f64>());
        e.register("counts", |words: Vec<String>| {
            let mut m: HashMap<String, usize> = HashMap::new();
            for w in words {
                *m.entry(w).or_default() += 1;
            }
            m
        });
        e.register("keys", |m: HashMap<String, f64>| {
            let mut k: Vec<String> = m.into_keys().collect();
            k.sort();
            k.join(",")
        });

        assert_eq!(
            e.eval_as::<String>("return table.concat(range(5), ' ')")
                .unwrap(),
            "1 2 3 4 5"
        );
        assert_eq!(e.eval_as::<f64>("return total({1, 2, 3.5})").unwrap(), 6.5);
        assert_eq!(
            e.eval_as::<f64>("return counts({'a', 'b', 'a'}).a")
                .unwrap(),
            2.0
        );
        assert_eq!(
            e.eval_as::<String>("return keys({z = 1, a = 2, m = 3})")
                .unwrap(),
            "a,m,z"
        );
        assert_eq!(
            e.eval_as::<Vec<String>>("return {'x', 'y'}").unwrap(),
            ["x", "y"]
        );
    });
}

#[test]
fn argument_errors_name_the_function_and_position() {
    let mut e = Engine::new();
    e.register("add", |a: f64, b: f64| a + b);
    e.register("count", |n: u8| n);
    e.register("total", |xs: Vec<f64>| xs.iter().sum::<f64>());
    e.register("sum", |xs: Variadic<f64>| xs.iter().sum::<f64>());
    assert_eq!(
        eval_err(&mut e, "add(1, 'two')"),
        "invalid argument #2 to 'add' (number expected, got string)"
    );
    assert_eq!(
        eval_err(&mut e, "add(1)"),
        "invalid argument #2 to 'add' (number expected, got nil)"
    );
    assert_eq!(
        eval_err(&mut e, "count(1.5)"),
        "invalid argument #1 to 'count' (integer (u8) expected, got number 1.5)"
    );
    assert_eq!(
        eval_err(&mut e, "count(300)"),
        "invalid argument #1 to 'count' (integer (u8) expected, got number 300)"
    );
    assert_eq!(
        eval_err(&mut e, "total({1, 'x'})"),
        "invalid argument #1 to 'total' (number at [2] expected, got string)"
    );
    assert_eq!(
        eval_err(&mut e, "sum(1, 2, true)"),
        "invalid argument #3 to 'sum' (number expected, got boolean)"
    );
    // scripts can catch them like any error, with a position prefix
    let caught: String = e
        .eval_as("local ok, m = pcall(add, 1, {}) return m")
        .unwrap();
    assert_eq!(
        caught,
        "<eval>:1: invalid argument #2 to 'add' (number expected, got table)"
    );
}

#[test]
fn natives_raise_errors_by_returning_err() {
    let mut e = Engine::new();
    e.register("half", |n: i64| -> Result<i64, String> {
        if n % 2 == 0 {
            Ok(n / 2)
        } else {
            Err(format!("{n} is odd"))
        }
    });
    e.register(
        "fail_with_table",
        |cx: &mut Context| -> Result<(), ScriptError> {
            let t = cx.new_table();
            cx.set(&t, "code", 42)?;
            Err(ScriptError::raise(Value::Table(t)))
        },
    );
    assert_eq!(e.eval_as::<i64>("return half(10)").unwrap(), 5);
    let err = e
        .eval_named("half.rhue", "local x = 1\nreturn half(7)")
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Runtime);
    assert_eq!(err.to_string(), "7 is odd");
    assert_eq!(
        err.diagnostic().span().text("local x = 1\nreturn half(7)"),
        "half(7)"
    );
    assert_eq!(
        e.eval_as::<String>("local ok, m = pcall(half, 3) return m")
            .unwrap(),
        "<eval>:1: 3 is odd"
    );
    // a raised value reaches pcall untouched
    assert_eq!(
        e.eval_as::<f64>("local ok, t = pcall(fail_with_table) return t.code")
            .unwrap(),
        42.0
    );
}

// ─ Calling Scripts from Rust ─────────────────────────────────────────────────

#[test]
fn rust_calls_script_functions() {
    each(|e| {
        e.eval(
            "function greet(who, punct) return 'hi ' .. who .. (punct or '') end
             function stats(...) local n = select('#', ...) return n, ... end
             counter = (function() local n = 0 return function() n += 1 return n end end)()",
        )
        .unwrap();
        let s: String = e.call_global("greet", ("Zae", "!")).unwrap();
        assert_eq!(s, "hi Zae!");
        let s: String = e.call_global("greet", "Aalto").unwrap();
        assert_eq!(s, "hi Aalto");

        let (n, a, b): (f64, String, bool) = e.call_global("stats", ("x", true)).unwrap();
        assert_eq!((n, a.as_str(), b), (2.0, "x", true));
        let all: Variadic<f64> = e.call_global("stats", (1, 2, 3)).unwrap();
        assert_eq!(*all, [3.0, 1.0, 2.0, 3.0]);

        // a function handle keeps working (and keeps its upvalues alive) after
        // the global is gone and the heap is collected
        let counter: Function = e.get("counter").unwrap();
        e.set_global("counter", Value::Nil);
        e.gc();
        for want in 1..=3 {
            let n: f64 = e.call(&counter, ()).unwrap();
            assert_eq!(n, want as f64);
        }
    });
}

#[test]
fn errors_from_called_scripts_come_back_to_rust() {
    let mut e = Engine::new();
    e.eval_named(
        "lib.rhue",
        "function boom(x)\n  if x > 1 then error('too big: ' .. x) end\n  return x\nend",
    )
    .unwrap();
    let err = e.call_global::<f64>("boom", 5).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Runtime);
    assert_eq!(err.to_string(), "too big: 5");
    assert_eq!(err.source_name(), "lib.rhue");

    // results that don't fit the requested types are errors too
    let err = e.call_global::<String>("boom", 1).unwrap_err();
    assert_eq!(
        err.to_string(),
        "invalid result #1 (string expected, got number)"
    );
    let err = e.call_global::<f64>("nope", ()).unwrap_err();
    assert_eq!(err.to_string(), "global 'nope' is not a function");
    // the engine is still usable
    assert_eq!(e.call_global::<f64>("boom", 1).unwrap(), 1.0);
}

#[test]
fn every_host_call_gets_the_full_instruction_budget() {
    let mut e = Engine::new();
    e.set_instruction_limit(Some(10_000));
    e.eval("function spin(n) local s = 0 for i = 1, n do s += i end return s end")
        .unwrap();
    for _ in 0..5 {
        let s: f64 = e.call_global("spin", 1000).unwrap();
        assert_eq!(s, 500_500.0);
    }
    let err = e.call_global::<f64>("spin", 1_000_000).unwrap_err();
    assert!(err.to_string().contains("instruction limit"), "{err}");
}

#[test]
fn typed_globals_and_eval_as() {
    let mut e = Engine::new();
    e.eval("score = 12 name = 'Aalto' flags = {true, false}")
        .unwrap();
    assert_eq!(e.get::<u32>("score").unwrap(), 12);
    assert_eq!(e.get::<String>("name").unwrap(), "Aalto");
    assert_eq!(e.get::<Vec<bool>>("flags").unwrap(), [true, false]);
    assert_eq!(e.get::<Option<f64>>("missing").unwrap(), None);
    assert_eq!(
        e.get::<f64>("name").unwrap_err().to_string(),
        "global 'name': number expected, got string"
    );
    assert_eq!(
        e.eval_as::<(f64, String)>("return 1, 'a'").unwrap(),
        (1.0, "a".to_string())
    );
    assert_eq!(
        e.eval_as::<f64>("return 'x'").unwrap_err().to_string(),
        "invalid result #1 (number expected, got string)"
    );
}

// ─ Natives That Call Back Into Scripts ───────────────────────────────────────

#[test]
fn natives_call_back_into_scripts() {
    each(|e| {
        e.register(
            "map",
            |cx: &mut Context, xs: Vec<Value>, f: Function| -> Result<Vec<Value>, ScriptError> {
                xs.into_iter().map(|x| cx.call(&f, x)).collect()
            },
        );
        e.register(
            "apply_n",
            |cx: &mut Context, f: Function, n: u32, x: f64| {
                let mut x = x;
                for _ in 0..n {
                    x = cx.call(&f, x)?;
                }
                Ok::<_, ScriptError>(x)
            },
        );
        assert_eq!(
            e.eval_as::<String>(
                "return table.concat(map({1, 2, 3}, function(x) return x * x end), ',')"
            )
            .unwrap(),
            "1,4,9"
        );
        assert_eq!(
            e.eval_as::<f64>("return apply_n(function(x) return x * 2 end, 10, 1)")
                .unwrap(),
            1024.0
        );
        // recursion through a native: script -> map -> script -> map ...
        assert_eq!(
            e.eval_as::<f64>(
                "local function depth(n)
                     if n == 0 then return 0 end
                     return map({n}, function(x) return depth(x - 1) + 1 end)[1]
                 end
                 return depth(50)"
            )
            .unwrap(),
            50.0
        );
    });
}

#[test]
fn errors_pass_through_natives_unchanged() {
    let mut e = Engine::new();
    e.register(
        "call_it",
        |cx: &mut Context, f: Function| -> Result<Value, ScriptError> { cx.call(&f, ()) },
    );
    // the error value (a table here) and pcall semantics survive the trip
    // through Rust
    let code: f64 = e
        .eval_as("local ok, err = pcall(call_it, function() error({code = 7}) end) return err.code")
        .unwrap();
    assert_eq!(code, 7.0);
    // uncaught, the traceback runs through the native into the script
    let src = "local function inner()\n  error('deep')\nend\ncall_it(inner)";
    let err = e.eval_named("tb.rhue", src).unwrap_err();
    assert_eq!(err.to_string(), "deep");
    assert_eq!(err.diagnostic().span().text(src), "error('deep')");
    let rendered = err.render(false);
    assert!(rendered.contains("in `inner` (tb.rhue:2"), "{rendered}");
    assert!(
        rendered.contains("called from <main> (tb.rhue:4"),
        "{rendered}"
    );
    // a native can also handle the error itself
    e.register("safely", |cx: &mut Context, f: Function| {
        match cx.call::<Value>(&f, ()) {
            Ok(_) => "ok".to_string(),
            Err(err) => format!("caught: {err}"),
        }
    });
    assert_eq!(
        e.eval_as::<String>("return safely(function() error('x', 0) end)")
            .unwrap(),
        "caught: x"
    );
}

#[test]
fn the_instruction_limit_cannot_be_caught_through_a_native() {
    let mut e = Engine::new();
    e.set_instruction_limit(Some(50_000));
    e.register(
        "safely",
        |cx: &mut Context, f: Function| -> Result<bool, ScriptError> {
            match cx.call::<()>(&f, ()) {
                Ok(()) => Ok(true),
                // swallowing it here would let a script escape the budget, so
                // natives can't: is_catchable() is false and it must propagate
                Err(err) if !err.is_catchable() => Err(err),
                Err(_) => Ok(false),
            }
        },
    );
    let err = e
        .eval("pcall(safely, function() while true do end end) return 'escaped'")
        .unwrap_err();
    assert!(err.to_string().contains("instruction limit"), "{err}");
    // the engine recovers for the next run
    assert_eq!(e.eval_as::<f64>("return 1 + 1").unwrap(), 2.0);
}

#[test]
fn natives_can_return_functions() {
    each(|e| {
        e.register("make_adder", |cx: &mut Context, n: f64| {
            cx.create_function("adder", move |x: f64| x + n)
        });
        assert_eq!(
            e.eval_as::<f64>("local add5 = make_adder(5) return add5(10) + add5(1)")
                .unwrap(),
            21.0
        );
    });
}

#[test]
fn native_nesting_is_bounded() {
    // each script -> native -> script round trip uses Rust stack. A limit turns
    // runaway nesting into an error well before the (2 MB) test thread's stack
    // would overflow
    let mut e = Engine::new();
    e.register(
        "call_it",
        |cx: &mut Context, f: Function, n: f64| -> Result<f64, ScriptError> { cx.call(&f, n) },
    );
    let err = e
        .eval("local function f(n) return call_it(f, n + 1) end return f(0)")
        .unwrap_err();
    assert!(
        err.to_string().contains("natives nested too deeply"),
        "{err}"
    );
    let deep: f64 = e
        .eval_as("local function f(n) if n == 150 then return n end return call_it(f, n + 1) end return f(0)")
        .unwrap();
    assert_eq!(deep, 150.0);
}

// ─ Standard Library Pieces That Call Back ────────────────────────────────────

#[test]
fn sort_with_a_comparator_xpcall_and_nested_pcall() {
    each(|e| {
        assert_eq!(
            e.eval_as::<String>(
                "local t = {}
                 for i = 1, 40 do t[i] = (i * 7) % 40 end
                 table.sort(t, function(a, b) return a > b end)
                 return table.concat(t, ' ', 1, 5)"
            )
            .unwrap(),
            "39 38 37 36 35"
        );
        // stable for equal keys, tables are moved, not copied
        assert_eq!(
            e.eval_as::<String>(
                "local t = {{k = 2, n = 'a'}, {k = 1, n = 'b'}, {k = 2, n = 'c'}, {k = 1, n = 'd'}}
                 table.sort(t, function(x, y) return x.k < y.k end)
                 local out = {} for _, v in t do table.insert(out, v.n) end
                 return table.concat(out)"
            )
            .unwrap(),
            "bdac"
        );
        // an inconsistent comparator mixes up the order but loses nothing
        assert_eq!(
            e.eval_as::<f64>(
                "local t = {} for i = 1, 100 do t[i] = i end
                 table.sort(t, function() return true end)
                 local s = 0 for _, v in t do s += v end return s"
            )
            .unwrap(),
            5050.0
        );
        assert_eq!(
            e.eval_as::<String>(
                "local ok, m = pcall(table.sort, {3, 2, 1}, function(a, b) error('cmp', 0) end) return m"
            )
            .unwrap(),
            "cmp"
        );
        assert_eq!(
            e.eval_as::<(bool, String)>(
                "return xpcall(function() error('bad', 0) end, function(m) return 'handled ' .. m end)"
            )
            .unwrap(),
            (false, "handled bad".to_string())
        );
        assert_eq!(
            e.eval_as::<(bool, f64)>("return xpcall(function(a, b) return a + b end, print, 2, 3)")
                .unwrap(),
            (true, 5.0)
        );
        assert_eq!(
            e.eval_as::<(bool, bool, String)>("return pcall(pcall, error, 'x')")
                .unwrap(),
            (true, false, "x".to_string())
        );
    });
}

// ─ Userdata ──────────────────────────────────────────────────────────────────

#[derive(UserData, Clone, Debug, PartialEq)]
struct Player {
    name: String,
    hp: f64,
    #[rhue(readonly)]
    id: u32,
    #[rhue(skip)]
    secret: String,
    #[rhue(name = "isAlive")]
    alive: bool,
}

impl Player {
    fn new(name: &str) -> Self {
        Player {
            name: name.into(),
            hp: 100.0,
            id: 1,
            secret: "hidden".into(),
            alive: true,
        }
    }
}

fn player_engine() -> [Engine; 2] {
    engines().map(|mut e| {
        e.register_type::<Player>()
            .method("heal", |p: &mut Player, amount: f64| {
                p.hp += amount;
                p.hp
            })
            .method("damage", |p: &mut Player, amount: f64| {
                p.hp = (p.hp - amount).max(0.0);
                p.alive = p.hp > 0.0;
            })
            .method("describe", |p: &mut Player| {
                format!("{} ({} hp)", p.name, p.hp)
            })
            .function("new", |name: String| Player::new(&name));
        e
    })
}

#[test]
fn derived_fields_and_methods() {
    for mut e in player_engine() {
        e.set_global("p", Player::new("Aalto"));
        assert_eq!(e.eval_as::<String>("return p.name").unwrap(), "Aalto");
        assert_eq!(e.eval_as::<f64>("return p:heal(25)").unwrap(), 125.0);
        e.eval("p.name ..= '!' p.hp -= 5").unwrap();
        assert_eq!(
            e.eval_as::<String>("return p:describe()").unwrap(),
            "Aalto! (120 hp)"
        );
        assert!(e.eval_as::<bool>("return p.isAlive").unwrap());
        e.eval("p:damage(500)").unwrap();
        assert!(!e.eval_as::<bool>("return p.isAlive").unwrap());
        assert_eq!(e.eval_as::<f64>("return p.id").unwrap(), 1.0);
        assert_eq!(
            e.eval_as::<String>("return type(p) .. ' ' .. typeof(p)")
                .unwrap(),
            "userdata Player"
        );
        assert!(
            e.eval_as::<String>("return tostring(p)")
                .unwrap()
                .starts_with("Player: 0x")
        );

        // static functions live in a global table named after the type
        assert_eq!(
            e.eval_as::<String>("return Player.new('Zae').name")
                .unwrap(),
            "Zae"
        );
        // Rust reads back a copy...
        let copy: Player = e.get("p").unwrap();
        assert_eq!(copy.name, "Aalto!");
        assert_eq!(copy.secret, "hidden");
        // ...or a handle to the object itself
        let handle: Ud<Player> = e.get("p").unwrap();
        handle.borrow_mut().hp = 42.0;
        assert_eq!(e.eval_as::<f64>("return p.hp").unwrap(), 42.0);
    }
}

#[test]
fn userdata_member_errors() {
    let [mut e, _] = player_engine();
    e.set_global("p", Player::new("Aalto"));
    assert_eq!(
        eval_err(&mut e, "return p.secret"),
        "'secret' is not a valid member of Player"
    );
    assert_eq!(
        eval_err(&mut e, "p.id = 5"),
        "'id' is a read-only member of Player"
    );
    assert_eq!(
        eval_err(&mut e, "p.nope = 5"),
        "'nope' is not a valid member of Player"
    );
    assert_eq!(
        eval_err(&mut e, "p.heal = 5"),
        "'heal' is a method, and can't be assigned, of Player"
    );
    assert_eq!(
        eval_err(&mut e, "p.hp = 'lots'"),
        "invalid value for 'hp' of Player (number expected, got string)"
    );
    assert_eq!(
        eval_err(&mut e, "return p[1]"),
        "attempt to index Player with 1"
    );
    // `.` instead of `:` passes the wrong receiver
    assert_eq!(
        eval_err(&mut e, "p.heal(10)"),
        "invalid argument #1 to 'heal' (Player expected, got number)"
    );
    assert_eq!(
        eval_err(&mut e, "p:heal('x')"),
        "invalid argument #2 to 'heal' (number expected, got string)"
    );
    assert_eq!(
        eval_err(&mut e, "return p + 1"),
        "attempt to perform arithmetic (add) on userdata"
    );
}

#[test]
fn userdata_identity_and_equality() {
    for mut e in player_engine() {
        e.register("same", |a: Ud<Player>, b: Ud<Player>| a == b);
        e.register("passthrough", |p: Ud<Player>| p);
        let p = e.context().create_userdata(Player::new("A"));
        e.set_global("p", p.clone());
        e.set_global("q", Player::new("A"));
        assert!(e.eval_as::<bool>("return p == p and p ~= q").unwrap());
        assert!(e.eval_as::<bool>("return same(p, passthrough(p))").unwrap());
        assert!(
            e.eval_as::<bool>("local t = {[p] = 'found'} return t[passthrough(p)] == 'found'")
                .unwrap()
        );
        let back: Ud<Player> = e.get("p").unwrap();
        assert_eq!(back, p);
        let other: Ud<Player> = e.get("q").unwrap();
        assert_ne!(other, p);
        // anyUserData downcasts only to the right type
        let any: AnyUserData = e.get("p").unwrap();
        assert_eq!(any.type_name(), "Player");
        assert!(any.is::<Player>());
        assert!(any.downcast::<Vec2>().is_none());
    }
}

#[derive(UserData, Clone, Copy, Debug, PartialEq)]
#[rhue(name = "Vector2")]
struct Vec2 {
    x: f64,
    y: f64,
}

#[test]
fn userdata_as_parameters_and_results() {
    each(|e| {
        e.register_type::<Vec2>()
            .function("new", |x: f64, y: f64| Vec2 { x, y })
            .method("add", |a: &mut Vec2, b: Vec2| Vec2 {
                x: a.x + b.x,
                y: a.y + b.y,
            })
            .method("length", |v: &mut Vec2| v.x.hypot(v.y));
        e.register("dot", |a: Vec2, b: Vec2| a.x * b.x + a.y * b.y);
        e.register("player", |name: String| Player::new(&name));
        assert_eq!(
            e.eval_as::<f64>(
                "local a, b = Vector2.new(1, 2), Vector2.new(3, 4)
                 return a:add(b):length(), dot(a, b)"
            )
            .unwrap(),
            (4f64 * 4.0 + 6.0 * 6.0).sqrt()
        );
        assert_eq!(
            e.eval_as::<f64>("return dot(Vector2.new(1, 0), Vector2.new(5, 9))")
                .unwrap(),
            5.0
        );
        assert_eq!(
            eval_err(e, "dot(Vector2.new(1, 0), player('x'))"),
            "invalid argument #2 to 'dot' (Vector2 expected, got Player)"
        );
        let v: Vec2 = e
            .eval_as("return Vector2.new(1, 2):add(Vector2.new(1, 1))")
            .unwrap();
        assert_eq!(v, Vec2 { x: 2.0, y: 3.0 });
    });
}

#[test]
fn methods_can_call_back_and_reentry_is_an_error() {
    let [mut e, _] = player_engine();
    e.register_type::<Player>().method(
        "each_hp",
        |cx: &mut Context, p: &mut Player, f: Function| -> Result<(), ScriptError> {
            cx.call::<()>(&f, p.hp)
        },
    );
    e.set_global("p", Player::new("A"));
    assert_eq!(
        e.eval_as::<f64>("local seen p:each_hp(function(hp) seen = hp end) return seen")
            .unwrap(),
        100.0
    );
    // while a method runs, the object is mutably borrowed: touching it from the
    // callback is an error, not a panic
    let err = eval_err(&mut e, "p:each_hp(function() return p.hp end)");
    assert_eq!(
        err,
        "Player is already in use (a method on it is still running)"
    );
}

#[test]
fn shared_methods_let_callbacks_read_the_same_object() {
    let [mut e, _] = player_engine();
    e.register_type::<Player>()
        .method(
            "watch",
            |cx: &mut Context, p: &Player, f: Function| -> Result<f64, ScriptError> {
                cx.call::<f64>(&f, p.hp)
            },
        )
        .method("hp_plus", |p: &Player, n: f64| p.hp + n);
    e.set_global("p", Player::new("A"));
    // reading it from the callback is fine now, getters and other &T methods
    assert_eq!(
        e.eval_as::<f64>("return p:watch(function(hp) return p.hp + p:hp_plus(1) + hp end)")
            .unwrap(),
        301.0
    );
    // changing it still isn't: a &mut method needs the object to itself
    let err = eval_err(&mut e, "p:watch(function() p:heal(1) return 0 end)");
    assert_eq!(
        err,
        "Player is already in use (a method on it is still running)"
    );
    // and the checker sees a &T method like any other
    assert_eq!(e.global_type("p").as_deref(), Some("Player"));
    e.check("t.rhue", "--!strict\nlocal n: number = p:hp_plus(2)")
        .unwrap();
}

/// Counts drops, to see when the collector frees userdata.
struct Tracked(Rc<Cell<u32>>);

impl Drop for Tracked {
    fn drop(&mut self) {
        self.0.set(self.0.get() + 1);
    }
}

impl UserData for Tracked {
    const NAME: &'static str = "Tracked";
}

#[test]
fn unreachable_userdata_is_collected_and_dropped() {
    let drops = Rc::new(Cell::new(0));
    let mut e = Engine::new();
    let d = drops.clone();
    e.register("make", move || Tracked(d.clone()));
    e.eval("for i = 1, 100 do local t = make() end keep = make()")
        .unwrap();
    e.gc();
    assert_eq!(drops.get(), 100);
    assert_eq!(e.heap_stats().userdata, 1);
    // a Rust handle keeps it alive after the script lets go
    let held: Ud<Tracked> = e.get("keep").unwrap();
    e.eval("keep = nil").unwrap();
    e.gc();
    assert_eq!(drops.get(), 100);
    drop(held);
    e.gc();
    assert_eq!(drops.get(), 101);
    assert_eq!(e.heap_stats().userdata, 0);
}

#[test]
fn userdata_without_fields_still_has_a_type() {
    let mut e = Engine::new();
    let counter = Rc::new(Cell::new(0));
    e.set_global("t", Tracked(counter));
    assert_eq!(e.eval_as::<String>("return typeof(t)").unwrap(), "Tracked");
    assert_eq!(
        eval_err(&mut e, "return t.x"),
        "'x' is not a valid member of Tracked"
    );
}

#[test]
#[should_panic(expected = "from one Engine was passed to another")]
fn userdata_from_another_engine_panics() {
    let mut a = Engine::new();
    let mut b = Engine::new();
    a.set_global("p", Player::new("A"));
    let p: Ud<Player> = a.get("p").unwrap();
    b.set_global("p", p);
}

// ─ Tables from Rust ──────────────────────────────────────────────────────────

#[test]
fn reading_and_writing_tables_from_rust() {
    each(|e| {
        let t: Table = e
            .eval_as("return {1, 2, 3, name = 'cfg', nested = {deep = true}}")
            .unwrap();
        let mut cx = e.context();
        assert_eq!(cx.len(&t), 3);
        assert_eq!(cx.get::<String>(&t, "name").unwrap(), "cfg");
        assert_eq!(cx.get::<f64>(&t, 2).unwrap(), 2.0);
        let nested: Table = cx.get(&t, "nested").unwrap();
        assert!(cx.get::<bool>(&nested, "deep").unwrap());
        assert_eq!(
            cx.get::<f64>(&t, "name").unwrap_err().to_string(),
            "field 'name': number expected, got string"
        );
        cx.set(&t, "added", vec![10, 20]).unwrap();
        cx.set(&t, 1, Value::Nil).unwrap();
        assert!(cx.set(&t, Value::Nil, 1).is_err());
        assert_eq!(cx.pairs(&t).len(), 5);
        let fresh = cx.new_table();
        cx.set(&fresh, "x", 1.5).unwrap();
        cx.set_global("fresh", fresh);
        e.set_global("t", t);
        assert_eq!(
            e.eval_as::<f64>("return t.added[2] + fresh.x").unwrap(),
            21.5
        );
        assert!(e.eval_as::<bool>("return t[1] == nil").unwrap());
    });
}

// ─ Rooting Across Callbacks ((Most Useful Under GC Stress)) ────────────────────

#[test]
fn sort_keeps_items_alive_when_the_comparator_empties_the_table() {
    each(|e| {
        let names: String = e
            .eval_as(
                "local t = {{n = 'c'}, {n = 'a'}, {n = 'b'}}
                 table.sort(t, function(x, y)
                     table.clear(t)              -- the only other reference is gone
                     local junk = {} for i = 1, 50 do junk[i] = {i} end
                     return x.n < y.n
                 end)
                 local out = {} for _, v in t do table.insert(out, v.n) end
                 return table.concat(out)",
            )
            .unwrap();
        assert_eq!(names, "abc");
    });
}

#[test]
fn a_frame_waiting_on_a_native_keeps_its_varargs_and_traceback() {
    each(|e| {
        e.register(
            "call_it",
            |cx: &mut Context, f: Function| -> Result<Value, ScriptError> { cx.call(&f, ()) },
        );
        let s: String = e
            .eval_as(
                "local function f(...)
                     call_it(function() local junk = {} for i = 1, 50 do junk[i] = {tostring(i)} end end)
                     return table.concat({...}, ',')
                 end
                 return f('a' .. 1, 'b' .. 2, 'c' .. 3)",
            )
            .unwrap();
        assert_eq!(s, "a1,b2,c3");
    });
}
