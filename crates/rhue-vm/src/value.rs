use crate::heap::{BufRef, FnRef, Heap, StrRef, TableRef, ThreadRef, UdRef};
use rhue_bytecode::format_number;

/// A Rhue value, as the VM sees it.
///
/// `Copy` and 16 bytes. Heap objects are referenced by handle (see
/// [`crate::heap`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value {
    Nil,
    Bool(bool),
    Number(f64),
    String(StrRef),
    Function(FnRef),
    Table(TableRef),
    UserData(UdRef),
    Thread(ThreadRef),
    Buffer(BufRef),
    /// Luau's `vector`: three 32-bit floats, stored inline (no heap).
    Vector([f32; 3]),
}

impl Value {
    /// The name `type()` will return, also used in error messages.
    pub fn type_name(self) -> &'static str {
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

    /// Lua truthiness: only `nil` and `false` are falsy.
    #[inline]
    pub fn is_truthy(self) -> bool {
        !matches!(self, Value::Nil | Value::Bool(false))
    }

    pub fn as_number(self) -> Option<f64> {
        match self {
            Value::Number(n) => Some(n),
            _ => None,
        }
    }

    /// Raw equality (`==` without metamethods). Strings are interned, so
    /// comparing handles compares contents. NaN is not equal to itself.
    #[inline]
    pub fn raw_eq(self, other: Value) -> bool {
        self == other
    }

    /// Formats like Luau's `tostring`.
    pub fn display(self, heap: &Heap) -> String {
        match self {
            Value::Nil => "nil".into(),
            Value::Bool(b) => b.to_string(),
            Value::Number(n) => format_number(n),
            Value::String(s) => heap.get(s).to_owned(),
            Value::Function(f) => format!("function: 0x{:08x}", f.id()),
            Value::Table(t) => format!("table: 0x{:08x}", t.id()),
            Value::UserData(u) => format!("{}: 0x{:08x}", heap.userdata_type_name(u), u.id()),
            Value::Thread(t) => format!("thread: 0x{:08x}", t.id()),
            Value::Buffer(b) => format!("buffer: 0x{:08x}", b.id()),
            Value::Vector(v) => vector_text(v),
        }
    }
}

/// `tostring` of a vector, as Luau writes it: `1, 2, 3`.
pub fn vector_text(v: [f32; 3]) -> String {
    // Each component at `f32` precision (`0.1`, not the `f64` it widens to).
    v.map(|c| {
        if !c.is_finite() {
            format_number(f64::from(c))
        } else if c.abs() < 1e16 {
            c.to_string()
        } else {
            // `1e20` as Lua writes it: `1e+20`.
            format!("{c:e}").replacen('e', "e+", 1)
        }
    })
    .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_is_small() {
        assert_eq!(std::mem::size_of::<Value>(), 16);
    }

    #[test]
    fn truthiness() {
        assert!(!Value::Nil.is_truthy());
        assert!(!Value::Bool(false).is_truthy());
        assert!(Value::Bool(true).is_truthy());
        assert!(Value::Number(0.0).is_truthy());
    }

    #[test]
    fn nan_is_not_equal_to_itself() {
        assert!(!Value::Number(f64::NAN).raw_eq(Value::Number(f64::NAN)));
    }
}
