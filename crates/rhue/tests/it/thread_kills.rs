//! Killing and cancelling threads from the host (2.3). A kill reaches a
//! thread in any state, scripts can't catch it, and it stops at the thread's
//! edge.

use std::cell::Cell;
use std::rc::Rc;

use rhue::{Context, Engine, Function, ScriptError, TaskState, ThreadEnd, Value};

fn s(text: &str) -> Value {
    Value::String(text.into())
}

fn thread(v: &Value) -> rhue::Thread {
    match v {
        Value::Thread(t) => t.clone(),
        other => panic!("not a thread: {other:?}"),
    }
}

/// `kill(co)` and `cancel(co)` mark a thread from a script, `killself()`
/// kills the running one and returns the kill, `pause()` waits on the host.
fn engine() -> Engine {
    let mut e = Engine::new();
    e.register("kill", |cx: &mut Context<'_>, t: Value| {
        cx.kill_thread(&thread(&t))
    });
    e.register("cancel", |cx: &mut Context<'_>, t: Value| {
        cx.cancel_thread(&thread(&t))
    });
    e.register(
        "killself",
        |cx: &mut Context<'_>| -> Result<(), ScriptError> {
            let me = cx.current_thread().expect("in a coroutine");
            cx.kill_thread(&me);
            Err(ScriptError::killed())
        },
    );
    e.register("pause", || -> Result<(), ScriptError> {
        Err(ScriptError::yield_with(vec![]))
    });
    e
}

// ─ A Suspended Thread ────────────────────────────────────────────────────────

#[test]
fn a_suspended_thread_dies_at_once() {
    let mut e = engine();
    let src = "local get
local co = coroutine.create(function()
  local v = 5
  get = function() return v end
  coroutine.yield()
  error('ran on')
end)
coroutine.resume(co)
local marked = kill(co)
local ok, err = coroutine.resume(co)
return marked, coroutine.status(co), ok, err, get()";
    let got = e.eval(src).unwrap();
    // its upvalues got closed, nothing after the yield ran
    assert_eq!(
        got,
        [
            Value::Bool(true),
            s("dead"),
            Value::Bool(false),
            s("cannot resume dead coroutine"),
            Value::Number(5.0)
        ]
    );
    // a dead one can't be killed again
    let got = e
        .eval("local co = coroutine.create(function() end) coroutine.resume(co) return kill(co)")
        .unwrap();
    assert_eq!(got, [Value::Bool(false)]);
}

// ─ A Running Thread ──────────────────────────────────────────────────────────

#[test]
fn a_thread_that_kills_itself_never_returns_from_the_call() {
    let mut e = engine();
    let src = "local ran = false
local co = coroutine.create(function()
  pcall(function() killself() end)
  ran = true
end)
local ok, err = coroutine.resume(co)
return ok, err, ran, coroutine.status(co)";
    let got = e.eval(src).unwrap();
    assert_eq!(
        got,
        [
            Value::Bool(false),
            s("thread killed"),
            Value::Bool(false),
            s("dead")
        ]
    );
}

#[test]
fn a_marked_thread_dies_at_its_next_back_edge() {
    let mut e = engine();
    // kill() only marks, it doesn't return the kill, so the loop gets one go
    let src = "local n = 0
local co = coroutine.create(function()
  kill(coroutine.running())
  for i = 1, 1000 do n += 1 end
end)
local ok, err = coroutine.resume(co)
return ok, err, n";
    let got = e.eval(src).unwrap();
    assert_eq!(
        got,
        [Value::Bool(false), s("thread killed"), Value::Number(1.0)]
    );
    // and one that returns first still counts as killed
    let src = "local co = coroutine.create(function() kill(coroutine.running()) return 'done' end)
return coroutine.resume(co)";
    let got = e.eval(src).unwrap();
    assert_eq!(got, [Value::Bool(false), s("thread killed")]);
}

#[test]
fn a_kill_reaches_a_thread_waiting_on_one_it_resumed() {
    let mut e = engine();
    // it dies at its first back-edge after the inner one hands back
    let src = "local n = 0
local outer
outer = coroutine.create(function()
  local inner = coroutine.create(function()
    kill(outer)
    coroutine.yield('inner yielded')
  end)
  coroutine.resume(inner)
  for i = 1, 1000 do n += 1 end
end)
local ok, err = coroutine.resume(outer)
return ok, err, n, coroutine.status(outer)";
    let got = e.eval(src).unwrap();
    assert_eq!(
        got,
        [
            Value::Bool(false),
            s("thread killed"),
            Value::Number(1.0),
            s("dead")
        ]
    );
}

#[test]
fn pcall_and_xpcall_cant_catch_it() {
    let mut e = engine();
    let src = "local caught = 0
local co = coroutine.create(function()
  pcall(function()
    xpcall(function() killself() end, function() caught += 1 end)
  end)
  caught += 10
end)
local ok, err = coroutine.resume(co)
return ok, err, caught";
    let got = e.eval(src).unwrap();
    assert_eq!(
        got,
        [Value::Bool(false), s("thread killed"), Value::Number(0.0)]
    );
}

// ─ The Edge ──────────────────────────────────────────────────────────────────

#[test]
fn a_wrap_function_raises_it_in_its_caller() {
    let mut e = engine();
    let src = "local f = coroutine.wrap(function() killself() end)
local ok, err = pcall(f)
return ok, err";
    let got = e.eval(src).unwrap();
    assert_eq!(got[0], Value::Bool(false));
    let err = got[1].to_string();
    assert!(err.ends_with("thread killed"), "{err}");
}

#[test]
fn the_host_gets_a_killed_error() {
    let mut e = engine();
    let err = e.spawn("t.rhue", "killself()").unwrap_err();
    assert!(err.is_killed(), "{err}");
    // and a plain error isn't one
    let err = e.spawn("t.rhue", "error('boom')").unwrap_err();
    assert!(!err.is_killed());
}

#[test]
fn a_host_kill_of_a_waiting_task() {
    let mut e = engine();
    let mut t = e.spawn("t.rhue", "pause() error('ran on')").unwrap();
    {
        let mut cx = e.context();
        assert!(cx.kill_thread(t.thread()));
        assert_eq!(cx.thread_status(t.thread()), "dead");
        assert_eq!(cx.thread_end(t.thread()), Some(ThreadEnd::Killed));
    }
    let err = e.resume(&mut t, vec![]).unwrap_err();
    assert!(err.to_string().contains("dead"), "{err}");
}

#[test]
fn how_threads_ended() {
    let mut e = engine();
    let src = "local function done() end
local a = coroutine.create(done)
local b = coroutine.create(function() error('x') end)
local c = coroutine.create(function() killself() end)
local d = coroutine.create(function() cancel(coroutine.running()) coroutine.yield() end)
local alive = coroutine.create(done)
coroutine.resume(a) coroutine.resume(b) coroutine.resume(c) coroutine.resume(d)
return a, b, c, d, alive";
    let got = e.eval(src).unwrap();
    let cx = e.context();
    let ends: Vec<_> = got.iter().map(|v| cx.thread_end(&thread(v))).collect();
    assert_eq!(
        ends,
        [
            Some(ThreadEnd::Finished),
            Some(ThreadEnd::Failed),
            Some(ThreadEnd::Killed),
            Some(ThreadEnd::Cancelled),
            None
        ]
    );
}

// ─ Cancel ────────────────────────────────────────────────────────────────────

#[test]
fn a_cancelled_thread_runs_until_it_pauses() {
    let mut e = engine();
    let src = "local n = 0
local co = coroutine.create(function()
  cancel(coroutine.running())
  for i = 1, 100 do n += 1 end
  coroutine.yield()
  n = -1
end)
local ok, err = coroutine.resume(co)
return ok, err, n";
    let got = e.eval(src).unwrap();
    assert_eq!(
        got,
        [
            Value::Bool(false),
            s("thread cancelled"),
            Value::Number(100.0)
        ]
    );
    // a native that waits counts as a pause too
    let err = e
        .spawn(
            "t.rhue",
            "cancel(coroutine.running()) pause() error('ran on')",
        )
        .unwrap_err();
    assert!(err.is_killed(), "{err}");
    assert!(err.to_string().contains("thread cancelled"), "{err}");
}

#[test]
fn a_cancelled_thread_that_never_pauses_just_finishes() {
    let mut e = engine();
    let src = "local co = coroutine.create(function()
  cancel(coroutine.running())
  return 'done'
end)
return coroutine.resume(co)";
    let got = e.eval(src).unwrap();
    assert_eq!(got, [Value::Bool(true), s("done")]);
}

#[test]
fn a_suspended_thread_cancelled_dies_at_once() {
    let mut e = engine();
    let src = "local co = coroutine.create(function() coroutine.yield() error('ran on') end)
coroutine.resume(co)
return cancel(co), coroutine.status(co)";
    let got = e.eval(src).unwrap();
    assert_eq!(got, [Value::Bool(true), s("dead")]);
}

// ─ Natives in Between ────────────────────────────────────────────────────────

#[test]
fn a_native_cant_swallow_a_kill() {
    let mut e = engine();
    let saw = Rc::new(Cell::new(false));
    let seen = saw.clone();
    e.register(
        "swallow",
        move |cx: &mut Context<'_>, f: Function| -> Result<bool, ScriptError> {
            let r = cx.call::<()>(&f, ());
            seen.set(r.as_ref().is_err_and(ScriptError::is_killed));
            Ok(true)
        },
    );
    // the thread still dies, at the loop's first back-edge
    let src = "local n = 0
local co = coroutine.create(function()
  local r = swallow(function() killself() end)
  for i = 1, 1000 do n += 1 end
end)
local ok, err = coroutine.resume(co)
return ok, err, n";
    let got = e.eval(src).unwrap();
    assert!(saw.get(), "the native saw the kill as an error");
    assert_eq!(
        got,
        [Value::Bool(false), s("thread killed"), Value::Number(1.0)]
    );
}

// ─ Tasks ─────────────────────────────────────────────────────────────────────

#[test]
fn a_killed_task_finishes_without_an_error() {
    let mut e = engine();
    e.open_tasks();
    let t = e
        .spawn("t.rhue", "task.wait(1) killself() error('ran on')")
        .unwrap();
    assert!(e.step(2.0).is_empty());
    assert!(t.is_finished());
    assert_eq!(t.state(), TaskState::Finished(vec![]));
    // killed by the host while it waits on the clock
    let t = e.spawn("t.rhue", "task.wait(1) error('ran on')").unwrap();
    assert!(e.context().kill_thread(t.thread()));
    assert!(e.step(2.0).is_empty());
}

#[test]
fn the_main_thread_cant_be_killed() {
    let mut e = engine();
    // on the main thread coroutine.running() gives the main thread or nil,
    // either way there's no kill
    let got = e
        .eval("local me = coroutine.running() if me == nil then return false end return kill(me)")
        .unwrap();
    assert_eq!(got, [Value::Bool(false)]);
}

#[test]
fn the_instruction_limit_stays_exact() {
    let mut e = engine();
    e.set_instruction_limit(Some(100_000));
    // kills hand their slice back, so many of them don't eat the limit
    let src = "for i = 1, 200 do
  local co = coroutine.create(function() kill(coroutine.running()) local x = 1 end)
  coroutine.resume(co)
end
local n = 0
for i = 1, 1000 do n += i end
return n";
    let got = e.eval(src).unwrap();
    assert_eq!(got, [Value::Number(500_500.0)]);
}
