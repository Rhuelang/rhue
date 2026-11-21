//! Milestone 25: async hosts. Async natives, the drivers that run scripts
//! calling them (`eval_async`, `call_async`, `run_until_idle`), the `task`
//! clock on real time, and interrupting scripts from somewhere else.
//!
//! Time is tokio's paused clock, so waits are instant and deterministic.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use rhue::{Engine, Function, Value};

#[expect(clippy::cast_possible_truncation, reason = "test values are small")]
fn engine() -> Engine {
    let mut e = Engine::new();
    e.open_tasks();
    e.set_sleep(tokio::time::sleep);
    // read tokio's clock too, std's keeps going while tokio's is paused and
    // the waits come out a millisecond short when the machine is busy
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    let start = tokio::time::Instant::now();
    e.set_clock(move || epoch + start.elapsed());
    e.register_async("wait_ms", |ms: f64| async move {
        tokio::time::sleep(Duration::from_millis(ms as u64)).await;
        ms
    });
    e.register_async("fetch", |path: String| async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        match path.as_str() {
            "/missing" => Err(format!("{path} not found")),
            _ => Ok(format!("contents of {path}")),
        }
    });
    e
}

fn text(values: &[Value]) -> String {
    values
        .iter()
        .map(|v| v.to_string())
        .collect::<Vec<_>>()
        .join(" | ")
}

#[tokio::test(start_paused = true)]
async fn async_natives_return_and_raise() {
    let mut e = engine();
    let v = e
        .eval_async("local a = fetch('/a') local n = wait_ms(5) return a, n + 1")
        .await
        .unwrap();
    assert_eq!(text(&v), "contents of /a | 6");

    // an Err is raised where the script called the function, and pcall catches
    // it like any other error
    let v = e
        .eval_async("local ok, err = pcall(fetch, '/missing') return ok, err")
        .await
        .unwrap();
    // (with the call's position, as for errors from any native)
    assert_eq!(text(&v), "false | <eval>:1: /missing not found");
    let err = e
        .eval_async("local x = 1\nlocal page = fetch('/missing')\nreturn page")
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "/missing not found");
    // raised at the script's own call, with no wrapper frame in between
    let src = "local x = 1
local page = fetch('/missing')
return page";
    let err = e.eval_async(src).await.unwrap_err();
    assert_eq!(err.diagnostic().span().text(src), "fetch('/missing')");
    let r = err.render(false);
    assert!(!r.contains("[async"), "{r}");
    // xpcall sees it too
    let v = e
        .eval_async("return xpcall(fetch, function(err) return 'handled ' .. err end, '/missing')")
        .await
        .unwrap();
    assert_eq!(text(&v), "false | handled <eval>:1: /missing not found");
    assert!(err.render(false).contains(":2:"), "{}", err.render(false));
}

#[tokio::test(start_paused = true)]
async fn waits_overlap() {
    let mut e = engine();
    let log = Rc::new(RefCell::new(Vec::new()));
    let l = log.clone();
    e.register("note", move |s: String| l.borrow_mut().push(s));
    let start = tokio::time::Instant::now();
    e.eval_async(
        "task.spawn(function() wait_ms(30) note('slow') end)
         task.spawn(function() wait_ms(10) note('fast') end)
         wait_ms(20)
         note('main')",
    )
    .await
    .unwrap();
    // the main script finishes at 20ms, the slow task is still waiting
    assert_eq!(*log.borrow(), ["fast", "main"]);
    assert_eq!(start.elapsed(), Duration::from_millis(20));
    assert!(e.run_until_idle().await.is_empty());
    assert_eq!(*log.borrow(), ["fast", "main", "slow"]);
    assert_eq!(start.elapsed(), Duration::from_millis(30));
}

#[tokio::test(start_paused = true)]
async fn the_task_clock_runs_on_real_time() {
    let mut e = engine();
    let start = tokio::time::Instant::now();
    let v = e
        .eval_async(
            "local waited = task.wait(1.5)
             local got
             task.delay(0.5, function(x) got = x end, 'late')
             task.wait(1)
             return waited, got",
        )
        .await
        .unwrap();
    assert_eq!(text(&v), "1.5 | late");
    assert_eq!(start.elapsed(), Duration::from_millis(2500));

    // without a sleep function there's nothing to wait with
    let mut bare = Engine::new();
    bare.open_tasks();
    let err = bare.eval_async("task.wait(1)").await.unwrap_err();
    assert!(err.to_string().contains("set_sleep"), "{err}");
}

#[tokio::test(start_paused = true)]
async fn where_async_functions_can_be_called() {
    let mut e = engine();
    // not from a plain eval: nothing would resume the script
    let err = e.eval("return wait_ms(1)").unwrap_err();
    assert!(err.to_string().contains("eval_async"), "{err}");
    // not inside the script's own coroutine: the wait would end up in its
    // resumer instead of the host
    let err = e
        .eval_async("return coroutine.wrap(function() return wait_ms(1) end)()")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("task.spawn"), "{err}");
    // a plain yield at the top just gives the host's executor a turn
    let v = e
        .eval_async("local n = 0 for i = 1, 3 do coroutine.yield() n += i end return n")
        .await
        .unwrap();
    assert_eq!(text(&v), "6");
}

#[tokio::test(start_paused = true)]
async fn call_async_and_types() {
    let mut e = engine();
    e.eval("function load_both(a, b) return fetch(a) .. ' + ' .. fetch(b) end")
        .unwrap();
    let f: Function = e.get("load_both").unwrap();
    let s: String = e.call_async(&f, ("/x", "/y")).await.unwrap();
    assert_eq!(s, "contents of /x + contents of /y");

    // async natives are typed like any other
    assert_eq!(e.global_type("fetch").unwrap(), "(string) -> string");
    assert!(
        e.check("t.rhue", "--!strict\nlocal n: number = fetch('/a')")
            .is_err()
    );
}

#[tokio::test(start_paused = true)]
async fn errors_from_background_tasks() {
    let mut e = engine();
    let mut task = e
        .spawn(
            "bg",
            "task.spawn(function() fetch('/missing') end) return fetch('/ok')",
        )
        .unwrap();
    assert!(!task.is_finished());
    let errors = e.run_until_idle().await;
    assert!(task.is_finished());
    assert_eq!(errors.len(), 1);
    assert!(errors[0].to_string().contains("not found"));
    let _ = e.resume(&mut task, Vec::new());
}

#[tokio::test(start_paused = true)]
async fn dropping_the_driver_cancels() {
    let mut e = engine();
    let dropped = Rc::new(Cell::new(false));
    let d = dropped.clone();
    e.register_async("forever", move || {
        struct Flag(Rc<Cell<bool>>);
        impl Drop for Flag {
            fn drop(&mut self) {
                self.0.set(true);
            }
        }
        let flag = Flag(d.clone());
        async move {
            let _flag = flag;
            std::future::pending::<()>().await;
        }
    });
    let timed_out = tokio::time::timeout(Duration::from_secs(1), e.eval_async("forever()")).await;
    assert!(timed_out.is_err());
    // the future went with it, and nothing is left waiting
    assert!(dropped.get());
    assert!(e.run_until_idle().await.is_empty());
    assert_eq!(text(&e.eval_async("return 1").await.unwrap()), "1");
}

#[tokio::test(start_paused = true)]
async fn interrupting_a_waiting_script() {
    let mut e = engine();
    e.register_async("forever", std::future::pending::<()>);
    let handle = e.interrupt_handle();
    let (result, ()) = tokio::join!(e.eval_async("forever()"), async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        handle.interrupt();
    });
    let err = result.unwrap_err();
    assert!(err.is_interrupted(), "{err}");
    // the engine carries on
    assert_eq!(text(&e.eval_async("return 2").await.unwrap()), "2");
}

#[test]
fn interrupting_a_running_script_from_another_thread() {
    let mut e = Engine::new();
    let handle = e.interrupt_handle();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        handle.interrupt();
    });
    // pcall can't catch it, so the loop really stops
    let err = e
        .eval("while true do pcall(function() while true do end end) end")
        .unwrap_err();
    t.join().unwrap();
    assert!(err.is_interrupted(), "{err}");
    assert!(!e.eval("error('x')").unwrap_err().is_interrupted());

    // an interrupt with nothing running stops the next script
    e.interrupt_handle().interrupt();
    let err = e
        .eval("local n = 0 for i = 1, 10 do n += i end")
        .unwrap_err();
    assert!(err.is_interrupted());
    assert_eq!(e.eval("return 3").unwrap(), [Value::Number(3.0)]);
}

#[test]
fn instruction_limits_still_count_exactly_enough() {
    // the budget is handed out in slices now, the limit is still the limit
    let mut e = Engine::new();
    e.set_instruction_limit(Some(100_000));
    assert!(e.eval("local n = 0 for i = 1, 1000 do n += i end").is_ok());
    let err = e
        .eval("local n = 0 for i = 1, 1000000 do n += i end")
        .unwrap_err();
    assert!(
        err.to_string().contains("instruction limit (100000)"),
        "{err}"
    );
    assert!(!err.is_interrupted());
    // each run starts a fresh budget
    for _ in 0..5 {
        e.eval("local n = 0 for i = 1, 20000 do n += i end")
            .unwrap();
    }
}
