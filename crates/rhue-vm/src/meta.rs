//! Metatables: event names, metamethod lookup, and the slow paths of the
//! instructions they affect (indexing, assignment, arithmetic, comparison,
//! concatenation, length, calls).
//!
//! The dispatch loop handles the common cases inline and only comes here when a
//! value isn't something the instruction handles directly, or when a table
//! lookup misses on a table with a metatable.
//!
//! A Lua metamethod runs in the same dispatch loop as the instruction that
//! needs it, as a normal call frame right above the caller's registers
//! ([`Vm::meta_call`]). When it returns its result goes straight to the
//! instruction's destination register, or for `__eq`, `__lt` and `__le`,
//! decides the jump after the comparison. So metamethods cost about as much as
//! a call, nest as deep as calls, and can yield. Only `__iter`, `__tostring`
//! (and metamethods reached from natives) still run nested.

use smallvec::SmallVec;

use crate::heap::{StrRef, TableRef};
use crate::{Frame, MAX_NATIVE_DEPTH, Rets, Thrown, Value, Vm, arith_values, compare};

/// Where a metamethod's result goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MetaDest {
    /// This absolute stack slot (the instruction's `R(A)`).
    Reg(usize),
    /// Its truth decides the comparison's jump. `true` is the instruction's
    /// expected result (`A`).
    Cond(bool),
    /// Nowhere (`__newindex`).
    Discard,
}
use rhue_bytecode::{ArithOp, Op};

/// How many `__index` / `__newindex` tables a lookup follows before giving up
/// (a cycle would loop forever otherwise).
const MAX_CHAIN: usize = 100;

/// The metamethods Rhue knows, in the order of [`EVENT_NAMES`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    Index,
    NewIndex,
    Call,
    Add,
    Sub,
    Mul,
    Div,
    IDiv,
    Mod,
    Pow,
    Unm,
    Concat,
    Len,
    Eq,
    Lt,
    Le,
    ToString,
    Metatable,
    Iter,
}

pub(crate) const EVENT_NAMES: [&str; 19] = [
    "__index",
    "__newindex",
    "__call",
    "__add",
    "__sub",
    "__mul",
    "__div",
    "__idiv",
    "__mod",
    "__pow",
    "__unm",
    "__concat",
    "__len",
    "__eq",
    "__lt",
    "__le",
    "__tostring",
    "__metatable",
    "__iter",
];

impl Event {
    fn of_arith(op: ArithOp) -> Event {
        match op {
            ArithOp::Add => Event::Add,
            ArithOp::Sub => Event::Sub,
            ArithOp::Mul => Event::Mul,
            ArithOp::Div => Event::Div,
            ArithOp::IDiv => Event::IDiv,
            ArithOp::Mod => Event::Mod,
            ArithOp::Pow => Event::Pow,
        }
    }
}

/// The interned event names, rooted for the VM's lifetime.
pub(crate) struct Events([StrRef; EVENT_NAMES.len()]);

impl Events {
    pub(crate) fn intern(heap: &mut crate::Heap) -> Self {
        Events(EVENT_NAMES.map(|name| heap.intern(name)))
    }

    pub(crate) fn get(&self, e: Event) -> StrRef {
        self.0[e as usize]
    }

    pub(crate) fn all(&self) -> impl Iterator<Item = StrRef> + '_ {
        self.0.iter().copied()
    }
}

/// `t[key]` for a string key when `t`'s metatable has an `__index` *table* with
/// no metatable of its own (the usual class). The one lookup the dispatch loop
/// does inline. `None` means take the slow path. It's a free function so it
/// only borrows the heap and the event names.
#[inline]
pub(crate) fn class_lookup(
    heap: &crate::Heap,
    events: &Events,
    meta: TableRef,
    key: StrRef,
) -> Option<Value> {
    match heap.table(meta).get_str(events.get(Event::Index)) {
        Value::Table(class) => {
            let class = heap.table(class);
            match class.get_str(key) {
                Value::Nil if class.meta().is_some() => None,
                v => Some(v),
            }
        }
        Value::Nil => Some(Value::Nil),
        _ => None,
    }
}

impl Vm {
    /// The metatable of `v`, a table's own or the shared one for strings.
    pub(crate) fn metatable_of(&self, v: Value) -> Option<TableRef> {
        match v {
            Value::Table(t) => self.heap.table(t).meta(),
            Value::String(_) => self.string_meta,
            _ => None,
        }
    }

    /// `v`'s metamethod for `e`, or nil.
    pub(crate) fn metamethod(&self, v: Value, e: Event) -> Value {
        match self.metatable_of(v) {
            Some(m) => self.heap.table(m).get_str(self.events.get(e)),
            None => Value::Nil,
        }
    }

    /// Calls a metamethod nested, to the end (for `__iter` and `__tostring`).
    /// The frame gets parked so the collector and tracebacks see it, and the
    /// call nests like a native's (so it can't yield).
    pub(crate) fn call_meta(
        &mut self,
        f: &mut Frame,
        pc: usize,
        func: Value,
        args: &[Value],
    ) -> Result<Rets, Thrown> {
        if self.native_depth >= MAX_NATIVE_DEPTH {
            return Err(self.throw(f, pc, "metamethods nested too deeply".into()));
        }
        let placeholder = self.placeholder_frame(f.closure);
        self.frames.push(std::mem::replace(f, placeholder));
        self.native_depth += 1;
        let result = self.call_top_thrown(func, args, false);
        self.native_depth -= 1;
        *f = self.frames.pop().expect("the metamethod's caller");
        result
    }

    fn call_meta1(
        &mut self,
        f: &mut Frame,
        pc: usize,
        func: Value,
        args: &[Value],
    ) -> Result<Value, Thrown> {
        Ok(self
            .call_meta(f, pc, func, args)?
            .first()
            .copied()
            .unwrap_or(Value::Nil))
    }

    /// Calls metamethod `h` for the instruction at `pc` in this dispatch loop.
    /// A Lua function gets a frame right above `f`'s registers (and becomes
    /// `f`, the caller waits in `frames`), which delivers its first result to
    /// `dest` when it returns. Anything else (a native, a callable table) runs
    /// now and its result gets delivered right away.
    pub(crate) fn meta_call(
        &mut self,
        f: &mut Frame,
        pc: usize,
        h: Value,
        args: &[Value],
        dest: MetaDest,
    ) -> Result<(), Thrown> {
        if self.budget == 0 {
            self.charge_slow(f, pc, 1)?;
        } else {
            self.budget -= 1;
        }
        let slot = f.base + f.proto().max_stack as usize;
        let n = args.len();
        if !self.ensure_stack(slot + 1 + n) {
            return Err(self.throw(f, pc, "stack overflow".into()));
        }
        self.stack[slot] = h;
        self.stack[slot + 1..slot + 1 + n].copy_from_slice(args);
        let frames = self.frames.len();
        let want = if dest == MetaDest::Discard { 0 } else { 1 };
        let mut top = 0;
        self.call(f, pc, slot, n, Some(want), &mut top, false)?;
        if self.frames.len() > frames {
            // `f` is the metamethod's frame now
            match dest {
                MetaDest::Reg(r) => f.ret_to = r as u32,
                MetaDest::Cond(expect) => f.cond = Some(expect),
                MetaDest::Discard => {}
            }
            return Ok(());
        }
        let v = self.stack[slot];
        match dest {
            MetaDest::Reg(r) => self.stack[r] = v,
            MetaDest::Cond(expect) => self.finish_cond(f, v.is_truthy(), expect)?,
            MetaDest::Discard => {}
        }
        Ok(())
    }

    /// `obj[key]` with `__index`, for `GETTABLE`, `GETFIELD` and `SELF`. The
    /// value goes to `dest` (now, or when an `__index` function returns).
    #[inline(never)]
    pub(crate) fn index_meta(
        &mut self,
        f: &mut Frame,
        pc: usize,
        obj: Value,
        key: Value,
        dest: usize,
    ) -> Result<(), Thrown> {
        let found = |vm: &mut Vm, v: Value| {
            vm.stack[dest] = v;
            Ok(())
        };
        let mut obj = obj;
        for _ in 0..MAX_CHAIN {
            let handler = match obj {
                Value::Table(t) => {
                    let table = self.heap.table(t);
                    let v = table.get(key);
                    if v != Value::Nil {
                        return found(self, v);
                    }
                    match table.meta() {
                        Some(m) => self.heap.table(m).get_str(self.events.get(Event::Index)),
                        None => return found(self, Value::Nil),
                    }
                }
                Value::UserData(u) => {
                    let v = self.ud_member(f, pc, u, key)?;
                    return found(self, v);
                }
                Value::Vector(c) => match crate::vector_component(c, key, &self.heap) {
                    Some(n) => return found(self, Value::Number(n)),
                    None => {
                        let m = format!("attempt to index vector with {}", self.describe_key(key));
                        return Err(self.throw(f, pc, m));
                    }
                },
                Value::String(_) => {
                    let v = self.index(obj, key).map_err(|m| self.throw(f, pc, m))?;
                    return found(self, v);
                }
                other => {
                    let m = format!(
                        "attempt to index {} with {}",
                        other.type_name(),
                        self.describe_key(key)
                    );
                    return Err(self.throw(f, pc, m));
                }
            };
            match handler {
                Value::Nil => return found(self, Value::Nil),
                Value::Function(_) => {
                    return self.meta_call(f, pc, handler, &[obj, key], MetaDest::Reg(dest));
                }
                next => obj = next,
            }
        }
        Err(self.throw(f, pc, "'__index' chain too long; possible loop".into()))
    }

    /// `obj[key] = value` with `__newindex`.
    #[inline(never)]
    pub(crate) fn newindex_meta(
        &mut self,
        f: &mut Frame,
        pc: usize,
        obj: Value,
        key: Value,
        value: Value,
    ) -> Result<(), Thrown> {
        let mut obj = obj;
        for _ in 0..MAX_CHAIN {
            let handler = match obj {
                Value::Table(t) => {
                    let table = self.heap.table(t);
                    match table.meta() {
                        Some(m) if table.get(key) == Value::Nil => {
                            self.heap.table(m).get_str(self.events.get(Event::NewIndex))
                        }
                        _ => Value::Nil,
                    }
                }
                Value::UserData(u) => return self.ud_assign(f, pc, u, key, value),
                _ => Value::Nil,
            };
            match handler {
                Value::Nil => {
                    return self
                        .set_index(obj, key, value)
                        .map_err(|m| self.throw(f, pc, m));
                }
                Value::Function(_) => {
                    return self.meta_call(f, pc, handler, &[obj, key, value], MetaDest::Discard);
                }
                next => obj = next,
            }
        }
        Err(self.throw(f, pc, "'__newindex' chain too long; possible loop".into()))
    }

    /// The first of `a`'s and `b`'s metamethods for `e`.
    fn binary_metamethod(&self, a: Value, b: Value, e: Event) -> Value {
        match self.metamethod(a, e) {
            Value::Nil => self.metamethod(b, e),
            h => h,
        }
    }

    /// Arithmetic that isn't on two numbers: `__add` and friends, or the error.
    #[inline(never)]
    pub(crate) fn arith_meta(
        &mut self,
        f: &mut Frame,
        pc: usize,
        op: ArithOp,
        (l, r): (Value, Value),
        dest: usize,
    ) -> Result<(), Thrown> {
        match self.binary_metamethod(l, r, Event::of_arith(op)) {
            Value::Nil => match arith_values(op, l, r) {
                Ok(v) => {
                    self.stack[dest] = v;
                    Ok(())
                }
                Err(m) => Err(self.throw(f, pc, m)),
            },
            h => self.meta_call(f, pc, h, &[l, r], MetaDest::Reg(dest)),
        }
    }

    #[inline(never)]
    pub(crate) fn unm_meta(
        &mut self,
        f: &mut Frame,
        pc: usize,
        v: Value,
        dest: usize,
    ) -> Result<(), Thrown> {
        if let Value::Vector(c) = v {
            self.stack[dest] = Value::Vector(c.map(|x| -x));
            return Ok(());
        }
        match self.metamethod(v, Event::Unm) {
            Value::Nil => {
                let m = format!("attempt to perform arithmetic (unm) on {}", v.type_name());
                Err(self.throw(f, pc, m))
            }
            h => self.meta_call(f, pc, h, &[v, v], MetaDest::Reg(dest)),
        }
    }

    #[inline(never)]
    pub(crate) fn concat_meta(
        &mut self,
        f: &mut Frame,
        pc: usize,
        (l, r): (Value, Value),
        dest: usize,
    ) -> Result<(), Thrown> {
        match self.binary_metamethod(l, r, Event::Concat) {
            Value::Nil => {
                let m = format!(
                    "attempt to concatenate {} with {}",
                    l.type_name(),
                    r.type_name()
                );
                Err(self.throw(f, pc, m))
            }
            h => self.meta_call(f, pc, h, &[l, r], MetaDest::Reg(dest)),
        }
    }

    #[inline(never)]
    pub(crate) fn len_meta(
        &mut self,
        f: &mut Frame,
        pc: usize,
        v: Value,
        dest: usize,
    ) -> Result<(), Thrown> {
        match (self.metamethod(v, Event::Len), v) {
            (Value::Nil, Value::Table(t)) => {
                self.stack[dest] = Value::Number(self.heap.table(t).len() as f64);
                Ok(())
            }
            (Value::Nil, _) => {
                let m = format!("attempt to get length of a {} value", v.type_name());
                Err(self.throw(f, pc, m))
            }
            (h, _) => self.meta_call(f, pc, h, &[v], MetaDest::Reg(dest)),
        }
    }

    /// `l == r` for two values that aren't raw-equal (`expect` is the
    /// instruction's `A`). `__eq` only applies when both are tables.
    #[inline(never)]
    pub(crate) fn eq_meta(
        &mut self,
        f: &mut Frame,
        pc: usize,
        (l, r): (Value, Value),
        expect: bool,
    ) -> Result<(), Thrown> {
        let h = if matches!((l, r), (Value::Table(_), Value::Table(_))) {
            self.binary_metamethod(l, r, Event::Eq)
        } else {
            Value::Nil
        };
        match h {
            Value::Nil => self.finish_cond(f, false, expect),
            h => self.meta_call(f, pc, h, &[l, r], MetaDest::Cond(expect)),
        }
    }

    /// `l < r` or `l <= r` beyond numbers and strings: `__lt` / `__le`.
    #[inline(never)]
    pub(crate) fn compare_meta(
        &mut self,
        f: &mut Frame,
        pc: usize,
        op: Op,
        (l, r): (Value, Value),
        expect: bool,
    ) -> Result<(), Thrown> {
        let event = if op == Op::Lt { Event::Lt } else { Event::Le };
        match self.binary_metamethod(l, r, event) {
            Value::Nil => match compare(op, l, r, &self.heap) {
                Ok(b) => self.finish_cond(f, b, expect),
                Err(m) => Err(self.throw(f, pc, m)),
            },
            h => self.meta_call(f, pc, h, &[l, r], MetaDest::Cond(expect)),
        }
    }

    /// Makes the value at `slot` callable. While it isn't a function its
    /// `__call` takes its place and it becomes the first argument. Returns the
    /// new argument count.
    pub(crate) fn resolve_call(&mut self, slot: usize, nargs: usize) -> Result<usize, String> {
        for nargs in (nargs..).take(MAX_CHAIN) {
            let callee = self.stack[slot];
            if let Value::Function(_) = callee {
                return Ok(nargs);
            }
            let handler = self.metamethod(callee, Event::Call);
            if handler == Value::Nil {
                return Err(format!("attempt to call a {} value", callee.type_name()));
            }
            if !self.ensure_stack(slot + nargs + 2) {
                return Err("stack overflow".into());
            }
            self.stack.copy_within(slot..slot + 1 + nargs, slot + 1);
            self.stack[slot] = handler;
        }
        Err("'__call' chain too long; possible loop".into())
    }

    /// For `for k, v in obj`: if `obj` has `__iter`, its results (iterator,
    /// state, control) replace the three loop slots at `a`.
    pub(crate) fn iter_meta(&mut self, f: &mut Frame, pc: usize, a: usize) -> Result<bool, Thrown> {
        let obj = self.stack[a];
        let handler = self.metamethod(obj, Event::Iter);
        if handler == Value::Nil {
            return Ok(false);
        }
        let results = self.call_meta(f, pc, handler, &[obj])?;
        for i in 0..3 {
            self.stack[a + i] = results.get(i).copied().unwrap_or(Value::Nil);
        }
        if !matches!(self.stack[a], Value::Function(_)) {
            let m = format!(
                "'__iter' must return a function, got {}",
                self.stack[a].type_name()
            );
            return Err(self.throw(f, pc, m));
        }
        Ok(true)
    }

    /// `INTERP` when a piece might have `__tostring`.
    pub(crate) fn interp_meta(
        &mut self,
        f: &mut Frame,
        pc: usize,
        from: usize,
        count: usize,
    ) -> Result<Value, Thrown> {
        let mut out = String::new();
        for i in 0..count {
            let v = self.stack[from + i];
            let handler = match v {
                Value::Table(_) => self.metamethod(v, Event::ToString),
                _ => Value::Nil,
            };
            if handler == Value::Nil {
                out.push_str(&self.display(v));
                continue;
            }
            match self.call_meta1(f, pc, handler, &[v])? {
                Value::String(s) => out.push_str(self.heap.get(s)),
                _ => {
                    return Err(self.throw(f, pc, "'__tostring' must return a string".into()));
                }
            }
        }
        Ok(Value::String(self.heap.intern_owned(out)))
    }

    /// `tostring(v)` with `__tostring`, for natives (`tostring`, `print`).
    pub(crate) fn tostring_meta(&mut self, v: Value) -> Result<Option<Value>, crate::NativeError> {
        let handler = match v {
            Value::Table(_) => self.metamethod(v, Event::ToString),
            _ => Value::Nil,
        };
        if handler == Value::Nil {
            return Ok(None);
        }
        let results: SmallVec<[Value; 3]> = self
            .call_top_thrown(handler, &[v], false)
            .map_err(|t| crate::NativeError::Rethrow(Box::new(crate::Rethrown(t))))?;
        match results.first() {
            Some(s @ Value::String(_)) => Ok(Some(*s)),
            _ => Err(crate::NativeError::Message(
                "'__tostring' must return a string".into(),
            )),
        }
    }
}
