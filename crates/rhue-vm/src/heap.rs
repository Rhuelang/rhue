//! The object heap (strings, tables, functions and upvalue cells) and the
//! mark-and-sweep garbage collector.
//!
//! **Handles, not pointers.** Every object lives in a `Slab` and gets named by
//! `index + generation`. Freeing a slot bumps its generation, so a stale handle
//! fails a check instead of quietly aliasing whatever reused the slot. `Value`
//! stays `Copy`, and the workspace forbids `unsafe` (see `Cargo.toml`).
//!
//! **Interning.** Every string is interned, equal text means an equal handle,
//! so string equality and table-key hashing are O(1). The intern map is weak,
//! the collector drops entries for strings it frees.
//!
//! **Collection.** [`Heap::collect`] marks from the roots the VM hands it
//! (stack, frames, globals, open upvalues, host pins) with an explicit worklist
//! (no recursion, so deep structures can't blow the stack), then sweeps every
//! slab. The VM only collects at *safe points* (the top of its dispatch loop),
//! when every live value can be reached from a root.

use std::cell::Cell;
use std::fmt;
use std::hash::BuildHasher;
use std::rc::Rc;

use rhue_bytecode::Proto;

use crate::table::Table;
use crate::{NativeCtx, NativeError, Value};

macro_rules! handle {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub struct $name {
            index: u32,
            generation: u32,
        }

        impl $name {
            /// A stable number for `tostring` (`table: 0x0000002a`).
            pub fn id(self) -> u32 {
                self.index
            }
        }
    };
}

handle!(
    /// A handle to an interned string.
    StrRef
);
handle!(
    /// A handle to a function (Lua closure or native).
    FnRef
);
handle!(
    /// A handle to a table.
    TableRef
);
/// An upvalue cell, shared by every closure that captures the variable
/// (reference-counted, so reading one is a single `Cell::get` with no heap
/// lookup). A closed cell holds a `Value` handle, which doesn't own anything,
/// so cells can't make reference cycles. The collector marks what closed cells
/// hold through the closures that reach them.
pub type UpvalRef = Rc<Cell<Upvalue>>;
handle!(
    /// A handle to a userdata object (a Rust value owned by the VM).
    UdRef
);
handle!(
    /// A handle to a thread (a coroutine, or the main thread).
    ThreadRef
);
handle!(
    /// A handle to a buffer, a fixed-size mutable array of bytes.
    BufRef
);

/// Where a coroutine is in its life (`coroutine.status`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoStatus {
    /// Made and not started yet, or stopped in `coroutine.yield`.
    Suspended,
    /// The thread running right now.
    Running,
    /// Waiting for a coroutine it resumed.
    Normal,
    /// Returned, errored, or got closed.
    Dead,
}

impl CoStatus {
    pub fn name(self) -> &'static str {
        match self {
            CoStatus::Suspended => "suspended",
            CoStatus::Running => "running",
            CoStatus::Normal => "normal",
            CoStatus::Dead => "dead",
        }
    }
}

/// A stop the host asked for that hasn't happened yet (see
/// [`Vm::kill_thread`](crate::Vm::kill_thread)).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Mark {
    #[default]
    None,
    /// Dies the next time it runs, wherever it is.
    Kill,
    /// Dies the next time it tries to pause.
    Cancel,
}

/// How a dead thread ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ThreadEnd {
    /// Returned.
    Finished,
    /// An error nothing caught.
    Failed,
    /// [`Vm::kill_thread`](crate::Vm::kill_thread), or `coroutine.close`.
    Killed,
    /// [`Vm::cancel_thread`](crate::Vm::cancel_thread).
    Cancelled,
}

/// A thread's execution state while it isn't the one running: its value stack,
/// call frames and open upvalues. The running thread's state lives in the VM
/// itself, and resuming and yielding swap these.
#[derive(Default)]
pub(crate) struct ThreadState {
    pub(crate) stack: Vec<Value>,
    pub(crate) stack_top: usize,
    pub(crate) frames: Vec<crate::Frame>,
    pub(crate) open_upvalues: Vec<(usize, UpvalRef)>,
    pub(crate) boundary: usize,
    /// For a coroutine stopped in `yield`: where that call's results go.
    pub(crate) pending: Option<crate::coroutine::Parked>,
}

/// A coroutine (or the main thread).
pub struct Thread {
    pub(crate) state: ThreadState,
    pub(crate) status: CoStatus,
    /// The VM's nesting depth while this thread runs at its base. `yield` only
    /// works there (not from inside a native's callback).
    pub(crate) base_depth: usize,
    /// Bytes counted for this thread's saved stack.
    pub(crate) bytes: usize,
    /// Last resumed by the host instead of a script (see
    /// [`Vm::resumed_by_host`](crate::Vm::resumed_by_host)).
    pub(crate) by_host: bool,
    /// A kill or cancel waiting to happen.
    pub(crate) mark: Mark,
    /// How it ended, once it's dead.
    pub(crate) end: Option<ThreadEnd>,
}

impl Thread {
    pub fn status(&self) -> CoStatus {
        self.status
    }

    /// How it ended, or `None` while it's alive.
    pub fn end(&self) -> Option<ThreadEnd> {
        self.end
    }
}

impl fmt::Debug for Thread {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<thread {}>", self.status.name())
    }
}

/// A Rust value a script can hold. `data` is whatever the host stored (the
/// `rhue` crate stores an `Rc<RefCell<T>>`), and `type_id` indexes the VM's
/// userdata types, which say how scripts index it.
pub struct UserData {
    pub type_id: u32,
    pub data: Rc<dyn std::any::Any>,
}

impl fmt::Debug for UserData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<userdata type {}>", self.type_id)
    }
}

/// An upvalue is *open* while the variable it captures is still alive in a
/// stack register, and *closed* once that register's scope ends (then the value
/// lives in the cell itself). Every closure capturing the same variable shares
/// one cell, so they all see each other's writes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Upvalue {
    /// Register `index` of `thread`'s stack.
    Open {
        index: u32,
        thread: ThreadRef,
    },
    Closed(Value),
}

/// What a native returns. Up to three values are stored inline, so most native
/// calls don't allocate.
pub type Rets = smallvec::SmallVec<[Value; 3]>;

/// A native function. Gets its arguments, returns its results or an error.
pub type NativeFn = Rc<dyn Fn(&mut NativeCtx<'_>, &[Value]) -> Result<Rets, NativeError>>;

/// Natives the VM implements itself, since they need to call back into the
/// interpreter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Special {
    Pcall,
    /// `xpcall`. Its `func` is what runs when a native or the host calls it.
    Xpcall,
    /// `coroutine.resume`.
    Resume,
    /// `coroutine.yield`.
    Yield,
    /// A function made by `coroutine.wrap` (its coroutine is `bound`).
    Wrapped,
}

pub struct Native {
    pub name: Rc<str>,
    pub func: NativeFn,
    /// A shortcut for the common case of a builtin (see [`FastFn`]).
    pub fast: Option<FastFn>,
    pub special: Option<Special>,
    /// A value the native carries around (the coroutine of a `coroutine.wrap`
    /// function), the collector keeps it alive.
    pub bound: Value,
}

impl fmt::Debug for Native {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<native {}>", self.name)
    }
}

/// A builtin's fast path. Given up to three arguments it returns its results,
/// or `None` to run the full native instead (for anything unusual, every error
/// included, so messages come from one place). It runs without a native
/// context, argument buffer or result vector.
pub type FastFn = fn(&mut crate::Vm, &[Value]) -> Option<Fast>;

/// What a fast path returns, one result or two (an iterator's step).
#[derive(Clone, Copy, Debug)]
pub enum Fast {
    One(Value),
    Two(Value, Value),
}

#[derive(Debug)]
pub struct LuaClosure {
    pub proto: Rc<Proto>,
    pub upvalues: Rc<[UpvalRef]>,
    /// The prototype's constants as runtime values (strings interned).
    pub constants: Rc<[Value]>,
    /// The same, plus those of the prototypes nested in it (what `CLOSURE`
    /// gives the closures it makes).
    pub tree: Rc<Constants>,
    /// The table its global reads and writes go to when it runs in an
    /// environment (see [`crate::Vm::load_in`]). `None` for the VM's own
    /// globals. Closures it makes inherit it.
    pub env: Option<TableRef>,
}

/// A prototype's constants as runtime values, and (in the same order as
/// [`Proto::protos`]) its nested prototypes'. Built once when a chunk loads and
/// shared by every closure made from it, so making a closure doesn't convert or
/// intern anything.
#[derive(Debug, Default)]
pub struct Constants {
    pub values: Rc<[Value]>,
    pub children: Box<[Rc<Constants>]>,
    /// The collection that last marked this tree (so a tree shared by a ton of
    /// closures only gets walked once per collection).
    marked: Cell<u64>,
}

impl Constants {
    pub fn new(values: Rc<[Value]>, children: Box<[Rc<Constants>]>) -> Self {
        Constants {
            values,
            children,
            marked: Cell::new(u64::MAX),
        }
    }

    fn mark(&self, epoch: u64, marks: &mut Marks) {
        if self.marked.replace(epoch) == epoch {
            return;
        }
        for &v in self.values.iter() {
            marks.value(v);
        }
        for c in self.children.iter() {
            c.mark(epoch, marks);
        }
    }
}

#[derive(Debug)]
pub enum Function {
    /// Shared so a call frame can hold the closure with one refcount.
    Lua(Rc<LuaClosure>),
    Native(Native),
}

#[derive(Debug)]
struct Slot<T> {
    generation: u32,
    value: Option<T>,
}

/// A vector of slots with a free list.
#[derive(Debug)]
struct Slab<T> {
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
}

impl<T> Default for Slab<T> {
    fn default() -> Self {
        Slab {
            slots: Vec::new(),
            free: Vec::new(),
        }
    }
}

impl<T> Slab<T> {
    fn alloc(&mut self, value: T) -> (u32, u32) {
        match self.free.pop() {
            Some(i) => {
                let slot = &mut self.slots[i as usize];
                slot.value = Some(value);
                (i, slot.generation)
            }
            None => {
                let i = u32::try_from(self.slots.len())
                    .expect("more than 4 billion objects of one kind");
                self.slots.push(Slot {
                    generation: 0,
                    value: Some(value),
                });
                (i, 0)
            }
        }
    }

    fn get(&self, index: u32, generation: u32, what: &str) -> &T {
        let slot = &self.slots[index as usize];
        assert_eq!(
            slot.generation, generation,
            "stale {what} handle (a VM or GC bug)"
        );
        slot.value.as_ref().expect("live slot")
    }

    fn get_mut(&mut self, index: u32, generation: u32, what: &str) -> &mut T {
        let slot = &mut self.slots[index as usize];
        assert_eq!(
            slot.generation, generation,
            "stale {what} handle (a VM or GC bug)"
        );
        slot.value.as_mut().expect("live slot")
    }

    fn live(&self) -> usize {
        self.slots.len() - self.free.len()
    }

    /// Frees every live slot `keep` says no to, calling `on_free` with each.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "slab slots and type ids are capped well under u32"
    )]
    fn sweep(&mut self, keep: &[bool], mut on_free: impl FnMut(T)) {
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if slot.value.is_some() && !keep[i] {
                let v = slot.value.take().expect("checked");
                slot.generation = slot.generation.wrapping_add(1);
                self.free.push(i as u32);
                on_free(v);
            }
        }
    }
}

/// Live object counts and memory use, for `collectgarbage("count")` and
/// [`crate::Vm::heap_stats`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct HeapStats {
    pub strings: usize,
    pub tables: usize,
    pub functions: usize,
    pub userdata: usize,
    pub threads: usize,
    pub buffers: usize,
    /// Approximate bytes in use.
    pub bytes: usize,
    /// Collections run so far.
    pub collections: u64,
}

const STRING_BYTES: usize = 32;
const FUNCTION_BYTES: usize = 64;
const USERDATA_BYTES: usize = 48;
const THREAD_BYTES: usize = 128;
const BUFFER_BYTES: usize = 32;
const MIN_THRESHOLD: usize = 256 * 1024;

/// The weak intern map, string contents → handle. Hashed with foldhash, seeded
/// randomly per heap (so scripts can't pick colliding keys ahead of time), and
/// looked up by `&str` with the hash computed once.
struct InternTable {
    table: hashbrown::HashTable<(Rc<str>, StrRef)>,
    state: foldhash::fast::RandomState,
}

impl Default for InternTable {
    fn default() -> Self {
        InternTable {
            table: hashbrown::HashTable::new(),
            state: foldhash::fast::RandomState::default(),
        }
    }
}

impl fmt::Debug for InternTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "InternTable({} strings)", self.table.len())
    }
}

impl InternTable {
    #[inline]
    fn hash(&self, text: &str) -> u64 {
        self.state.hash_one(text)
    }

    #[inline]
    fn get(&self, hash: u64, text: &str) -> Option<StrRef> {
        self.table
            .find(hash, |(s, _)| **s == *text)
            .map(|&(_, r)| r)
    }

    fn insert(&mut self, hash: u64, text: Rc<str>, r: StrRef) {
        let state = &self.state;
        self.table
            .insert_unique(hash, (text, r), |(s, _)| state.hash_one(&**s));
    }

    fn remove(&mut self, text: &Rc<str>) {
        let hash = self.hash(text);
        if let Ok(entry) = self.table.find_entry(hash, |(s, _)| Rc::ptr_eq(s, text)) {
            entry.remove();
        }
    }
}

#[derive(Debug)]
pub struct Heap {
    strings: Slab<Rc<str>>,
    interned: InternTable,
    tables: Slab<Table>,
    functions: Slab<Function>,
    userdata: Slab<UserData>,
    threads: Slab<Thread>,
    buffers: Slab<Box<[u8]>>,
    /// Reused by every collection.
    marks: Marks,
    /// Names of the userdata types, indexed by `UserData::type_id`.
    ud_type_names: Vec<Rc<str>>,
    /// Approximate live bytes (updated on allocation and table growth).
    bytes: usize,
    /// Collect when `bytes` passes this.
    threshold: usize,
    /// After a collection, `threshold = live bytes * pause / 100`.
    pause_percent: usize,
    collections: u64,
    /// Collect at every safe point (a testing aid for finding missing roots).
    stress: bool,
    /// The memory limit, if any (see [`Heap::set_limit`]).
    limit: Option<usize>,
}

impl Default for Heap {
    fn default() -> Self {
        Heap {
            strings: Slab::default(),
            interned: InternTable::default(),
            tables: Slab::default(),
            functions: Slab::default(),
            userdata: Slab::default(),
            threads: Slab::default(),
            buffers: Slab::default(),
            marks: Marks::default(),
            ud_type_names: Vec::new(),
            bytes: 0,
            threshold: MIN_THRESHOLD,
            pause_percent: 200,
            collections: 0,
            stress: false,
            limit: None,
        }
    }
}

impl Heap {
    pub fn new() -> Self {
        Self::default()
    }

    // ─ Strings ───────────────────────────────────────────────────────────────

    /// The handle for `text`, allocating it if it isn't interned yet.
    pub fn intern(&mut self, text: &str) -> StrRef {
        let hash = self.interned.hash(text);
        if let Some(r) = self.interned.get(hash, text) {
            return r;
        }
        self.insert_string(hash, Rc::from(text))
    }

    /// Like [`Heap::intern`] but takes ownership (saves a copy for freshly
    /// built strings like concatenation results).
    pub fn intern_owned(&mut self, text: String) -> StrRef {
        let hash = self.interned.hash(&text);
        if let Some(r) = self.interned.get(hash, &text) {
            return r;
        }
        self.insert_string(hash, Rc::from(text))
    }

    fn insert_string(&mut self, hash: u64, text: Rc<str>) -> StrRef {
        self.bytes += STRING_BYTES + text.len();
        let (index, generation) = self.strings.alloc(text.clone());
        let r = StrRef { index, generation };
        self.interned.insert(hash, text, r);
        r
    }

    /// The text behind a handle.
    pub fn get(&self, r: StrRef) -> &str {
        self.strings.get(r.index, r.generation, "string")
    }

    pub fn get_rc(&self, r: StrRef) -> Rc<str> {
        self.strings.get(r.index, r.generation, "string").clone()
    }

    // ─ Userdata ──────────────────────────────────────────────────────────────

    pub fn alloc_userdata(&mut self, ud: UserData) -> UdRef {
        self.bytes += USERDATA_BYTES + std::mem::size_of_val(&*ud.data);
        let (index, generation) = self.userdata.alloc(ud);
        UdRef { index, generation }
    }

    pub fn userdata(&self, r: UdRef) -> &UserData {
        self.userdata.get(r.index, r.generation, "userdata")
    }

    /// Adds a userdata type name and returns its id.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "slab slots and type ids are capped well under u32"
    )]
    pub fn add_userdata_type(&mut self, name: &str) -> u32 {
        self.ud_type_names.push(Rc::from(name));
        (self.ud_type_names.len() - 1) as u32
    }

    /// The type name of a userdata (`"Player"`), what `typeof` returns.
    pub fn userdata_type_name(&self, r: UdRef) -> &str {
        &self.ud_type_names[self.userdata(r).type_id as usize]
    }

    // ─ Buffers ───────────────────────────────────────────────────────────────

    /// A buffer of `len` zero bytes (callers check the memory limit).
    pub fn alloc_buffer(&mut self, bytes: Box<[u8]>) -> BufRef {
        self.bytes += BUFFER_BYTES + bytes.len();
        let (index, generation) = self.buffers.alloc(bytes);
        BufRef { index, generation }
    }

    pub fn buffer(&self, r: BufRef) -> &[u8] {
        self.buffers.get(r.index, r.generation, "buffer")
    }

    pub fn buffer_mut(&mut self, r: BufRef) -> &mut [u8] {
        self.buffers.get_mut(r.index, r.generation, "buffer")
    }

    // ─ Tables ────────────────────────────────────────────────────────────────

    pub fn alloc_table(&mut self, table: Table) -> TableRef {
        self.bytes += table.bytes();
        let (index, generation) = self.tables.alloc(table);
        TableRef { index, generation }
    }

    pub fn table(&self, r: TableRef) -> &Table {
        self.tables.get(r.index, r.generation, "table")
    }

    /// A table to change in a way that can't grow it (overwriting an existing
    /// entry). Anything else goes through [`with_table`](Self::with_table).
    pub(crate) fn table_mut(&mut self, r: TableRef) -> &mut Table {
        self.tables.get_mut(r.index, r.generation, "table")
    }

    /// Mutates a table, keeping the byte count in step with how it grew.
    pub fn with_table<R>(&mut self, r: TableRef, f: impl FnOnce(&mut Table) -> R) -> R {
        let t = self.tables.get_mut(r.index, r.generation, "table");
        let before = t.bytes();
        let result = f(t);
        let after = t.bytes();
        self.bytes = (self.bytes + after).saturating_sub(before);
        result
    }

    // ─ Functions ─────────────────────────────────────────────────────────────

    pub fn alloc_function(&mut self, f: Function) -> FnRef {
        self.bytes += FUNCTION_BYTES
            + match &f {
                Function::Lua(c) => (c.upvalues.len() + c.constants.len()) * 16,
                Function::Native(_) => 0,
            };
        let (index, generation) = self.functions.alloc(f);
        FnRef { index, generation }
    }

    pub fn function(&self, r: FnRef) -> &Function {
        self.functions.get(r.index, r.generation, "function")
    }

    /// Puts `cl` in function `r`'s slot, so every handle to it runs `cl` from
    /// now on (hot reload).
    pub(crate) fn replace_closure(&mut self, r: FnRef, cl: Rc<LuaClosure>) {
        let slot = self.functions.get_mut(r.index, r.generation, "function");
        let before = match slot {
            Function::Lua(c) => c.upvalues.len() + c.constants.len(),
            Function::Native(_) => 0,
        };
        let after = cl.upvalues.len() + cl.constants.len();
        *slot = Function::Lua(cl);
        self.bytes = (self.bytes + after * 16).saturating_sub(before * 16);
    }

    /// Gives native `r` a fast path.
    pub(crate) fn set_fast(&mut self, r: FnRef, fast: FastFn) {
        if let Function::Native(n) = self.functions.get_mut(r.index, r.generation, "function") {
            n.fast = Some(fast);
        }
    }

    // ─ Threads ───────────────────────────────────────────────────────────────

    pub(crate) fn alloc_thread(&mut self, state: ThreadState, status: CoStatus) -> ThreadRef {
        let bytes = THREAD_BYTES + state.stack.capacity() * 16;
        self.bytes += bytes;
        let (index, generation) = self.threads.alloc(Thread {
            state,
            status,
            base_depth: 0,
            bytes,
            by_host: false,
            mark: Mark::None,
            end: None,
        });
        ThreadRef { index, generation }
    }

    pub fn thread(&self, r: ThreadRef) -> &Thread {
        self.threads.get(r.index, r.generation, "thread")
    }

    pub(crate) fn thread_mut(&mut self, r: ThreadRef) -> &mut Thread {
        self.threads.get_mut(r.index, r.generation, "thread")
    }

    /// Stores a thread's state while it isn't running, counting its stack.
    pub(crate) fn park_thread(&mut self, r: ThreadRef, state: ThreadState) {
        let bytes = THREAD_BYTES + state.stack.capacity() * 16;
        let t = self.threads.get_mut(r.index, r.generation, "thread");
        let before = t.bytes;
        t.state = state;
        t.bytes = bytes;
        self.bytes = (self.bytes + bytes).saturating_sub(before);
    }

    /// A parked thread's state, to swap with the running one in place.
    pub(crate) fn thread_state_mut(&mut self, r: ThreadRef) -> &mut ThreadState {
        &mut self.threads.get_mut(r.index, r.generation, "thread").state
    }

    /// Updates the bytes counted for thread `r` after its state changed in
    /// place (what [`park_thread`](Self::park_thread) does for a new state).
    pub(crate) fn account_thread(&mut self, r: ThreadRef) {
        let t = self.threads.get_mut(r.index, r.generation, "thread");
        let bytes = THREAD_BYTES + t.state.stack.capacity() * 16;
        let before = t.bytes;
        t.bytes = bytes;
        self.bytes = (self.bytes + bytes).saturating_sub(before);
    }

    /// Takes a thread's state out to run it.
    pub(crate) fn unpark_thread(&mut self, r: ThreadRef) -> ThreadState {
        std::mem::take(&mut self.threads.get_mut(r.index, r.generation, "thread").state)
    }

    // ─ Upvalues ──────────────────────────────────────────────────────────────

    /// A new upvalue cell (its memory counts as part of the closures holding
    /// it).
    pub fn alloc_upvalue(&mut self, u: Upvalue) -> UpvalRef {
        Rc::new(Cell::new(u))
    }

    // ─ Collection ────────────────────────────────────────────────────────────

    /// Whether enough's been allocated since the last collection.
    #[inline]
    pub fn should_collect(&self) -> bool {
        self.stress || self.bytes >= self.threshold || self.over_limit()
    }

    /// Caps (roughly) how many bytes scripts can keep allocated.
    pub fn set_limit(&mut self, limit: Option<usize>) {
        self.limit = limit;
    }

    pub fn limit(&self) -> Option<usize> {
        self.limit
    }

    /// More allocated than the limit allows.
    #[inline]
    pub fn over_limit(&self) -> bool {
        self.limit.is_some_and(|l| self.bytes > l)
    }

    /// Whether allocating `extra` more bytes would pass the limit.
    #[inline]
    pub fn would_exceed(&self, extra: usize) -> bool {
        self.limit
            .is_some_and(|l| self.bytes.saturating_add(extra) > l)
    }

    /// Stress mode, collect at every safe point. Really slow, it's for tests
    /// hunting down objects the collector can't see.
    pub fn set_stress(&mut self, on: bool) {
        self.stress = on;
    }

    /// Makes the VM collect at its next safe point (`collectgarbage()`).
    pub fn request_collection(&mut self) {
        self.threshold = 0;
    }

    /// Sets the pause. After a collection the next one starts once the heap
    /// grows to `percent`% of what survived (default 200, so double).
    pub fn set_pause(&mut self, percent: usize) {
        self.pause_percent = percent.max(100);
    }

    pub fn stats(&self) -> HeapStats {
        HeapStats {
            strings: self.strings.live(),
            tables: self.tables.live(),
            functions: self.functions.live(),
            userdata: self.userdata.live(),
            threads: self.threads.live(),
            buffers: self.buffers.live(),
            bytes: self.bytes,
            collections: self.collections,
        }
    }

    /// Marks everything reachable from `roots` and frees the rest.
    pub fn collect(&mut self, roots: impl IntoIterator<Item = GcRoot>) {
        // the mark vectors stick around between collections, allocating (and
        // freeing) them every time costs more than clearing them
        let mut marks = std::mem::take(&mut self.marks);
        let reset = |v: &mut Vec<bool>, n: usize| {
            v.clear();
            v.resize(n, false);
        };
        reset(&mut marks.strings, self.strings.slots.len());
        reset(&mut marks.tables, self.tables.slots.len());
        reset(&mut marks.functions, self.functions.slots.len());
        reset(&mut marks.userdata, self.userdata.slots.len());
        reset(&mut marks.threads, self.threads.slots.len());
        reset(&mut marks.buffers, self.buffers.slots.len());
        marks.work.clear();
        for root in roots {
            marks.push(root);
        }
        let epoch = self.collections;
        while let Some(item) = marks.work.pop() {
            match item {
                Work::Table(r) => self.table(r).for_each_ref(|v| marks.value(v)),
                Work::Function(r) => match self.function(r) {
                    Function::Lua(c) => {
                        for u in c.upvalues.iter() {
                            match u.get() {
                                Upvalue::Closed(v) => marks.value(v),
                                // an open upvalue's value lives on its thread's
                                // stack, so the thread stays alive with it (the
                                // running one's a root)
                                Upvalue::Open { thread, .. } => marks.value(Value::Thread(thread)),
                            }
                        }
                        c.tree.mark(epoch, &mut marks);
                        if let Some(env) = c.env {
                            marks.value(Value::Table(env));
                        }
                    }
                    Function::Native(n) => marks.value(n.bound),
                },
                Work::Thread(r) => {
                    let st = &self.thread(r).state;
                    for &v in &st.stack[..st.stack_top] {
                        marks.value(v);
                    }
                    for frame in &st.frames {
                        frame.for_each_root(|v| marks.value(v));
                    }
                }
            }
        }

        let mut freed_bytes = 0usize;
        let interned = &mut self.interned;
        self.strings.sweep(&marks.strings, |s| {
            freed_bytes += STRING_BYTES + s.len();
            interned.remove(&s);
        });
        self.tables
            .sweep(&marks.tables, |t| freed_bytes += t.bytes());
        self.functions.sweep(&marks.functions, |f| {
            freed_bytes += FUNCTION_BYTES
                + match &f {
                    Function::Lua(c) => (c.upvalues.len() + c.constants.len()) * 16,
                    Function::Native(_) => 0,
                }
        });
        self.userdata.sweep(&marks.userdata, |u| {
            freed_bytes += USERDATA_BYTES + std::mem::size_of_val(&*u.data)
        });
        self.threads
            .sweep(&marks.threads, |t| freed_bytes += t.bytes);
        self.buffers
            .sweep(&marks.buffers, |b| freed_bytes += BUFFER_BYTES + b.len());
        self.bytes = self.bytes.saturating_sub(freed_bytes);
        self.threshold = (self.bytes / 100 * self.pause_percent).max(MIN_THRESHOLD);
        self.collections += 1;
        self.marks = marks;
    }
}

/// Something the collector has to keep alive (and everything it references).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GcRoot {
    Value(Value),
}

#[derive(Debug)]
enum Work {
    Table(TableRef),
    Function(FnRef),
    Thread(ThreadRef),
}

#[derive(Default, Debug)]
struct Marks {
    strings: Vec<bool>,
    tables: Vec<bool>,
    functions: Vec<bool>,
    userdata: Vec<bool>,
    threads: Vec<bool>,
    buffers: Vec<bool>,
    work: Vec<Work>,
}

impl Marks {
    fn push(&mut self, root: GcRoot) {
        match root {
            GcRoot::Value(v) => self.value(v),
        }
    }

    fn value(&mut self, v: Value) {
        match v {
            Value::String(s) => self.strings[s.index as usize] = true,
            Value::Table(t) => {
                if !std::mem::replace(&mut self.tables[t.index as usize], true) {
                    self.work.push(Work::Table(t));
                }
            }
            Value::Function(f) => {
                if !std::mem::replace(&mut self.functions[f.index as usize], true) {
                    self.work.push(Work::Function(f));
                }
            }
            // userdata holds no script values (a host value inside it that
            // keeps something alive does it through a pin)
            Value::UserData(u) => self.userdata[u.index as usize] = true,
            Value::Thread(t) => {
                if !std::mem::replace(&mut self.threads[t.index as usize], true) {
                    self.work.push(Work::Thread(t));
                }
            }
            // bytes only
            Value::Buffer(b) => self.buffers[b.index as usize] = true,
            Value::Nil | Value::Bool(_) | Value::Number(_) | Value::Vector(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning_dedupes() {
        let mut h = Heap::new();
        let a = h.intern("hello");
        let b = h.intern_owned(String::from("hello"));
        let c = h.intern("world");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(h.get(c), "world");
        assert_eq!(h.stats().strings, 2);
    }

    #[test]
    fn collect_frees_unreachable_and_keeps_reachable() {
        let mut h = Heap::new();
        let kept = h.intern("kept");
        let _gone = h.intern("gone");
        let t = h.alloc_table(Table::default());
        h.with_table(t, |tbl| {
            tbl.set(Value::Number(1.0), Value::String(kept)).unwrap()
        });
        let _garbage_table = h.alloc_table(Table::default());
        h.collect([GcRoot::Value(Value::Table(t))]);
        let s = h.stats();
        assert_eq!((s.strings, s.tables), (1, 1));
        assert_eq!(h.get(kept), "kept");
        // the freed string can be re-interned (new handle, same text)
        let again = h.intern("gone");
        assert_eq!(h.get(again), "gone");
    }

    #[test]
    #[should_panic(expected = "stale table handle")]
    fn stale_handles_are_caught() {
        let mut h = Heap::new();
        let t = h.alloc_table(Table::default());
        h.collect([]);
        let _reuse = h.alloc_table(Table::default());
        let _ = h.table(t);
    }

    #[test]
    fn cycles_are_collected() {
        let mut h = Heap::new();
        let a = h.alloc_table(Table::default());
        let b = h.alloc_table(Table::default());
        h.with_table(a, |t| t.set(Value::Number(1.0), Value::Table(b)).unwrap());
        h.with_table(b, |t| t.set(Value::Number(1.0), Value::Table(a)).unwrap());
        h.collect([]);
        assert_eq!(h.stats().tables, 0);
    }
}
