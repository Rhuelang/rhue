//! Scripts as tasks the host drives, and the `task` library.
//!
//! [`Engine::spawn`] runs a script (or [`Engine::spawn_function`] a function)
//! as a coroutine until it first yields, from `coroutine.yield`, from a native
//! returning [`ScriptError::yield_with`], or from `task.wait`. The host picks
//! it back up with [`Engine::resume`].
//!
//! [`Engine::open_tasks`] adds Luau's `task` library on top (`task.wait`,
//! `task.delay`, `task.spawn`, `task.defer` and `task.cancel`), run by a
//! scheduler whose clock the host moves forward with [`Engine::step`] (once a
//! frame, say). Nothing runs by itself, time only passes in `step`.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use crate::{Context, Engine, Error, Function, IntoValue, ScriptError, Thread, Value, Variadic};

/// A script or function running as a coroutine (see [`Engine::spawn`]).
#[derive(Clone, Debug)]
pub struct Task {
    thread: Thread,
    /// Shared with the scheduler, which updates it when it resumes the task
    /// (from `task.wait`, say).
    state: Rc<RefCell<TaskState>>,
}

/// Where a [`Task`] stopped.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum TaskState {
    /// It yielded these values and can be resumed.
    Suspended(Vec<Value>),
    /// It returned these values.
    Finished(Vec<Value>),
}

impl Task {
    /// Where it stopped last (whoever resumed it, the host or the scheduler).
    pub fn state(&self) -> TaskState {
        self.state.borrow().clone()
    }

    /// Whether it's returned or failed (it won't run again).
    pub fn is_finished(&self) -> bool {
        matches!(*self.state.borrow(), TaskState::Finished(_))
    }

    /// The coroutine it runs in (what `task.cancel` takes, for example).
    pub fn thread(&self) -> &Thread {
        &self.thread
    }
}

/// How a timer resumes its thread.
#[derive(Debug)]
enum Wake {
    /// `task.wait`, with the time that actually passed since it started.
    Elapsed { since: f64 },
    /// `task.delay`, with the arguments it was given.
    Args(Vec<Value>),
}

#[derive(Debug)]
struct Timer {
    at: f64,
    seq: u64,
    thread: Thread,
    wake: Wake,
}

/// The `task` library's state, shared by its natives and the engine.
#[derive(Debug, Default)]
pub(crate) struct Scheduler {
    now: f64,
    seq: u64,
    timers: Vec<Timer>,
    /// `task.defer`red threads and their arguments, in order.
    deferred: Vec<(Thread, Vec<Value>)>,
    /// Errors from tasks the scheduler (or `task.spawn`) resumed, for the next
    /// `step` to report.
    errors: Vec<Error>,
    /// The host's [`Task`]s, so resuming one here updates its state.
    tasks: Vec<(Thread, Weak<RefCell<TaskState>>)>,
}

impl Scheduler {
    fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    fn forget(&mut self, t: &Thread) {
        self.timers.retain(|x| x.thread != *t);
        self.deferred.retain(|(x, _)| x != t);
    }

    /// Forgets a task its driver gave up on.
    pub(crate) fn abandon(&mut self, t: &Thread) {
        self.forget(t);
        self.tasks.retain(|(x, _)| x != t);
    }

    /// Whether `t` waits on a timer or is deferred.
    pub(crate) fn waits(&self, t: &Thread) -> bool {
        self.timers.iter().any(|x| x.thread == *t) || self.deferred.iter().any(|(x, _)| x == t)
    }

    pub(crate) fn is_idle(&self) -> bool {
        self.timers.is_empty() && self.deferred.is_empty()
    }

    pub(crate) fn has_deferred(&self) -> bool {
        !self.deferred.is_empty()
    }

    /// Clock time until the earliest timer, if there is one.
    pub(crate) fn next_timer_in(&self) -> Option<f64> {
        self.timers
            .iter()
            .map(|t| t.at)
            .min_by(f64::total_cmp)
            .map(|at| (at - self.now).max(0.0))
    }

    pub(crate) fn take_errors(&mut self) -> Vec<Error> {
        std::mem::take(&mut self.errors)
    }
}

/// The most `step` will do in one call, so a task that keeps deferring itself
/// can't hang the host.
const MAX_DEFERRED_PER_STEP: usize = 10_000;

/// The thread `task.spawn` and friends run, a thread passed in or a new one for
/// a function.
fn thread_of(cx: &mut Context<'_>, v: &Value, f: &str) -> Result<Thread, ScriptError> {
    match v {
        Value::Thread(t) => Ok(t.clone()),
        Value::Function(func) => Ok(cx.create_thread(func)),
        other => Err(ScriptError::new(format!(
            "invalid argument #1 to '{f}' (function or thread expected, got {})",
            other.type_name()
        ))),
    }
}

/// What a waiting thread gets when it's woken: results for the call it's
/// waiting in, or an error for that call to raise.
pub(crate) enum ResumeWith {
    With(Vec<Value>),
    Error(Value),
}

/// Resumes `t`, recording an error instead of raising it, and updating the
/// host's [`Task`] for it if there is one.
pub(crate) fn run(
    cx: &mut Context<'_>,
    sched: &Rc<RefCell<Scheduler>>,
    t: &Thread,
    args: Vec<Value>,
) {
    run_with(cx, sched, t, ResumeWith::With(args));
}

/// [`run`], with an error for the waiting call to raise as the other choice.
pub(crate) fn run_with(
    cx: &mut Context<'_>,
    sched: &Rc<RefCell<Scheduler>>,
    t: &Thread,
    wake: ResumeWith,
) {
    if cx.thread_status(t) != "suspended" {
        return;
    }
    let result = match wake {
        ResumeWith::With(args) => cx.resume::<Variadic<Value>>(t, Variadic(args)),
        ResumeWith::Error(e) => cx.resume_error::<Variadic<Value>>(t, e),
    };
    let dead = cx.thread_status(t) == "dead";
    let mut st = sched.borrow_mut();
    let state = st
        .tasks
        .iter()
        .find(|(x, _)| x == t)
        .and_then(|(_, w)| w.upgrade());
    match result {
        Ok(values) => {
            if let Some(state) = state {
                *state.borrow_mut() = if dead {
                    TaskState::Finished(values.0)
                } else {
                    TaskState::Suspended(values.0)
                };
            }
        }
        Err(e) => {
            if let Some(state) = state {
                *state.borrow_mut() = TaskState::Finished(Vec::new());
            }
            // a killed task just finished, it isn't an error step() reports
            if !e.is_killed() {
                st.errors.push(Error::from_script(e));
            }
        }
    }
    if dead {
        st.tasks.retain(|(x, w)| x != t && w.strong_count() > 0);
    }
}

const TASK_TYPE: &str = "{
    wait: (number?) -> number,
    spawn: (<A...>((A...) -> ...any, A...) -> thread) & ((thread, ...any) -> thread),
    defer: (<A...>((A...) -> ...any, A...) -> thread) & ((thread, ...any) -> thread),
    delay: (<A...>(number, (A...) -> ...any, A...) -> thread) & ((number, thread, ...any) -> thread),
    cancel: (thread) -> (),
}";

impl Engine {
    /// Compiles `source` and runs it as a task until it first yields or
    /// finishes. Yielding works anywhere in it, including at the top level and
    /// inside `pcall`'d functions.
    pub fn spawn(&mut self, name: &str, source: &str) -> Result<Task, Error> {
        let chunk = self.compile(name, source)?;
        self.spawn_chunk(&chunk)
    }

    /// [`spawn`](Self::spawn) for a chunk that's already compiled (or
    /// [loaded](Self::load)), so one script can start many tasks without
    /// compiling it each time. Each gets its own thread.
    pub fn spawn_chunk(&mut self, chunk: &crate::Chunk) -> Result<Task, Error> {
        self.verify(&chunk.0)?;
        let f = self.vm.load(&chunk.0);
        let Value::Function(f) = self.context().lift(f) else {
            unreachable!("loading a chunk gives a function")
        };
        self.spawn_function(&f, Vec::new())
    }

    /// Runs `f(args...)` as a task until it first yields or finishes.
    pub fn spawn_function(&mut self, f: &Function, args: Vec<Value>) -> Result<Task, Error> {
        let thread = self.context().create_thread(f);
        let mut task = Task {
            thread,
            state: Rc::new(RefCell::new(TaskState::Suspended(Vec::new()))),
        };
        if let Some(s) = &self.tasks {
            s.borrow_mut()
                .tasks
                .push((task.thread.clone(), Rc::downgrade(&task.state)));
        }
        self.resume(&mut task, args)?;
        Ok(task)
    }

    /// Continues a suspended task, passing `args` as the results of the `yield`
    /// (or yielding native) it stopped in. An error inside it comes back as
    /// `Err` and leaves it dead, same as resuming a finished one.
    pub fn resume(&mut self, task: &mut Task, args: Vec<Value>) -> Result<(), Error> {
        let mut cx = self.context();
        let result = cx.resume::<Variadic<Value>>(&task.thread, Variadic(args));
        self.after_resume(task, result)
    }

    /// [`resume`](Self::resume), but the call the task is waiting in raises
    /// `error` instead of returning, for a native that waited and then failed.
    /// Under `pcall` the script gets `false, error` and carries on.
    pub fn resume_error(&mut self, task: &mut Task, error: impl IntoValue) -> Result<(), Error> {
        let mut cx = self.context();
        let result = cx.resume_error::<Variadic<Value>>(&task.thread, error);
        self.after_resume(task, result)
    }

    fn after_resume(
        &mut self,
        task: &mut Task,
        result: Result<Variadic<Value>, ScriptError>,
    ) -> Result<(), Error> {
        let cx = self.context();
        let values = match result {
            Ok(v) => v.0,
            Err(e) => {
                *task.state.borrow_mut() = TaskState::Finished(Vec::new());
                return Err(Error::from_script(e));
            }
        };
        *task.state.borrow_mut() = if cx.thread_status(&task.thread) == "dead" {
            TaskState::Finished(values)
        } else {
            TaskState::Suspended(values)
        };
        self.drain_deferred();
        Ok(())
    }

    /// Adds the `task` library (`wait`, `delay`, `spawn`, `defer`, `cancel`),
    /// driven by [`step`](Self::step). Its clock starts at 0 and only moves
    /// when the host calls `step`.
    pub fn open_tasks(&mut self) {
        if self.tasks.is_some() {
            return;
        }
        let sched: Rc<RefCell<Scheduler>> = Rc::default();
        let mut cx = self.context();
        let lib = cx.new_table();

        let s = sched.clone();
        let wait = cx.create_function(
            "wait",
            move |cx: &mut Context<'_>, secs: Option<f64>| -> Result<f64, ScriptError> {
                let Some(me) = cx.current_thread() else {
                    return Err(ScriptError::new(
                        "task.wait can only be called in a task (run the script with Engine::spawn)",
                    ));
                };
                let mut st = s.borrow_mut();
                let (now, seq) = (st.now, st.next_seq());
                st.timers.push(Timer {
                    at: now + secs.unwrap_or(0.0).max(0.0),
                    seq,
                    thread: me,
                    wake: Wake::Elapsed { since: now },
                });
                Err(ScriptError::yield_with(Vec::new()))
            },
        );

        let s = sched.clone();
        let spawn = cx.create_function(
            "spawn",
            move |cx: &mut Context<'_>, args: Variadic<Value>| -> Result<Thread, ScriptError> {
                let mut args = args.0;
                let target = if args.is_empty() {
                    Value::Nil
                } else {
                    args.remove(0)
                };
                let t = thread_of(cx, &target, "spawn")?;
                run(cx, &s, &t, args);
                Ok(t)
            },
        );

        let s = sched.clone();
        let defer = cx.create_function(
            "defer",
            move |cx: &mut Context<'_>, args: Variadic<Value>| -> Result<Thread, ScriptError> {
                let mut args = args.0;
                let target = if args.is_empty() {
                    Value::Nil
                } else {
                    args.remove(0)
                };
                let t = thread_of(cx, &target, "defer")?;
                s.borrow_mut().deferred.push((t.clone(), args));
                Ok(t)
            },
        );

        let s = sched.clone();
        let delay = cx.create_function(
            "delay",
            move |cx: &mut Context<'_>,
                  secs: f64,
                  args: Variadic<Value>|
                  -> Result<Thread, ScriptError> {
                let mut args = args.0;
                let target = if args.is_empty() {
                    Value::Nil
                } else {
                    args.remove(0)
                };
                let t = thread_of(cx, &target, "delay")?;
                let mut st = s.borrow_mut();
                let (now, seq) = (st.now, st.next_seq());
                st.timers.push(Timer {
                    at: now + secs.max(0.0),
                    seq,
                    thread: t.clone(),
                    wake: Wake::Args(args),
                });
                Ok(t)
            },
        );

        let s = sched.clone();
        let cancel = cx.create_function(
            "cancel",
            move |cx: &mut Context<'_>, t: Thread| -> Result<(), ScriptError> {
                s.borrow_mut().forget(&t);
                if cx.thread_status(&t) == "suspended" {
                    cx.close_thread(&t)?;
                }
                Ok(())
            },
        );

        for (name, f) in [
            ("wait", wait),
            ("spawn", spawn),
            ("defer", defer),
            ("delay", delay),
            ("cancel", cancel),
        ] {
            cx.set(&lib, name, f).expect("a string key");
        }
        let r = cx.table_ref_of(&lib);
        cx.vm.add_library(r);
        cx.set_global("task", lib);
        self.declare("task", TASK_TYPE)
            .expect("the task library's type parses");
        self.tasks = Some(sched);
    }

    /// Moves the task clock forward by `dt` seconds, then resumes every task
    /// whose `task.wait` or `task.delay` is due (earliest first), then
    /// everything deferred. Returns the errors tasks raised since the last step
    /// (one task failing doesn't stop the others). Does nothing without
    /// [`open_tasks`](Self::open_tasks).
    pub fn step(&mut self, dt: f64) -> Vec<Error> {
        let Some(sched) = self.tasks.clone() else {
            return Vec::new();
        };
        let due: Vec<Timer> = {
            let mut st = sched.borrow_mut();
            st.now += dt.max(0.0);
            let now = st.now;
            let (mut due, keep): (Vec<Timer>, Vec<Timer>) = std::mem::take(&mut st.timers)
                .into_iter()
                .partition(|t| t.at <= now);
            st.timers = keep;
            due.sort_by(|a, b| a.at.total_cmp(&b.at).then(a.seq.cmp(&b.seq)));
            due
        };
        for timer in due {
            let args = match timer.wake {
                Wake::Elapsed { since } => vec![Value::Number(sched.borrow().now - since)],
                Wake::Args(args) => args,
            };
            let mut cx = self.context();
            run(&mut cx, &sched, &timer.thread, args);
        }
        self.drain_deferred();
        std::mem::take(&mut sched.borrow_mut().errors)
    }

    /// Runs deferred threads (and whatever they defer, up to a limit).
    fn drain_deferred(&mut self) {
        let Some(sched) = self.tasks.clone() else {
            return;
        };
        let mut ran = 0;
        while ran < MAX_DEFERRED_PER_STEP {
            let batch = std::mem::take(&mut sched.borrow_mut().deferred);
            if batch.is_empty() {
                break;
            }
            for (t, args) in batch {
                ran += 1;
                let mut cx = self.context();
                run(&mut cx, &sched, &t, args);
            }
        }
    }

    /// The task clock, the total of every `step`'s `dt`.
    pub fn task_clock(&self) -> f64 {
        self.tasks.as_ref().map_or(0.0, |s| s.borrow().now)
    }

    /// How many tasks are waiting on the clock or deferred.
    pub fn pending_tasks(&self) -> usize {
        self.tasks
            .as_ref()
            .map_or(0, |s| s.borrow().timers.len() + s.borrow().deferred.len())
    }
}
