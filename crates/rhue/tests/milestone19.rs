//! Milestone 19: embedding II. Natives that suspend scripts, tasks the host
//! drives, the `task` library, environments, and serde.

use std::cell::RefCell;
use std::rc::Rc;

use rhue::{Engine, ScriptError, TaskState, Value};

fn s(text: &str) -> Value {
    Value::String(text.into())
}

/// An engine whose `print` lines are collected.
fn engine() -> (Engine, Rc<RefCell<Vec<String>>>) {
    let mut e = Engine::new();
    let out: Rc<RefCell<Vec<String>>> = Rc::default();
    let o = out.clone();
    e.on_print(move |line| o.borrow_mut().push(line.to_string()));
    (e, out)
}

fn take(out: &Rc<RefCell<Vec<String>>>) -> Vec<String> {
    std::mem::take(&mut *out.borrow_mut())
}

// --- natives that yield ----------------------------------------------------------------

#[test]
fn natives_suspend_scripts() {
    let (mut e, out) = engine();
    // A native that asks the host something: the script stops there, and
    // the host's answer becomes the native's result.
    e.register("ask", |question: String| -> Result<String, ScriptError> {
        Err(ScriptError::yield_with(vec![Value::String(
            question.into(),
        )]))
    });
    let mut t = e
        .spawn(
            "dialog.rhue",
            "local name = ask('name?')\nprint('hi ' .. name)\nlocal n = ask('age?')\nreturn name, n + 1",
        )
        .unwrap();
    assert_eq!(t.state(), TaskState::Suspended(vec![s("name?")]));
    e.resume(&mut t, vec![s("Aalto")]).unwrap();
    assert_eq!(take(&out), ["hi Aalto"]);
    assert_eq!(t.state(), TaskState::Suspended(vec![s("age?")]));
    e.resume(&mut t, vec![Value::Number(3.0)]).unwrap();
    assert_eq!(
        t.state(),
        TaskState::Finished(vec![s("Aalto"), Value::Number(4.0)])
    );
    assert!(t.is_finished());
    // Resuming a finished task is an error.
    let err = e.resume(&mut t, vec![]).unwrap_err();
    assert!(
        err.render(false).contains("dead coroutine"),
        "{}",
        err.render(false)
    );
}

#[test]
fn yields_reach_through_script_code_but_not_natives() {
    let (mut e, _) = engine();
    e.register("pause", || -> Result<(), ScriptError> {
        Err(ScriptError::yield_with(vec![]))
    });
    // Through Lua functions, loops, metamethods and pcall: fine.
    let src = "local log = {}
local mt = { __index = function(_, k) pause() return k .. '!' end }
local t = setmetatable({}, mt)
for i = 1, 2 do
  local ok, v = pcall(function() pause() return t.x end)
  table.insert(log, tostring(ok) .. ' ' .. v)
end
return table.concat(log, ', ')";
    let mut t = e.spawn("y.rhue", src).unwrap();
    let mut n = 0;
    while !t.is_finished() {
        e.resume(&mut t, vec![]).unwrap();
        n += 1;
    }
    assert_eq!(n, 4, "two pauses per iteration");
    assert_eq!(t.state(), TaskState::Finished(vec![s("true x!, true x!")]));
    // From the main thread, or through a native, it's the usual error.
    let err = e.eval("pause()").unwrap_err();
    assert_eq!(
        err.diagnostic().message,
        "attempt to yield from outside a coroutine"
    );
    let mut t = e.spawn("n.rhue", "return pcall(pause)").unwrap();
    assert!(t.is_finished());
    let TaskState::Finished(v) = t.state() else {
        panic!()
    };
    assert_eq!(v[0], Value::Bool(false));
    assert!(v[1].to_string().contains("C-call boundary"), "{}", v[1]);
    let _ = &mut t;
}

#[test]
fn tasks_report_errors() {
    let (mut e, _) = engine();
    let err = e.spawn("bad.rhue", "error('nope')").unwrap_err();
    assert!(err.render(false).contains("nope"));
    assert!(e.spawn("syntax.rhue", "local = 1").is_err());
    let f: rhue::Function = e
        .eval_as("return function(a, b) coroutine.yield(a + b) return a * b end")
        .unwrap();
    let mut t = e
        .spawn_function(&f, vec![Value::Number(3.0), Value::Number(4.0)])
        .unwrap();
    assert_eq!(t.state(), TaskState::Suspended(vec![Value::Number(7.0)]));
    e.resume(&mut t, vec![]).unwrap();
    assert_eq!(t.state(), TaskState::Finished(vec![Value::Number(12.0)]));
}

// --- the task library --------------------------------------------------------------

#[test]
fn task_wait_and_delay_follow_the_clock() {
    let (mut e, out) = engine();
    e.open_tasks();
    let t = e
        .spawn(
            "main.rhue",
            "print('start')
local waited = task.wait(1)
print('woke after ' .. waited)
task.delay(0.5, function(x) print('delayed ' .. x) end, 'arg')
task.wait()
print('next step')
return 'done'",
        )
        .unwrap();
    assert_eq!(take(&out), ["start"]);
    assert_eq!(e.pending_tasks(), 1);
    assert!(e.step(0.5).is_empty());
    assert!(take(&out).is_empty(), "not yet");
    // Steps overshoot: wait reports the time that really passed.
    e.step(0.75);
    assert_eq!(take(&out), ["woke after 1.25"]);
    // `task.wait()` waits for the next step; the delay for its time.
    e.step(0.25);
    assert_eq!(take(&out), ["next step"]);
    assert!(t.is_finished());
    assert_eq!(t.state(), TaskState::Finished(vec![s("done")]));
    e.step(0.25);
    assert_eq!(take(&out), ["delayed arg"]);
    assert_eq!(e.pending_tasks(), 0);
    assert_eq!(e.task_clock(), 1.75);
}

#[test]
fn task_spawn_defer_and_cancel() {
    let (mut e, out) = engine();
    e.open_tasks();
    e.spawn(
        "main.rhue",
        "task.defer(print, 'deferred runs after this resumption')
local child = task.spawn(function(name)
  print('child ' .. name .. ' starts at once')
  task.wait(1)
  print('child wakes')
end, 'a')
print('back in main, child is ' .. coroutine.status(child))
local doomed = task.delay(1, print, 'never')
local sleeper = task.spawn(function() task.wait(1) print('never either') end)
task.cancel(doomed)
task.cancel(sleeper)
print(coroutine.status(sleeper))",
    )
    .unwrap();
    assert_eq!(
        take(&out),
        [
            "child a starts at once",
            "back in main, child is suspended",
            "dead",
            "deferred runs after this resumption",
        ]
    );
    e.step(1.0);
    assert_eq!(take(&out), ["child wakes"]);
    assert_eq!(e.pending_tasks(), 0);
}

#[test]
fn task_errors_dont_stop_other_tasks() {
    let (mut e, out) = engine();
    e.open_tasks();
    e.spawn(
        "main.rhue",
        "task.spawn(function() task.wait(1) error('first fails') end)
task.spawn(function() task.wait(1) print('second runs') end)
task.defer(function() error('deferred fails') end)",
    )
    .unwrap();
    let errors = e.step(1.0);
    assert_eq!(take(&out), ["second runs"]);
    let messages: Vec<String> = errors
        .iter()
        .map(|e| e.diagnostic().message.clone())
        .collect();
    assert_eq!(messages.len(), 2, "{messages:?}");
    assert!(messages.iter().any(|m| m.contains("deferred fails")));
    assert!(messages.iter().any(|m| m.contains("first fails")));
    assert!(e.step(1.0).is_empty(), "reported once");
}

#[test]
fn task_library_edges() {
    let (mut e, _) = engine();
    assert!(e.eval("return task").unwrap()[0].is_nil(), "opt-in");
    assert!(e.step(1.0).is_empty(), "no scheduler: nothing to do");
    e.open_tasks();
    e.open_tasks();
    let err = e.eval("task.wait(1)").unwrap_err();
    assert!(err.diagnostic().message.contains("Engine::spawn"));
    let err = e.eval("task.spawn(42)").unwrap_err();
    assert!(
        err.diagnostic()
            .message
            .contains("function or thread expected, got number")
    );
    // A task that keeps deferring itself can't hang a step.
    e.spawn(
        "loop.rhue",
        "local function again() task.defer(again) end\nagain()",
    )
    .unwrap();
    e.step(0.0);
    assert!(e.pending_tasks() > 0);
    // Typed.
    e.check("t.rhue", "--!strict\nlocal dt: number = task.wait(0.5)\nlocal th = task.spawn(function(n: number) end, 1)\ntask.cancel(th)").unwrap();
    assert!(
        e.check(
            "t.rhue",
            "--!strict\ntask.spawn(function(n: number) end, 'x')"
        )
        .is_err()
    );
    assert!(
        e.check("t.rhue", "--!strict\nlocal s: string = task.wait()")
            .is_err()
    );
}

// --- environments --------------------------------------------------------------------

#[test]
fn environments_keep_globals_apart() {
    let (mut e, out) = engine();
    let a = e.create_environment();
    let b = e.create_environment();
    e.eval_in(
        &a,
        "name = 'A'\nfunction hello() print('hi from ' .. name) end",
    )
    .unwrap();
    e.eval_in(&b, "name = 'B'").unwrap();
    e.eval_in(&a, "hello()").unwrap();
    assert_eq!(take(&out), ["hi from A"]);
    // Neither sees the other's, and the engine's own globals are untouched.
    assert!(e.eval_in(&b, "return hello").unwrap()[0].is_nil());
    assert!(e.global("name").is_nil());
    // Functions keep their environment wherever they're called from.
    let hello = e.eval_in(&a, "return hello").unwrap().remove(0);
    e.context().set(b.globals(), "borrowed", hello).unwrap();
    e.eval_in(&b, "borrowed()").unwrap();
    assert_eq!(take(&out), ["hi from A"]);
    // The shared library and host globals are visible; shadowing stays local.
    e.set_global("version", 7.0);
    let v = e
        .eval_in(
            &a,
            "local v = version\nprint = function() end\nreturn v, math.max(1, 2)",
        )
        .unwrap();
    assert_eq!(v, [Value::Number(7.0), Value::Number(2.0)]);
    e.eval("print('the real print')").unwrap();
    assert_eq!(take(&out), ["the real print"]);
    // The host can read and seed an environment's globals.
    let mut cx = e.context();
    assert_eq!(cx.get::<String>(a.globals(), "name").unwrap(), "A");
    cx.set(b.globals(), "seed", 41.0).unwrap();
    assert_eq!(
        e.eval_in(&b, "return seed + 1").unwrap(),
        [Value::Number(42.0)]
    );
}

#[test]
fn environments_are_type_checked_separately() {
    let (mut e, _) = engine();
    let a = e.create_environment();
    let b = e.create_environment();
    e.eval_in(
        &a,
        "--!strict\nlimit = 10\nfunction double(n: number): number return n * 2 end",
    )
    .unwrap();
    // Its own environment knows them...
    e.eval_in(&a, "--!strict\nlocal x: number = double(limit)")
        .unwrap();
    let err = e
        .eval_in(&a, "--!strict\nlocal s: string = double(1)")
        .unwrap_err();
    assert!(
        err.render(false)
            .contains("could not be converted into 'string'")
    );
    // ...others don't.
    let err = e.eval_in(&b, "--!strict\nprint(double(1))").unwrap_err();
    assert_eq!(err.diagnostic().message, "Unknown global 'double'");
    let err = e.eval("--!strict\nprint(limit)").unwrap_err();
    assert_eq!(err.diagnostic().message, "Unknown global 'limit'");
    // And engine globals defined later are still seen everywhere.
    e.eval("--!strict\nshared = 'x'").unwrap();
    e.eval_in(&b, "--!strict\nlocal s: string = shared")
        .unwrap();
}

#[test]
fn environments_with_tasks_and_frozen_tables() {
    let (mut e, out) = engine();
    e.open_tasks();
    let sandbox = e.create_environment();
    e.spawn_in(
        &sandbox,
        "bot.rhue",
        "count = 0\nwhile true do count += 1 task.wait(1) end",
    )
    .unwrap();
    for _ in 0..3 {
        e.step(1.0);
    }
    assert_eq!(
        e.context().get::<f64>(sandbox.globals(), "count").unwrap(),
        4.0
    );
    assert!(e.global("count").is_nil());
    // A frozen environment table makes its globals read-only.
    let locked = e.create_environment();
    e.context().set(locked.globals(), "x", 1.0).unwrap();
    e.set_global("lock", locked.globals().clone());
    e.eval("table.freeze(lock)").unwrap();
    let err = e.eval_in(&locked, "x = 2").unwrap_err();
    assert_eq!(
        err.diagnostic().message,
        "attempt to modify a readonly table"
    );
    assert_eq!(
        e.eval_in(&locked, "return x").unwrap(),
        [Value::Number(1.0)]
    );
    let _ = take(&out);
}

#[test]
fn environment_globals_survive_collection() {
    let (mut e, _) = engine();
    e.set_gc_stress(true);
    let env = e.create_environment();
    e.eval_in(&env, "data = { 1, 2, 3 }\nfunction sum() local s = 0 for _, v in data do s += v end return s end").unwrap();
    let f: rhue::Function = e
        .eval_in(&env, "return sum")
        .unwrap()
        .remove(0)
        .try_into_function();
    drop(env);
    e.gc();
    // The function still reaches its environment.
    assert_eq!(e.call::<f64>(&f, ()).unwrap(), 6.0);
}

trait IntoFunction {
    fn try_into_function(self) -> rhue::Function;
}

impl IntoFunction for Value {
    fn try_into_function(self) -> rhue::Function {
        match self {
            Value::Function(f) => f,
            other => panic!("not a function: {other:?}"),
        }
    }
}

// --- serde ---------------------------------------------------------------------------

#[cfg(feature = "serde")]
mod serde_tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
    enum Shape {
        Dot,
        Circle(f64),
        Rect { w: f64, h: f64 },
        Pair(u8, u8),
    }

    #[derive(Serialize, Deserialize, Debug, PartialEq, Clone)]
    struct Scene {
        name: String,
        layers: u32,
        visible: bool,
        offset: (i32, i32),
        shapes: Vec<Shape>,
        tags: std::collections::BTreeMap<String, i64>,
        note: Option<String>,
        #[serde(with = "bytes")]
        data: Vec<u8>,
    }

    /// `Vec<u8>` as bytes (a buffer), like `serde_bytes`.
    mod bytes {
        pub fn serialize<S: serde::Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
            s.serialize_bytes(v)
        }
        pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
            struct V;
            impl serde::de::Visitor<'_> for V {
                type Value = Vec<u8>;
                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("bytes")
                }
                fn visit_bytes<E>(self, v: &[u8]) -> Result<Vec<u8>, E> {
                    Ok(v.to_vec())
                }
                fn visit_byte_buf<E>(self, v: Vec<u8>) -> Result<Vec<u8>, E> {
                    Ok(v)
                }
            }
            d.deserialize_bytes(V)
        }
    }

    fn scene() -> Scene {
        Scene {
            name: "intro".into(),
            layers: 3,
            visible: true,
            offset: (-4, 9),
            shapes: vec![
                Shape::Dot,
                Shape::Circle(1.5),
                Shape::Rect { w: 2.0, h: 3.0 },
                Shape::Pair(1, 2),
            ],
            tags: [("a".to_string(), 1), ("b".to_string(), -2)].into(),
            note: None,
            data: vec![0, 255, 7],
        }
    }

    #[test]
    fn rust_values_round_trip() {
        let mut e = Engine::new();
        let mut cx = e.context();
        let v = rhue::serde::to_value(&mut cx, &scene()).unwrap();
        let back: Scene = rhue::serde::from_value(&mut cx, v).unwrap();
        assert_eq!(back, scene());
    }

    #[test]
    fn scripts_see_plain_tables() {
        let mut e = Engine::new();
        let v = rhue::serde::to_value(&mut e.context(), &scene()).unwrap();
        e.set_global("scene", v);
        let out: String = e
            .eval_as(
                "local s = scene
local kinds = {}
for _, shape in s.shapes do
  table.insert(kinds, if type(shape) == 'string' then shape else next(shape))
end
return `{s.name} {s.layers} {s.offset[1]},{s.offset[2]} {table.concat(kinds, '/')} `
  .. `{s.shapes[3].Rect.h} {s.tags.b} {tostring(s.note)} {buffer.readu8(s.data, 1)}`",
            )
            .unwrap();
        assert_eq!(out, "intro 3 -4,9 Dot/Circle/Rect/Pair 3 -2 nil 255");
        // And script-made tables read back.
        e.eval(
            "scene.layers += 1\nscene.note = 'edited'\ntable.insert(scene.shapes, { Circle = 9 })",
        )
        .unwrap();
        let v = e.global("scene");
        let back: Scene = rhue::serde::from_value(&mut e.context(), v).unwrap();
        assert_eq!(back.layers, 4);
        assert_eq!(back.note.as_deref(), Some("edited"));
        assert_eq!(back.shapes.last(), Some(&Shape::Circle(9.0)));
    }

    #[test]
    fn mismatches_are_errors() {
        let mut e = Engine::new();
        let mut from = |src: &str| -> Result<Scene, rhue::serde::Error> {
            let v = e.eval(src).unwrap().remove(0);
            rhue::serde::from_value(&mut e.context(), v)
        };
        let err = from("return { name = 1 }").unwrap_err();
        assert!(err.to_string().contains("expected a string"), "{err}");
        let err = from("return 'x'").unwrap_err();
        assert!(err.to_string().contains("expected a table"), "{err}");
        let mut e = Engine::new();
        let v = e.eval("return 1.5").unwrap().remove(0);
        assert!(rhue::serde::from_value::<u32>(&mut e.context(), v).is_err());
        let v = e.eval("return -1").unwrap().remove(0);
        assert!(rhue::serde::from_value::<u8>(&mut e.context(), v).is_err());
        let v = e.eval("return print").unwrap().remove(0);
        let err = rhue::serde::from_value::<String>(&mut e.context(), v).unwrap_err();
        assert!(err.to_string().contains("function"), "{err}");
        // Maps with keys that aren't 1..n read as maps, not sequences.
        let v = e.eval("return { [1] = 'a', [3] = 'c' }").unwrap().remove(0);
        let m: std::collections::BTreeMap<u32, String> =
            rhue::serde::from_value(&mut e.context(), v).unwrap();
        assert_eq!(m.len(), 2);
        // A natives' conversion error becomes a script error.
        e.register(
            "parse",
            |cx: &mut rhue::Context<'_>, v: Value| -> Result<u32, ScriptError> {
                Ok(rhue::serde::from_value::<u32>(cx, v)?)
            },
        );
        let err = e.eval("parse('x')").unwrap_err();
        assert!(
            err.diagnostic().message.contains("invalid type"),
            "{}",
            err.diagnostic().message
        );
    }
}
