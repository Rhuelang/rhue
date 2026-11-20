//! The host's clock (`Engine::set_clock`), which is what a wasm host has to use.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use rhue::Engine;

#[test]
fn os_time_and_date_read_the_host_clock() {
    let mut e = Engine::new();
    // 2026-11-19 12:00:00 UTC
    e.set_clock(|| Duration::from_secs(1_795_089_600));
    assert_eq!(
        e.eval_as::<f64>("return os.time()").unwrap(),
        1_795_089_600.0
    );
    assert_eq!(
        e.eval_as::<String>(r#"return os.date("!%Y-%m-%d %H:%M")"#)
            .unwrap(),
        "2026-11-19 12:00"
    );
}

#[test]
fn os_clock_counts_from_when_the_clock_was_set() {
    let mut e = Engine::new();
    let now = Rc::new(Cell::new(Duration::from_secs(1000)));
    let reading = now.clone();
    e.set_clock(move || reading.get());
    assert_eq!(e.eval_as::<f64>("return os.clock()").unwrap(), 0.0);
    now.set(Duration::from_millis(1_001_500));
    assert_eq!(e.eval_as::<f64>("return os.clock()").unwrap(), 1.5);
}

#[test]
fn task_timers_follow_the_host_clock() {
    let mut e = Engine::new();
    e.open_tasks();
    let now = Rc::new(Cell::new(Duration::ZERO));
    let reading = now.clone();
    e.set_clock(move || reading.get());
    // the sleep moves the fake clock along instead of waiting
    let moved = now.clone();
    e.set_sleep(move |d| {
        moved.set(moved.get() + d);
        std::future::ready(())
    });
    let woke: f64 = block_on(e.eval_async("task.wait(5) return os.clock()")).unwrap()[0]
        .as_number()
        .unwrap();
    assert!(woke >= 5.0, "{woke}");
}

/// Runs a future that's ready as soon as nothing outside it is in the way.
fn block_on<F: std::future::Future>(f: F) -> F::Output {
    use std::task::{Context, Poll, Waker};
    let mut f = std::pin::pin!(f);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}
