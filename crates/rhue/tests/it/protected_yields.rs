//! `pcall` and `xpcall` right around a native that yields (2.3). The usual
//! `pcall(require, m)`, where `require` waits on the host.

use rhue::{Engine, ScriptError, Task, TaskState, Value};

fn s(text: &str) -> Value {
    Value::String(text.into())
}

fn n(x: f64) -> Value {
    Value::Number(x)
}

/// `pause()` stops the script and the host's resume values become its
/// results. `fail(msg)` raises `msg`.
fn engine() -> Engine {
    let mut e = Engine::new();
    e.register("pause", || -> Result<(), ScriptError> {
        Err(ScriptError::yield_with(vec![]))
    });
    e.register("fail", |msg: String| -> Result<(), ScriptError> {
        Err(ScriptError::new(msg))
    });
    e
}

fn finished(t: &Task) -> Vec<Value> {
    match t.state() {
        TaskState::Finished(v) => v,
        other => panic!("not finished: {other:?}"),
    }
}

/// Runs `src` as a task, resuming it with each of `resumes` in turn, and
/// gives back what it finished with.
fn run(e: &mut Engine, src: &str, resumes: Vec<Vec<Value>>) -> Vec<Value> {
    let mut t = e.spawn("t.rhue", src).unwrap();
    for values in resumes {
        assert!(!t.is_finished(), "finished early: {:?}\n{src}", t.state());
        e.resume(&mut t, values).unwrap();
    }
    finished(&t)
}

fn text(v: &Value) -> String {
    v.to_string()
}

// ─ pcall ─────────────────────────────────────────────────────────────────────

#[test]
fn pcall_around_a_yielding_native() {
    let mut e = engine();
    let got = run(&mut e, "return pcall(pause)", vec![vec![n(1.0), n(2.0)]]);
    assert_eq!(got, [Value::Bool(true), n(1.0), n(2.0)]);
    // not a tail call, with a fixed number of results and with all of them
    let src = "local ok, a = pcall(pause)
local t = { pcall(pause) }
return ok, a, #t, t[3]";
    let got = run(
        &mut e,
        src,
        vec![vec![n(1.0), n(2.0)], vec![n(3.0), n(4.0)]],
    );
    assert_eq!(got, [Value::Bool(true), n(1.0), n(3.0), n(4.0)]);
}

#[test]
fn pcall_around_a_yielding_native_inside_more_pcalls() {
    let mut e = engine();
    let src = "local ok, inner, v = pcall(function() return pcall(pause) end)
return ok, inner, v";
    let got = run(&mut e, src, vec![vec![s("x")]]);
    assert_eq!(got, [Value::Bool(true), Value::Bool(true), s("x")]);
    // what comes back can still fail the outer one
    let src = "return pcall(function()
  local ok, v = pcall(pause)
  error(v, 0)
end)";
    let got = run(&mut e, src, vec![vec![s("boom")]]);
    assert_eq!(got, [Value::Bool(false), s("boom")]);
}

#[test]
fn pcall_around_coroutine_yield() {
    let mut e = engine();
    let mut t = e
        .spawn("t.rhue", "return pcall(coroutine.yield, 'out')")
        .unwrap();
    assert_eq!(t.state(), TaskState::Suspended(vec![s("out")]));
    e.resume(&mut t, vec![s("back")]).unwrap();
    assert_eq!(finished(&t), [Value::Bool(true), s("back")]);
    // and in a plain coroutine
    let src = "local co = coroutine.create(function() return pcall(coroutine.yield, 1) end)
local _, y = coroutine.resume(co)
local _, ok, v = coroutine.resume(co, 'two')
return y, ok, v";
    let got = e.eval(src).unwrap();
    assert_eq!(got, [n(1.0), Value::Bool(true), s("two")]);
}

#[test]
fn pcall_errors_without_a_yield_still_come_back_false() {
    let mut e = engine();
    let got = e.eval("return pcall(fail, 'boom')").unwrap();
    assert_eq!(got[0], Value::Bool(false));
    assert!(text(&got[1]).ends_with("boom"), "{}", got[1]);
    let got = e.eval("return pcall(error, 'x')").unwrap();
    assert_eq!(got, [Value::Bool(false), s("x")]);
    let got = e.eval("return pcall(42)").unwrap();
    assert_eq!(
        got,
        [Value::Bool(false), s("attempt to call a number value")]
    );
}

// ─ xpcall ────────────────────────────────────────────────────────────────────

#[test]
fn xpcall_around_a_yielding_native() {
    let mut e = engine();
    let src = "return xpcall(pause, function(err) return 'handled ' .. err end)";
    let got = run(&mut e, src, vec![vec![n(5.0)]]);
    assert_eq!(got, [Value::Bool(true), n(5.0)]);
    let src = "local ok, v = xpcall(pause, print)
return ok, v";
    let got = run(&mut e, src, vec![vec![s("v")]]);
    assert_eq!(got, [Value::Bool(true), s("v")]);
}

#[test]
fn xpcall_handler_runs_after_a_resume() {
    let mut e = engine();
    e.set_gc_stress(true);
    // the handler waits on the stack while the thread is parked
    let src = "return xpcall(function()
  local v = pause()
  error('got ' .. v, 0)
end, function(err) return 'handled ' .. err end)";
    let mut t = e.spawn("t.rhue", src).unwrap();
    e.gc();
    e.resume(&mut t, vec![s("x")]).unwrap();
    assert_eq!(finished(&t), [Value::Bool(false), s("handled got x")]);
}

#[test]
fn xpcall_errors() {
    let mut e = engine();
    let got = e
        .eval("return xpcall(fail, function(err) return 'handled: ' .. err end, 'boom')")
        .unwrap();
    assert_eq!(got[0], Value::Bool(false));
    let v = text(&got[1]);
    assert!(v.starts_with("handled: ") && v.ends_with("boom"), "{v}");
    // a handler that fails too gives its own error
    let got = e
        .eval("return xpcall(error, function() error('again', 0) end, 'first')")
        .unwrap();
    assert_eq!(got, [Value::Bool(false), s("again")]);
    // the arguments get checked like they did when it was a plain native
    let err = e.eval("return xpcall(print)").unwrap_err();
    assert!(
        err.to_string().contains("missing argument #2 to 'xpcall'"),
        "{err}"
    );
    let err = e.eval("return xpcall(print, 1)").unwrap_err();
    assert!(
        err.to_string().contains("invalid argument #2 to 'xpcall'"),
        "{err}"
    );
    // a native calling it still gets the nested version
    let got = e
        .eval("return select(2, pcall(xpcall, error, function(err) return 'h ' .. err end, 'e'))")
        .unwrap();
    assert_eq!(got, [Value::Bool(false), s("h e")]);
}

// ─ What Stays an Error ───────────────────────────────────────────────────────

#[test]
fn yields_that_still_cant_happen() {
    let mut e = engine();
    // pcall(pcall, native) runs the inner pcall nested
    let mut t = e.spawn("t.rhue", "return pcall(pcall, pause)").unwrap();
    let got = finished(&t);
    assert_eq!(got[..2], [Value::Bool(true), Value::Bool(false)]);
    assert!(text(&got[2]).contains("R-call boundary"), "{}", got[2]);
    // an xpcall handler can't yield
    let src = "return xpcall(function() error('x', 0) end, function(err) pause() return err end)";
    t = e.spawn("t.rhue", src).unwrap();
    let got = finished(&t);
    assert_eq!(got[0], Value::Bool(false));
    assert!(text(&got[1]).contains("R-call boundary"), "{}", got[1]);
    // and outside a coroutine there's nothing to yield to
    for src in ["return pcall(pause)", "return pcall(coroutine.yield)"] {
        let got = e.eval(src).unwrap();
        assert_eq!(got[0], Value::Bool(false));
        assert!(
            text(&got[1]).contains("outside a coroutine"),
            "{src}: {}",
            got[1]
        );
    }
    let _ = &mut t;
}
