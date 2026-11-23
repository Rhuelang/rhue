//! Running scripts from async Rust.
//!
//! An async native (registered with [`Engine::register_async`]) starts its
//! future, parks it against the thread that called it, and yields. A driver
//! ([`Engine::eval_async`], [`Engine::call_async`], [`Engine::run_until_idle`])
//! polls every parked future, and when one finishes it resumes that thread with
//! the result. The same driver runs the `task` library on real time, using the
//! sleep function the host hands [`Engine::set_sleep`]. Rhue doesn't pick a
//! runtime for you. The futures are `!Send`, so on tokio they go on a
//! `LocalSet` or a current-thread runtime.

use std::cell::RefCell;
use std::fmt;
use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use crate::context::Repr;
use crate::tasks::{ResumeWith, Scheduler, Task, TaskState};
use crate::{
    Context, Engine, Error, FromValues, Function, IntoValues, ScriptError, Thread, Value, Variadic,
};

type BoxFuture<T> = Pin<Box<dyn Future<Output = T>>>;

/// Turns a finished future's output into what the script gets.
pub(crate) type Finish = Box<dyn FnOnce(&mut Context<'_>) -> Result<Vec<Value>, ScriptError>>;

struct Pending {
    thread: Thread,
    future: BoxFuture<Finish>,
}

/// What the drivers share with the async natives.
#[doc(hidden)]
#[derive(Default)]
pub struct AsyncState {
    pending: Vec<Pending>,
    sleep: Option<Rc<dyn Fn(Duration) -> BoxFuture<()>>>,
    /// Errors from threads nobody's waiting on, for `run_until_idle`.
    errors: Vec<Error>,
}

impl AsyncState {
    fn forget(&mut self, t: &Thread) {
        self.pending.retain(|p| p.thread != *t);
    }

    fn waits(&self, t: &Thread) -> bool {
        self.pending.iter().any(|p| p.thread == *t)
    }
}

impl fmt::Debug for AsyncState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AsyncState")
            .field("pending", &self.pending.len())
            .field("sleep", &self.sleep.is_some())
            .finish()
    }
}

/// What an async native does when it's called: parks `future` against the
/// calling thread and yields. Called from the code `register_async` generates.
pub(crate) fn suspend(
    cx: &mut Context<'_>,
    name: &str,
    state: &Rc<RefCell<AsyncState>>,
    future: BoxFuture<Finish>,
) -> Result<(), ScriptError> {
    let Some(thread) = cx.current_thread() else {
        return Err(ScriptError::new(format!(
            "'{name}' is async, so the script has to run with eval_async, call_async or as a task"
        )));
    };
    if !cx.vm.resumed_by_host() {
        return Err(ScriptError::new(format!(
            "'{name}' is async and can't be called inside a coroutine (use task.spawn instead)"
        )));
    }
    state.borrow_mut().pending.push(Pending { thread, future });
    Err(ScriptError::yield_with(Vec::new()))
}

/// Stops a script from another task or thread. Cheap to clone, and it's `Send +
/// Sync`.
///
/// A running script stops the next time it loops or calls something (checking
/// costs nothing until then), with an error `pcall` can't catch. A script
/// waiting in [`Engine::eval_async`] stops right away. If nothing's running,
/// the next script run on the engine is the one that gets stopped.
///
/// A script that's busy computing is hogging the thread it runs on, so on a
/// single-threaded runtime this has to get called from another thread.
#[derive(Clone, Debug)]
pub struct InterruptHandle(Arc<rhue_vm::Interrupt>);

impl InterruptHandle {
    /// Asks the engine's script to stop.
    pub fn interrupt(&self) {
        self.0.interrupt();
    }
}

/// What one turn of a driver found to do.
enum Turn {
    Work {
        finished: Vec<(Thread, Finish)>,
        main_yielded: bool,
    },
    Interrupted,
    NoSleep,
    NoClock,
}

/// Cleans up after a driver's task if the driver gets dropped before it
/// finishes (an async timeout, say). Its futures get dropped and its timers
/// forgotten, so nothing resumes it.
struct Abandon {
    asyncs: Rc<RefCell<AsyncState>>,
    tasks: Option<Rc<RefCell<Scheduler>>>,
    thread: Thread,
}

impl Drop for Abandon {
    fn drop(&mut self) {
        if let Ok(mut st) = self.asyncs.try_borrow_mut() {
            st.forget(&self.thread);
        }
        if let Some(s) = &self.tasks
            && let Ok(mut s) = s.try_borrow_mut()
        {
            s.abandon(&self.thread);
        }
    }
}

impl Engine {
    /// Registers an async Rust function. Scripts call it like any other
    /// function, and the script waits (without blocking the host) until the
    /// future finishes. The parameters and result work as in
    /// [`register`](Self::register), and an `Err` result is raised in the
    /// script where it called the function.
    ///
    /// Only scripts run by a driver can call it: [`eval_async`](Self::eval_async),
    /// [`call_async`](Self::call_async), or tasks while
    /// [`run_until_idle`](Self::run_until_idle) runs.
    ///
    /// ```
    /// # async fn demo() -> Result<(), rhue::Error> {
    /// let mut engine = rhue::Engine::new();
    /// engine.register_async("double_later", |n: f64| async move { n * 2.0 });
    /// let v = engine.eval_async("return double_later(21)").await?;
    /// assert_eq!(v, [rhue::Value::Number(42.0)]);
    /// # Ok(()) }
    /// ```
    pub fn register_async<M, F: crate::native::IntoAsyncNative<M>>(&mut self, name: &str, f: F) {
        F::declare_types(&mut self.vm);
        let sig = F::signature();
        // the native itself, no script wrapper: a failed future resumes the
        // script with resume_error, which raises at the call, and pcall(native)
        // works since pcall can wrap a native that waits
        let raw = f.into_async_native(Rc::from(name), self.asyncs.clone());
        let native = self.vm.native(name, raw);
        self.vm.set_global(name, native);
        crate::userdata::declare(
            crate::type_env(&mut self.vm).declare_global(name, &sig),
            &sig,
        );
    }

    /// The function the drivers use to wait for `task.wait` and
    /// `task.delay`, such as tokio's `sleep`. Without one, a script that
    /// waits on the task clock under a driver fails.
    ///
    /// ```ignore
    /// engine.set_sleep(tokio::time::sleep);
    /// ```
    pub fn set_sleep<F, Fut>(&mut self, sleep: F)
    where
        F: Fn(Duration) -> Fut + 'static,
        Fut: Future<Output = ()> + 'static,
    {
        self.asyncs.borrow_mut().sleep = Some(Rc::new(move |d| Box::pin(sleep(d))));
    }

    /// Where `os.time`, `os.date`, `os.clock` and the `task` timers get the
    /// time: a function giving the time since the Unix epoch. Without one
    /// Rhue uses std's clocks, which `wasm32-unknown-unknown` doesn't have, so
    /// set one there. `os.clock` starts again from 0.
    ///
    /// ```ignore
    /// // in a browser, through js-sys
    /// engine.set_clock(|| Duration::from_secs_f64(js_sys::Date::now() / 1000.0));
    /// ```
    pub fn set_clock<F>(&mut self, clock: F)
    where
        F: Fn() -> Duration + 'static,
    {
        self.vm.set_clock(Rc::new(clock));
    }

    /// A handle that stops this engine's scripts (see [`InterruptHandle`]).
    pub fn interrupt_handle(&self) -> InterruptHandle {
        InterruptHandle(self.vm.interrupt_handle())
    }

    /// [`eval`](Self::eval) for scripts that call async functions or wait on
    /// the `task` clock. Dropping the future stops the script.
    pub async fn eval_async(&mut self, source: &str) -> Result<Vec<Value>, Error> {
        let mut task = self.spawn("<eval>", source)?;
        self.finish_task(&mut task).await
    }

    /// Calls `f(args...)` the way [`eval_async`](Self::eval_async) runs a
    /// script, converting its results.
    pub async fn call_async<R: FromValues>(
        &mut self,
        f: &Function,
        args: impl IntoValues,
    ) -> Result<R, Error> {
        let args = args.into_values(&mut self.context());
        let mut task = self.spawn_function(f, args)?;
        let values = self.finish_task(&mut task).await?;
        let mut cx = self.context();
        R::from_values(values, &mut cx).map_err(|e| {
            Error::from_script(ScriptError::new(format!(
                "invalid result #{} ({})",
                e.index, e.error
            )))
        })
    }

    /// Drives everything waiting (async calls, `task` timers, deferred tasks)
    /// until nothing's left, and returns the errors scripts raised along the
    /// way. Tasks started with [`spawn`](Self::spawn) or `task.spawn` that call
    /// async functions finish here.
    pub async fn run_until_idle(&mut self) -> Vec<Error> {
        if let Err(e) = self.drive(None).await {
            self.asyncs.borrow_mut().errors.push(e);
        }
        let mut errors = std::mem::take(&mut self.asyncs.borrow_mut().errors);
        if let Some(s) = &self.tasks {
            errors.extend(s.borrow_mut().take_errors());
        }
        errors
    }

    async fn finish_task(&mut self, task: &mut Task) -> Result<Vec<Value>, Error> {
        let _abandon = Abandon {
            asyncs: self.asyncs.clone(),
            tasks: self.tasks.clone(),
            thread: task.thread().clone(),
        };
        self.drive(Some(task)).await?;
        match task.state() {
            TaskState::Finished(values) => Ok(values),
            TaskState::Suspended(_) => unreachable!("drive returns once its task finishes"),
        }
    }

    /// Whether `t` is waiting on something a driver will resume it for.
    fn is_waiting(&self, t: &Thread) -> bool {
        self.asyncs.borrow().waits(t) || self.tasks.as_ref().is_some_and(|s| s.borrow().waits(t))
    }

    fn idle(&self) -> bool {
        self.asyncs.borrow().pending.is_empty()
            && self.tasks.as_ref().is_none_or(|s| s.borrow().is_idle())
    }

    /// The driver. Runs until `main` finishes (or without one, until nothing's
    /// waiting). Errors from `main` come back, others get kept for
    /// `run_until_idle`.
    async fn drive(&mut self, mut main: Option<&mut Task>) -> Result<(), Error> {
        let interrupt = self.vm.interrupt_handle();
        let asyncs = self.asyncs.clone();
        let tasks = self.tasks.clone();
        // times here are the VM's uptime, from its clock (std's, or the host's)
        let uptime = self.vm.uptime_fn();
        let mut clock = uptime().unwrap_or_default();
        let mut sleeping: Option<(Duration, BoxFuture<()>)> = None;
        // a deadline the host's sleep said has passed. the host's sleep is the
        // one that counts (it might be a test's paused clock), so time is at
        // least this far along even if the clock disagrees
        let mut reached: Option<Duration> = None;
        let mut yielded_to_host = false;
        loop {
            match &main {
                Some(t) if t.is_finished() => return Ok(()),
                None if self.idle() => return Ok(()),
                _ => {}
            }
            let main_thread = main.as_ref().map(|t| t.thread().clone());
            // suspended but not waiting on anything, a plain `coroutine.yield`
            // at the top, which hands control back to the host's executor
            let main_yielded = main_thread.as_ref().is_some_and(|t| !self.is_waiting(t));

            let turn = poll_fn(|cx| {
                if interrupt.take() {
                    return Poll::Ready(Turn::Interrupted);
                }
                interrupt.register(cx.waker());
                let mut finished = Vec::new();
                {
                    let mut st = asyncs.borrow_mut();
                    let mut i = 0;
                    while i < st.pending.len() {
                        match st.pending[i].future.as_mut().poll(cx) {
                            Poll::Ready(finish) => {
                                let p = st.pending.remove(i);
                                finished.push((p.thread, finish));
                            }
                            Poll::Pending => i += 1,
                        }
                    }
                }
                let mut due = false;
                if let Some(sched) = &tasks {
                    let s = sched.borrow();
                    if s.has_deferred() {
                        due = true;
                    } else if let Some(wait) = s.next_timer_in() {
                        let at = clock + Duration::from_secs_f64(wait);
                        let Some(now) = uptime() else {
                            return Poll::Ready(Turn::NoClock);
                        };
                        if now.max(reached.unwrap_or(clock)) >= at {
                            due = true;
                        } else {
                            let sleep = asyncs.borrow().sleep.clone();
                            let Some(sleep) = sleep else {
                                return Poll::Ready(Turn::NoSleep);
                            };
                            if sleeping.as_ref().is_none_or(|(when, _)| *when != at) {
                                sleeping = Some((at, sleep(at.saturating_sub(now.max(clock)))));
                            }
                            if let Some((_, f)) = &mut sleeping
                                && f.as_mut().poll(cx).is_ready()
                            {
                                sleeping = None;
                                reached = Some(at);
                                due = true;
                            }
                        }
                    }
                }
                if main_yielded && !yielded_to_host {
                    // let the executor run something else first
                    yielded_to_host = true;
                    cx.waker().wake_by_ref();
                    return Poll::Pending;
                }
                if !finished.is_empty() || due || main_yielded {
                    Poll::Ready(Turn::Work {
                        finished,
                        main_yielded,
                    })
                } else {
                    Poll::Pending
                }
            })
            .await;

            let (finished, main_yielded) = match turn {
                Turn::Work {
                    finished,
                    main_yielded,
                } => (finished, main_yielded),
                Turn::Interrupted => {
                    return Err(Error::from_script(ScriptError::new(rhue_vm::INTERRUPTED)));
                }
                Turn::NoClock => {
                    return Err(Error::from_script(ScriptError::new(
                        "a script waited on the task clock, but there's no clock to read here (Engine::set_clock)",
                    )));
                }
                Turn::NoSleep => {
                    return Err(Error::from_script(ScriptError::new(
                        "a script waited on the task clock, but the engine has no sleep function (Engine::set_sleep)",
                    )));
                }
            };
            yielded_to_host = false;
            for (thread, finish) in finished {
                let wake = match finish(&mut self.context()) {
                    Ok(values) => ResumeWith::With(values),
                    Err(e) => ResumeWith::Error(error_value(e)),
                };
                self.resume_waiting(&thread, wake, &mut main)?;
            }
            if main_yielded
                && let Some(t) = main.as_deref_mut()
                && !t.is_finished()
            {
                self.resume(t, Vec::new())?;
            }
            // the task clock follows real time while a driver runs
            let now = uptime()
                .unwrap_or(clock)
                .max(reached.take().unwrap_or(clock));
            let dt = now.saturating_sub(clock).as_secs_f64();
            clock = now;
            if tasks.is_some() {
                let errors = self.step(dt);
                self.asyncs.borrow_mut().errors.extend(errors);
            }
        }
    }

    /// Resumes a thread whose future finished: the driver's own task (whose
    /// error is the driver's), a scheduled task, or a bare thread.
    fn resume_waiting(
        &mut self,
        thread: &Thread,
        wake: ResumeWith,
        main: &mut Option<&mut Task>,
    ) -> Result<(), Error> {
        if let Some(t) = main.as_deref_mut()
            && t.thread() == thread
        {
            return match wake {
                ResumeWith::With(values) => self.resume(t, values),
                ResumeWith::Error(e) => self.resume_error(t, e),
            };
        }
        if self.context().thread_status(thread) != "suspended" {
            return Ok(());
        }
        match self.tasks.clone() {
            Some(s) => crate::tasks::run_with(&mut self.context(), &s, thread, wake),
            None => {
                let mut cx = self.context();
                let result = match wake {
                    ResumeWith::With(values) => {
                        cx.resume::<Variadic<Value>>(thread, Variadic(values))
                    }
                    ResumeWith::Error(e) => cx.resume_error::<Variadic<Value>>(thread, e),
                };
                if let Err(e) = result {
                    self.asyncs.borrow_mut().errors.push(Error::from_script(e));
                }
            }
        }
        Ok(())
    }
}

/// What a failed async call raises in the script, the error's value if it has
/// one, otherwise its message.
fn error_value(e: ScriptError) -> Value {
    match e.0 {
        Repr::Value(v) => v,
        _ => Value::from(e.message()),
    }
}
