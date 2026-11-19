//! Milestone 26: hot reload. Modules swapped over to new source while scripts
//! keep running: functions run the new code through every handle to them,
//! tables and captured locals keep their state, and a broken edit changes
//! nothing. Everything also runs with the collector going at every safe point.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use rhue::{Context, Engine, ErrorKind, Function, TaskState, Value};

fn both(test: impl Fn(&mut Engine)) {
    for stress in [false, true] {
        let mut e = Engine::new();
        e.set_gc_stress(stress);
        test(&mut e);
    }
}

fn text(values: &[Value]) -> String {
    values
        .iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

fn run(e: &mut Engine, src: &str) -> String {
    match e.eval(src) {
        Ok(v) => text(&v),
        Err(err) => panic!("{}", err.render(false)),
    }
}

const COUNTER_V1: &str = "
    local M = { hits = 0, label = 'v1' }
    local count = 0
    local function bump() count += 1 return count end
    function M.hit() M.hits += 1 return 'one ' .. bump() end
    function M.old_only() return 'still here' end
    return M";

const COUNTER_V2: &str = "
    local M = { hits = 0, label = 'v2', extra = 'new key' }
    local count = 0
    local function bump() count += 10 return count end
    function M.hit() M.hits += 1 return 'two ' .. bump() end
    function M.fresh() return 'brand new' end
    return M";

#[test]
fn functions_swap_and_state_carries_over() {
    both(|e| {
        e.add_module("counter", COUNTER_V1);
        assert_eq!(
            run(e, "c = require('counter') return c.hit(), c.hit()"),
            "one 1 one 2"
        );
        let r = e.reload_module("counter", COUNTER_V2).unwrap();
        assert!(r.was_loaded);
        assert!(r.functions >= 2, "{r:?}");
        // same table, new code, old counts
        assert_eq!(
            run(
                e,
                "return c == require('counter'), c.hit(), c.hits, c.label, c.extra, c.fresh(), c.old_only"
            ),
            "true two 12 3 v1 new key brand new nil"
        );
    });
}

#[test]
fn every_handle_runs_the_new_code() {
    both(|e| {
        e.add_module("greet", "return { hi = function(n) return 'hi ' .. n end }");
        run(
            e,
            "local g = require('greet')
             saved = g.hi
             handlers = { g.hi }",
        );
        let held: Function = e.get("saved").unwrap();
        e.reload_module(
            "greet",
            "return { hi = function(n) return 'hey ' .. n end }",
        )
        .unwrap();
        let from_rust: String = e.call(&held, "rust").unwrap();
        assert_eq!(from_rust, "hey rust");
        assert_eq!(
            run(e, "return saved('global'), handlers[1]('table')"),
            "hey global hey table"
        );
    });
}

#[test]
fn a_module_that_returns_a_function() {
    both(|e| {
        e.add_module("f", "local n = 0 return function() n += 1 return n end");
        run(e, "f = require('f') f() f()");
        e.reload_module("f", "local n = 0 return function() n += 100 return n end")
            .unwrap();
        assert_eq!(run(e, "return f(), f == require('f')"), "102 true");
    });
}

#[test]
fn classes_keep_their_instances() {
    both(|e| {
        e.add_module(
            "acct",
            "local Account = {}
             Account.__index = Account
             function Account.new(b) return setmetatable({ balance = b }, Account) end
             function Account:describe() return 'balance ' .. self.balance end
             return Account",
        );
        run(e, "local A = require('acct') a = A.new(5)");
        e.reload_module(
            "acct",
            "local Account = {}
             Account.__index = Account
             function Account.new(b) return setmetatable({ balance = b }, Account) end
             function Account:describe() return 'has $' .. self.balance end
             function Account:double() self.balance *= 2 return self end
             return Account",
        )
        .unwrap();
        assert_eq!(
            run(
                e,
                "local A = require('acct')
                 return a:describe(), a:double():describe(), getmetatable(a) == A, A.new(1):describe()"
            ),
            "has $5 has $10 true has $1"
        );
    });
}

#[test]
fn nested_tables_merge_too() {
    both(|e| {
        e.add_module(
            "cfg",
            "return { stats = { kills = 0 }, util = { name = function() return 'a' end } }",
        );
        run(
            e,
            "local c = require('cfg') c.stats.kills = 7 stats = c.stats",
        );
        e.reload_module(
            "cfg",
            "return { stats = { kills = 0, deaths = 0 }, util = { name = function() return 'b' end } }",
        )
        .unwrap();
        assert_eq!(
            run(
                e,
                "local c = require('cfg') return c.stats == stats, c.stats.kills, c.stats.deaths, c.util.name()"
            ),
            "true 7 0 b"
        );
    });
}

#[test]
fn broken_edits_change_nothing() {
    both(|e| {
        e.add_module("m", "return { v = function() return 1 end }");
        run(e, "m = require('m')");

        let err = e
            .reload_module("m", "return { v = function( }")
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Syntax);
        let err = e
            .reload_module("m", "--!strict\nlocal x: number = 'no' return {}")
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Type);
        let err = e
            .reload_module("m", "error('boom') return { v = function() return 2 end }")
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::Runtime);
        assert!(err.to_string().contains("boom"), "{err}");

        assert_eq!(run(e, "return m.v(), m == require('m')"), "1 true");
    });
}

#[test]
fn before_the_first_require_it_just_waits() {
    both(|e| {
        e.add_module("later", "return 'old'");
        let r = e.reload_module("later", "return 'new'").unwrap();
        assert!(!r.was_loaded);
        assert_eq!(run(e, "return require('later')"), "new");
    });
}

#[test]
fn reload_asks_the_loader_again() {
    both(|e| {
        let files = Rc::new(RefCell::new(HashMap::from([(
            "lib.shapes".to_string(),
            "return { area = function(w, h) return w * h end }".to_string(),
        )])));
        let reader = files.clone();
        e.set_module_loader(move |name| reader.borrow().get(name).cloned());
        assert_eq!(
            run(e, "shapes = require('lib.shapes') return shapes.area(2, 3)"),
            "6"
        );
        assert_eq!(e.loaded_modules(), ["lib.shapes"]);

        files.borrow_mut().insert(
            "lib.shapes".to_string(),
            "return { area = function(w, h) return w * h / 2 end }".to_string(),
        );
        e.reload("lib.shapes").unwrap();
        assert_eq!(run(e, "return shapes.area(2, 3)"), "3");
        assert!(e.reload("nope").is_err());
    });
}

#[test]
fn checked_code_sees_the_new_type() {
    let mut e = Engine::new();
    e.add_module(
        "t",
        "--!strict\nreturn { n = function(): number return 1 end }",
    );
    run(&mut e, "--!strict\nlocal x: number = require('t').n()");
    e.reload_module(
        "t",
        "--!strict\nreturn { n = function(): string return 'one' end }",
    )
    .unwrap();
    let err = e
        .check("main.rhue", "--!strict\nlocal x: number = require('t').n()")
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Type);
    assert!(
        e.check("main.rhue", "--!strict\nlocal x: string = require('t').n()")
            .is_ok()
    );
}

#[test]
fn a_running_task_finishes_on_the_old_code() {
    both(|e| {
        e.open_tasks();
        e.add_module(
            "npc",
            "local M = {}
             function M.talk()
                 local first = 'old start'
                 task.wait(1)
                 return first .. ', old end'
             end
             return M",
        );
        let t = e
            .spawn(
                "main",
                "local npc = require('npc') return npc.talk(), npc.talk()",
            )
            .unwrap();
        e.reload_module(
            "npc",
            "local M = {}
             function M.talk()
                 task.wait(1)
                 return 'new'
             end
             return M",
        )
        .unwrap();
        for _ in 0..3 {
            let errors = e.step(1.0);
            assert!(errors.is_empty(), "{}", errors[0].render(false));
        }
        // the call that was mid-wait finishes the old way, the next one's new
        assert_eq!(
            t.state(),
            TaskState::Finished(vec![Value::from("old start, old end"), Value::from("new")])
        );
    });
}

#[test]
fn natives_can_reload_from_inside_a_script() {
    both(|e| {
        e.add_module("m", "return { v = function() return 'old' end }");
        e.register(
            "swap",
            |cx: &mut Context, src: String| -> Result<f64, String> {
                let r = cx.reload_module("m", &src).map_err(|e| e.to_string())?;
                Ok(r.functions as f64)
            },
        );
        assert_eq!(
            run(
                e,
                "local m = require('m')
                 local before = m.v()
                 local swapped = swap(\"return { v = function() return 'new' end }\")
                 return before, swapped, m.v()"
            ),
            "old 1 new"
        );
    });
}

#[test]
fn plain_values_get_replaced() {
    both(|e| {
        e.add_module("n", "return 1");
        run(e, "n = require('n')");
        e.reload_module("n", "return 2").unwrap();
        assert_eq!(run(e, "return n, require('n')"), "1 2");
    });
}

#[test]
fn a_module_cant_reload_itself_mid_load() {
    let mut e = Engine::new();
    e.register("swap", |cx: &mut Context| -> Result<(), String> {
        cx.reload_module("self", "return 2")
            .map(|_| ())
            .map_err(|e| e.to_string())
    });
    e.add_module("self", "swap() return 1");
    let err = e.eval("return require('self')").unwrap_err();
    assert!(err.to_string().contains("still loading"), "{err}");
}
