//! Coroutines. Each one is a [`Thread`] with its own value stack, call frames
//! and open upvalues. The running thread's state lives in the `Vm` itself.
//! `resume` parks the resumer's state in its thread object, loads the
//! coroutine's, and runs it in a nested interpreter loop until it yields,
//! returns or fails, then swaps back.
//!
//! **Yielding.** `coroutine.yield` doesn't unwind the Rust stack through
//! anything clever. It's only allowed at the depth the coroutine was resumed at
//! (`Vm::depth`), so the only Rust frames between the yield and the resume are
//! the coroutine's own dispatch loop. That loop puts its current frame back on
//! the coroutine's frame list and returns a yield signal, then the next resume
//! pops that frame, drops the resume arguments where the `yield` call's results
//! go, and keeps going. Yielding from inside a native's callback (like a
//! `table.sort` comparator) raises "attempt to yield across a C-call boundary",
//! naming the native in the way (found by walking the parked frames, so only
//! the error pays for it). Metamethods run in the same dispatch loop, so they
//! can yield. So can a native right inside `pcall` or `xpcall`, since those
//! call it from the dispatch loop too and the [`Parked`] record says to add
//! the `true`.

use std::rc::Rc;

use rhue_bytecode::Op;
use smallvec::smallvec;

use crate::heap::{CoStatus, Mark, ThreadEnd, ThreadRef, ThreadState};
use crate::{
    CANCELLED, Frame, Function, KILLED, MAX_NATIVE_DEPTH, NativeCtx, NativeError, Rethrown, Rets,
    Special, Table, Thrown, Upvalue, Value, Vm,
};

type R = Result<Rets, NativeError>;

/// Where a parked call's results go when its thread resumes.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Parked {
    /// The call's slot, its results land here.
    pub(crate) slot: usize,
    /// How many results the caller wants (`None` is all of them).
    pub(crate) want: Option<usize>,
    /// Whether `pcall` or `xpcall` sits right around the call.
    pub(crate) catch: Catch,
}

/// What catches errors for a parked call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Catch {
    None,
    /// `pcall`, so the results get a leading `true`.
    Pcall,
    /// `xpcall`, the same, and its handler waits at the call's slot.
    Xpcall,
}

impl Vm {
    /// A new suspended coroutine that will run `body`.
    pub fn new_thread(&mut self, body: Value) -> Value {
        let state = ThreadState {
            stack: vec![body],
            stack_top: 1,
            ..ThreadState::default()
        };
        Value::Thread(self.heap.alloc_thread(state, CoStatus::Suspended))
    }

    /// Resumes coroutine `co` from the host (or a native). Gives back its
    /// yielded or returned values, or the error that killed it.
    pub fn resume(&mut self, co: ThreadRef, args: &[Value]) -> Result<Vec<Value>, Rethrown> {
        self.resume_host(co, args)
    }

    /// [`resume`](Self::resume), but the call `co` is parked in raises `error`
    /// instead of returning. Under `pcall` that's `false, error`, otherwise it
    /// unwinds like any error and kills `co` if nothing catches it. A string
    /// gets the call's position, like `error("msg")` there would.
    pub fn resume_error(&mut self, co: ThreadRef, error: Value) -> Result<Vec<Value>, Rethrown> {
        // nothing's parked yet, so there's no call to raise at
        let t = self.heap.thread(co);
        if t.status == CoStatus::Suspended && self.heap.thread_state_mut(co).pending.is_none() {
            let m = "cannot resume a coroutine that hasn't started with an error";
            return Err(Rethrown(self.throw_here(m.into())));
        }
        // run_thread takes it. it's a field and not an argument so a plain
        // resume doesn't pay for carrying it
        self.raise_on_resume = Some(error);
        let result = self.resume_host(co, &[]);
        self.raise_on_resume = None;
        result
    }

    fn resume_host(&mut self, co: ThreadRef, args: &[Value]) -> Result<Vec<Value>, Rethrown> {
        if !self.is_running() {
            self.reset_budget();
        }
        self.heap.thread_mut(co).by_host = true;
        let outcome = self.resume_thread(co, args);
        // a native resumed it, and something killed that native's thread
        if self.current_mark() == Mark::Kill {
            return Err(Rethrown(self.stop_thrown(None)));
        }
        match outcome {
            Ok(Ok(values)) => Ok(values.into_vec()),
            Ok(Err(t)) => Err(Rethrown(t)),
            Err(m) => Err(Rethrown(self.throw_here(m))),
        }
    }

    /// Whether the running coroutine was last resumed by the host (or a native,
    /// through [`resume`](Self::resume)) instead of a script's
    /// `coroutine.resume` or `wrap`. If so, a yield goes back to the host.
    pub fn resumed_by_host(&self) -> bool {
        self.current != self.main_thread && self.heap.thread(self.current).by_host
    }

    /// `coroutine.status`, from the running thread's point of view.
    pub fn thread_status(&self, t: ThreadRef) -> CoStatus {
        self.heap.thread(t).status()
    }

    /// The running coroutine, or `None` on the main thread.
    pub fn running_coroutine(&self) -> Option<ThreadRef> {
        (self.current != self.main_thread).then_some(self.current)
    }

    /// The thread running right now.
    pub fn current_thread(&self) -> ThreadRef {
        self.current
    }

    pub fn is_main_thread(&self, t: ThreadRef) -> bool {
        t == self.main_thread
    }

    fn take_state(&mut self) -> ThreadState {
        ThreadState {
            stack: std::mem::take(&mut self.stack),
            stack_top: std::mem::replace(&mut self.stack_top, 0),
            frames: std::mem::take(&mut self.frames),
            open_upvalues: std::mem::take(&mut self.open_upvalues),
            boundary: std::mem::replace(&mut self.boundary, 0),
            pending: None,
        }
    }

    /// Swaps the running state with thread `r`'s parked one, field by field.
    /// That's the cheap way to park the running thread into an empty slot, or
    /// take a parked thread's state out (leaving the slot empty).
    fn swap_state(&mut self, r: ThreadRef) {
        let t = self.heap.thread_state_mut(r);
        std::mem::swap(&mut self.stack, &mut t.stack);
        std::mem::swap(&mut self.stack_top, &mut t.stack_top);
        std::mem::swap(&mut self.frames, &mut t.frames);
        std::mem::swap(&mut self.open_upvalues, &mut t.open_upvalues);
        std::mem::swap(&mut self.boundary, &mut t.boundary);
    }

    /// Runs coroutine `co` until it yields, returns or fails. The outer `Err`
    /// is a refusal (it isn't suspended), the inner one is an error raised
    /// inside it, which also kills it.
    pub(crate) fn resume_thread(
        &mut self,
        co: ThreadRef,
        args: &[Value],
    ) -> Result<Result<Rets, Thrown>, String> {
        match self.heap.thread(co).status {
            CoStatus::Suspended => {}
            CoStatus::Dead => return Err("cannot resume dead coroutine".into()),
            CoStatus::Running if co == self.current => {
                return Err("cannot resume non-suspended coroutine".into());
            }
            _ => return Err("cannot resume non-suspended coroutine".into()),
        }
        if self.depth >= MAX_NATIVE_DEPTH {
            return Err("stack overflow (coroutines nested too deeply)".into());
        }
        // put the running thread aside, it waits as `normal`. the running
        // thread's slot is empty while it runs, and so is the VM's state
        // between the two swaps
        let prev = self.current;
        self.swap_state(prev);
        self.heap.account_thread(prev);
        self.heap.thread_mut(prev).status = CoStatus::Normal;
        self.resumers.push(prev);

        self.swap_state(co);
        let pending = self.heap.thread_state_mut(co).pending.take();
        self.current = co;
        self.depth += 1;
        let depth = self.depth;
        {
            let t = self.heap.thread_mut(co);
            t.status = CoStatus::Running;
            t.base_depth = depth;
        }
        let outcome = stacker::maybe_grow(128 * 1024, 2 * 1024 * 1024, || {
            self.run_thread(pending, args)
        });
        self.depth -= 1;

        // `None` while it's only suspended
        let (result, end) = match outcome {
            Err(t) if t.yielding => {
                let values = std::mem::take(&mut self.yielded);
                (Ok(values), None)
            }
            // marked by a native it called and returned before its next call
            // or back-edge, it still counts as killed
            Ok(_) if self.marked != 0 && self.current_mark() == Mark::Kill => {
                (Err(self.stop_thrown(None)), Some(ThreadEnd::Killed))
            }
            Ok(values) => (Ok(values), Some(ThreadEnd::Finished)),
            Err(t) => {
                let end = match () {
                    () if !t.is_kill() => ThreadEnd::Failed,
                    () if t.error.message == CANCELLED => ThreadEnd::Cancelled,
                    () => ThreadEnd::Killed,
                };
                (Err(t), Some(end))
            }
        };
        match end {
            Some(end) => {
                self.close_upvalues(0);
                self.take_state();
                self.heap.account_thread(co);
                self.finish_thread(co, end);
            }
            None => {
                self.swap_state(co);
                self.heap.thread_state_mut(co).pending = self.yield_dest.take();
                self.heap.account_thread(co);
                self.heap.thread_mut(co).status = CoStatus::Suspended;
            }
        }

        self.resumers.pop();
        self.swap_state(prev);
        self.current = prev;
        self.heap.thread_mut(prev).status = CoStatus::Running;
        Ok(result)
    }

    /// Starts the loaded coroutine, or continues it from its `yield` (raising
    /// there instead if `resume_error` left an error).
    fn run_thread(&mut self, pending: Option<Parked>, args: &[Value]) -> Result<Rets, Thrown> {
        match pending {
            None => {
                let n = args.len();
                if !self.ensure_stack(1 + n) {
                    return Err(self.throw_here("stack overflow".into()));
                }
                self.stack[1..1 + n].copy_from_slice(args);
                let n = self.resolve_call(0, n).map_err(|m| self.throw_here(m))?;
                let Value::Function(callee) = self.stack[0] else {
                    unreachable!("resolve_call leaves a function")
                };
                match self.heap.function(callee) {
                    Function::Lua(_) => {
                        let frame = self
                            .enter_lua(callee, 0, n, None, false, false)
                            .map_err(|m| self.throw_here(m))?;
                        self.run(frame, 0)
                    }
                    // a native body runs nested, so it can't yield
                    Function::Native(_) => self.invoke_at(0, n, false),
                }
            }
            Some(parked) => {
                let f = self.frames.pop().expect("a suspended coroutine's frame");
                if let Some(value) = self.raise_on_resume.take() {
                    return self.raise_parked(f, parked, value);
                }
                let end = if parked.catch == Catch::None {
                    self.place_results(parked.slot, args, parked.want)
                } else {
                    let mut values = Rets::with_capacity(args.len() + 1);
                    values.push(Value::Bool(true));
                    values.extend_from_slice(args);
                    self.place_results(parked.slot, &values, parked.want)
                };
                self.run(f, if parked.want.is_none() { end } else { 0 })
            }
        }
    }

    /// Raises `value` at the call frame `f` is parked in, then carries on from
    /// wherever that lands.
    fn raise_parked(&mut self, mut f: Frame, parked: Parked, value: Value) -> Result<Rets, Thrown> {
        // the frame resumes after the call, so the call is the one before
        let pc = f.pc.saturating_sub(1);
        let error = self.error_at(&f, pc, String::new());
        // level 1 so a string gets the call's position, like error("msg")
        let thrown = self.raise(error, NativeError::Raise { value, level: 1 }, 0);
        let mut top = 0;
        match parked.catch {
            Catch::None => self.unwind(&mut f, &mut top, thrown)?,
            catch => {
                let values = if catch == Catch::Xpcall {
                    let handler = self.stack[parked.slot];
                    self.run_handler(&mut f, handler, thrown.value)?
                } else {
                    smallvec![Value::Bool(false), thrown.value]
                };
                let end = self.place_results(parked.slot, &values, parked.want);
                if parked.want.is_none() {
                    top = end;
                }
            }
        }
        self.run(f, top)
    }

    /// `coroutine.yield(...)` called from `f`. Hands the arguments to the
    /// resumer and tells the dispatch loop to suspend.
    pub(crate) fn yield_from(
        &mut self,
        f: &Frame,
        pc: usize,
        func_slot: usize,
        nargs: usize,
        want: Option<usize>,
    ) -> Result<(), Thrown> {
        if let Err(m) = self.check_yield() {
            self.stop_at_pause(f, pc)?;
            return Err(self.throw(f, pc, m));
        }
        self.yielded = Rets::from_slice(&self.stack[func_slot + 1..func_slot + 1 + nargs]);
        self.yield_dest = Some(Parked {
            slot: func_slot,
            want,
            catch: Catch::None,
        });
        Err(Thrown::yield_signal())
    }

    pub(crate) fn check_yield(&self) -> Result<(), String> {
        if self.current == self.main_thread {
            Err(OUTSIDE_COROUTINE.into())
        } else if self.depth != self.heap.thread(self.current).base_depth {
            Err(self.boundary_message())
        } else {
            Ok(())
        }
    }

    /// Why a `yield` reached through a nested call can't happen: outside any
    /// coroutine, or a native in the way.
    pub(crate) fn nested_yield_message(&self) -> String {
        if self.current == self.main_thread {
            OUTSIDE_COROUTINE.into()
        } else {
            self.boundary_message()
        }
    }

    pub(crate) fn is_yieldable(&self) -> bool {
        self.current != self.main_thread && self.depth == self.heap.thread(self.current).base_depth
    }

    /// The C-call boundary error, naming the native that can't be paused.
    #[cold]
    fn boundary_message(&self) -> String {
        match self.native_in_the_way() {
            Some(name) => format!(
                "attempt to yield across a C-call boundary: '{name}' called this and can't be paused partway, so wait before or after calling '{name}' instead"
            ),
            None => "attempt to yield across a C-call boundary: a native called this and can't be paused partway, so wait before or after that call instead".into(),
        }
    }

    /// The nearest native a script frame on this thread is waiting on, which
    /// is what a yield would have to pause partway through. A native that
    /// calls back into scripts parks its caller in `frames`, and that
    /// frame's last instruction is the call, with the native in its slot.
    fn native_in_the_way(&self) -> Option<Rc<str>> {
        self.frames.iter().rev().find_map(|f| {
            // placeholders sit at pc 0
            let ins = *f.cl.proto.code.get(f.pc.checked_sub(1)?)?;
            if !matches!(ins.op()?, Op::Call | Op::TailCall | Op::TForCall) {
                return None;
            }
            match self.stack.get(f.base + usize::from(ins.a()))? {
                Value::Function(r) => match self.heap.function(*r) {
                    Function::Native(n) => Some(n.name.clone()),
                    Function::Lua(_) => None,
                },
                _ => None,
            }
        })
    }

    /// `coroutine.resume(co, ...)` or a `coroutine.wrap` function, called from
    /// `f`. Returns the results to put at the call's slot.
    pub(crate) fn resume_from(
        &mut self,
        f: &mut Frame,
        pc: usize,
        special: Special,
        bound: Value,
        func_slot: usize,
        nargs: usize,
    ) -> Result<Rets, Thrown> {
        let args = &self.stack[func_slot + 1..func_slot + 1 + nargs];
        let (co, args) = match special {
            Special::Wrapped => (bound, Rets::from_slice(args)),
            _ => (
                args.first().copied().unwrap_or(Value::Nil),
                Rets::from_slice(args.get(1..).unwrap_or(&[])),
            ),
        };
        let Value::Thread(co) = co else {
            let m = resume_arg_error(co, nargs);
            return Err(self.throw(f, pc, m));
        };
        self.heap.thread_mut(co).by_host = false;
        // the caller waits in `frames` (now part of its thread's state), where
        // the collector and tracebacks can see it
        let placeholder = self.placeholder_frame(f.closure);
        self.frames.push(std::mem::replace(f, placeholder));
        let outcome = self.resume_thread(co, &args);
        *f = self.frames.pop().expect("resume's caller");
        // a resumer killed while it waited dies at its next charge, since
        // budget slices are one charge long while a kill's waiting
        self.finish_resume(special, outcome)
            .map_err(|e| e.unwrap_or_else(|m| self.throw(f, pc, m)))
    }

    /// Same thing for a resume that runs nested (from a native, the host, or
    /// `pcall(coroutine.resume, ...)`).
    pub(crate) fn resume_here(
        &mut self,
        special: Special,
        bound: Value,
        slot: usize,
        nargs: usize,
    ) -> Result<Rets, Thrown> {
        let args = &self.stack[slot + 1..slot + 1 + nargs];
        let (co, args) = match special {
            Special::Wrapped => (bound, Rets::from_slice(args)),
            _ => (
                args.first().copied().unwrap_or(Value::Nil),
                Rets::from_slice(args.get(1..).unwrap_or(&[])),
            ),
        };
        let Value::Thread(co) = co else {
            let m = resume_arg_error(co, nargs);
            return Err(self.throw_here(m));
        };
        let outcome = self.resume_thread(co, &args);
        if self.current_mark() == Mark::Kill {
            return Err(self.stop_thrown(None));
        }
        self.finish_resume(special, outcome)
            .map_err(|e| e.unwrap_or_else(|m| self.throw_here(m)))
    }

    /// `resume` reports the outcome as `true, ...` / `false, error`, a `wrap`
    /// function returns the values or raises the error. Errors scripts can't
    /// catch always propagate. `Err(Err(m))` is a message to raise at the call
    /// site.
    fn finish_resume(
        &mut self,
        special: Special,
        outcome: Result<Result<Rets, Thrown>, String>,
    ) -> Result<Rets, Result<Thrown, String>> {
        match (special, outcome) {
            (Special::Wrapped, Ok(Ok(values))) => Ok(values),
            (Special::Wrapped, Err(m)) => Err(Err(m)),
            (_, Ok(Ok(mut values))) => {
                values.insert(0, Value::Bool(true));
                Ok(values)
            }
            (_, Err(m)) => Ok(smallvec![Value::Bool(false), self.string(&m)]),
            // a killed coroutine is just dead to whoever resumed it
            (Special::Wrapped, Ok(Err(t))) if t.is_kill() => Err(Err(t.error.message)),
            (Special::Wrapped, Ok(Err(t))) => Err(Ok(t)),
            (_, Ok(Err(t))) if t.catchable || t.is_kill() => {
                Ok(smallvec![Value::Bool(false), t.value])
            }
            (_, Ok(Err(t))) => Err(Ok(t)),
        }
    }

    /// `coroutine.close`, kills a suspended (or dead) coroutine and closes the
    /// upvalues still open on its stack.
    pub fn close_thread(&mut self, co: ThreadRef) -> Result<(), String> {
        match self.heap.thread(co).status {
            CoStatus::Suspended => self.end_parked(co, ThreadEnd::Killed),
            CoStatus::Dead => {
                let end = self.heap.thread(co).end.unwrap_or(ThreadEnd::Killed);
                self.end_parked(co, end);
            }
            s => return Err(format!("cannot close a {} coroutine", s.name())),
        }
        Ok(())
    }

    /// Kills thread `t`, in any state. A suspended one dies right away without
    /// running any of its code. One that's running (a native killing its own
    /// thread), or waiting on a coroutine it resumed, dies the next time it
    /// runs: at its next call, loop back-edge or pause, or when it returns, so
    /// only straight-line code can run in between. Scripts can't
    /// catch it, and it stops at the thread's edge: `coroutine.resume` gives
    /// `false, "thread killed"`, a `coroutine.wrap` function raises that in
    /// its caller, and the host gets an error whose [`Rethrown::killed`] is
    /// `true`. A native killing its own thread should also return
    /// `NativeError::Uncatchable(KILLED)` so nothing runs after it.
    ///
    /// `false` for the main thread (there's
    /// [`interrupt_handle`](Self::interrupt_handle) for that) or one that's
    /// already dead.
    pub fn kill_thread(&mut self, t: ThreadRef) -> bool {
        self.stop_thread(t, Mark::Kill)
    }

    /// [`kill_thread`](Self::kill_thread), but a thread that's running gets to
    /// keep going until it next pauses (a `yield` or a native that waits), and
    /// dies there instead. One that finishes without pausing just finishes. A
    /// suspended one dies right away.
    pub fn cancel_thread(&mut self, t: ThreadRef) -> bool {
        self.stop_thread(t, Mark::Cancel)
    }

    /// How thread `t` ended, or `None` while it's alive.
    pub fn thread_end(&self, t: ThreadRef) -> Option<ThreadEnd> {
        self.heap.thread(t).end
    }

    fn stop_thread(&mut self, t: ThreadRef, mark: Mark) -> bool {
        if t == self.main_thread {
            return false;
        }
        match self.heap.thread(t).status {
            CoStatus::Dead => false,
            CoStatus::Suspended => {
                let end = if mark == Mark::Cancel {
                    ThreadEnd::Cancelled
                } else {
                    ThreadEnd::Killed
                };
                self.end_parked(t, end);
                true
            }
            CoStatus::Running | CoStatus::Normal => {
                let th = self.heap.thread_mut(t);
                // its next yield fails check_yield, which is where a pause
                // looks for the mark, so plain yields don't pay for it
                th.base_depth = usize::MAX;
                match (th.mark, mark) {
                    (Mark::None, _) => {
                        th.mark = mark;
                        self.marked += 1;
                        if mark == Mark::Kill {
                            self.kills += 1;
                        }
                    }
                    // a kill wins over a cancel
                    (Mark::Cancel, Mark::Kill) => {
                        th.mark = Mark::Kill;
                        self.kills += 1;
                    }
                    _ => {}
                }
                // whoever runs next charges straight away, and from then on
                // slices are one charge long until the kill happens
                if mark == Mark::Kill {
                    self.drain_slice();
                }
                true
            }
        }
    }

    /// A parked (or dead) thread dies on the spot: its upvalues close, its
    /// frames go, and none of its code runs.
    fn end_parked(&mut self, t: ThreadRef, end: ThreadEnd) {
        let st = self.heap.unpark_thread(t);
        for (i, u) in &st.open_upvalues {
            let i = *i;
            let v = st.stack.get(i).copied().unwrap_or(Value::Nil);
            u.set(Upvalue::Closed(v));
        }
        self.heap.park_thread(t, ThreadState::default());
        self.finish_thread(t, end);
    }

    /// Records that `t` is dead and how, and clears its mark.
    fn finish_thread(&mut self, t: ThreadRef, end: ThreadEnd) {
        let th = self.heap.thread_mut(t);
        th.status = CoStatus::Dead;
        th.end = Some(end);
        if th.mark != Mark::None {
            if th.mark == Mark::Kill {
                self.kills -= 1;
            }
            th.mark = Mark::None;
            self.marked -= 1;
        }
    }

    /// The running thread's mark. A compare while no thread has one.
    #[inline]
    pub(crate) fn current_mark(&self) -> Mark {
        if self.marked == 0 {
            Mark::None
        } else {
            self.heap.thread(self.current).mark
        }
    }

    /// A thread about to park with a kill or cancel waiting dies instead.
    #[inline]
    pub(crate) fn stop_at_pause(&mut self, f: &Frame, pc: usize) -> Result<(), Thrown> {
        if self.current_mark() == Mark::None {
            return Ok(());
        }
        Err(self.stop_thrown(Some((f, pc))))
    }

    /// The error the running thread dies with, raised at `f`'s `pc` (or
    /// wherever it is). The value scripts see is the plain message.
    #[cold]
    pub(crate) fn stop_thrown(&mut self, at: Option<(&Frame, usize)>) -> Thrown {
        let m = if self.current_mark() == Mark::Cancel {
            CANCELLED
        } else {
            KILLED
        };
        let mut t = match at {
            Some((f, pc)) => self.throw(f, pc, m.into()),
            None => self.throw_here(m.into()),
        };
        t.catchable = false;
        t.value = Value::String(self.heap.intern(m));
        self.drain_slice();
        t
    }

    /// Hands the current budget slice back to the reserve, so the next charge
    /// refills and looks for a kill. The instruction limit stays exact.
    pub(crate) fn drain_slice(&mut self) {
        self.reserve = self.reserve.saturating_add(self.budget);
        self.budget = 0;
    }
}

/// Yielding on the main thread, with what to do about it.
const OUTSIDE_COROUTINE: &str = "attempt to yield from outside a coroutine. Run it inside coroutine.wrap or coroutine.create, or have the host run it as a task";

fn resume_arg_error(got: Value, nargs: usize) -> String {
    let got = if nargs == 0 {
        "no value"
    } else {
        got.type_name()
    };
    format!("invalid argument #1 to 'resume' (thread expected, got {got})")
}

// ─ The Library ───────────────────────────────────────────────────────────────

pub(crate) fn install(vm: &mut Vm) {
    let mut table = Table::default();
    let entries: [(&str, crate::NativeFn); 6] = [
        ("create", Rc::new(create)),
        ("status", Rc::new(status)),
        ("running", Rc::new(running)),
        ("isyieldable", Rc::new(isyieldable)),
        ("close", Rc::new(close)),
        ("wrap", Rc::new(wrap)),
    ];
    for (name, func) in entries {
        let key = vm.string(name);
        let f = vm.native(name, func);
        let _ = table.set(key, f);
    }
    for (name, special) in [("resume", Special::Resume), ("yield", Special::Yield)] {
        let key = vm.string(name);
        let f = vm.special_function(name, special, Value::Nil);
        let _ = table.set(key, f);
    }
    let t = vm.heap_mut().alloc_table(table);
    vm.add_library(t);
    vm.set_global("coroutine", Value::Table(t));
}

fn thread_arg(args: &[Value], fname: &str) -> Result<ThreadRef, NativeError> {
    match args.first().copied() {
        Some(Value::Thread(t)) => Ok(t),
        other => Err(NativeError::Message(format!(
            "invalid argument #1 to '{fname}' (thread expected, got {})",
            other.map_or("no value", Value::type_name)
        ))),
    }
}

fn body_arg(args: &[Value], fname: &str) -> Result<Value, NativeError> {
    match args.first().copied() {
        Some(f @ Value::Function(_)) => Ok(f),
        other => Err(NativeError::Message(format!(
            "invalid argument #1 to '{fname}' (function expected, got {})",
            other.map_or("no value", Value::type_name)
        ))),
    }
}

fn create(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let body = body_arg(args, "create")?;
    Ok(smallvec![ctx.vm().new_thread(body)])
}

fn wrap(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let body = body_arg(args, "wrap")?;
    let vm = ctx.vm();
    let co = vm.new_thread(body);
    Ok(smallvec![vm.special_function("wrap", Special::Wrapped, co)])
}

fn status(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let t = thread_arg(args, "status")?;
    let name = ctx.heap().thread(t).status().name();
    Ok(smallvec![ctx.string(name)])
}

fn running(ctx: &mut NativeCtx<'_>, _args: &[Value]) -> R {
    let vm = ctx.vm();
    let t = vm.current;
    Ok(smallvec![
        Value::Thread(t),
        Value::Bool(t == vm.main_thread)
    ])
}

fn isyieldable(ctx: &mut NativeCtx<'_>, _args: &[Value]) -> R {
    Ok(smallvec![Value::Bool(ctx.vm().is_yieldable())])
}

fn close(ctx: &mut NativeCtx<'_>, args: &[Value]) -> R {
    let t = thread_arg(args, "close")?;
    ctx.vm().close_thread(t)?;
    Ok(smallvec![Value::Bool(true)])
}
