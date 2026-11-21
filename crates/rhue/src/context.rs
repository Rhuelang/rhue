//! [`Context`], getting at a running (or idle) engine from Rust, and
//! [`ScriptError`], the error type for everything done through it.

use std::fmt;
use std::rc::Rc;

use rhue_vm::NativeError;

use crate::convert::{ArgError, ConversionError, FromValue, FromValues, IntoValue, IntoValues};
use crate::userdata::{Ud, UserData};
use crate::{AnyUserData, Buffer, Function, Table, Thread, Value};

/// A view of an engine's VM. Call functions, read and write tables and globals,
/// make userdata.
///
/// Natives get one when they're registered with `&mut Context` as the first
/// parameter, and the host gets one from
/// [`Engine::context`](crate::Engine::context).
pub struct Context<'a> {
    pub(crate) vm: &'a mut rhue_vm::Vm,
}

impl<'a> Context<'a> {
    pub(crate) fn new(vm: &'a mut rhue_vm::Vm) -> Self {
        Context { vm }
    }

    /// Calls a function and converts its results. Inside a native, an error
    /// from the callee can be returned with `?` and it propagates unchanged
    /// (same error value, same traceback).
    pub fn call<R: FromValues>(
        &mut self,
        func: &Function,
        args: impl IntoValues,
    ) -> Result<R, ScriptError> {
        self.call_value(&Value::Function(func.clone()), args)
    }

    /// Calls anything callable, a function or a table whose metatable has
    /// `__call`.
    pub fn call_value<R: FromValues>(
        &mut self,
        func: &Value,
        args: impl IntoValues,
    ) -> Result<R, ScriptError> {
        let f = self.lower(func.clone());
        let args: Vec<rhue_vm::Value> = args
            .into_values(self)
            .into_iter()
            .map(|v| self.lower(v))
            .collect();
        let results = self
            .vm
            .try_call(f, &args)
            .map_err(|r| ScriptError(Repr::Thrown(Box::new(r))))?;
        let results = results.into_iter().map(|v| self.lift(v)).collect();
        R::from_values(results, self).map_err(ScriptError::result)
    }

    /// Fails with the uncatchable "not enough memory" error if allocating
    /// `bytes` more would go over the engine's memory limit. Natives that build
    /// something big should call this first.
    pub fn reserve(&self, bytes: usize) -> Result<(), ScriptError> {
        if self.vm.would_exceed(bytes) {
            Err(ScriptError(Repr::OutOfMemory))
        } else {
            Ok(())
        }
    }

    /// Charges `n` instructions to the script's instruction budget, for a native
    /// that does a lot of work in one call (a loop over a big argument, say).
    /// Time spent in natives isn't counted otherwise, and this is also where
    /// they notice an [`interrupt`](crate::InterruptHandle). Fails with an error
    /// scripts can't catch once the budget's gone.
    pub fn charge(&mut self, n: u64) -> Result<(), ScriptError> {
        self.vm.charge(n).map_err(|e| match e {
            NativeError::Uncatchable(m) => ScriptError(Repr::Uncatchable(m)),
            other => unreachable!("charge only stops scripts: {other:?}"),
        })
    }

    /// A new coroutine that runs `func` when it's first resumed.
    pub fn create_thread(&mut self, func: &Function) -> Thread {
        let body = self.lower(Value::Function(func.clone()));
        let t = self.vm.new_thread(body);
        let Value::Thread(t) = self.lift(t) else {
            unreachable!("new_thread makes a thread")
        };
        t
    }

    /// Resumes a coroutine, running it until it yields (giving the values it
    /// yielded) or returns (giving its results). An error inside it comes back
    /// as `Err` and leaves it dead, same as resuming a dead one.
    pub fn resume<R: FromValues>(
        &mut self,
        co: &Thread,
        args: impl IntoValues,
    ) -> Result<R, ScriptError> {
        self.check_owner(&co.pin);
        let args: Vec<rhue_vm::Value> = args
            .into_values(self)
            .into_iter()
            .map(|v| self.lower(v))
            .collect();
        let results = self
            .vm
            .resume(co.r, &args)
            .map_err(|r| ScriptError(Repr::Thrown(Box::new(r))))?;
        let results = results.into_iter().map(|v| self.lift(v)).collect();
        R::from_values(results, self).map_err(ScriptError::result)
    }

    /// The coroutine running right now, or `None` on the main thread (where
    /// nothing can yield).
    pub fn current_thread(&mut self) -> Option<Thread> {
        let t = self.vm.running_coroutine()?;
        match self.lift(rhue_vm::Value::Thread(t)) {
            Value::Thread(t) => Some(t),
            _ => unreachable!("lifting a thread gives a thread"),
        }
    }

    /// Kills a suspended coroutine, like `coroutine.close` (closing a dead one
    /// does nothing).
    pub fn close_thread(&mut self, co: &Thread) -> Result<(), ScriptError> {
        self.check_owner(&co.pin);
        self.vm.close_thread(co.r).map_err(ScriptError::new)
    }

    /// `"suspended"`, `"running"`, `"normal"` or `"dead"`.
    pub fn thread_status(&self, co: &Thread) -> &'static str {
        self.vm.thread_status(co.r).name()
    }

    /// Hangs `data` on a thread, replacing what was there, so a native can
    /// tell which script is calling (see [`thread_data`](Self::thread_data)).
    /// It goes away with the thread. Don't put the thread's own handle in it,
    /// that keeps the thread alive.
    pub fn set_thread_data<T: std::any::Any>(&mut self, co: &Thread, data: T) {
        self.check_owner(&co.pin);
        self.vm.set_thread_data(co.r, Some(Rc::new(data)));
    }

    /// What [`set_thread_data`](Self::set_thread_data) hung on a thread, if
    /// it's a `T`.
    pub fn thread_data<T: std::any::Any>(&self, co: &Thread) -> Option<Rc<T>> {
        self.vm.thread_data(co.r)?.downcast().ok()
    }

    /// The environment of the script function that called this native, if it
    /// runs in one (see [`Engine::create_environment`](crate::Engine::create_environment)).
    /// `None` on the engine's own globals, or outside a native.
    pub fn caller_environment(&mut self) -> Option<crate::Environment> {
        let table = self.vm.caller_env()?;
        crate::environment::find(self, table)
    }

    /// [`Engine::reload_module`](crate::Engine::reload_module) from inside a
    /// native or a debug hook, so a host can reload a module without waiting
    /// for the script to finish. The new module code runs nested here.
    pub fn reload_module(
        &mut self,
        name: &str,
        source: &str,
    ) -> Result<crate::Reloaded, crate::Error> {
        crate::modules::reload(self.vm, name, source)
    }

    /// Calls the global function `name`.
    pub fn call_global<R: FromValues>(
        &mut self,
        name: &str,
        args: impl IntoValues,
    ) -> Result<R, ScriptError> {
        let f: Function = self
            .global::<Option<Function>>(name)?
            .ok_or_else(|| ScriptError::new(format!("global '{name}' is not a function")))?;
        self.call(&f, args)
    }

    /// Reads a global, converted to `T`.
    pub fn global<T: FromValue>(&mut self, name: &str) -> Result<T, ScriptError> {
        let v = self.vm.global(name);
        let v = self.lift(v);
        T::from_value(v, self).map_err(|e| ScriptError::new(format!("global '{name}': {e}")))
    }

    /// Sets a global.
    pub fn set_global(&mut self, name: &str, value: impl IntoValue) {
        let v = value.into_value(self);
        let v = self.lower(v);
        self.vm.set_global(name, v);
    }

    /// Wraps a Rust function as a script function value (without making it a
    /// global), like for handing a callback back to a script.
    pub fn create_function<M>(&mut self, name: &str, f: impl crate::IntoNative<M>) -> Function {
        let native = f.into_native(Rc::from(name));
        let v = self.vm.native(name, native);
        let Value::Function(f) = self.lift(v) else {
            unreachable!("native returns a function")
        };
        f
    }

    /// A buffer holding a copy of `bytes`, like `buffer.fromstring` would make
    /// (but any bytes).
    pub fn create_buffer(&mut self, bytes: &[u8]) -> Buffer {
        let r = self.vm.heap_mut().alloc_buffer(bytes.into());
        let Value::Buffer(b) = self.lift(rhue_vm::Value::Buffer(r)) else {
            unreachable!("lifting a buffer gives a buffer")
        };
        b
    }

    /// A copy of a buffer's bytes.
    pub fn buffer_bytes(&self, b: &Buffer) -> Vec<u8> {
        self.check_owner(&b.pin);
        self.vm.heap().buffer(b.r).to_vec()
    }

    /// Copies `bytes` into a buffer at `offset`, an error if they don't fit.
    pub fn write_buffer(
        &mut self,
        b: &Buffer,
        offset: usize,
        bytes: &[u8],
    ) -> Result<(), ScriptError> {
        self.check_owner(&b.pin);
        let buf = self.vm.heap_mut().buffer_mut(b.r);
        let end = offset
            .checked_add(bytes.len())
            .filter(|&e| e <= buf.len())
            .ok_or_else(|| ScriptError::new("buffer access out of bounds"))?;
        buf[offset..end].copy_from_slice(bytes);
        Ok(())
    }

    /// A new, empty table.
    pub fn new_table(&mut self) -> Table {
        let t = self.vm.heap_mut().alloc_table(rhue_vm::Table::default());
        self.table_handle(t)
    }

    pub(crate) fn table_from_list(&mut self, values: Vec<Value>) -> Table {
        let mut table = rhue_vm::Table::with_capacity(values.len(), 0);
        for (i, v) in values.into_iter().enumerate() {
            let v = self.lower(v);
            let _ = table.set(rhue_vm::Value::Number((i + 1) as f64), v);
        }
        let t = self.vm.heap_mut().alloc_table(table);
        self.table_handle(t)
    }

    /// `t[key]`, converted.
    pub fn get<T: FromValue>(&mut self, t: &Table, key: impl IntoValue) -> Result<T, ScriptError> {
        let key = key.into_value(self);
        let shown = key.to_string();
        let v = self.raw_get(t, key);
        T::from_value(v, self).map_err(|e| ScriptError::new(format!("field '{shown}': {e}")))
    }

    pub(crate) fn raw_get(&mut self, t: &Table, key: Value) -> Value {
        let key = self.lower(key);
        let r = self.table_ref(t);
        let v = self.vm.heap().table(r).get(key);
        self.lift(v)
    }

    /// `t[key] = value`. Fails for a nil or NaN key.
    pub fn set(
        &mut self,
        t: &Table,
        key: impl IntoValue,
        value: impl IntoValue,
    ) -> Result<(), ScriptError> {
        let (key, value) = (key.into_value(self), value.into_value(self));
        let (key, value) = (self.lower(key), self.lower(value));
        let r = self.table_ref(t);
        self.vm
            .heap_mut()
            .with_table(r, |tbl| tbl.set(key, value))
            .map_err(|e| ScriptError::new(e.message()))
    }

    /// `#t`.
    pub fn len(&self, t: &Table) -> usize {
        let r = self.table_ref(t);
        self.vm.heap().table(r).len()
    }

    /// Every global variable, by name.
    pub fn globals_table(&mut self) -> Vec<(String, Value)> {
        let raw: Vec<_> = self.vm.globals().collect();
        raw.into_iter()
            .map(|(k, v)| (self.vm.heap().get(k).to_string(), self.lift(v)))
            .collect()
    }

    /// Every key and value of a table (raw, in iteration order).
    pub fn pairs(&mut self, t: &Table) -> Vec<(Value, Value)> {
        let r = self.table_ref(t);
        let mut out = Vec::new();
        let mut key = rhue_vm::Value::Nil;
        while let Ok(Some((k, v))) = self.vm.heap().table(r).next(key) {
            out.push((k, v));
            key = k;
        }
        out.into_iter()
            .map(|(k, v)| (self.lift(k), self.lift(v)))
            .collect()
    }

    /// Moves `value` into the VM as userdata of type `T`.
    pub fn create_userdata<T: UserData>(&mut self, value: T) -> Ud<T> {
        let ty = crate::userdata::type_id_of::<T>(self.vm);
        let cell = Rc::new(std::cell::RefCell::new(value));
        let v = self.vm.new_userdata(ty, cell.clone());
        let Value::UserData(any) = self.lift(v) else {
            unreachable!("new_userdata returns userdata")
        };
        Ud { any, cell }
    }

    // ─ Conversion to and from the VM's Values ────────────────────────────────

    fn check_owner(&self, pin: &rhue_vm::Pin) {
        assert_eq!(
            pin.vm_id(),
            self.vm.id(),
            "a function, table or userdata from one Engine was passed to another"
        );
    }

    pub(crate) fn table_ref_of(&self, t: &Table) -> rhue_vm::TableRef {
        self.table_ref(t)
    }

    fn table_ref(&self, t: &Table) -> rhue_vm::TableRef {
        self.check_owner(&t.pin);
        t.r
    }

    fn table_handle(&self, r: rhue_vm::TableRef) -> Table {
        Table {
            r,
            pin: Rc::new(
                self.vm
                    .pin(rhue_vm::Value::Table(r))
                    .expect("tables can be pinned"),
            ),
        }
    }

    /// Host value → VM value (interning strings, unwrapping handles).
    #[inline]
    pub(crate) fn lower(&mut self, v: Value) -> rhue_vm::Value {
        match v {
            Value::Nil => rhue_vm::Value::Nil,
            Value::Bool(b) => rhue_vm::Value::Bool(b),
            Value::Number(n) => rhue_vm::Value::Number(n),
            Value::String(s) => self.vm.string(&s),
            Value::Function(f) => {
                self.check_owner(&f.pin);
                rhue_vm::Value::Function(f.r)
            }
            Value::Table(t) => rhue_vm::Value::Table(self.table_ref(&t)),
            Value::UserData(u) => {
                self.check_owner(&u.pin);
                rhue_vm::Value::UserData(u.r)
            }
            Value::Thread(t) => {
                self.check_owner(&t.pin);
                rhue_vm::Value::Thread(t.r)
            }
            Value::Buffer(b) => {
                self.check_owner(&b.pin);
                rhue_vm::Value::Buffer(b.r)
            }
            Value::Vector(v) => rhue_vm::Value::Vector(v),
        }
    }

    /// VM value → host value (copying strings, pinning heap objects).
    #[inline]
    pub(crate) fn lift(&self, v: rhue_vm::Value) -> Value {
        let pin = || Rc::new(self.vm.pin(v).expect("heap objects can be pinned"));
        match v {
            rhue_vm::Value::Nil => Value::Nil,
            rhue_vm::Value::Bool(b) => Value::Bool(b),
            rhue_vm::Value::Number(n) => Value::Number(n),
            rhue_vm::Value::String(s) => Value::String(self.vm.heap().get_rc(s)),
            rhue_vm::Value::Function(r) => Value::Function(Function { r, pin: pin() }),
            rhue_vm::Value::Table(r) => Value::Table(Table { r, pin: pin() }),
            rhue_vm::Value::UserData(r) => {
                let heap = self.vm.heap();
                Value::UserData(AnyUserData {
                    r,
                    data: heap.userdata(r).data.clone(),
                    type_name: Rc::from(heap.userdata_type_name(r)),
                    pin: pin(),
                })
            }
            rhue_vm::Value::Thread(r) => Value::Thread(Thread { r, pin: pin() }),
            rhue_vm::Value::Buffer(r) => Value::Buffer(Buffer { r, pin: pin() }),
            rhue_vm::Value::Vector(v) => Value::Vector(v),
        }
    }
}

/// An error raised by a script, a native, or a conversion.
///
/// Natives return it (or anything that converts into it, like `String`) to
/// raise an error in the script that called them. An error that came from a
/// script through [`Context::call`] keeps its original value and traceback when
/// it's passed on.
pub struct ScriptError(pub(crate) Repr);

pub(crate) enum Repr {
    Thrown(Box<rhue_vm::Rethrown>),
    Message(String),
    Value(Value),
    OutOfMemory,
    Uncatchable(String),
    Yield(Vec<Value>),
}

impl ScriptError {
    /// An error with this message (a script's `pcall` sees it with the call's
    /// position in front, like errors from the standard library).
    pub fn new(message: impl Into<String>) -> Self {
        ScriptError(Repr::Message(message.into()))
    }

    /// Raises any value, like `error(value, 0)`. A table, say, that a script's
    /// `pcall` gets as-is.
    pub fn raise(value: Value) -> Self {
        ScriptError(Repr::Value(value))
    }

    /// Not an error. Suspends the script's coroutine and hands `values` to
    /// whoever resumed it, like `coroutine.yield(...)` would, and when the
    /// coroutine gets resumed the values passed become the native's results.
    /// Return it (`Err(ScriptError::yield_with(...))`) from a native a script
    /// calls directly inside a coroutine, like one started with
    /// [`Engine::spawn`](crate::Engine::spawn). Anywhere else the script gets
    /// the error yielding there would give.
    pub fn yield_with(values: Vec<Value>) -> Self {
        ScriptError(Repr::Yield(values))
    }

    fn result(e: ArgError) -> Self {
        ScriptError::new(format!("invalid result #{} ({})", e.index, e.error))
    }

    pub(crate) fn argument(function: &str, e: ArgError) -> Self {
        ScriptError::new(format!(
            "invalid argument #{} to '{function}' ({})",
            e.index, e.error
        ))
    }

    /// The error's message (for a raised non-string value, its `tostring`).
    pub fn message(&self) -> String {
        match &self.0 {
            Repr::Thrown(r) => r.error().message.clone(),
            Repr::Message(m) => m.clone(),
            Repr::Value(v) => v.to_string(),
            Repr::OutOfMemory => rhue_vm::OUT_OF_MEMORY.to_string(),
            Repr::Uncatchable(m) => m.clone(),
            Repr::Yield(_) => "attempt to yield across a C-call boundary".to_string(),
        }
    }

    /// `false` for errors scripts can't catch (the instruction limit).
    pub fn is_catchable(&self) -> bool {
        match &self.0 {
            Repr::Thrown(r) => r.catchable(),
            Repr::OutOfMemory | Repr::Uncatchable(_) => false,
            _ => true,
        }
    }

    pub(crate) fn into_native(self, cx: &mut Context<'_>) -> NativeError {
        match self.0 {
            Repr::Thrown(r) => NativeError::Rethrow(r),
            Repr::Message(m) => NativeError::Message(m),
            Repr::Value(v) => NativeError::Raise {
                value: cx.lower(v),
                level: 0,
            },
            Repr::OutOfMemory => NativeError::OutOfMemory,
            Repr::Uncatchable(m) => NativeError::Uncatchable(m),
            Repr::Yield(values) => {
                NativeError::Yield(values.into_iter().map(|v| cx.lower(v)).collect())
            }
        }
    }
}

impl fmt::Display for ScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

impl fmt::Debug for ScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ScriptError({:?})", self.message())
    }
}

impl std::error::Error for ScriptError {}

impl From<String> for ScriptError {
    fn from(m: String) -> Self {
        ScriptError::new(m)
    }
}

impl From<&str> for ScriptError {
    fn from(m: &str) -> Self {
        ScriptError::new(m)
    }
}

impl From<ConversionError> for ScriptError {
    fn from(e: ConversionError) -> Self {
        ScriptError::new(e.to_string())
    }
}
