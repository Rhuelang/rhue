//! The host-side [`Value`] and handles to heap objects.

use std::any::Any;
use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use crate::userdata::{Ud, UserData};

/// A Rhue value, owned and independent of the VM's heap.
///
/// Strings get copied out of the VM. Functions, tables and userdata are
/// handles, and while any clone of one is alive the garbage collector keeps the
/// object alive too. Handles only mean something to the [`Engine`] that made
/// them, passing one to another engine panics.
///
/// [`Engine`]: crate::Engine
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// `nil`.
    Nil,
    /// `true` or `false`.
    Bool(bool),
    /// A number (always `f64`, like Luau).
    Number(f64),
    /// A string (UTF-8).
    String(Rc<str>),
    /// A function, from a script or registered from Rust.
    Function(Function),
    /// A table.
    Table(Table),
    /// A Rust value exposed to scripts (see [`UserData`](crate::UserData)).
    UserData(AnyUserData),
    /// A coroutine.
    Thread(Thread),
    /// A buffer (a fixed-size byte array).
    Buffer(Buffer),
    /// A `vector`: three 32-bit floats.
    Vector([f32; 3]),
}

/// A handle to a buffer (`buffer.create`) inside an engine, a fixed-size
/// mutable array of bytes. Read and write it through a
/// [`Context`](crate::Context).
#[derive(Clone, Debug)]
pub struct Buffer {
    pub(crate) r: rhue_vm::BufRef,
    pub(crate) pin: Rc<rhue_vm::Pin>,
}

impl PartialEq for Buffer {
    fn eq(&self, other: &Self) -> bool {
        self.r == other.r && self.pin.vm_id() == other.pin.vm_id()
    }
}

/// A handle to a function (script or native) inside an engine. Call it with
/// [`Engine::call`](crate::Engine::call) or [`Context::call`](crate::Context::call).
#[derive(Clone, Debug)]
pub struct Function {
    pub(crate) r: rhue_vm::FnRef,
    pub(crate) pin: Rc<rhue_vm::Pin>,
}

impl PartialEq for Function {
    fn eq(&self, other: &Self) -> bool {
        self.r == other.r && self.pin.vm_id() == other.pin.vm_id()
    }
}

/// A handle to a table inside an engine. Read and write it through a
/// [`Context`](crate::Context) (`engine.context()`, or the one a native receives).
#[derive(Clone, Debug)]
pub struct Table {
    pub(crate) r: rhue_vm::TableRef,
    pub(crate) pin: Rc<rhue_vm::Pin>,
}

impl PartialEq for Table {
    fn eq(&self, other: &Self) -> bool {
        self.r == other.r && self.pin.vm_id() == other.pin.vm_id()
    }
}

/// A handle to a coroutine inside an engine. Make one with
/// [`Context::create_thread`](crate::Context::create_thread) and run it with
/// [`Context::resume`](crate::Context::resume), or get one from a script.
#[derive(Clone, Debug)]
pub struct Thread {
    pub(crate) r: rhue_vm::ThreadRef,
    pub(crate) pin: Rc<rhue_vm::Pin>,
}

impl PartialEq for Thread {
    fn eq(&self, other: &Self) -> bool {
        self.r == other.r && self.pin.vm_id() == other.pin.vm_id()
    }
}

/// A handle to a userdata object of any type. [`AnyUserData::downcast`]
/// turns it into a typed [`Ud<T>`].
#[derive(Clone)]
pub struct AnyUserData {
    pub(crate) r: rhue_vm::UdRef,
    pub(crate) data: Rc<dyn Any>,
    pub(crate) type_name: Rc<str>,
    pub(crate) pin: Rc<rhue_vm::Pin>,
}

impl AnyUserData {
    /// The name scripts see (`typeof(x)`), e.g. `"Player"`.
    pub fn type_name(&self) -> &str {
        &self.type_name
    }

    /// Whether this is userdata holding a `T`.
    pub fn is<T: UserData>(&self) -> bool {
        self.data.is::<RefCell<T>>()
    }

    /// The object as a `T`, if that's what it holds.
    pub fn downcast<T: UserData>(&self) -> Option<Ud<T>> {
        let cell = self.data.clone().downcast::<RefCell<T>>().ok()?;
        Some(Ud {
            any: self.clone(),
            cell,
        })
    }
}

impl PartialEq for AnyUserData {
    fn eq(&self, other: &Self) -> bool {
        self.r == other.r && self.pin.vm_id() == other.pin.vm_id()
    }
}

impl fmt::Debug for AnyUserData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AnyUserData({}: 0x{:08x})", self.type_name, self.r.id())
    }
}

impl Value {
    /// The name `type()` returns. Userdata report `"userdata"`, see
    /// [`Value::type_of`] for the name `typeof()` gives.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Nil => "nil",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Function(_) => "function",
            Value::Table(_) => "table",
            Value::UserData(_) => "userdata",
            Value::Thread(_) => "thread",
            Value::Buffer(_) => "buffer",
            Value::Vector(_) => "vector",
        }
    }

    /// Like Luau's `typeof`, userdata report their own type name.
    pub fn type_of(&self) -> &str {
        match self {
            Value::UserData(u) => u.type_name(),
            other => other.type_name(),
        }
    }

    /// The number, if this is one.
    pub fn as_number(&self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// The string, if this is one.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// Whether this is `nil`.
    pub fn is_nil(&self) -> bool {
        matches!(self, Value::Nil)
    }

    /// Whether a condition would treat this as true (anything but `nil` and `false`).
    pub fn is_truthy(&self) -> bool {
        !matches!(self, Value::Nil | Value::Bool(false))
    }
}

/// Formats like Luau's `tostring`.
impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Nil => f.write_str("nil"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Number(n) => f.write_str(&rhue_bytecode::format_number(*n)),
            Value::String(s) => f.write_str(s),
            Value::Function(func) => write!(f, "function: 0x{:08x}", func.r.id()),
            Value::Table(t) => write!(f, "table: 0x{:08x}", t.r.id()),
            Value::UserData(u) => write!(f, "{}: 0x{:08x}", u.type_name, u.r.id()),
            Value::Thread(t) => write!(f, "thread: 0x{:08x}", t.r.id()),
            Value::Buffer(b) => write!(f, "buffer: 0x{:08x}", b.r.id()),
            Value::Vector(v) => f.write_str(&rhue_vm::vector_text(*v)),
        }
    }
}

impl From<f64> for Value {
    fn from(n: f64) -> Self {
        Value::Number(n)
    }
}

impl From<i32> for Value {
    fn from(n: i32) -> Self {
        Value::Number(n as f64)
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Bool(b)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::String(Rc::from(s))
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::String(Rc::from(s))
    }
}

impl From<()> for Value {
    fn from(_: ()) -> Self {
        Value::Nil
    }
}

impl From<Function> for Value {
    fn from(f: Function) -> Self {
        Value::Function(f)
    }
}

impl From<Table> for Value {
    fn from(t: Table) -> Self {
        Value::Table(t)
    }
}
