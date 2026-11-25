//! Resuming a parked thread into an error (2.3), for a native that waited on
//! the host and then failed. With `pcall` around it the script gets `false,
//! err` and carries on.

use rhue::{Engine, ScriptError, Task, TaskState, Value};

fn s(text: &str) -> Value {
    Value::String(text.into())
}

/// `pause()` stops the script until the host resumes it.
fn engine() -> Engine {
    let mut e = Engine::new();
    e.register("pause", || -> Result<(), ScriptError> {
        Err(ScriptError::yield_with(vec![]))
    });
    e
}

fn finished(t: &Task) -> Vec<Value> {
    match t.state() {
        TaskState::Finished(v) => v,
        other => panic!("not finished: {other:?}"),
    }
}

/// Spawns `src`, resumes it into `error` and gives back what it finished with.
fn fail_into(e: &mut Engine, src: &str, error: Value) -> Vec<Value> {
    let mut t = e.spawn("t.rhue", src).unwrap();
    assert!(!t.is_finished(), "finished early: {:?}\n{src}", t.state());
    e.resume_error(&mut t, error).unwrap();
    finished(&t)
}

// ─ Under pcall ───────────────────────────────────────────────────────────────

#[test]
fn pcall_around_the_native_gets_false() {
    let mut e = engine();
    let got = fail_into(&mut e, "return pcall(pause)", s("not found"));
    assert_eq!(got, [Value::Bool(false), s("t.rhue:1: not found")]);
    // a fixed number of results, and the script keeps going after
    let src = "local ok, err = pcall(pause)
local after = 'ran'
return ok, err, after";
    let got = fail_into(&mut e, src, s("nope"));
    assert_eq!(got, [Value::Bool(false), s("t.rhue:1: nope"), s("ran")]);
}

#[test]
fn xpcall_runs_its_handler() {
    let mut e = engine();
    e.set_gc_stress(true);
    let src = "return xpcall(pause, function(err) return 'handled ' .. err end)";
    let mut t = e.spawn("t.rhue", src).unwrap();
    e.gc();
    e.resume_error(&mut t, s("x")).unwrap();
    assert_eq!(finished(&t), [Value::Bool(false), s("handled t.rhue:1: x")]);
    // around a script function that waits, it's the unwind that finds it
    let src = "return xpcall(function() local v = pause() return v end, function(err) return 'h ' .. err end)";
    let got = fail_into(&mut e, src, s("y"));
    assert_eq!(got, [Value::Bool(false), s("h t.rhue:1: y")]);
}

#[test]
fn a_pcall_further_up_catches_it() {
    let mut e = engine();
    let src = "local function load()
  return pause()
end
local ok, err = pcall(function() return load() + 1 end)
return ok, err";
    let got = fail_into(&mut e, src, s("timed out"));
    // positioned where the native was called
    assert_eq!(got, [Value::Bool(false), s("t.rhue:2: timed out")]);
}

#[test]
fn other_values_pass_through() {
    let mut e = engine();
    let got = fail_into(&mut e, "return pcall(pause)", Value::Number(404.0));
    assert_eq!(got, [Value::Bool(false), Value::Number(404.0)]);
    let err = e.eval("return { code = 7 }").unwrap().remove(0);
    let src = "local ok, err = pcall(pause)
return ok, type(err), err.code";
    let got = fail_into(&mut e, src, err);
    assert_eq!(got, [Value::Bool(false), s("table"), Value::Number(7.0)]);
}

// ─ Not Caught ────────────────────────────────────────────────────────────────

#[test]
fn nothing_catches_it_and_the_task_dies() {
    let mut e = engine();
    let src = "local v = pause()
return v";
    let mut t = e.spawn("t.rhue", src).unwrap();
    let err = e.resume_error(&mut t, s("not found")).unwrap_err();
    // it points at the call that was waiting, with a normal traceback
    assert_eq!(err.diagnostic().span().text(src), "pause()");
    let r = err.render(false);
    assert!(r.contains("not found") && r.contains("t.rhue:1:"), "{r}");
    assert!(t.is_finished());
    // and it stays dead
    let err = e.resume_error(&mut t, s("again")).unwrap_err();
    assert!(err.to_string().contains("dead"), "{err}");
}

#[test]
fn a_coroutine_resumed_into_an_error_fails_its_resumer() {
    let mut e = engine();
    let src = "local co = coroutine.create(function() pause() end)
coroutine.resume(co)
return co";
    let co = e.eval(src).unwrap().remove(0);
    let Value::Thread(co) = co else {
        panic!("not a thread: {co:?}")
    };
    let mut cx = e.context();
    let err = cx.resume_error::<()>(&co, "broke").unwrap_err();
    assert!(err.to_string().contains("broke"), "{err}");
    assert_eq!(cx.thread_status(&co), "dead");
}

// ─ Where It Can Be Parked ────────────────────────────────────────────────────

#[test]
fn parked_in_coroutine_yield() {
    let mut e = engine();
    let src = "local ok, err = pcall(function() coroutine.yield() end)
return ok, err";
    let got = fail_into(&mut e, src, s("y"));
    assert_eq!(got, [Value::Bool(false), s("t.rhue:1: y")]);
    let got = fail_into(&mut e, "return pcall(coroutine.yield)", s("z"));
    assert_eq!(got, [Value::Bool(false), s("t.rhue:1: z")]);
}

#[test]
fn a_coroutine_that_hasnt_started_cant_take_one() {
    let mut e = engine();
    let f = e.eval("return function() return 1 end").unwrap().remove(0);
    let Value::Function(f) = f else {
        panic!("not a function: {f:?}")
    };
    let mut cx = e.context();
    let co = cx.create_thread(&f);
    let err = cx.resume_error::<()>(&co, "early").unwrap_err();
    assert!(err.to_string().contains("hasn't started"), "{err}");
    // nothing happened to it
    assert_eq!(cx.thread_status(&co), "suspended");
    assert_eq!(cx.resume::<f64>(&co, ()).unwrap(), 1.0);
}
