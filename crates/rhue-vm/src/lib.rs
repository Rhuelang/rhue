//! The Rhue virtual machine, a register-based interpreter for [`Proto`]s.
//!
//! This is part of [Rhue](https://docs.rs/rhue)'s guts. If you're embedding
//! Rhue, use the `rhue` crate, that's the API that stays stable.
//!
//! **Stack and frames.** All registers live in one `Vec<Value>`. Calling a Lua
//! function pushes a `Frame` whose registers start right above the callee's
//! slot (`base`), so the arguments are already where the callee's parameters
//! are. Results get copied back down to the callee's slot. Calls never recurse
//! on the Rust stack, so deep Lua recursion is bounded by [`MAX_FRAMES`] and
//! not the host's stack size.
//!
//! **Upvalues.** While a captured variable's register is live, closures reach
//! it through an *open* upvalue (a stack index). `CLOSE`, or returning from the
//! frame, copies the value into the cell (*closed*). Open upvalues are kept in
//! a map by stack index, so two closures over the same variable share one cell.
//!
//! **Errors.** A raised error carries a Lua value (for `pcall`) and a
//! [`RuntimeError`] (for the host). `pcall` marks the frame it starts as
//! *protected*, and raising unwinds frames (closing their upvalues) until it
//! hits one, whose caller then gets `false, value`.
//!
//! **Garbage collection.** At the top of the dispatch loop (a safe point, every
//! live value is in a register, a frame, a global, an upvalue cell or a host
//! pin) the VM collects once the heap grows past its threshold.

mod bitlib;
mod buffer;
mod clock;
mod coroutine;
mod debug;
mod debuglib;
mod fastcall;
pub mod hash;
mod hashpart;
pub mod heap;
mod meta;
mod oslib;
pub mod pattern;
mod reload;
mod stdlib;
mod strpat;
pub mod table;
mod value;
mod vectorlib;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use hash::{FxHashMap, FxIndexMap};
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Waker;

pub use debug::{DebugCtx, DebugEvent, DebugHook, FrameInfo, HookMask};
pub use heap::{
    BufRef, CoStatus, Constants, Fast, FastFn, FnRef, Function, GcRoot, Heap, HeapStats,
    LuaClosure, Native, NativeFn, Rets, Special, StrRef, TableRef, Thread, ThreadRef, UdRef,
    UpvalRef, Upvalue, UserData,
};
pub use reload::SwapStats;
pub use smallvec::{SmallVec, smallvec};
pub use table::{Key, KeyError, Table};
pub use value::{Value, vector_text};

use rhue_bytecode::{ArithOp, Constant, Instr, Op, Proto, arith, format_number};
use rhue_common::{Diagnostic, SourceFile, Span};

/// Max call depth before `stack overflow`.
pub const MAX_FRAMES: usize = 10_000;
/// Max stack slots across all frames.
pub const MAX_STACK: usize = 1_000_000;

/// One entry of a runtime error's traceback: a function and where it was (for
/// callers, the call site).
#[derive(Clone, Debug, PartialEq)]
pub struct TraceEntry {
    pub function: String,
    pub span: Span,
    pub source: Rc<SourceFile>,
}

/// An error raised while running bytecode.
#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeError {
    pub message: String,
    pub span: Span,
    /// The source `span` refers to.
    pub source: Rc<SourceFile>,
    /// Innermost first, the failing function and then each caller.
    pub traceback: Vec<TraceEntry>,
}

impl RuntimeError {
    pub fn to_diagnostic(&self) -> Diagnostic {
        let mut d = Diagnostic::error(&self.message, self.span).with_label("error raised here");
        if self.traceback.len() > 1 {
            d = d.with_note(self.traceback_text());
        }
        d
    }

    /// `stack traceback:` and then one line per frame. Runs of identical frames
    /// (deep recursion) collapse into one line with a count, and really long
    /// traces keep their first and last few entries.
    pub fn traceback_text(&self) -> String {
        let mut groups: Vec<(usize, &TraceEntry, usize)> = Vec::new();
        for (i, e) in self.traceback.iter().enumerate() {
            match groups.last_mut() {
                Some((_, last, count))
                    if i > 1 && last.function == e.function && last.span == e.span =>
                {
                    *count += 1
                }
                _ => groups.push((i, e, 1)),
            }
        }
        const HEAD: usize = 8;
        const TAIL: usize = 3;
        let mut lines = vec!["stack traceback:".to_string()];
        let line = |(i, e, count): &(usize, &TraceEntry, usize)| {
            let mut l = trace_line(e, *i > 0);
            if *count > 1 {
                l.push_str(&format!(" (repeated {count} times)"));
            }
            l
        };
        if groups.len() <= HEAD + TAIL + 1 {
            lines.extend(groups.iter().map(line));
        } else {
            lines.extend(groups[..HEAD].iter().map(line));
            lines.push(format!("  ... {} more", groups.len() - HEAD - TAIL));
            lines.extend(groups[groups.len() - TAIL..].iter().map(line));
        }
        lines.join("\n")
    }
}

fn trace_line(e: &TraceEntry, is_caller: bool) -> String {
    let (line, col) = e.source.line_col(e.span.start);
    let verb = if is_caller { "called from" } else { "in" };
    format!("  {verb} {} ({}:{line}:{col})", e.function, e.source.name)
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RuntimeError {}

/// An error raised by a native function.
#[derive(Clone, Debug, PartialEq)]
pub enum NativeError {
    /// A message. Becomes `"file:line: message"` when `pcall` catches it.
    Message(String),
    /// `error(value, level)`, raise any value. Strings get a position prefix
    /// for `level` 1 (where `error` was called) or 2 (the caller of that
    /// function), level 0 adds none.
    Raise { value: Value, level: u32 },
    /// An error from a function the native called through [`NativeCtx::call`],
    /// passed on as-is (same value, same traceback).
    Rethrow(Box<Rethrown>),
    /// The memory limit would be exceeded (see [`NativeCtx::reserve`]). Scripts
    /// can't catch it.
    OutOfMemory,
    /// An error scripts can't catch, like running out of instructions or
    /// getting interrupted (see [`NativeCtx::charge`]).
    Uncatchable(String),
    /// Not an error. Suspends the running coroutine and hands these values to
    /// whoever resumed it (like `coroutine.yield`), and the values the next
    /// resume passes become the native's results. Only works when a script
    /// called the native directly, in a coroutine. Anywhere else it fails the
    /// same way yielding across a C-call boundary would.
    Yield(Rets),
}

/// An error that escaped a [`NativeCtx::call`]. Return it (as
/// `NativeError::Rethrow`, which `?` does) to let it keep going up, or inspect
/// it to handle it.
#[derive(Clone, Debug, PartialEq)]
pub struct Rethrown(Thrown);

impl Rethrown {
    /// The value `pcall` would see.
    pub fn value(&self) -> Value {
        self.0.value
    }

    /// The host-facing error, with its traceback.
    pub fn error(&self) -> &RuntimeError {
        &self.0.error
    }

    /// `false` for errors scripts can't catch (the instruction limit).
    pub fn catchable(&self) -> bool {
        self.0.catchable
    }
}

impl From<String> for NativeError {
    fn from(m: String) -> Self {
        NativeError::Message(m)
    }
}

impl From<&str> for NativeError {
    fn from(m: &str) -> Self {
        NativeError::Message(m.to_string())
    }
}

/// What a native can touch while it runs: the heap, output, and (through
/// [`call`](Self::call)) other functions, scripts included.
///
/// **Rooting.** The arguments a native gets stay rooted for the whole call. A
/// value the native makes or takes out of a table isn't rooted by itself, so if
/// the native calls back into a script while holding one only in a Rust
/// variable, the collector might free it. Keep it in a rooted table, or pass it
/// to [`root`](Self::root).
pub struct NativeCtx<'a> {
    vm: &'a mut Vm,
    /// The Lua frame that called this native, if any. It only moves into the
    /// VM's frame list when the native reaches the whole VM (to call a
    /// function, say), since tracebacks and the collector have to see it there,
    /// but most natives never get that far so they skip the move.
    caller: Option<&'a mut Frame>,
    parked: bool,
}

impl Drop for NativeCtx<'_> {
    fn drop(&mut self) {
        if self.parked
            && let Some(f) = self.caller.as_deref_mut()
        {
            *f = self.vm.frames.pop().expect("the native's parked caller");
        }
    }
}

impl NativeCtx<'_> {
    /// Moves the calling frame into the VM's frame list (once).
    fn park(&mut self) {
        if !self.parked
            && let Some(f) = self.caller.as_deref_mut()
        {
            let placeholder = self.vm.placeholder_frame(f.closure);
            self.vm.frames.push(std::mem::replace(f, placeholder));
            self.parked = true;
        }
    }

    pub fn heap(&self) -> &Heap {
        &self.vm.heap
    }

    pub fn heap_mut(&mut self) -> &mut Heap {
        &mut self.vm.heap
    }

    /// `tostring` through `__tostring`. The string it returned, or `None` when
    /// `v` has no `__tostring`.
    pub fn tostring_meta(&mut self, v: Value) -> Result<Option<Value>, NativeError> {
        if let Value::Table(t) = v
            && self.vm.heap.table(t).meta().is_some()
        {
            self.park();
            return self.vm.tostring_meta(v);
        }
        Ok(None)
    }

    /// Checks that `bytes` more can be allocated under the memory limit, for a
    /// native about to build something big (`string.rep`, `table.concat`).
    /// Fails with [`NativeError::OutOfMemory`].
    pub fn reserve(&self, bytes: usize) -> Result<(), NativeError> {
        if self.vm.heap.would_exceed(bytes) {
            Err(NativeError::OutOfMemory)
        } else {
            Ok(())
        }
    }

    /// Charges `n` instructions to the script's budget, for a native whose work
    /// grows with its arguments (`table.move` over a huge range, say). Without
    /// this, time spent in a native is free and an interrupt can't reach it.
    /// Fails once the budget's gone or the script got interrupted.
    pub fn charge(&mut self, n: u64) -> Result<(), NativeError> {
        self.vm.charge(n)
    }

    /// Takes the rest of the script's budget out of the VM, for a loop that
    /// counts its own steps (see [`Meter`]). Give it back with
    /// [`return_budget`](Self::return_budget) before calling anything.
    pub fn meter(&mut self) -> Rc<Meter> {
        // the VM keeps one around, so taking it doesn't touch the interrupt's
        // reference count
        let vm = &mut *self.vm;
        let meter = vm.meter.clone();
        meter.load(vm.budget, vm.reserve);
        vm.budget = 0;
        vm.reserve = 0;
        meter
    }

    /// Puts what's left of `meter`'s budget back into the VM. The meter is
    /// empty afterwards, refill it with [`refill`](Self::refill).
    pub fn return_budget(&mut self, meter: &Meter) {
        // what the native spent comes out of the slice it started on, so a loop
        // that keeps calling natives still runs it dry and gets the interrupt
        // checked
        let left = meter.drain();
        let used = meter.taken.get().saturating_sub(left);
        let vm = &mut *self.vm;
        vm.budget = meter.slice.get().saturating_sub(used).min(left);
        vm.reserve = left - vm.budget;
    }

    /// Takes the budget back out into `meter` (after a
    /// [`return_budget`](Self::return_budget)).
    pub fn refill(&mut self, meter: &Meter) {
        let vm = &mut *self.vm;
        meter.load(vm.budget, vm.reserve);
        vm.budget = 0;
        vm.reserve = 0;
    }

    /// The VM this native runs in.
    pub fn vm(&mut self) -> &mut Vm {
        self.park();
        self.vm
    }

    /// Formats a value like `tostring`.
    pub fn display(&self, v: Value) -> String {
        v.display(&self.vm.heap)
    }

    pub fn string(&mut self, text: &str) -> Value {
        Value::String(self.vm.heap.intern(text))
    }

    pub fn str(&self, v: Value) -> Option<&str> {
        match v {
            Value::String(s) => Some(self.vm.heap.get(s)),
            _ => None,
        }
    }

    pub fn new_table(&mut self, table: Table) -> Value {
        Value::Table(self.vm.heap.alloc_table(table))
    }

    /// Writes a line of output (where `print` goes).
    pub fn print(&mut self, line: &str) {
        (self.vm.output)(line);
    }

    /// Keeps `v` alive until this native returns.
    pub fn root(&mut self, v: Value) {
        self.vm.native_roots.push(v);
    }

    /// Calls any function value (a script function, a native, `pcall`) and
    /// returns all its results. An error comes back as `NativeError::Rethrow`,
    /// and returning it with `?` lets it propagate.
    pub fn call(&mut self, func: Value, args: &[Value]) -> Result<Rets, NativeError> {
        self.park();
        self.vm
            .call_top_thrown(func, args, false)
            .map_err(|t| NativeError::Rethrow(Box::new(Rethrown(t))))
    }
}

/// A call in progress.
pub(crate) struct Frame {
    closure: FnRef,
    /// The closure's prototype, constants and upvalues (one `Rc`, so entering
    /// and leaving a frame is cheap. frames are kept small since every call
    /// moves one).
    cl: Rc<LuaClosure>,
    pc: usize,
    /// Absolute stack index of register 0.
    base: usize,
    /// Arguments past the declared parameters, for `...`.
    varargs: Box<[Value]>,
    /// Where results go (the callee's own slot) and how many the caller wants
    /// (`ALL_RESULTS` means all, which sets `top`).
    ret_to: u32,
    ret_want: u32,
    /// Started by `pcall`. Errors stop here, and normal results get a leading
    /// `true`.
    protected: bool,
    is_main: bool,
    /// A metamethod frame for `__eq`/`__lt`/`__le`. When it returns, its first
    /// result decides the caller's jump (`Some(expected)`).
    cond: Option<bool>,
    /// The debug hook was told about this call.
    call_reported: bool,
    /// Set when this frame got entered by tail calls: where they came from, for
    /// tracebacks (their frames are gone). Reused, not reallocated, by each
    /// tail call after.
    trail: Option<Box<TailTrail>>,
}

/// The most recent tail calls that led to a frame, newest first.
#[derive(Default)]
struct TailTrail {
    sites: [Option<TailSite>; 2],
    /// Older tail calls that don't fit anymore.
    elided: u32,
}

#[derive(Clone, Copy)]
struct TailSite {
    closure: FnRef,
    /// Past the `TAILCALL`, like a waiting caller's `pc`.
    pc: u32,
    is_main: bool,
}

impl TailTrail {
    fn push(&mut self, site: TailSite) {
        if self.sites[1].is_some() {
            self.elided += 1;
        }
        self.sites[1] = self.sites[0];
        self.sites[0] = Some(site);
    }
}

const ALL_RESULTS: u32 = u32::MAX;

impl Frame {
    /// The values this frame keeps alive besides its registers.
    pub(crate) fn for_each_root(&self, mut mark: impl FnMut(Value)) {
        mark(Value::Function(self.closure));
        if let Some(trail) = &self.trail {
            for site in trail.sites.iter().flatten() {
                mark(Value::Function(site.closure));
            }
        }
        for &v in self.varargs.iter() {
            mark(v);
        }
    }

    fn proto(&self) -> &Proto {
        &self.cl.proto
    }

    fn ret_to(&self) -> usize {
        self.ret_to as usize
    }

    fn ret_want(&self) -> Option<usize> {
        (self.ret_want != ALL_RESULTS).then_some(self.ret_want as usize)
    }
}

/// A raised error on its way up the stack.
#[derive(Clone, Debug, PartialEq)]
struct Thrown {
    error: RuntimeError,
    /// The Lua value `pcall` hands back.
    value: Value,
    /// Instruction-limit errors can't be caught by scripts.
    catchable: bool,
    /// Not an error, `coroutine.yield` suspending the running coroutine (it
    /// passes through to the coroutine's base dispatch loop).
    yielding: bool,
}

impl Thrown {
    fn yield_signal() -> Thrown {
        thread_local! {
            // made once, a yield happens on every generator step
            static SOURCE: Rc<SourceFile> = SourceFile::new("[yield]", "");
        }
        // never shown, `run` and `resume_thread` catch it by `yielding`.
        // nothing here allocates, yields happen a lot
        Thrown {
            error: RuntimeError {
                message: String::new(),
                span: Span::default(),
                source: SOURCE.with(Rc::clone),
                traceback: Vec::new(),
            },
            value: Value::Nil,
            catchable: false,
            yielding: true,
        }
    }
}

/// An object kept alive for the host (see [`Vm::pin`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PinKey {
    Table(TableRef),
    Function(FnRef),
    UserData(UdRef),
    Thread(ThreadRef),
    Buffer(BufRef),
}

/// How scripts see one kind of userdata: `obj.field` calls a getter, `obj.field
/// = v` a setter, and `obj:method(...)` finds a function here. Natives get the
/// object itself as their first argument.
struct UdType {
    name: Rc<str>,
    methods: FxHashMap<StrRef, Value>,
    getters: FxHashMap<StrRef, NativeFn>,
    setters: FxHashMap<StrRef, NativeFn>,
}

type PinTable = Rc<RefCell<HashMap<PinKey, usize>>>;

/// How much of the instruction budget gets handed out at once. Every time a
/// slice runs out the VM checks for an [`Interrupt`], so scripts notice one
/// within about this many loop iterations or calls.
const BUDGET_SLICE: u64 = 1 << 14;

/// The error a script stops with when it gets interrupted.
pub const INTERRUPTED: &str = "script interrupted";

fn limit_message(limit: Option<u64>) -> String {
    format!(
        "script exceeded its instruction limit ({})",
        limit.unwrap_or(u64::MAX)
    )
}

/// How many steps a [`Meter`] takes between looks at the interrupt flag.
const METER_CHECK: u64 = 1 << 12;

/// Instruction budget a native takes out of the VM (see [`NativeCtx::meter`])
/// to count steps of its own long loop without going through the
/// [`NativeCtx`] every time, the pattern matcher's backtracking say. Hand it
/// back with [`NativeCtx::return_budget`] before anything else runs, scripts
/// included.
#[derive(Debug)]
pub struct Meter {
    /// Steps left before the next look at the interrupt flag.
    chunk: Cell<u64>,
    /// The rest of the budget, past `chunk`.
    rest: Cell<u64>,
    limit: Cell<Option<u64>>,
    interrupt: Option<Arc<Interrupt>>,
    /// Whether it ran out (or got interrupted), so the error it gave isn't
    /// one scripts can catch.
    stopped: Cell<bool>,
    /// What was left of the VM's slice, and of the whole budget, when the
    /// meter took it. Handing back a fresh slice instead would mean the VM
    /// never runs one dry, and the interrupt only gets read when it does.
    slice: Cell<u64>,
    taken: Cell<u64>,
}

impl Meter {
    fn with_budget(left: u64, limit: Option<u64>, interrupt: Option<Arc<Interrupt>>) -> Self {
        // the first chunk skips the interrupt check, the VM just did one or is
        // about to
        let chunk = left.min(METER_CHECK);
        Meter {
            chunk: Cell::new(chunk),
            rest: Cell::new(left - chunk),
            limit: Cell::new(limit),
            interrupt,
            stopped: Cell::new(false),
            slice: Cell::new(0),
            taken: Cell::new(0),
        }
    }

    /// A meter that never runs out, for code with no VM around.
    pub fn unlimited() -> Self {
        Self::with_budget(u64::MAX, None, None)
    }

    /// Spends one step. Fails once the budget's gone or the script got
    /// interrupted. The error's a plain `String` so a matcher's results stay
    /// small, turn it into the right [`NativeError`] with [`fail`](Self::fail).
    #[inline]
    pub fn tick(&self) -> Result<(), String> {
        let c = self.chunk.get();
        if c == 0 {
            return self.next_chunk();
        }
        self.chunk.set(c - 1);
        Ok(())
    }

    #[cold]
    #[inline(never)]
    fn next_chunk(&self) -> Result<(), String> {
        if self.interrupt.as_ref().is_some_and(|i| i.take()) {
            self.stopped.set(true);
            return Err(INTERRUPTED.into());
        }
        let rest = self.rest.get();
        if rest == 0 {
            self.stopped.set(true);
            return Err(limit_message(self.limit.get()));
        }
        let take = rest.min(METER_CHECK);
        self.rest.set(rest - take);
        // this step comes out of the new chunk
        self.chunk.set(take - 1);
        Ok(())
    }

    /// Takes up to a chunk of steps out at once, for a loop that keeps its own
    /// count (give back what it doesn't use with [`give_back`](Self::give_back)).
    /// Fails like [`tick`](Self::tick) when there's nothing left.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "stack slots and pcs stay far under u32 (the stack and code are capped), opcodes are a byte"
    )]
    pub fn take_chunk(&self) -> Result<u32, String> {
        if self.chunk.get() == 0 {
            self.next_chunk()?;
            // next_chunk spent one step for its caller, this one's the caller
            self.chunk.set(self.chunk.get() + 1);
        }
        Ok(self.chunk.replace(0) as u32)
    }

    /// Returns steps taken with [`take_chunk`](Self::take_chunk) and not used.
    pub fn give_back(&self, steps: u32) {
        self.chunk.set(self.chunk.get() + u64::from(steps));
    }

    /// The error for a message that came out of code using this meter: one
    /// scripts can't catch if the meter's the one that stopped it.
    pub fn fail(&self, message: String) -> NativeError {
        if self.stopped.get() {
            NativeError::Uncatchable(message)
        } else {
            NativeError::Message(message)
        }
    }

    /// Loads the VM's budget into the meter, for a fresh use: `slice` is what
    /// was left of the current slice, `reserve` the rest.
    fn load(&self, slice: u64, reserve: u64) {
        let left = slice.saturating_add(reserve);
        let chunk = left.min(METER_CHECK);
        self.chunk.set(chunk);
        self.rest.set(left - chunk);
        self.stopped.set(false);
        self.slice.set(slice);
        self.taken.set(left);
    }

    /// Takes everything that's left out of the meter.
    fn drain(&self) -> u64 {
        self.chunk.replace(0).saturating_add(self.rest.replace(0))
    }
}

/// Stops a VM's running script from another task or thread. The script fails
/// with [`INTERRUPTED`] (which `pcall` can't catch) the next time it loops or
/// calls something. If nothing's running, the next script run is the one that
/// gets stopped. Hosts waiting on a script can register a waker to hear about
/// it.
#[derive(Debug, Default)]
pub struct Interrupt {
    flag: AtomicBool,
    waker: Mutex<Option<Waker>>,
}

impl Interrupt {
    /// Asks the script to stop.
    pub fn interrupt(&self) {
        self.flag.store(true, Ordering::Release);
        if let Some(w) = self.waker.lock().unwrap_or_else(|e| e.into_inner()).take() {
            w.wake();
        }
    }

    /// Whether there's an interrupt waiting to be delivered.
    pub fn is_set(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    /// Takes a waiting interrupt, for a host that delivers it itself (a script
    /// waiting on the host instead of running).
    pub fn take(&self) -> bool {
        self.flag.swap(false, Ordering::AcqRel)
    }

    /// Wakes `waker` when [`interrupt`](Self::interrupt) is called.
    pub fn register(&self, waker: &Waker) {
        *self.waker.lock().unwrap_or_else(|e| e.into_inner()) = Some(waker.clone());
    }
}

/// Keeps a table or function alive across garbage collections while the host
/// holds it. Unpins on drop.
pub struct Pin {
    key: PinKey,
    pins: Weak<RefCell<HashMap<PinKey, usize>>>,
    vm_id: u64,
}

impl Pin {
    pub fn vm_id(&self) -> u64 {
        self.vm_id
    }
}

impl std::fmt::Debug for Pin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Pin({:?})", self.key)
    }
}

impl Drop for Pin {
    fn drop(&mut self) {
        if let Some(pins) = self.pins.upgrade() {
            let mut pins = pins.borrow_mut();
            if let Some(n) = pins.get_mut(&self.key) {
                *n -= 1;
                if *n == 0 {
                    pins.remove(&self.key);
                }
            }
        }
    }
}

static NEXT_VM_ID: AtomicU64 = AtomicU64::new(1);

pub struct Vm {
    id: u64,
    heap: Heap,
    /// Insertion-ordered so `GETGLOBAL` can use inline-cache hints.
    globals: FxIndexMap<StrRef, Value>,
    stack: Vec<Value>,
    /// How much of `stack` is in use, frames and pending calls live below it.
    /// Slots above it are spare (allocated for register windows, maybe stale)
    /// and aren't collector roots. Every slot gets written before `stack_top`
    /// grows over it.
    stack_top: usize,
    frames: Vec<Frame>,
    /// Open upvalues sorted by stack index (new ones are almost always above
    /// the old ones, so it's a push).
    open_upvalues: Vec<(usize, UpvalRef)>,
    instruction_limit: Option<u64>,
    output: Box<dyn FnMut(&str)>,
    pins: PinTable,
    /// The table string methods are looked up in (`("x"):upper()`).
    string_methods: Option<TableRef>,
    /// The metatable every string shares (`{__index = string}`).
    string_meta: Option<TableRef>,
    /// Interned metamethod names (`__index`, …).
    events: meta::Events,
    /// Values the standard library holds on the Rust side (like the `next` that
    /// `pairs` returns), kept alive no matter what scripts do.
    internal_roots: Vec<Value>,
    /// Values natives asked to keep alive ([`NativeCtx::root`]).
    native_roots: Vec<Value>,
    /// `frames.len()` when the innermost `run` started. A `RETURN` or an unwind
    /// that reaches it ends that `run` instead of popping further.
    boundary: usize,
    /// How many natives are running inside each other (each one that calls back
    /// into the VM uses Rust stack).
    native_depth: usize,
    /// Instructions left in the current slice of the budget, shared by nested
    /// runs. When it runs out, `charge_slow` checks for an interrupt and
    /// refills it from `reserve`.
    budget: u64,
    /// The rest of the instruction limit that hasn't been handed out yet.
    reserve: u64,
    /// Set from anywhere to stop the running script (see [`Interrupt`]).
    interrupt: Arc<Interrupt>,
    /// Where the time comes from, see [`Vm::set_clock`].
    clock: clock::Clock,
    /// Lent to natives that count their own steps (see [`NativeCtx::meter`]).
    meter: Rc<Meter>,
    /// The main thread, and the thread running now (the main thread or a
    /// coroutine). The running thread's state is in the fields above.
    main_thread: ThreadRef,
    current: ThreadRef,
    /// Threads waiting in `coroutine.resume` for the one they resumed,
    /// outermost first.
    resumers: Vec<ThreadRef>,
    /// How many interpreter loops are nested on the Rust stack (natives calling
    /// back, nested metamethods, resumes). A coroutine can only yield from the
    /// depth it was resumed at.
    depth: usize,
    /// The values a `yield` passes out, on their way to `resume`.
    yielded: Rets,
    /// Where the suspended `yield` call's results go.
    yield_dest: Option<(usize, Option<usize>)>,
    /// The debug hook, if any (see [`Vm::set_debug_hook`]).
    hooks: debug::HookState,
    /// Stands in for the current frame while it sits in `frames` during a
    /// native call.
    placeholder: Rc<LuaClosure>,
    ud_types: Vec<UdType>,
    ud_type_ids: HashMap<std::any::TypeId, u32>,
    /// Data the embedding layer keeps with the VM (the `rhue` crate keeps its
    /// type-checking environment here).
    host_data: Option<Box<dyn std::any::Any>>,
}

/// How deep natives can call back into scripts that call natives (Lua's C-stack
/// limit does the same job).
pub const MAX_NATIVE_DEPTH: usize = 200;

/// The error raised when the memory limit is reached.
pub const OUT_OF_MEMORY: &str = "not enough memory";

/// How many stack slots above a Lua frame's base always exist: every register a
/// `u8` operand can name, plus the few above `R(A)` that `FORLOOP`, `TFORCALL`
/// and `SELF` touch. With the window this size, register accesses don't need
/// bounds checks.
const REG_WINDOW: usize = 256 + 8;

/// A native's arguments, copied off the stack (inline for up to 8).
type ArgBuf = SmallVec<[Value; 8]>;

impl Default for Vm {
    fn default() -> Self {
        Self::new()
    }
}

impl Vm {
    /// A VM with the standard library installed.
    pub fn new() -> Self {
        let mut vm = Self::bare();
        stdlib::install(&mut vm);
        fastcall::install(&mut vm);
        vm
    }

    /// A VM with no globals at all.
    pub fn bare() -> Self {
        let mut heap = Heap::new();
        let events = meta::Events::intern(&mut heap);
        let main_thread = heap.alloc_thread(heap::ThreadState::default(), CoStatus::Running);
        let interrupt: Arc<Interrupt> = Arc::default();
        Vm {
            main_thread,
            current: main_thread,
            resumers: Vec::new(),
            depth: 0,
            yielded: Rets::new(),
            yield_dest: None,
            hooks: debug::HookState::default(),
            id: NEXT_VM_ID.fetch_add(1, Ordering::Relaxed),
            heap,
            events,
            string_meta: None,
            globals: FxIndexMap::default(),
            stack: Vec::new(),
            stack_top: 0,
            frames: Vec::new(),
            open_upvalues: Vec::new(),
            instruction_limit: None,
            output: Box::new(|line| println!("{line}")),
            pins: Rc::new(RefCell::new(HashMap::new())),
            string_methods: None,
            internal_roots: Vec::new(),
            native_roots: Vec::new(),
            boundary: 0,
            native_depth: 0,
            budget: 0,
            reserve: u64::MAX,
            meter: Rc::new(Meter::with_budget(0, None, Some(interrupt.clone()))),
            clock: clock::Clock::default(),
            interrupt,
            placeholder: Rc::new(LuaClosure {
                proto: Rc::new(Proto::default()),
                upvalues: Rc::from(Vec::new()),
                constants: Rc::from(Vec::new()),
                tree: Rc::default(),
                env: None,
            }),
            ud_types: Vec::new(),
            ud_type_ids: HashMap::new(),
            host_data: None,
        }
    }

    /// A slot for the embedding layer's own data.
    pub fn host_data(&mut self) -> &mut Option<Box<dyn std::any::Any>> {
        &mut self.host_data
    }

    /// Tells VMs apart, so a handle from one never gets used in another.
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn heap(&self) -> &Heap {
        &self.heap
    }

    pub fn heap_mut(&mut self) -> &mut Heap {
        &mut self.heap
    }

    /// Interns `text` and gives it back as a string value.
    pub fn string(&mut self, text: &str) -> Value {
        Value::String(self.heap.intern(text))
    }

    pub fn set_global(&mut self, name: &str, value: Value) {
        let key = self.heap.intern(name);
        if value == Value::Nil {
            self.globals.swap_remove(&key);
        } else {
            self.globals.insert(key, value);
        }
    }

    /// Every global, in definition order.
    pub fn globals(&self) -> impl Iterator<Item = (StrRef, Value)> + '_ {
        self.globals.iter().map(|(&k, &v)| (k, v))
    }

    pub fn global(&mut self, name: &str) -> Value {
        let key = self.heap.intern(name);
        self.globals.get(&key).copied().unwrap_or(Value::Nil)
    }

    /// Wraps a native function in a function value.
    pub fn native(&mut self, name: &str, func: NativeFn) -> Value {
        Value::Function(self.heap.alloc_function(Function::Native(Native {
            name: name.into(),
            func,
            fast: None,
            special: None,
            bound: Value::Nil,
        })))
    }

    /// Registers a native function as a global.
    pub fn register_native(&mut self, name: &str, func: NativeFn) {
        let f = self.native(name, func);
        self.set_global(name, f);
    }

    pub(crate) fn register_special(&mut self, name: &str, special: Special) {
        let f = self.special_function(name, special, Value::Nil);
        self.set_global(name, f);
    }

    /// A function the VM runs itself (see [`Special`]).
    pub(crate) fn special_function(&mut self, name: &str, special: Special, bound: Value) -> Value {
        let func: NativeFn = Rc::new(|_, _| unreachable!("special natives are run by the VM"));
        Value::Function(self.heap.alloc_function(Function::Native(Native {
            name: name.into(),
            func,
            fast: None,
            special: Some(special),
            bound,
        })))
    }

    pub(crate) fn set_string_methods(&mut self, t: TableRef) {
        self.string_methods = Some(t);
        let meta = self.heap.alloc_table(Table::with_capacity(0, 1));
        let index = Value::String(self.events.get(meta::Event::Index));
        let _ = self
            .heap
            .with_table(meta, |m| m.set(index, Value::Table(t)));
        self.string_meta = Some(meta);
    }

    pub(crate) fn keep_alive(&mut self, v: Value) {
        self.internal_roots.push(v);
    }

    /// Caps how much memory scripts can keep allocated (roughly, in bytes, as
    /// the collector counts it). Hitting it is an error scripts can't catch,
    /// raised after a collection fails to free enough, or before building a
    /// string or table that wouldn't fit. `None` means no limit.
    pub fn set_memory_limit(&mut self, limit: Option<usize>) {
        self.heap.set_limit(limit);
    }

    /// Whether allocating `bytes` more would go over the memory limit.
    pub fn would_exceed(&self, bytes: usize) -> bool {
        self.heap.would_exceed(bytes)
    }

    /// How much memory the heap's holding, as the collector counts it.
    pub fn memory_used(&self) -> usize {
        self.heap.stats().bytes
    }

    #[cold]
    #[inline(never)]
    fn out_of_memory(&mut self, f: &Frame, pc: usize) -> Thrown {
        let mut t = self.throw(f, pc, OUT_OF_MEMORY.to_string());
        t.catchable = false;
        t
    }

    /// Where `print` output goes (stdout by default).
    pub fn set_output(&mut self, output: impl FnMut(&str) + 'static) {
        self.output = Box::new(output);
    }

    /// Caps how many instructions one `execute` call can run, so a host can
    /// stop runaway scripts (`while true do end`). `None` means no limit, and
    /// scripts can't catch this error with `pcall`. The budget gets charged at
    /// every backward jump (the distance jumped) and every call (one), so the
    /// count's approximate but no loop or recursion gets past it.
    pub fn set_instruction_limit(&mut self, limit: Option<u64>) {
        self.instruction_limit = limit;
        self.meter.limit.set(limit);
    }

    /// Formats a value like Luau's `tostring`.
    pub fn display(&self, value: Value) -> String {
        value.display(&self.heap)
    }

    /// Keeps a table or function alive while the returned [`Pin`] (or a clone
    /// of an `Rc` around it) exists. Other values don't need pinning.
    pub fn pin(&self, value: Value) -> Option<Pin> {
        let key = match value {
            Value::Table(t) => PinKey::Table(t),
            Value::Function(f) => PinKey::Function(f),
            Value::UserData(u) => PinKey::UserData(u),
            Value::Thread(t) => PinKey::Thread(t),
            Value::Buffer(b) => PinKey::Buffer(b),
            _ => return None,
        };
        *self.pins.borrow_mut().entry(key).or_insert(0) += 1;
        Some(Pin {
            key,
            pins: Rc::downgrade(&self.pins),
            vm_id: self.id,
        })
    }

    pub fn heap_stats(&self) -> HeapStats {
        self.heap.stats()
    }

    /// Runs a full collection now. Safe any time no script is running.
    pub fn collect_garbage(&mut self) {
        self.collect(None);
    }

    fn collect(&mut self, current: Option<&Frame>) {
        let mut roots: Vec<GcRoot> = self.stack[..self.stack_top]
            .iter()
            .map(|&v| GcRoot::Value(v))
            .collect();
        for frame in self.frames.iter().chain(current) {
            frame.for_each_root(|v| roots.push(GcRoot::Value(v)));
        }
        // the running thread and every thread waiting on a resume
        roots.push(GcRoot::Value(Value::Thread(self.main_thread)));
        roots.push(GcRoot::Value(Value::Thread(self.current)));
        roots.extend(
            self.resumers
                .iter()
                .map(|&t| GcRoot::Value(Value::Thread(t))),
        );
        for (&k, &v) in &self.globals {
            roots.push(GcRoot::Value(Value::String(k)));
            roots.push(GcRoot::Value(v));
        }
        for key in self.pins.borrow().keys() {
            roots.push(GcRoot::Value(match *key {
                PinKey::Table(t) => Value::Table(t),
                PinKey::Function(f) => Value::Function(f),
                PinKey::UserData(u) => Value::UserData(u),
                PinKey::Thread(t) => Value::Thread(t),
                PinKey::Buffer(b) => Value::Buffer(b),
            }));
        }
        if let Some(t) = self.string_methods {
            roots.push(GcRoot::Value(Value::Table(t)));
        }
        if let Some(t) = self.string_meta {
            roots.push(GcRoot::Value(Value::Table(t)));
        }
        roots.extend(self.events.all().map(|s| GcRoot::Value(Value::String(s))));
        roots.extend(self.internal_roots.iter().map(|&v| GcRoot::Value(v)));
        roots.extend(self.native_roots.iter().map(|&v| GcRoot::Value(v)));
        for ty in &self.ud_types {
            for (&k, &m) in &ty.methods {
                roots.push(GcRoot::Value(Value::String(k)));
                roots.push(GcRoot::Value(m));
            }
            for &k in ty.getters.keys().chain(ty.setters.keys()) {
                roots.push(GcRoot::Value(Value::String(k)));
            }
        }
        self.heap.collect(roots);
    }

    // ─ Userdata ──────────────────────────────────────────────────────────────

    /// The id of the userdata type for Rust type `rust`, making it (named
    /// `name`) on first use. The flag's `true` when it was just made.
    pub fn userdata_type(&mut self, rust: std::any::TypeId, name: &str) -> (u32, bool) {
        if let Some(&id) = self.ud_type_ids.get(&rust) {
            return (id, false);
        }
        let id = self.heap.add_userdata_type(name);
        self.ud_types.push(UdType {
            name: Rc::from(name),
            methods: FxHashMap::default(),
            getters: FxHashMap::default(),
            setters: FxHashMap::default(),
        });
        self.ud_type_ids.insert(rust, id);
        (id, true)
    }

    /// Makes `obj:name(...)` find `method` on userdata of type `ty`.
    pub fn add_method(&mut self, ty: u32, name: &str, method: Value) {
        let k = self.heap.intern(name);
        self.ud_types[ty as usize].methods.insert(k, method);
    }

    /// `obj.name` calls `get(obj)` and takes its first result.
    pub fn add_getter(&mut self, ty: u32, name: &str, get: NativeFn) {
        let k = self.heap.intern(name);
        self.ud_types[ty as usize].getters.insert(k, get);
    }

    /// `obj.name = v` calls `set(obj, v)`.
    pub fn add_setter(&mut self, ty: u32, name: &str, set: NativeFn) {
        let k = self.heap.intern(name);
        self.ud_types[ty as usize].setters.insert(k, set);
    }

    pub fn new_userdata(&mut self, ty: u32, data: Rc<dyn std::any::Any>) -> Value {
        assert!((ty as usize) < self.ud_types.len(), "unknown userdata type");
        Value::UserData(self.heap.alloc_userdata(UserData { type_id: ty, data }))
    }

    fn ud_member(
        &mut self,
        f: &mut Frame,
        pc: usize,
        u: UdRef,
        key: Value,
    ) -> Result<Value, Thrown> {
        let ty = &self.ud_types[self.heap.userdata(u).type_id as usize];
        let Value::String(k) = key else {
            let msg = format!(
                "attempt to index {} with {}",
                ty.name,
                self.describe_key(key)
            );
            return Err(self.throw(f, pc, msg));
        };
        if let Some(&m) = ty.methods.get(&k) {
            return Ok(m);
        }
        let Some(get) = ty.getters.get(&k).cloned() else {
            let msg = format!(
                "'{}' is not a valid member of {}",
                self.heap.get(k),
                ty.name
            );
            return Err(self.throw(f, pc, msg));
        };
        match self.native_from(f, &get, &[Value::UserData(u)]) {
            Ok(values) => Ok(values.first().copied().unwrap_or(Value::Nil)),
            Err(e) => Err(self.native_throw(f, pc, e)),
        }
    }

    fn ud_assign(
        &mut self,
        f: &mut Frame,
        pc: usize,
        u: UdRef,
        key: Value,
        value: Value,
    ) -> Result<(), Thrown> {
        let ty = &self.ud_types[self.heap.userdata(u).type_id as usize];
        let Value::String(k) = key else {
            let msg = format!(
                "attempt to index {} with {}",
                ty.name,
                self.describe_key(key)
            );
            return Err(self.throw(f, pc, msg));
        };
        let Some(set) = ty.setters.get(&k).cloned() else {
            let what = if ty.getters.contains_key(&k) {
                "a read-only member"
            } else if ty.methods.contains_key(&k) {
                "a method, and can't be assigned,"
            } else {
                "not a valid member"
            };
            let msg = format!("'{}' is {what} of {}", self.heap.get(k), ty.name);
            return Err(self.throw(f, pc, msg));
        };
        match self.native_from(f, &set, &[Value::UserData(u), value]) {
            Ok(_) => Ok(()),
            Err(e) => Err(self.native_throw(f, pc, e)),
        }
    }

    /// `obj[key] = value` for any value, userdata included.
    #[inline]
    fn set(
        &mut self,
        f: &mut Frame,
        pc: usize,
        obj: Value,
        key: Value,
        value: Value,
    ) -> Result<(), Thrown> {
        self.newindex_meta(f, pc, obj, key, value)
    }

    /// The runtime constants of `proto` and everything nested in it.
    fn load_constants(&mut self, proto: &Proto) -> Rc<Constants> {
        let values = proto
            .constants
            .iter()
            .map(|c| match c {
                Constant::Number(n) => Value::Number(*n),
                Constant::String(s) => Value::String(self.heap.intern(s)),
            })
            .collect::<Vec<_>>()
            .into();
        let children = proto
            .protos
            .iter()
            .map(|p| self.load_constants(p))
            .collect();
        Rc::new(Constants::new(values, children))
    }

    /// `CLOSURE`, a closure over nested prototype `index` of `parent` capturing
    /// registers from the frame at `base`. Kept out of line so the dispatch
    /// loop stays small.
    #[inline(never)]
    fn new_closure(&mut self, parent: &LuaClosure, index: usize, base: usize) -> FnRef {
        let child = parent.proto.protos[index].clone();
        let upvalues: Rc<[UpvalRef]> = child
            .upvalues
            .iter()
            .map(|d| {
                if d.in_stack {
                    self.find_upvalue(base + d.index as usize)
                } else {
                    parent.upvalues[d.index as usize].clone()
                }
            })
            .collect();
        let tree = parent.tree.children[index].clone();
        self.heap.alloc_function(Function::Lua(Rc::new(LuaClosure {
            proto: child,
            upvalues,
            constants: tree.values.clone(),
            tree,
            env: parent.env,
        })))
    }

    /// Runs a main-chunk prototype to the end and returns what it returned.
    pub fn execute(&mut self, proto: &Rc<Proto>) -> Result<Vec<Value>, RuntimeError> {
        let f = self.load(proto);
        self.call_top(f, &[], true)
    }

    /// [`execute`](Self::execute) in an environment (see
    /// [`load_in`](Self::load_in)).
    pub fn execute_in(
        &mut self,
        proto: &Rc<Proto>,
        env: TableRef,
    ) -> Result<Vec<Value>, RuntimeError> {
        let f = self.load_in(proto, env);
        self.call_top(f, &[], true)
    }

    /// A main chunk's prototype as a function value (not rooted, call it or
    /// store it before anything can collect). The prototype has to have passed
    /// the verifier.
    pub fn load(&mut self, proto: &Rc<Proto>) -> Value {
        self.load_with(proto, None)
    }

    /// Like [`load`](Self::load), but the chunk (and every function it makes)
    /// reads and writes globals in `env`. A read that finds nothing there falls
    /// back to the VM's globals (the standard library, say), and every write
    /// stays in `env`.
    pub fn load_in(&mut self, proto: &Rc<Proto>, env: TableRef) -> Value {
        self.load_with(proto, Some(env))
    }

    fn load_with(&mut self, proto: &Rc<Proto>, env: Option<TableRef>) -> Value {
        let tree = self.load_constants(proto);
        Value::Function(self.heap.alloc_function(Function::Lua(Rc::new(LuaClosure {
            proto: proto.clone(),
            upvalues: Rc::from(Vec::new()),
            constants: tree.values.clone(),
            tree,
            env,
        }))))
    }

    /// A global read in environment `env` (rare, so it's out of line).
    #[cold]
    #[inline(never)]
    fn env_get(
        heap: &Heap,
        globals: &FxIndexMap<StrRef, Value>,
        env: TableRef,
        name: StrRef,
    ) -> Value {
        match heap.table(env).get_str(name) {
            Value::Nil => globals.get(&name).copied().unwrap_or(Value::Nil),
            v => v,
        }
    }

    /// Calls a function value with `args` and returns all its results.
    ///
    /// From the host (no script running) this starts a fresh instruction
    /// budget, like [`execute`](Self::execute). From inside a native (through
    /// [`NativeCtx::vm`]) it runs nested in the current call and shares its
    /// budget. Prefer [`NativeCtx::call`] there, it keeps the error's Lua value
    /// for `pcall`.
    pub fn call_function(
        &mut self,
        func: Value,
        args: &[Value],
    ) -> Result<Vec<Value>, RuntimeError> {
        self.call_top(func, args, false)
    }

    /// `true` while a script or native is running in this VM.
    pub fn is_running(&self) -> bool {
        !self.frames.is_empty() || self.native_depth > 0 || self.depth > 0
    }

    /// Like [`call_function`](Self::call_function), but an error keeps its Lua
    /// value so a native can pass it on as-is.
    pub fn try_call(&mut self, func: Value, args: &[Value]) -> Result<Vec<Value>, Rethrown> {
        self.call_top_thrown(func, args, false)
            .map(Rets::into_vec)
            .map_err(Rethrown)
    }

    fn call_top(
        &mut self,
        func: Value,
        args: &[Value],
        main: bool,
    ) -> Result<Vec<Value>, RuntimeError> {
        self.call_top_thrown(func, args, main)
            .map(Rets::into_vec)
            .map_err(|t| t.error)
    }

    fn call_top_thrown(&mut self, func: Value, args: &[Value], main: bool) -> Result<Rets, Thrown> {
        let outermost = !self.is_running();
        if outermost {
            self.reset_budget();
            self.stack.clear();
            self.stack_top = 0;
            self.open_upvalues.clear();
            self.boundary = 0;
        }
        let result = self.invoke(func, args, main);
        if outermost {
            // drop the registers so they don't keep garbage alive
            self.close_upvalues(0);
            self.frames.clear();
            self.stack.clear();
            self.stack_top = 0;
            self.native_roots.clear();
        }
        result
    }

    fn invoke(&mut self, func: Value, args: &[Value], main: bool) -> Result<Rets, Thrown> {
        // everything below `stack_top` might belong to a frame that's waiting
        // on us, so the new call goes above it
        let slot = self.stack_top;
        if !self.ensure_stack(slot + 1 + args.len()) {
            return Err(self.throw_here("stack overflow".into()));
        }
        self.stack[slot] = func;
        self.stack[slot + 1..slot + 1 + args.len()].copy_from_slice(args);
        let result = self.invoke_at(slot, args.len(), main);
        // give the slots back. they stay allocated (and initialised), so a
        // native calling a script in a loop doesn't refill them every time
        self.stack_top = slot;
        result
    }

    /// Runs the function at `slot` (arguments above it) to the end.
    fn invoke_at(&mut self, slot: usize, nargs: usize, main: bool) -> Result<Rets, Thrown> {
        // a native that calls back into the VM nests a whole interpreter loop
        // on the Rust stack, so grow it in segments instead of counting on the
        // host thread's stack size
        self.depth += 1;
        let result = stacker::maybe_grow(128 * 1024, 2 * 1024 * 1024, || {
            self.invoke_at_inner(slot, nargs, main)
        });
        self.depth -= 1;
        result
    }

    fn invoke_at_inner(&mut self, slot: usize, nargs: usize, main: bool) -> Result<Rets, Thrown> {
        let callee = match self.stack[slot] {
            Value::Function(r) => r,
            _ => {
                let n = self
                    .resolve_call(slot, nargs)
                    .map_err(|m| self.throw_here(m))?;
                return self.invoke_at_inner(slot, n, main);
            }
        };
        match self.heap.function(callee) {
            Function::Native(Native {
                special: Some(special @ (Special::Resume | Special::Wrapped)),
                bound,
                ..
            }) => {
                let (special, bound) = (*special, *bound);
                self.resume_here(special, bound, slot, nargs)
            }
            Function::Native(Native {
                special: Some(Special::Yield),
                ..
            }) => {
                let m = match self.check_yield_nested() {
                    Some(m) => m,
                    None => "attempt to yield across a C-call boundary".into(),
                };
                Err(self.throw_here(m))
            }
            Function::Native(native) if native.special == Some(Special::Pcall) => {
                if nargs == 0 {
                    return Err(self.throw_here("missing argument #1 to 'pcall'".into()));
                }
                match self.invoke_at(slot + 1, nargs - 1, false) {
                    Ok(mut values) => {
                        values.insert(0, Value::Bool(true));
                        Ok(values)
                    }
                    Err(t) if t.catchable => Ok(smallvec![Value::Bool(false), t.value]),
                    Err(t) => Err(t),
                }
            }
            Function::Native(native) => {
                let func = native.func.clone();
                let args = ArgBuf::from_slice(&self.stack[slot + 1..slot + 1 + nargs]);
                self.run_native(&func, &args, None)
                    .map_err(|e| self.native_throw_here(e))
            }
            Function::Lua(_) => {
                let frame = match self.enter_lua(callee, slot, nargs, None, false, main) {
                    Ok(frame) => frame,
                    Err(m) => return Err(self.throw_here(m)),
                };
                let saved = self.boundary;
                self.boundary = self.frames.len();
                let result = self.run(frame, 0);
                self.boundary = saved;
                result
            }
        }
    }

    #[inline]
    fn run_native(
        &mut self,
        func: &NativeFn,
        args: &[Value],
        caller: Option<&mut Frame>,
    ) -> Result<Rets, NativeError> {
        if self.native_depth >= MAX_NATIVE_DEPTH {
            return Err("stack overflow (natives nested too deeply)".into());
        }
        let roots = self.native_roots.len();
        self.native_depth += 1;
        let result = func(
            &mut NativeCtx {
                vm: self,
                caller,
                parked: false,
            },
            args,
        );
        self.native_depth -= 1;
        self.native_roots.truncate(roots);
        result
    }

    /// A frame holding nothing, swapped in for the current frame while that one
    /// waits in `frames` (so tracebacks and the collector see it).
    #[inline]
    fn placeholder_frame(&self, closure: FnRef) -> Frame {
        Frame {
            closure,
            cl: self.placeholder.clone(),
            pc: 0,
            base: 0,
            varargs: Box::default(),
            ret_to: 0,
            ret_want: ALL_RESULTS,
            protected: false,
            is_main: false,
            cond: None,
            call_reported: false,
            trail: None,
        }
    }

    /// Runs a native called from `f` (parked in `frames` if it needs to be).
    #[inline]
    fn native_from(
        &mut self,
        f: &mut Frame,
        func: &NativeFn,
        args: &[Value],
    ) -> Result<Rets, NativeError> {
        self.run_native(func, args, Some(f))
    }

    /// Builds the frame for a call of Lua closure `callee` sitting at
    /// `func_slot` with `nargs` arguments above it.
    #[inline]
    fn enter_lua(
        &mut self,
        callee: FnRef,
        func_slot: usize,
        nargs: usize,
        want: Option<usize>,
        protected: bool,
        is_main: bool,
    ) -> Result<Frame, String> {
        let Function::Lua(closure) = self.heap.function(callee) else {
            unreachable!("enter_lua on a native")
        };
        let cl = closure.clone();
        self.enter_closure(callee, cl, func_slot, nargs, want, protected, is_main)
    }

    /// [`enter_lua`](Self::enter_lua) for a caller that already has the
    /// closure.
    #[inline]
    #[allow(clippy::too_many_arguments)]
    #[expect(
        clippy::cast_possible_truncation,
        reason = "stack slots and pcs stay far under u32 (the stack and code are capped), opcodes are a byte"
    )]
    fn enter_closure(
        &mut self,
        callee: FnRef,
        cl: Rc<LuaClosure>,
        func_slot: usize,
        nargs: usize,
        want: Option<usize>,
        protected: bool,
        is_main: bool,
    ) -> Result<Frame, String> {
        if self.frames.len() + 1 >= MAX_FRAMES {
            return Err("stack overflow".into());
        }
        let new_base = func_slot + 1;
        let np = cl.proto.num_params as usize;
        let varargs = if cl.proto.is_vararg && nargs > np {
            self.stack[new_base + np..new_base + nargs].into()
        } else {
            Box::default()
        };
        let frame_top = new_base + (cl.proto.max_stack as usize).max(nargs);
        if !self.ensure_stack(frame_top) {
            return Err("stack overflow".into());
        }
        // the register window just has to exist, it isn't in use (or rooted)
        // past `frame_top`
        if self.stack.len() < new_base + REG_WINDOW {
            self.stack.resize(new_base + REG_WINDOW, Value::Nil);
        }
        // missing parameters are nil, registers above them start clean
        for slot in &mut self.stack[new_base + nargs.min(np)..frame_top] {
            *slot = Value::Nil;
        }
        Ok(Frame {
            closure: callee,
            cl,
            pc: 0,
            base: new_base,
            varargs,
            ret_to: func_slot as u32,
            ret_want: want.map_or(ALL_RESULTS, |w| w as u32),
            protected,
            is_main,
            cond: None,
            call_reported: false,
            trail: None,
        })
    }

    /// Closes open upvalues at `from` and above. Most frames don't have any, so
    /// that check is inline and the loop isn't.
    #[inline]
    fn close_upvalues(&mut self, from: usize) {
        if self.open_upvalues.last().is_some_and(|&(i, _)| i >= from) {
            self.close_upvalues_slow(from);
        }
    }

    #[inline(never)]
    fn close_upvalues_slow(&mut self, from: usize) {
        while let Some((i, u)) = self.open_upvalues.last() {
            let (i, u) = (*i, u.clone());
            if i < from {
                break;
            }
            let v = self.stack.get(i).copied().unwrap_or(Value::Nil);
            u.set(Upvalue::Closed(v));
            self.open_upvalues.pop();
        }
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "stack slots and pcs stay far under u32 (the stack and code are capped), opcodes are a byte"
    )]
    fn find_upvalue(&mut self, index: usize) -> UpvalRef {
        match self.open_upvalues.binary_search_by_key(&index, |&(i, _)| i) {
            Ok(at) => self.open_upvalues[at].1.clone(),
            Err(at) => {
                let u = self.heap.alloc_upvalue(Upvalue::Open {
                    index: index as u32,
                    thread: self.current,
                });
                self.open_upvalues.insert(at, (index, u.clone()));
                u
            }
        }
    }

    fn span_of(frame: &Frame, pc: usize) -> Span {
        frame.proto().spans.get(pc).copied().unwrap_or_default()
    }

    fn name_of(frame: &Frame) -> String {
        if frame.is_main {
            "<main>".to_string()
        } else {
            format!("`{}`", frame.proto().display_name())
        }
    }

    /// Traceback entries for the tail calls that led to `frame`.
    fn trail_trace(&self, frame: &Frame) -> Vec<TraceEntry> {
        let Some(trail) = &frame.trail else {
            return Vec::new();
        };
        let sites: Vec<TailSite> = trail.sites.iter().flatten().copied().collect();
        let last = sites.len().saturating_sub(1);
        sites
            .iter()
            .enumerate()
            .filter_map(|(i, site)| {
                let Function::Lua(c) = self.heap.function(site.closure) else {
                    return None;
                };
                let mut function = if site.is_main {
                    "<main>".to_string()
                } else {
                    format!("`{}`", c.proto.display_name())
                };
                function.push_str(" (tail call)");
                if i == last && trail.elided > 0 {
                    function.push_str(&format!(
                        " (after {} more tail call{})",
                        trail.elided,
                        if trail.elided == 1 { "" } else { "s" }
                    ));
                }
                let pc = (site.pc as usize).saturating_sub(1);
                Some(TraceEntry {
                    function,
                    span: c.proto.spans.get(pc).copied().unwrap_or_default(),
                    source: c.proto.source.clone(),
                })
            })
            .collect()
    }

    /// Builds the host-facing error, with a traceback of every frame.
    fn error_at(&self, frame: &Frame, pc: usize, message: String) -> RuntimeError {
        let mut traceback = vec![TraceEntry {
            function: Self::name_of(frame),
            span: Self::span_of(frame, pc),
            source: frame.proto().source.clone(),
        }];
        traceback.extend(self.trail_trace(frame));
        traceback.extend(self.caller_trace());
        RuntimeError {
            message,
            span: Self::span_of(frame, pc),
            source: frame.proto().source.clone(),
            traceback,
        }
    }

    /// Every waiting frame, innermost first, at its call site.
    fn caller_trace(&self) -> impl Iterator<Item = TraceEntry> + '_ {
        // a caller's `pc` already points past its CALL
        self.frames.iter().rev().flat_map(|caller| {
            let mut entries = vec![TraceEntry {
                function: Self::name_of(caller),
                span: Self::span_of(caller, caller.pc.saturating_sub(1)),
                source: caller.proto().source.clone(),
            }];
            entries.extend(self.trail_trace(caller));
            entries
        })
    }

    /// An error raised outside any running Lua frame (in a native called
    /// through [`NativeCtx::call`] or by the host). It points at the Lua call
    /// site that led here, if there is one.
    fn error_here(&self, message: String) -> RuntimeError {
        let traceback: Vec<TraceEntry> = self.caller_trace().collect();
        match traceback.first() {
            Some(site) => RuntimeError {
                message,
                span: site.span,
                source: site.source.clone(),
                traceback,
            },
            None => RuntimeError {
                message,
                span: Span::default(),
                source: SourceFile::new("[host]", ""),
                traceback,
            },
        }
    }

    /// `"file:line: "`, Lua's position prefix for error messages.
    fn position(source: &SourceFile, span: Span) -> String {
        format!("{}:{}: ", source.name, source.line_col(span.start).0)
    }

    /// A runtime error at `pc` in `frame`, `pcall` sees `"file:line: msg"`.
    fn throw(&mut self, frame: &Frame, pc: usize, message: String) -> Thrown {
        let error = self.error_at(frame, pc, String::new());
        self.raise(error, NativeError::Message(message), 0)
    }

    fn throw_here(&mut self, message: String) -> Thrown {
        let error = self.error_here(String::new());
        self.raise(error, NativeError::Message(message), 1)
    }

    /// Turns a native's error into a throw at the call site in `frame`.
    fn native_throw(&mut self, frame: &Frame, pc: usize, e: NativeError) -> Thrown {
        let error = self.error_at(frame, pc, String::new());
        self.raise(error, e, 0)
    }

    fn native_throw_here(&mut self, e: NativeError) -> Thrown {
        let error = self.error_here(String::new());
        self.raise(error, e, 1)
    }

    /// Turns `e` into a throw, using `error` for the position and traceback.
    /// `skip` is how many levels of `error(v, level)` are natives with no
    /// position: 0 when a script called the native that raised, 1 when another
    /// native (or the host) did, like in `pcall(error, "x")`.
    fn raise(&mut self, mut error: RuntimeError, e: NativeError, skip: u32) -> Thrown {
        let positioned = |error: &RuntimeError, text: &str| {
            if error.traceback.is_empty() {
                text.to_string()
            } else {
                format!("{}{text}", Self::position(&error.source, error.span))
            }
        };
        let e = match e {
            // a yield that can't happen here (a native called from another
            // native, a metamethod, `pcall`...)
            NativeError::Yield(_) => NativeError::Message(
                self.check_yield_nested()
                    .unwrap_or_else(|| "attempt to yield across a C-call boundary".to_string()),
            ),
            e => e,
        };
        match e {
            NativeError::Yield(_) => unreachable!("turned into a message above"),
            NativeError::Rethrow(r) => r.0,
            NativeError::OutOfMemory => {
                error.message = OUT_OF_MEMORY.to_string();
                Thrown {
                    value: Value::String(self.heap.intern(OUT_OF_MEMORY)),
                    error,
                    catchable: false,
                    yielding: false,
                }
            }
            NativeError::Uncatchable(m) => {
                let value = Value::String(self.heap.intern(&m));
                error.message = m;
                Thrown {
                    value,
                    error,
                    catchable: false,
                    yielding: false,
                }
            }
            NativeError::Message(m) => {
                let text = positioned(&error, &m);
                error.message = m;
                Thrown {
                    value: Value::String(self.heap.intern_owned(text)),
                    error,
                    catchable: true,
                    yielding: false,
                }
            }
            NativeError::Raise { value, level } => {
                // level 1 is where `error` was called, level 2 the call site of
                // the function that called `error`
                let at = level
                    .checked_sub(1 + skip)
                    .map(|i| error.traceback.get(i as usize));
                let positioned_level = matches!(at, Some(Some(_)));
                if let Some(Some(entry)) = at {
                    error.span = entry.span;
                    error.source = entry.source.clone();
                }
                error.message = match value {
                    Value::String(s) => self.heap.get(s).to_string(),
                    Value::Nil => "nil".to_string(),
                    Value::Bool(_) | Value::Number(_) => value.display(&self.heap),
                    other => format!("(error object is a {} value)", other.type_name()),
                };
                let value = match value {
                    Value::String(s) if positioned_level => {
                        let text = positioned(&error, self.heap.get(s));
                        Value::String(self.heap.intern_owned(text))
                    }
                    v => v,
                };
                Thrown {
                    error,
                    value,
                    catchable: true,
                    yielding: false,
                }
            }
        }
    }

    #[cold]
    #[inline(never)]
    /// Starts a run's budget: the whole instruction limit in reserve and an
    /// empty slice, so the first charge also checks for an interrupt.
    fn reset_budget(&mut self) {
        self.budget = 0;
        self.reserve = self.instruction_limit.unwrap_or(u64::MAX);
    }

    /// The current slice can't pay for `n`, so stop for an interrupt or the
    /// limit, otherwise refill it. Out of line so the check in the dispatch
    /// loop stays a compare and a subtract.
    pub(crate) fn charge_slow(&mut self, f: &Frame, pc: usize, n: u64) -> Result<(), Thrown> {
        self.refill_budget(f, pc, n)?;
        self.budget -= n;
        Ok(())
    }

    /// Makes the slice big enough for `n` without spending it, unless the
    /// script got interrupted or hit the limit.
    #[cold]
    #[inline(never)]
    fn refill_budget(&mut self, f: &Frame, pc: usize, n: u64) -> Result<(), Thrown> {
        if self.interrupt.flag.swap(false, Ordering::AcqRel) {
            let mut t = self.throw(f, pc, INTERRUPTED.into());
            t.catchable = false;
            return Err(t);
        }
        while self.budget < n {
            if self.reserve == 0 {
                return Err(self.limit_reached(f, pc));
            }
            let take = self.reserve.min(BUDGET_SLICE);
            self.reserve -= take;
            self.budget += take;
        }
        Ok(())
    }

    /// Charges `n` instructions to the running script's budget (see
    /// [`NativeCtx::charge`]).
    #[inline]
    pub fn charge(&mut self, n: u64) -> Result<(), NativeError> {
        if self.budget >= n {
            self.budget -= n;
            return Ok(());
        }
        self.charge_native(n)
    }

    /// [`Vm::charge`] when the current slice can't pay: an interrupt or the
    /// limit stop the native, otherwise the slice gets refilled.
    #[cold]
    #[inline(never)]
    fn charge_native(&mut self, n: u64) -> Result<(), NativeError> {
        if self.interrupt.flag.swap(false, Ordering::AcqRel) {
            return Err(NativeError::Uncatchable(INTERRUPTED.into()));
        }
        while self.budget < n {
            if self.reserve == 0 {
                return Err(NativeError::Uncatchable(limit_message(
                    self.instruction_limit,
                )));
            }
            let take = self.reserve.min(BUDGET_SLICE.max(n));
            self.reserve -= take;
            self.budget += take;
        }
        self.budget -= n;
        Ok(())
    }

    /// Where `os.time`, `os.date`, `os.clock` and the task timers get the time
    /// from: a function giving the time since the Unix epoch. Without one the
    /// VM uses std's clocks, which don't exist on `wasm32-unknown-unknown`, so
    /// set one there (from `Date.now()`, say). `os.clock` starts again from 0.
    pub fn set_clock(&mut self, clock: Rc<dyn Fn() -> std::time::Duration>) {
        self.clock.set(clock);
    }

    /// The time since the Unix epoch, or `None` with no clock to read.
    pub fn wall_time(&self) -> Option<std::time::Duration> {
        self.clock.wall_time()
    }

    /// The time since the VM was made (or since its clock was set), what
    /// `os.clock` gives. `None` with no clock to read.
    pub fn uptime(&self) -> Option<std::time::Duration> {
        self.clock.uptime()
    }

    /// A function giving the time since the VM was made (or since its clock
    /// was set), never going backwards. `None` with no clock to read.
    pub fn uptime_fn(&self) -> Rc<dyn Fn() -> Option<std::time::Duration>> {
        self.clock.uptime_fn()
    }

    /// The flag that interrupts this VM's scripts, to hand off to other tasks
    /// or threads.
    pub fn interrupt_handle(&self) -> Arc<Interrupt> {
        self.interrupt.clone()
    }

    fn limit_reached(&mut self, f: &Frame, pc: usize) -> Thrown {
        let message = limit_message(self.instruction_limit);
        let mut t = self.throw(f, pc, message);
        t.catchable = false;
        t
    }

    /// Whether `l == r` (not raw-equal) might go to `__eq`: both tables and at
    /// least one has a metatable.
    #[inline]
    fn may_have_eq(&self, l: Value, r: Value) -> bool {
        match (l, r) {
            (Value::Table(x), Value::Table(y)) => {
                self.heap.table(x).meta().is_some() || self.heap.table(y).meta().is_some()
            }
            _ => false,
        }
    }

    /// Finishes a comparison decided out of line (by a metamethod). Skips the
    /// `JMP` after it, or takes it, like the dispatch loop would.
    fn finish_cond(&mut self, f: &mut Frame, result: bool, expect: bool) -> Result<(), Thrown> {
        if result != expect {
            f.pc += 1;
            return Ok(());
        }
        let jump = f.cl.proto.code.get(f.pc).copied();
        if let Some(j) = jump.filter(|j| j.op() == Some(Op::Jmp)) {
            let by = j.sax();
            if by < 0 {
                let n = by.unsigned_abs() as u64;
                if self.budget < n {
                    let pc = f.pc;
                    self.charge_slow(f, pc, n)?;
                } else {
                    self.budget -= n;
                }
            }
            f.pc = offset(f.pc + 1, by);
        }
        Ok(())
    }

    fn ensure_stack(&mut self, len: usize) -> bool {
        if len > MAX_STACK {
            return false;
        }
        // callers write every slot they claim here before anything reads it (or
        // the collector scans it), so spare slots don't need clearing
        if self.stack_top < len {
            self.stack_top = len;
            if self.stack.len() < len {
                self.stack.resize(len, Value::Nil);
            }
        }
        true
    }

    /// Copies `values` to `dest`, adjusted to `want` (padding with nil or
    /// truncating). Returns the new `top` (one past the last value written).
    fn place_results(&mut self, dest: usize, values: &[Value], want: Option<usize>) -> usize {
        let n = want.unwrap_or(values.len());
        self.ensure_stack(dest + n);
        for i in 0..n {
            self.stack[dest + i] = values.get(i).copied().unwrap_or(Value::Nil);
        }
        dest + n
    }

    fn index(&self, obj: Value, key: Value) -> Result<Value, String> {
        match obj {
            Value::Table(t) => Ok(self.heap.table(t).get(key)),
            Value::String(_) => match (self.string_methods, key) {
                (Some(m), Value::String(k)) => Ok(self.heap.table(m).get_str(k)),
                _ => Ok(Value::Nil),
            },
            other => Err(format!(
                "attempt to index {} with {}",
                other.type_name(),
                self.describe_key(key)
            )),
        }
    }

    fn set_index(&mut self, obj: Value, key: Value, value: Value) -> Result<(), String> {
        match obj {
            Value::Table(t) => self
                .heap
                .with_table(t, |tbl| tbl.set(key, value))
                .map_err(|e| e.message().to_string()),
            other => Err(format!(
                "attempt to index {} with {}",
                other.type_name(),
                self.describe_key(key)
            )),
        }
    }

    fn describe_key(&self, key: Value) -> String {
        match key {
            Value::String(s) => format!("'{}'", self.heap.get(s)),
            Value::Number(n) => format_number(n),
            other => other.type_name().to_string(),
        }
    }

    /// Runs `entry` until it returns. Frames below `self.boundary` belong to
    /// whoever called us and never get popped here.
    fn run(&mut self, entry: Frame, top: usize) -> Result<Rets, Thrown> {
        let mut f = entry;
        let mut top = top;
        loop {
            match self.dispatch(&mut f, &mut top) {
                Ok(values) => return Ok(values),
                // a coroutine suspends, its frame waits for the next resume
                Err(thrown) if thrown.yielding => {
                    self.frames.push(f);
                    return Err(thrown);
                }
                Err(thrown) if !thrown.catchable => {
                    let base = self.frames.get(self.boundary).map_or(f.base, |fr| fr.base);
                    self.close_upvalues(base);
                    self.frames.truncate(self.boundary);
                    return Err(thrown);
                }
                Err(thrown) => self.unwind(&mut f, &mut top, thrown)?,
            }
        }
    }

    /// Pops frames until a protected one, whose caller gets `false, value`.
    /// Reaching this run's boundary passes the error on.
    fn unwind(&mut self, f: &mut Frame, top: &mut usize, thrown: Thrown) -> Result<(), Thrown> {
        loop {
            self.close_upvalues(f.base);
            if f.protected {
                let (dest, want) = (f.ret_to(), f.ret_want());
                *f = self
                    .frames
                    .pop()
                    .expect("a protected frame always has a caller");
                let end = self.place_results(dest, &[Value::Bool(false), thrown.value], want);
                if want.is_none() {
                    *top = end;
                }
                return Ok(());
            }
            if self.frames.len() == self.boundary {
                return Err(thrown);
            }
            *f = self.frames.pop().expect("frames above the boundary");
        }
    }

    /// Calls the function at `func_slot` with `nargs` arguments above it.
    /// Natives run to the end right here, Lua functions get a new frame and run
    /// when the dispatch loop carries on.
    #[allow(clippy::too_many_arguments)]
    fn call(
        &mut self,
        f: &mut Frame,
        pc: usize,
        func_slot: usize,
        nargs: usize,
        want: Option<usize>,
        top: &mut usize,
        protected: bool,
    ) -> Result<(), Thrown> {
        let callee = match self.stack[func_slot] {
            Value::Function(r) => r,
            _ => {
                let n = self
                    .resolve_call(func_slot, nargs)
                    .map_err(|m| self.throw(f, pc, m))?;
                return self.call(f, pc, func_slot, n, want, top, protected);
            }
        };
        match self.heap.function(callee) {
            Function::Lua(closure) => {
                let cl = closure.clone();
                match self.enter_closure(callee, cl, func_slot, nargs, want, protected, false) {
                    Ok(callee_frame) => {
                        self.frames.push(std::mem::replace(f, callee_frame));
                        Ok(())
                    }
                    Err(m) => Err(self.throw(f, pc, m)),
                }
            }
            Function::Native(native) if native.special == Some(Special::Pcall) => {
                self.pcall(f, pc, func_slot, nargs, want, top)
            }
            Function::Native(Native {
                special: Some(Special::Yield),
                ..
            }) => self.yield_from(f, pc, func_slot, nargs, want),
            Function::Native(Native {
                special: Some(special @ (Special::Resume | Special::Wrapped)),
                bound,
                ..
            }) => {
                let (special, bound) = (*special, *bound);
                let mut results = self.resume_from(f, pc, special, bound, func_slot, nargs)?;
                if protected {
                    results.insert(0, Value::Bool(true));
                }
                let end = self.place_results(func_slot, &results, want);
                if want.is_none() {
                    *top = end;
                }
                Ok(())
            }
            Function::Native(native) => {
                if let (Some(fast), false, 0..=3) = (native.fast, protected, nargs) {
                    let mut args = [Value::Nil; 3];
                    args[..nargs].copy_from_slice(&self.stack[func_slot + 1..][..nargs]);
                    if let Some(out) = fast(self, &args[..nargs]) {
                        let end = match out {
                            Fast::One(v) => self.place_results(func_slot, &[v], want),
                            Fast::Two(v, w) => self.place_results(func_slot, &[v, w], want),
                        };
                        if want.is_none() {
                            *top = end;
                        }
                        return Ok(());
                    }
                }
                let Function::Native(native) = self.heap.function(callee) else {
                    unreachable!("matched as a native above")
                };
                let func = native.func.clone();
                let args = ArgBuf::from_slice(&self.stack[func_slot + 1..func_slot + 1 + nargs]);
                match self.native_from(f, &func, &args) {
                    Err(NativeError::Yield(values)) if !protected => {
                        if let Err(m) = self.check_yield() {
                            return Err(self.throw(f, pc, m));
                        }
                        self.yielded = values;
                        self.yield_dest = Some((func_slot, want));
                        Err(Thrown::yield_signal())
                    }
                    Ok(mut results) => {
                        if protected {
                            results.insert(0, Value::Bool(true));
                        }
                        let end = self.place_results(func_slot, &results, want);
                        if want.is_none() {
                            *top = end;
                        }
                        Ok(())
                    }
                    Err(e) => Err(self.native_throw(f, pc, e)),
                }
            }
        }
    }

    /// `return g(...)`. A Lua `g` takes over this frame (same caller, same
    /// result slot, still protected if this frame was), so tail recursion runs
    /// in constant stack. Anything else is a normal call whose results the
    /// `RETURN` after it passes on.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "stack slots and pcs stay far under u32 (the stack and code are capped), opcodes are a byte"
    )]
    fn tail_call(
        &mut self,
        f: &mut Frame,
        pc: usize,
        func_slot: usize,
        nargs: usize,
        top: &mut usize,
    ) -> Result<(), Thrown> {
        let nargs = self
            .resolve_call(func_slot, nargs)
            .map_err(|m| self.throw(f, pc, m))?;
        let Value::Function(callee) = self.stack[func_slot] else {
            unreachable!("resolve_call leaves a function")
        };
        if !matches!(self.heap.function(callee), Function::Lua(_)) {
            return self.call(f, pc, func_slot, nargs, None, top, false);
        }
        self.close_upvalues(f.base);
        let own_slot = f.base - 1;
        self.stack
            .copy_within(func_slot..func_slot + 1 + nargs, own_slot);
        let mut frame = self
            .enter_lua(callee, own_slot, nargs, None, false, false)
            .map_err(|m| self.throw(f, pc, m))?;
        frame.ret_to = f.ret_to;
        frame.ret_want = f.ret_want;
        frame.protected = f.protected;
        frame.cond = f.cond;
        let mut trail = f.trail.take().unwrap_or_default();
        trail.push(TailSite {
            closure: f.closure,
            pc: f.pc as u32,
            is_main: f.is_main,
        });
        frame.trail = Some(trail);
        *f = frame;
        Ok(())
    }

    /// `pcall(g, ...)`, calls `g` so errors come back as `false, value` instead
    /// of propagating.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "stack slots and pcs stay far under u32 (the stack and code are capped), opcodes are a byte"
    )]
    fn pcall(
        &mut self,
        f: &mut Frame,
        pc: usize,
        func_slot: usize,
        nargs: usize,
        want: Option<usize>,
        top: &mut usize,
    ) -> Result<(), Thrown> {
        if nargs == 0 {
            return Err(self.throw(f, pc, "missing argument #1 to 'pcall'".into()));
        }
        let protected_slot = func_slot + 1;
        // a callable table runs through its `__call`, like any call
        let nargs = match self.resolve_call(protected_slot, nargs - 1) {
            Ok(n) => n + 1,
            Err(_) => nargs,
        };
        let target = self.stack[protected_slot];
        let finish = |vm: &mut Vm, values: &[Value], top: &mut usize| {
            let end = vm.place_results(func_slot, values, want);
            if want.is_none() {
                *top = end;
            }
        };
        match target {
            // a Lua function runs in this dispatch loop, in a protected frame
            // whose results go to pcall's own slot
            Value::Function(r) if matches!(self.heap.function(r), Function::Lua(_)) => {
                let r = self.call(f, pc, protected_slot, nargs - 1, want, top, true);
                if r.is_ok() {
                    f.ret_to = func_slot as u32;
                }
                r
            }
            // a native (pcall itself included) runs nested, to the end
            Value::Function(_) => {
                let placeholder = self.placeholder_frame(f.closure);
                self.frames.push(std::mem::replace(f, placeholder));
                let result = self.invoke_at(protected_slot, nargs - 1, false);
                *f = self.frames.pop().expect("pcall's caller");
                match result {
                    Ok(mut values) => {
                        values.insert(0, Value::Bool(true));
                        finish(self, &values, top);
                        Ok(())
                    }
                    Err(t) if t.catchable => {
                        finish(self, &[Value::Bool(false), t.value], top);
                        Ok(())
                    }
                    Err(t) => Err(t),
                }
            }
            other => {
                let msg = format!("attempt to call a {} value", other.type_name());
                let v = self.string(&msg);
                finish(self, &[Value::Bool(false), v], top);
                Ok(())
            }
        }
    }

    fn dispatch(&mut self, f: &mut Frame, top: &mut usize) -> Result<Rets, Thrown> {
        macro_rules! fail {
            ($pc:expr, $($msg:tt)*) => {{
                let msg = format!($($msg)*);
                return Err(self.throw(f, $pc, msg));
            }};
        }
        // a safe point for the collector, every live value is in a register, a
        // frame, or a root. only instructions that can allocate check it
        macro_rules! gc_at {
            ($pc:expr) => {
                if self.heap.should_collect() {
                    self.collect(Some(f));
                    if self.heap.over_limit() {
                        return Err(self.out_of_memory(f, $pc));
                    }
                }
            };
        }
        // the compiler only jumps backwards with JMP, FORLOOP and TFORLOOP, but
        // any backward jump could loop, so every one gets charged
        macro_rules! back_edge {
            ($pc:expr, $by:expr) => {
                if $by < 0 {
                    charge!($pc, $by.unsigned_abs());
                }
            };
        }

        // the outer loop runs once per frame switch (call, return, or any slow
        // path that hands the whole frame to a helper). the inner loop keeps
        // the frame's code, constants, base and pc in locals so the common
        // instructions never touch `f` at all, and `f.pc` only gets written
        // back before something that reads it
        'frame: loop {
            // debug hooks: one flag on the fast path, the details are in
            // `debug.rs`
            let hooked = self.hooks.hook.is_some();
            if hooked && self.hook_mask().call && f.pc == 0 && !f.call_reported {
                f.call_reported = true;
                self.call_event(f)?;
                continue 'frame;
            }
            let base = f.base;
            let code: &[Instr] = &f.cl.proto.code;
            let cache = &f.cl.proto.cache;
            let k: &[Value] = &f.cl.constants;
            let mut pc = f.pc;

            loop {
                let at = pc;
                // charges `n` instructions to the budget (see
                // `set_instruction_limit`). when the slice runs out it gets
                // refilled (or the script stopped) and the instruction runs
                // again from the start, which is why every instruction charges
                // before it changes anything
                macro_rules! charge {
                    ($pc:expr, $n:expr) => {{
                        let n = $n as u64;
                        if self.budget < n {
                            self.refill_budget(f, $pc, n)?;
                            pc = at;
                            continue;
                        }
                        self.budget -= n;
                    }};
                }
                if hooked && let Some(event) = self.hook_due(&f.cl.proto, at) {
                    self.hook_event(f, at, event)?;
                    continue 'frame;
                }
                let Some(&ins) = code.get(at) else {
                    fail!(at.saturating_sub(1), "fell off the end of the bytecode");
                };
                pc += 1;
                let Some(op) = ins.op() else {
                    fail!(at, "invalid opcode {:#04x}", ins.0 & 0xff);
                };
                // operands index a fixed-size window over the frame's
                // registers. a `u8` (plus a small offset) always fits, so the
                // compiler drops the bounds checks
                let (a, b, c) = (ins.a() as usize, ins.b() as usize, ins.c() as usize);
                let regs: &mut [Value; REG_WINDOW] = (&mut self.stack[base..base + REG_WINDOW])
                    .try_into()
                    .expect("the register window is REG_WINDOW long");
                // `l < r` / `l <= r`, numbers inline, strings and errors out of
                // line
                macro_rules! less {
                    ($pc:expr, $op:ident, $l:expr, $r:expr, $cmp:tt) => {{
                        let (l, r) = ($l, $r);
                        match (l, r) {
                            (Value::Number(x), Value::Number(y)) => x $cmp y,
                            (Value::String(_), Value::String(_)) => {
                                match compare(Op::$op, l, r, &self.heap) {
                                    Ok(b) => b,
                                    Err(m) => fail!($pc, "{m}"),
                                }
                            }
                            // `__lt` / `__le`, or the error
                            _ => {
                                f.pc = pc;
                                self.compare_meta(f, $pc, Op::$op, (l, r), ins.a() != 0)?;
                                continue 'frame;
                            }
                        }
                    }};
                }
                // a comparison is always followed by the `JMP` it guards, skip
                // it or take it here without dispatching it
                macro_rules! cond {
                    ($pc:expr, $result:expr) => {{
                        if $result != (ins.a() != 0) {
                            pc += 1;
                        } else if let Some(j) = code.get(pc).filter(|j| j.op() == Some(Op::Jmp)) {
                            let by = j.sax();
                            if by < 0 {
                                charge!(pc, by.unsigned_abs());
                            }
                            pc = offset(pc + 1, by);
                        }
                    }};
                }
                // leaves the fast loop: saves `pc` so helpers that park the
                // frame (or tracebacks through it) see where it was
                macro_rules! sync {
                    () => {
                        f.pc = pc
                    };
                }
                macro_rules! gc {
                    () => {
                        gc_at!(at)
                    };
                }
                // arithmetic on two numbers inline, anything else (metamethods,
                // errors) out of line
                macro_rules! arith {
                    ($op:ident, $l:expr, $r:expr) => {{
                        let (l, r) = ($l, $r);
                        match (l, r) {
                            (Value::Number(x), Value::Number(y)) => {
                                regs[a] = Value::Number(arith(ArithOp::$op, x, y))
                            }
                            _ => {
                                sync!();
                                self.arith_meta(f, at, ArithOp::$op, (l, r), base + a)?;
                                continue 'frame;
                            }
                        }
                    }};
                }
                // a table read that missed on a table with a metatable has to
                // go through `__index`
                macro_rules! index_slow {
                    ($obj:expr, $key:expr) => {{
                        sync!();
                        self.index_meta(f, at, $obj, $key, base + a)?;
                        gc!();
                        continue 'frame;
                    }};
                }

                match op {
                    Op::LoadNil => regs[a] = Value::Nil,
                    Op::LoadB => {
                        regs[a] = Value::Bool(ins.b() != 0);
                        if ins.c() != 0 {
                            pc += 1;
                        }
                    }
                    Op::LoadN => regs[a] = Value::Number(ins.sbx() as f64),
                    Op::LoadK => regs[a] = k[ins.bx() as usize],
                    Op::Move => regs[a] = regs[b],
                    Op::GetGlobal => {
                        let Value::String(name) = k[ins.bx() as usize] else {
                            fail!(at, "global name is not a string");
                        };
                        if let Some(env) = f.cl.env {
                            regs[a] = Self::env_get(&self.heap, &self.globals, env, name);
                            continue;
                        }
                        let hint = cache.slot(at, code.len());
                        regs[a] = match self.globals.get_index(hint.get() as usize) {
                            Some((&key, &v)) if key == name => v,
                            _ => match self.globals.get_full(&name) {
                                Some((i, _, &v)) => {
                                    hint.set(u32::try_from(i).unwrap_or(u32::MAX));
                                    v
                                }
                                None => Value::Nil,
                            },
                        };
                    }
                    Op::SetGlobal => {
                        let Value::String(name) = k[ins.bx() as usize] else {
                            fail!(at, "global name is not a string");
                        };
                        if let Some(env) = f.cl.env {
                            let v = regs[a];
                            if let Err(e) =
                                self.heap.with_table(env, |t| t.set(Value::String(name), v))
                            {
                                fail!(at, "{}", e.message());
                            }
                            gc!();
                            continue;
                        }
                        match regs[a] {
                            Value::Nil => {
                                self.globals.swap_remove(&name);
                            }
                            v => {
                                self.globals.insert(name, v);
                            }
                        }
                    }

                    Op::Add => arith!(Add, regs[b], regs[c]),
                    Op::Sub => arith!(Sub, regs[b], regs[c]),
                    Op::Mul => arith!(Mul, regs[b], regs[c]),
                    Op::Div => arith!(Div, regs[b], regs[c]),
                    Op::IDiv => arith!(IDiv, regs[b], regs[c]),
                    Op::Mod => arith!(Mod, regs[b], regs[c]),
                    Op::Pow => arith!(Pow, regs[b], regs[c]),
                    Op::AddK => arith!(Add, regs[b], k[ins.c() as usize]),
                    Op::SubK => arith!(Sub, regs[b], k[ins.c() as usize]),
                    Op::MulK => arith!(Mul, regs[b], k[ins.c() as usize]),
                    Op::DivK => arith!(Div, regs[b], k[ins.c() as usize]),
                    Op::IDivK => arith!(IDiv, regs[b], k[ins.c() as usize]),
                    Op::ModK => arith!(Mod, regs[b], k[ins.c() as usize]),
                    Op::PowK => arith!(Pow, regs[b], k[ins.c() as usize]),
                    Op::Unm => match regs[b] {
                        Value::Number(n) => regs[a] = Value::Number(-n),
                        other => {
                            sync!();
                            self.unm_meta(f, at, other, base + a)?;
                            continue 'frame;
                        }
                    },
                    Op::Not => regs[a] = Value::Bool(!regs[b].is_truthy()),
                    Op::Len => match regs[b] {
                        Value::String(s) => regs[a] = Value::Number(self.heap.get(s).len() as f64),
                        Value::Table(t) if self.heap.table(t).meta().is_none() => {
                            regs[a] = Value::Number(self.heap.table(t).len() as f64)
                        }
                        other => {
                            sync!();
                            self.len_meta(f, at, other, base + a)?;
                            continue 'frame;
                        }
                    },
                    Op::Concat => {
                        let (l, r) = (regs[b], regs[c]);
                        if let (Value::String(x), Value::String(y)) = (l, r) {
                            let size = self.heap.get(x).len() + self.heap.get(y).len();
                            if self.heap.would_exceed(size) {
                                return Err(self.out_of_memory(f, at));
                            }
                        }
                        let mut out = String::new();
                        for v in [l, r] {
                            match v {
                                Value::String(s) => out.push_str(self.heap.get(s)),
                                Value::Number(n) => out.push_str(&format_number(n)),
                                _ => {
                                    sync!();
                                    self.concat_meta(f, at, (l, r), base + a)?;
                                    continue 'frame;
                                }
                            }
                        }
                        regs[a] = Value::String(self.heap.intern_owned(out));
                        gc!();
                    }

                    Op::Eq => {
                        let (l, r) = (regs[b], regs[c]);
                        let equal = l.raw_eq(r);
                        if !equal && self.may_have_eq(l, r) {
                            sync!();
                            self.eq_meta(f, at, (l, r), ins.a() != 0)?;
                            continue 'frame;
                        }
                        cond!(at, equal)
                    }
                    Op::Lt => cond!(at, less!(at, Lt, regs[b], regs[c], <)),
                    Op::Le => cond!(at, less!(at, Le, regs[b], regs[c], <=)),
                    Op::EqK => cond!(at, regs[b].raw_eq(k[c])),
                    Op::LtK => cond!(at, less!(at, Lt, regs[b], k[c], <)),
                    Op::LeK => cond!(at, less!(at, Le, regs[b], k[c], <=)),
                    // `R > K` is `K < R` (that's how its error reads)
                    Op::GtK => cond!(at, less!(at, Lt, k[c], regs[b], <)),
                    Op::GeK => cond!(at, less!(at, Le, k[c], regs[b], <=)),
                    Op::Jmp => {
                        let by = ins.sax();
                        if by < 0 {
                            // a loop's back-edge, charge the body it repeats
                            charge!(at, by.unsigned_abs());
                        }
                        pc = offset(pc, by);
                    }
                    Op::JmpIf => {
                        if regs[a].is_truthy() {
                            back_edge!(at, ins.sbx());
                            pc = offset(pc, ins.sbx() as i32);
                        }
                    }
                    Op::JmpIfNot => {
                        if !regs[a].is_truthy() {
                            back_edge!(at, ins.sbx());
                            pc = offset(pc, ins.sbx() as i32);
                        }
                    }

                    Op::ForPrep => {
                        let check = |v: Value, what: &str| match v {
                            Value::Number(n) => Ok(n),
                            other => Err(format!(
                                "invalid 'for' {what} (number expected, got {})",
                                other.type_name()
                            )),
                        };
                        let run = (|| {
                            let start = check(regs[a], "initial value")?;
                            let limit = check(regs[a + 1], "limit")?;
                            let step = check(regs[a + 2], "step")?;
                            if step == 0.0 {
                                return Err("'for' step is zero".to_string());
                            }
                            Ok(if step > 0.0 {
                                start <= limit
                            } else {
                                start >= limit
                            })
                        })();
                        match run {
                            Ok(true) => regs[a + 3] = regs[a],
                            Ok(false) => {
                                back_edge!(at, ins.sbx());
                                pc = offset(pc, ins.sbx() as i32);
                            }
                            Err(m) => fail!(at, "{m}"),
                        }
                    }
                    Op::ForLoop => {
                        let (Value::Number(idx), Value::Number(limit), Value::Number(step)) =
                            (regs[a], regs[a + 1], regs[a + 2])
                        else {
                            // FORPREP checked these, only bytecode that wasn't
                            // compiled from source gets here
                            fail!(at, "'for' loop state is not numeric");
                        };
                        let next = idx + step;
                        if if step > 0.0 {
                            next <= limit
                        } else {
                            next >= limit
                        } {
                            charge!(at, ins.sbx().unsigned_abs());
                            regs[a] = Value::Number(next);
                            regs[a + 3] = Value::Number(next);
                            pc = offset(pc, ins.sbx() as i32);
                        }
                    }

                    Op::GetUpval => {
                        self.stack[base + a] = match f.cl.upvalues[b].get() {
                            Upvalue::Open { index, thread } if thread == self.current => {
                                self.stack[index as usize]
                            }
                            Upvalue::Open { index, thread } => {
                                self.heap.thread(thread).state.stack[index as usize]
                            }
                            Upvalue::Closed(v) => v,
                        };
                    }
                    Op::SetUpval => {
                        let u = &f.cl.upvalues[b];
                        let v = regs[a];
                        match u.get() {
                            Upvalue::Open { index, thread } if thread == self.current => {
                                self.stack[index as usize] = v
                            }
                            Upvalue::Open { index, thread } => {
                                self.heap.thread_mut(thread).state.stack[index as usize] = v
                            }
                            Upvalue::Closed(_) => u.set(Upvalue::Closed(v)),
                        }
                    }
                    Op::Close => self.close_upvalues(base + a),
                    Op::Closure => {
                        let closure = self.new_closure(&f.cl, ins.bx() as usize, base);
                        self.stack[base + a] = Value::Function(closure);
                        gc!();
                    }
                    Op::Vararg => {
                        let n = if ins.b() == 0 {
                            f.varargs.len()
                        } else {
                            ins.b() as usize - 1
                        };
                        let a = base + a;
                        if !self.ensure_stack(a + n) {
                            fail!(at, "stack overflow");
                        }
                        for i in 0..n {
                            self.stack[a + i] = f.varargs.get(i).copied().unwrap_or(Value::Nil);
                        }
                        if ins.b() == 0 {
                            *top = a + n;
                        }
                    }

                    Op::NewTable => {
                        let t = self
                            .heap
                            .alloc_table(Table::with_capacity(ins.b() as usize, ins.c() as usize));
                        regs[a] = Value::Table(t);
                        gc!();
                    }
                    Op::DupTable => {
                        let t = &f.cl.proto.templates[ins.bx() as usize];
                        let keys = t.keys.iter().map(|&i| match k[i as usize] {
                            Value::String(s) => s,
                            _ => unreachable!("the verifier checks template keys are strings"),
                        });
                        let table = Table::from_template(t.array as usize, t.hash as usize, keys);
                        regs[a] = Value::Table(self.heap.alloc_table(table));
                        gc!();
                    }
                    Op::GetTable => {
                        let (obj, key) = (regs[b], regs[c]);
                        match obj {
                            Value::Table(t) => {
                                let table = self.heap.table(t);
                                let v = table.get(key);
                                if v == Value::Nil && table.meta().is_some() {
                                    index_slow!(obj, key);
                                }
                                regs[a] = v;
                            }
                            _ => index_slow!(obj, key),
                        }
                    }
                    Op::GetField => {
                        let (obj, key) = (regs[b], k[ins.c() as usize]);
                        match (obj, key) {
                            (Value::Table(t), Value::String(s)) => {
                                let hint = cache.slot(at, code.len());
                                let table = self.heap.table(t);
                                let mut v = table.get_str_hinted(s, hint);
                                if v == Value::Nil
                                    && let Some(m) = table.meta()
                                {
                                    match meta::class_lookup(&self.heap, &self.events, m, s) {
                                        Some(found) => v = found,
                                        None => index_slow!(obj, key),
                                    }
                                }
                                regs[a] = v;
                            }
                            _ => index_slow!(obj, key),
                        }
                    }
                    Op::SetTable => {
                        let (obj, key, v) = (regs[a], regs[b], regs[c]);
                        match obj {
                            Value::Table(t) if self.heap.table(t).meta().is_none() => {
                                if let Err(e) = self.heap.with_table(t, |tbl| tbl.set(key, v)) {
                                    fail!(at, "{}", e.message());
                                }
                                gc!();
                            }
                            _ => {
                                sync!();
                                self.set(f, at, obj, key, v)?;
                                gc!();
                                continue 'frame;
                            }
                        }
                    }
                    Op::SetField => {
                        let (obj, key) = (regs[a], k[ins.b() as usize]);
                        let v = regs[c];
                        match (obj, key) {
                            (Value::Table(t), Value::String(s)) => {
                                let hint = cache.slot(at, code.len());
                                if !self.heap.table_mut(t).set_existing_str(s, v, hint) {
                                    if self.heap.table(t).meta().is_some() {
                                        sync!();
                                        self.set(f, at, obj, key, v)?;
                                        gc!();
                                        continue 'frame;
                                    }
                                    if let Err(e) = self.heap.with_table(t, |tbl| tbl.set(key, v)) {
                                        fail!(at, "{}", e.message());
                                    }
                                    gc!();
                                }
                            }
                            _ => {
                                sync!();
                                self.set(f, at, obj, key, v)?;
                                gc!();
                                continue 'frame;
                            }
                        }
                    }
                    Op::SelfOp => {
                        let (obj, key) = (regs[b], k[ins.c() as usize]);
                        match (obj, key) {
                            (Value::Table(t), Value::String(s)) => {
                                let hint = cache.slot(at, code.len());
                                let table = self.heap.table(t);
                                let mut method = table.get_str_hinted(s, hint);
                                let class_method = match (method, table.meta()) {
                                    (Value::Nil, Some(m)) => {
                                        meta::class_lookup(&self.heap, &self.events, m, s)
                                    }
                                    _ => Some(method),
                                };
                                if let Some(found) = class_method {
                                    method = found;
                                } else {
                                    sync!();
                                    self.stack[base + a + 1] = obj;
                                    self.index_meta(f, at, obj, key, base + a)?;
                                    gc!();
                                    continue 'frame;
                                }
                                regs[a] = method;
                                regs[a + 1] = obj;
                            }
                            _ => {
                                sync!();
                                self.stack[base + a + 1] = obj;
                                self.index_meta(f, at, obj, key, base + a)?;
                                gc!();
                                continue 'frame;
                            }
                        }
                    }
                    Op::SetList => {
                        let start = match code.get(pc) {
                            Some(data) if data.op() == Some(Op::Data) => data.sax() as usize,
                            _ => fail!(at, "SETLIST without its DATA word"),
                        };
                        pc += 1;
                        let count = if b == 0 { *top - (base + a + 1) } else { b };
                        let Value::Table(t) = regs[a] else {
                            fail!(at, "SETLIST target is not a table")
                        };
                        let values = &self.stack[base + a + 1..base + a + 1 + count];
                        self.heap.with_table(t, |tbl| {
                            for (i, &v) in values.iter().enumerate() {
                                let _ = tbl.set(Value::Number((start + i + 1) as f64), v);
                            }
                        });
                        gc!();
                    }
                    Op::Data => fail!(at, "executed a DATA word"),
                    Op::TForCall => {
                        let nvars = ins.c() as usize;
                        match regs[a] {
                            // `__iter` gives the iterator triple once on the
                            // first step, then the loop calls it
                            Value::Table(t)
                                if self.heap.table(t).meta().is_some_and(|m| {
                                    self.heap
                                        .table(m)
                                        .get_str(self.events.get(meta::Event::Iter))
                                        != Value::Nil
                                }) =>
                            {
                                sync!();
                                self.iter_meta(f, at, base + a)?;
                                let a = base + a;
                                self.stack.copy_within(a..a + 3, a + 3);
                                // `__iter` already ran, no going back
                                if self.budget < 1 {
                                    self.refill_budget(f, at, 1)?;
                                }
                                self.budget -= 1;
                                self.call(f, at, a + 3, 2, Some(nvars), top, false)?;
                                gc!();
                                continue 'frame;
                            }
                            // luau's generalized iteration, `for k, v in t`
                            Value::Table(t) => match self.heap.table(t).next(regs[a + 2]) {
                                Ok(entry) => {
                                    let (key, v) = entry.unwrap_or((Value::Nil, Value::Nil));
                                    regs[a + 3] = key;
                                    for i in 1..nvars {
                                        regs[a + 3 + i] = if i == 1 { v } else { Value::Nil };
                                    }
                                }
                                Err(table::InvalidKey) => fail!(at, "invalid key to 'next'"),
                            },
                            Value::Function(_) => {
                                regs[a + 3] = regs[a];
                                regs[a + 4] = regs[a + 1];
                                regs[a + 5] = regs[a + 2];
                                charge!(at, 1);
                                sync!();
                                self.call(f, at, base + a + 3, 2, Some(nvars), top, false)?;
                                gc!();
                                continue 'frame;
                            }
                            other => {
                                fail!(at, "attempt to iterate over a {} value", other.type_name())
                            }
                        }
                    }
                    Op::TForLoop => {
                        if regs[a + 3] != Value::Nil {
                            charge!(at, ins.sbx().unsigned_abs());
                            regs[a + 2] = regs[a + 3];
                            pc = offset(pc, ins.sbx() as i32);
                        }
                    }

                    Op::Interp => {
                        let from = base + b;
                        let size: usize = (0..c)
                            .map(|i| match self.stack[from + i] {
                                Value::String(s) => self.heap.get(s).len(),
                                _ => 24,
                            })
                            .sum();
                        if self.heap.would_exceed(size) {
                            return Err(self.out_of_memory(f, at));
                        }
                        let mut out = String::new();
                        for i in 0..c {
                            match self.stack[from + i] {
                                Value::String(s) => out.push_str(self.heap.get(s)),
                                Value::Number(x) => out.push_str(&format_number(x)),
                                // might have `__tostring`
                                Value::Table(t) if self.heap.table(t).meta().is_some() => {
                                    sync!();
                                    let v = self.interp_meta(f, at, from, c)?;
                                    self.stack[base + a] = v;
                                    gc!();
                                    continue 'frame;
                                }
                                other => out.push_str(&self.display(other)),
                            }
                        }
                        self.stack[base + a] = Value::String(self.heap.intern_owned(out));
                        gc!();
                    }
                    Op::TailCall => {
                        let a = base + a;
                        let nargs = if b == 0 { *top - a - 1 } else { b - 1 };
                        charge!(at, 1);
                        sync!();
                        self.tail_call(f, at, a, nargs, top)?;
                        gc!();
                        continue 'frame;
                    }
                    Op::Call => {
                        let a = base + a;
                        let nargs = if b == 0 { *top - a - 1 } else { b - 1 };
                        let want = if ins.c() == 0 {
                            None
                        } else {
                            Some(ins.c() as usize - 1)
                        };
                        charge!(at, 1);
                        sync!();
                        self.call(f, at, a, nargs, want, top, false)?;
                        gc!();
                        continue 'frame;
                    }

                    Op::Return => {
                        let a = base + a;
                        let end = if c != 0 { *top } else { a + b };
                        self.close_upvalues(base);
                        if self.frames.len() == self.boundary {
                            let mut values = Rets::with_capacity(end - a + 1);
                            if f.protected {
                                values.push(Value::Bool(true));
                            }
                            values.extend_from_slice(&self.stack[a..end]);
                            return Ok(values);
                        }
                        // results move down to the callee's slot, which is
                        // below them, so a forward copy never overwrites one
                        // that hasn't been read
                        let ret_to = f.ret_to();
                        let mut dest = ret_to;
                        let count = end - a + f.protected as usize;
                        if f.protected {
                            self.stack[dest] = Value::Bool(true);
                            dest += 1;
                        }
                        if end - a <= 3 {
                            // one to three results (almost every return), moved
                            // by hand instead of through `memmove`
                            for i in 0..end - a {
                                self.stack[dest + i] = self.stack[a + i];
                            }
                        } else {
                            self.stack.copy_within(a..end, dest);
                        }
                        match f.ret_want() {
                            Some(want) if want > count => {
                                self.ensure_stack(ret_to + want);
                                self.stack[ret_to + count..ret_to + want].fill(Value::Nil);
                            }
                            Some(_) => {}
                            None => *top = ret_to + count,
                        }
                        let cond = f.cond;
                        *f = self.frames.pop().expect("frames above the boundary");
                        // a comparison metamethod, its result picks the jump
                        if let Some(expect) = cond {
                            let result = self.stack[ret_to].is_truthy();
                            self.finish_cond(f, result, expect)?;
                        }
                        continue 'frame;
                    }
                }
            }
        }
    }
}

#[inline]
#[expect(
    clippy::cast_possible_truncation,
    reason = "stack slots and pcs stay far under u32 (the stack and code are capped), opcodes are a byte"
)]
fn offset(pc: usize, by: i32) -> usize {
    (pc as i64 + by as i64) as usize
}

#[inline]
fn arith_values(op: ArithOp, l: Value, r: Value) -> Result<Value, String> {
    match (l, r) {
        (Value::Number(x), Value::Number(y)) => Ok(Value::Number(arith(op, x, y))),
        (Value::Vector(_), _) | (_, Value::Vector(_)) => vector_arith(op, l, r),
        (Value::Number(_), bad) | (bad, Value::Number(_)) => Err(format!(
            "attempt to perform arithmetic ({}) on {}",
            op.name(),
            bad.type_name()
        )),
        (l, r) => Err(format!(
            "attempt to perform arithmetic ({}) on {} and {}",
            op.name(),
            l.type_name(),
            r.type_name()
        )),
    }
}

/// Arithmetic with vectors, the way Luau defines it: componentwise between two
/// vectors (`+ - * / //`), and scaling by a number (`v * n`, `n * v`, `v / n`,
/// `n / v`, `v // n`). Done in `f32`.
#[expect(
    clippy::cast_possible_truncation,
    reason = "stack slots and pcs stay far under u32 (the stack and code are capped), opcodes are a byte"
)]
fn vector_arith(op: ArithOp, l: Value, r: Value) -> Result<Value, String> {
    let apply = |x: f32, y: f32| -> f32 {
        match op {
            ArithOp::Add => x + y,
            ArithOp::Sub => x - y,
            ArithOp::Mul => x * y,
            ArithOp::Div => x / y,
            ArithOp::IDiv => (x / y).floor(),
            ArithOp::Mod | ArithOp::Pow => unreachable!("rejected below"),
        }
    };
    let scalable = matches!(op, ArithOp::Mul | ArithOp::Div | ArithOp::IDiv);
    let v = match (l, r) {
        _ if matches!(op, ArithOp::Mod | ArithOp::Pow) => None,
        (Value::Vector(a), Value::Vector(b)) => Some([0, 1, 2].map(|i| apply(a[i], b[i]))),
        (Value::Vector(a), Value::Number(n)) if scalable => Some(a.map(|c| apply(c, n as f32))),
        (Value::Number(n), Value::Vector(b)) if matches!(op, ArithOp::Mul | ArithOp::Div) => {
            Some(b.map(|c| apply(n as f32, c)))
        }
        _ => None,
    };
    v.map(Value::Vector).ok_or_else(|| {
        format!(
            "attempt to perform arithmetic ({}) on {} and {}",
            op.name(),
            l.type_name(),
            r.type_name()
        )
    })
}

/// A vector's component by name (`x`, `y`, `z`, or Roblox's `X`, `Y`, `Z`).
fn vector_component(v: [f32; 3], key: Value, heap: &Heap) -> Option<f64> {
    let Value::String(s) = key else { return None };
    let i = match heap.get(s) {
        "x" | "X" => 0,
        "y" | "Y" => 1,
        "z" | "Z" => 2,
        _ => return None,
    };
    Some(f64::from(v[i]))
}

fn compare(op: Op, l: Value, r: Value, heap: &Heap) -> Result<bool, String> {
    let lt = op == Op::Lt;
    match (l, r) {
        (Value::Number(x), Value::Number(y)) => Ok(if lt { x < y } else { x <= y }),
        (Value::String(x), Value::String(y)) => {
            let (x, y) = (heap.get(x), heap.get(y));
            Ok(if lt { x < y } else { x <= y })
        }
        _ => Err(format!(
            "attempt to compare {} {} {}",
            l.type_name(),
            if lt { "<" } else { "<=" },
            r.type_name()
        )),
    }
}

#[cfg(test)]
mod tests;
