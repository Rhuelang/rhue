//! Freezing the library tables (2.3), so one environment can't break `math` or
//! `string` for every other one.

use rhue::{Engine, Value};

fn s(text: &str) -> Value {
    Value::String(text.into())
}

// every way a script could write to a library table or the string metatable
const WRITES: &[&str] = &[
    "math.floor = nil",
    "string.format = function() return 'pwned' end",
    "rawset(math, 'floor', nil)",
    "getmetatable('').__index = {}",
    "rawset(getmetatable(''), '__add', function() return 1 end)",
    "table.insert(math, 1)",
    "table.remove(string)",
    "table.sort(math)",
    "table.clear(string)",
    "setmetatable(math, {})",
    "coroutine.wrap = nil",
    "task.wait = nil",
];

const ORIGINALS: &str = "return math.floor(2.5), string.format('%d', 7), ('x'):upper(), \
                         getmetatable('').__index == string, task.wait ~= nil";

fn originals() -> Vec<Value> {
    vec![
        Value::Number(2.0),
        s("7"),
        s("X"),
        Value::Bool(true),
        Value::Bool(true),
    ]
}

#[test]
fn without_the_switch_libraries_leak_between_environments() {
    let mut e = Engine::new();
    let (a, b) = (e.create_environment(), e.create_environment());
    e.eval_in(&a, "math.floor = nil").unwrap();
    assert_eq!(e.eval_in(&b, "return math.floor").unwrap(), [Value::Nil]);
    assert_eq!(e.eval("return math.floor").unwrap(), [Value::Nil]);
}

#[test]
fn frozen_libraries_refuse_every_write() {
    let mut e = Engine::new();
    e.open_tasks();
    e.freeze_libraries();
    let (a, b) = (e.create_environment(), e.create_environment());
    for write in WRITES {
        let err = e
            .eval_in(&a, write)
            .expect_err(&format!("`{write}` should fail"));
        assert!(
            err.to_string().contains("readonly table"),
            "`{write}` failed with something else: {err}"
        );
    }
    assert_eq!(e.eval_in(&b, ORIGINALS).unwrap(), originals());
    assert_eq!(e.eval_in(&a, ORIGINALS).unwrap(), originals());
    assert_eq!(e.eval(ORIGINALS).unwrap(), originals());
}

#[test]
fn every_library_is_frozen() {
    let mut e = Engine::new();
    e.freeze_libraries();
    // opened after the freeze, so it gets frozen on the way in
    e.open_tasks();
    let all = e
        .eval(
            "for _, lib in { table, string, math, bit32, buffer, coroutine, debug, os, utf8, \
             vector, task, getmetatable('') } do \
                 if not table.isfrozen(lib) then return false end \
             end \
             return true",
        )
        .unwrap();
    assert_eq!(all, [Value::Bool(true)]);
}

#[test]
fn environments_and_host_tables_stay_writable() {
    let mut e = Engine::new();
    e.eval("Config = { retries = 3 }").unwrap();
    e.freeze_libraries();
    let a = e.create_environment();
    // a script can still shadow a library with its own global, it just can't
    // change the shared one
    e.eval_in(&a, "math = { floor = function() return 0 end }")
        .unwrap();
    assert_eq!(
        e.eval_in(&a, "return math.floor(2.5)").unwrap(),
        [Value::Number(0.0)]
    );
    assert_eq!(
        e.eval("return math.floor(2.5)").unwrap(),
        [Value::Number(2.0)]
    );
    // tables the host made are its own call
    e.eval("Config.retries = 4").unwrap();
    assert_eq!(
        e.eval("return Config.retries").unwrap(),
        [Value::Number(4.0)]
    );
}

#[test]
fn freezing_twice_is_fine() {
    let mut e = Engine::new();
    e.freeze_libraries();
    e.freeze_libraries();
    assert_eq!(
        e.eval("return table.isfrozen(math)").unwrap(),
        [Value::Bool(true)]
    );
}
