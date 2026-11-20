//! Converting between Rust types and script values.
//!
//! * [`IntoValue`] / [`FromValue`]: one value.
//! * [`IntoValues`] / [`FromValues`]: argument and result lists. A single
//!   `T` is a one-element list, `()` is empty, tuples are lists, and
//!   [`Variadic<T>`] is "all the rest".
//!
//! Conversions are strict, like a typed Luau signature. A `String`
//! parameter won't take a number, an integer parameter won't take `1.5`.
//! `Option<T>` takes `nil` (and a missing argument).

#![expect(
    clippy::cast_possible_truncation,
    reason = "converting script numbers into Rust's integer types follows `as` on purpose (saturating, like Luau)"
)]

use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::hash::Hash;
use std::ops::{Deref, DerefMut};
use std::rc::Rc;

use crate::{AnyUserData, Buffer, Context, Function, Table, Thread, Value};

/// A value had the wrong type for the Rust side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConversionError {
    /// The type that was wanted (`number`, `string`, `Player`...).
    pub expected: Cow<'static, str>,
    /// The type of what got passed instead.
    pub got: String,
}

impl ConversionError {
    /// An error for wanting `expected` but getting `got`.
    pub fn new(expected: impl Into<Cow<'static, str>>, got: &Value) -> Self {
        ConversionError {
            expected: expected.into(),
            got: got.type_of().to_string(),
        }
    }
}

impl fmt::Display for ConversionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} expected, got {}", self.expected, self.got)
    }
}

impl std::error::Error for ConversionError {}

/// Rust → script, for one value.
pub trait IntoValue {
    /// Converts `self` into a script value.
    fn into_value(self, cx: &mut Context<'_>) -> Value;

    /// The Luau type of the values this makes, for the type checker
    /// (`"number"`, `"{string}"`, `"Player?"`). `any` by default.
    fn luau_type() -> Cow<'static, str>
    where
        Self: Sized,
    {
        "any".into()
    }

    /// Sets up any userdata types [`luau_type`](Self::luau_type) mentions.
    #[doc(hidden)]
    fn declare_types(_vm: &mut rhue_vm::Vm)
    where
        Self: Sized,
    {
    }
}

/// Script → Rust, for one value.
pub trait FromValue: Sized {
    /// Converts a script value into `Self`, or says why it can't.
    fn from_value(value: Value, cx: &mut Context<'_>) -> Result<Self, ConversionError>;

    /// The Luau type this takes, for the type checker. `any` by default.
    fn luau_type() -> Cow<'static, str> {
        "any".into()
    }

    #[doc(hidden)]
    fn declare_types(_vm: &mut rhue_vm::Vm) {}
}

/// `T?`, parenthesising function and union types.
pub(crate) fn optional(t: &str) -> String {
    if t.contains("->") || t.contains('|') || t.contains('&') {
        format!("({t})?")
    } else {
        format!("{t}?")
    }
}

// ─ One Value ─────────────────────────────────────────────────────────────────

impl IntoValue for Value {
    fn into_value(self, _: &mut Context<'_>) -> Value {
        self
    }
}

impl FromValue for Value {
    fn from_value(value: Value, _: &mut Context<'_>) -> Result<Self, ConversionError> {
        Ok(value)
    }
}

impl IntoValue for bool {
    fn into_value(self, _: &mut Context<'_>) -> Value {
        Value::Bool(self)
    }

    fn luau_type() -> Cow<'static, str> {
        "boolean".into()
    }
}

impl FromValue for bool {
    fn from_value(value: Value, _: &mut Context<'_>) -> Result<Self, ConversionError> {
        match value {
            Value::Bool(b) => Ok(b),
            other => Err(ConversionError::new("boolean", &other)),
        }
    }

    fn luau_type() -> Cow<'static, str> {
        "boolean".into()
    }
}

macro_rules! floats {
    ($($t:ty),*) => {$(
        impl IntoValue for $t {
            #[inline]
            fn into_value(self, _: &mut Context<'_>) -> Value {
                Value::Number(self as f64)
            }

            fn luau_type() -> Cow<'static, str> {
                "number".into()
            }
        }

        impl FromValue for $t {
            #[inline]
            fn from_value(value: Value, _: &mut Context<'_>) -> Result<Self, ConversionError> {
                match value {
                    Value::Number(n) => Ok(n as $t),
                    other => Err(ConversionError::new("number", &other)),
                }
            }

            fn luau_type() -> Cow<'static, str> {
                "number".into()
            }
        }
    )*};
}
floats!(f64, f32);

/// A `vector` is three `f32`s.
impl IntoValue for [f32; 3] {
    #[inline]
    fn into_value(self, _: &mut Context<'_>) -> Value {
        Value::Vector(self)
    }

    fn luau_type() -> Cow<'static, str> {
        "vector".into()
    }
}

impl FromValue for [f32; 3] {
    #[inline]
    fn from_value(value: Value, _: &mut Context<'_>) -> Result<Self, ConversionError> {
        match value {
            Value::Vector(v) => Ok(v),
            other => Err(ConversionError::new("vector", &other)),
        }
    }

    fn luau_type() -> Cow<'static, str> {
        "vector".into()
    }
}

macro_rules! integers {
    ($($t:ty),*) => {$(
        impl IntoValue for $t {
            #[inline]
            fn into_value(self, _: &mut Context<'_>) -> Value {
                Value::Number(self as f64)
            }

            fn luau_type() -> Cow<'static, str> {
                "number".into()
            }
        }

        /// Takes numbers with no fractional part that fit the type.
        impl FromValue for $t {
            #[inline]
            fn from_value(value: Value, _: &mut Context<'_>) -> Result<Self, ConversionError> {
                match value {
                    Value::Number(n)
                        if n.fract() == 0.0 && n >= <$t>::MIN as f64 && n <= <$t>::MAX as f64 =>
                    {
                        Ok(n as $t)
                    }
                    Value::Number(n) => Err(ConversionError {
                        expected: concat!("integer (", stringify!($t), ")").into(),
                        got: format!("number {}", rhue_bytecode::format_number(n)),
                    }),
                    other => Err(ConversionError::new("number", &other)),
                }
            }

            fn luau_type() -> Cow<'static, str> {
                "number".into()
            }
        }
    )*};
}
integers!(i8, i16, i32, i64, isize, u8, u16, u32, u64, usize);

impl IntoValue for &str {
    fn into_value(self, _: &mut Context<'_>) -> Value {
        Value::String(Rc::from(self))
    }

    fn luau_type() -> Cow<'static, str> {
        "string".into()
    }
}

impl IntoValue for String {
    fn into_value(self, _: &mut Context<'_>) -> Value {
        Value::String(Rc::from(self))
    }

    fn luau_type() -> Cow<'static, str> {
        "string".into()
    }
}

impl IntoValue for &String {
    fn into_value(self, _: &mut Context<'_>) -> Value {
        Value::String(Rc::from(self.as_str()))
    }

    fn luau_type() -> Cow<'static, str> {
        "string".into()
    }
}

impl IntoValue for Rc<str> {
    fn into_value(self, _: &mut Context<'_>) -> Value {
        Value::String(self)
    }

    fn luau_type() -> Cow<'static, str> {
        "string".into()
    }
}

impl FromValue for String {
    fn from_value(value: Value, _: &mut Context<'_>) -> Result<Self, ConversionError> {
        match value {
            Value::String(s) => Ok(s.to_string()),
            other => Err(ConversionError::new("string", &other)),
        }
    }

    fn luau_type() -> Cow<'static, str> {
        "string".into()
    }
}

impl FromValue for Rc<str> {
    fn from_value(value: Value, _: &mut Context<'_>) -> Result<Self, ConversionError> {
        match value {
            Value::String(s) => Ok(s),
            other => Err(ConversionError::new("string", &other)),
        }
    }

    fn luau_type() -> Cow<'static, str> {
        "string".into()
    }
}

/// `None` is `nil`.
impl<T: IntoValue> IntoValue for Option<T> {
    fn into_value(self, cx: &mut Context<'_>) -> Value {
        match self {
            Some(v) => v.into_value(cx),
            None => Value::Nil,
        }
    }

    fn luau_type() -> Cow<'static, str> {
        optional(&T::luau_type()).into()
    }

    fn declare_types(vm: &mut rhue_vm::Vm) {
        T::declare_types(vm)
    }
}

/// `nil` (or a missing argument) is `None`.
impl<T: FromValue> FromValue for Option<T> {
    fn from_value(value: Value, cx: &mut Context<'_>) -> Result<Self, ConversionError> {
        match value {
            Value::Nil => Ok(None),
            v => T::from_value(v, cx).map(Some).map_err(|e| ConversionError {
                expected: format!("{}?", e.expected).into(),
                got: e.got,
            }),
        }
    }

    fn luau_type() -> Cow<'static, str> {
        optional(&T::luau_type()).into()
    }

    fn declare_types(vm: &mut rhue_vm::Vm) {
        T::declare_types(vm)
    }
}

macro_rules! handles {
    ($($t:ident, $name:literal, $luau:literal);*) => {$(
        impl IntoValue for $t {
            #[inline]
            fn into_value(self, _: &mut Context<'_>) -> Value {
                Value::$t(self)
            }

            fn luau_type() -> Cow<'static, str> {
                $luau.into()
            }
        }

        impl FromValue for $t {
            #[inline]
            fn from_value(value: Value, _: &mut Context<'_>) -> Result<Self, ConversionError> {
                match value {
                    Value::$t(h) => Ok(h),
                    other => Err(ConversionError::new($name, &other)),
                }
            }

            fn luau_type() -> Cow<'static, str> {
                $luau.into()
            }
        }
    )*};
}
handles!(Function, "function", "(...any) -> ...any"; Table, "table", "{[any]: any}"; Thread, "thread", "thread"; Buffer, "buffer", "buffer");

impl IntoValue for AnyUserData {
    fn into_value(self, _: &mut Context<'_>) -> Value {
        Value::UserData(self)
    }
}

impl FromValue for AnyUserData {
    fn from_value(value: Value, _: &mut Context<'_>) -> Result<Self, ConversionError> {
        match value {
            Value::UserData(u) => Ok(u),
            other => Err(ConversionError::new("userdata", &other)),
        }
    }
}

/// A new table with the elements at keys `1..=n`.
impl<T: IntoValue> IntoValue for Vec<T> {
    fn into_value(self, cx: &mut Context<'_>) -> Value {
        let values: Vec<Value> = self.into_iter().map(|v| v.into_value(cx)).collect();
        Value::Table(cx.table_from_list(values))
    }

    fn luau_type() -> Cow<'static, str> {
        format!("{{{}}}", T::luau_type()).into()
    }

    fn declare_types(vm: &mut rhue_vm::Vm) {
        T::declare_types(vm)
    }
}

/// Reads keys `1..=#t`.
impl<T: FromValue> FromValue for Vec<T> {
    fn from_value(value: Value, cx: &mut Context<'_>) -> Result<Self, ConversionError> {
        let Value::Table(t) = value else {
            return Err(ConversionError::new("table", &value));
        };
        (1..=cx.len(&t))
            .map(|i| {
                let v = cx.raw_get(&t, Value::Number(i as f64));
                T::from_value(v, cx).map_err(|e| ConversionError {
                    expected: format!("{} at [{i}]", e.expected).into(),
                    got: e.got,
                })
            })
            .collect()
    }

    fn luau_type() -> Cow<'static, str> {
        format!("{{{}}}", T::luau_type()).into()
    }

    fn declare_types(vm: &mut rhue_vm::Vm) {
        T::declare_types(vm)
    }
}

/// A new table with these keys and values.
impl<K: IntoValue, V: IntoValue> IntoValue for HashMap<K, V> {
    fn into_value(self, cx: &mut Context<'_>) -> Value {
        let t = cx.new_table();
        for (k, v) in self {
            let (k, v) = (k.into_value(cx), v.into_value(cx));
            // keys from a Rust map can only be invalid if they're NaN
            let _ = cx.set(&t, k, v);
        }
        Value::Table(t)
    }

    fn luau_type() -> Cow<'static, str> {
        format!("{{[{}]: {}}}", K::luau_type(), V::luau_type()).into()
    }

    fn declare_types(vm: &mut rhue_vm::Vm) {
        K::declare_types(vm);
        V::declare_types(vm);
    }
}

/// Every key-value pair of the table.
impl<K: FromValue + Eq + Hash, V: FromValue> FromValue for HashMap<K, V> {
    fn from_value(value: Value, cx: &mut Context<'_>) -> Result<Self, ConversionError> {
        let Value::Table(t) = value else {
            return Err(ConversionError::new("table", &value));
        };
        cx.pairs(&t)
            .into_iter()
            .map(|(k, v)| Ok((K::from_value(k, cx)?, V::from_value(v, cx)?)))
            .collect()
    }

    fn luau_type() -> Cow<'static, str> {
        format!("{{[{}]: {}}}", K::luau_type(), V::luau_type()).into()
    }

    fn declare_types(vm: &mut rhue_vm::Vm) {
        K::declare_types(vm);
        V::declare_types(vm);
    }
}

// ─ Lists of Values ───────────────────────────────────────────────────────────

/// Any number of values of one type. As a parameter it takes all the remaining
/// arguments, as a result it returns several values (not a table).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Variadic<T>(pub Vec<T>);

impl<T> Deref for Variadic<T> {
    type Target = Vec<T>;
    fn deref(&self) -> &Vec<T> {
        &self.0
    }
}

impl<T> DerefMut for Variadic<T> {
    fn deref_mut(&mut self) -> &mut Vec<T> {
        &mut self.0
    }
}

impl<T> FromIterator<T> for Variadic<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Variadic(iter.into_iter().collect())
    }
}

impl<T> IntoIterator for Variadic<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

/// Argument `index` (1-based) had the wrong type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArgError {
    /// Which argument (0-based) didn't convert.
    pub index: usize,
    /// Why it didn't.
    pub error: ConversionError,
}

/// The arguments (or results) being converted, in order.
pub struct Args {
    source: ArgSource,
    /// 1-based index of the next value.
    next: usize,
}

enum ArgSource {
    /// Values already converted (results of a call from Rust).
    Lifted(std::vec::IntoIter<Value>),
    /// A native's arguments straight from the VM, converted one at a time as
    /// parameters take them (no list gets built for the common case).
    Raw(rhue_vm::SmallVec<[rhue_vm::Value; 8]>, usize),
}

impl Args {
    pub(crate) fn new(values: Vec<Value>, first_index: usize) -> Self {
        Args {
            source: ArgSource::Lifted(values.into_iter()),
            next: first_index,
        }
    }

    pub(crate) fn raw(values: &[rhue_vm::Value], first_index: usize) -> Self {
        Args {
            source: ArgSource::Raw(rhue_vm::SmallVec::from_slice(values), 0),
            next: first_index,
        }
    }

    /// The next value, if any are left.
    #[inline]
    fn take(&mut self, cx: &mut Context<'_>) -> Option<Value> {
        let v = match &mut self.source {
            ArgSource::Lifted(it) => it.next(),
            ArgSource::Raw(values, at) => {
                let v = values.get(*at).map(|&v| cx.lift(v));
                *at += 1;
                v
            }
        };
        if v.is_some() {
            self.next += 1;
        }
        v
    }
}

/// Something that takes its value(s) from an argument list. Any [`FromValue`]
/// takes one (`nil` if missing), [`Variadic`] takes the rest.
pub trait FromArg: Sized {
    /// Takes this parameter's value (or values) from the arguments.
    fn from_arg(args: &mut Args, cx: &mut Context<'_>) -> Result<Self, ArgError>;

    /// The parameter's type in a Luau signature (`"number"`, `"...string"`).
    fn param_type() -> String;

    #[doc(hidden)]
    fn declare_types(_vm: &mut rhue_vm::Vm) {}
}

impl<T: FromValue> FromArg for T {
    fn from_arg(args: &mut Args, cx: &mut Context<'_>) -> Result<Self, ArgError> {
        let index = args.next;
        let v = match args.take(cx) {
            Some(v) => v,
            None => {
                // a missing argument is nil (and still counts for errors)
                args.next += 1;
                Value::Nil
            }
        };
        T::from_value(v, cx).map_err(|error| ArgError { index, error })
    }

    fn param_type() -> String {
        T::luau_type().into_owned()
    }

    fn declare_types(vm: &mut rhue_vm::Vm) {
        <T as FromValue>::declare_types(vm)
    }
}

impl<T: FromValue> FromArg for Variadic<T> {
    fn from_arg(args: &mut Args, cx: &mut Context<'_>) -> Result<Self, ArgError> {
        let mut out = Vec::new();
        loop {
            let index = args.next;
            let Some(v) = args.take(cx) else {
                break;
            };
            out.push(T::from_value(v, cx).map_err(|error| ArgError { index, error })?);
        }
        Ok(Variadic(out))
    }

    fn param_type() -> String {
        format!("...{}", T::luau_type())
    }

    fn declare_types(vm: &mut rhue_vm::Vm) {
        <T as FromValue>::declare_types(vm)
    }
}

/// Script values → a Rust argument or result list.
pub trait FromValues: Sized {
    /// Builds `Self` from all of a call's results.
    fn from_values(values: Vec<Value>, cx: &mut Context<'_>) -> Result<Self, ArgError>;
}

impl<T: FromArg> FromValues for T {
    fn from_values(values: Vec<Value>, cx: &mut Context<'_>) -> Result<Self, ArgError> {
        T::from_arg(&mut Args::new(values, 1), cx)
    }
}

/// Rust values → a script argument or result list.
pub trait IntoValues {
    /// Converts `self` into the values a call returns.
    fn into_values(self, cx: &mut Context<'_>) -> Vec<Value>;

    /// Appends the values, converted for the VM, to `out` (what natives return.
    /// overridden so a single value doesn't build a list).
    #[doc(hidden)]
    fn lower_into(self, cx: &mut Context<'_>, out: &mut rhue_vm::Rets)
    where
        Self: Sized,
    {
        for v in self.into_values(cx) {
            let v = cx.lower(v);
            out.push(v);
        }
    }

    /// The list's types as the result of a Luau function type (`"number"`,
    /// `"(number, string)"`, `"()"`, `"...any"`).
    fn pack_type() -> String
    where
        Self: Sized;

    /// The same, as entries of a list (no parentheses).
    #[doc(hidden)]
    fn pack_entries() -> Vec<String>
    where
        Self: Sized;

    #[doc(hidden)]
    fn declare_types(_vm: &mut rhue_vm::Vm)
    where
        Self: Sized,
    {
    }
}

impl<T: IntoValue> IntoValues for T {
    fn into_values(self, cx: &mut Context<'_>) -> Vec<Value> {
        vec![self.into_value(cx)]
    }

    fn lower_into(self, cx: &mut Context<'_>, out: &mut rhue_vm::Rets) {
        let v = self.into_value(cx);
        let v = cx.lower(v);
        out.push(v);
    }

    fn pack_type() -> String {
        T::luau_type().into_owned()
    }

    fn pack_entries() -> Vec<String> {
        vec![T::luau_type().into_owned()]
    }

    fn declare_types(vm: &mut rhue_vm::Vm) {
        <T as IntoValue>::declare_types(vm)
    }
}

impl<T: IntoValue> IntoValues for Variadic<T> {
    fn into_values(self, cx: &mut Context<'_>) -> Vec<Value> {
        self.0.into_iter().map(|v| v.into_value(cx)).collect()
    }

    fn pack_type() -> String {
        format!("...{}", T::luau_type())
    }

    fn pack_entries() -> Vec<String> {
        vec![Self::pack_type()]
    }

    fn declare_types(vm: &mut rhue_vm::Vm) {
        <T as IntoValue>::declare_types(vm)
    }
}

macro_rules! tuples {
    ($($A:ident),*) => {
        impl<$($A: FromArg),*> FromValues for ($($A,)*) {
            #[allow(unused_variables, unused_mut, clippy::unused_unit)]
            fn from_values(values: Vec<Value>, cx: &mut Context<'_>) -> Result<Self, ArgError> {
                let mut args = Args::new(values, 1);
                Ok(($($A::from_arg(&mut args, cx)?,)*))
            }
        }

        impl<$($A: IntoValues),*> IntoValues for ($($A,)*) {
            #[allow(non_snake_case, unused_variables, unused_mut)]
            fn into_values(self, cx: &mut Context<'_>) -> Vec<Value> {
                let ($($A,)*) = self;
                let mut out = Vec::new();
                $(out.extend($A.into_values(cx));)*
                out
            }

            #[allow(non_snake_case, unused_variables)]
            fn lower_into(self, cx: &mut Context<'_>, out: &mut rhue_vm::Rets) {
                let ($($A,)*) = self;
                $($A.lower_into(cx, out);)*
            }

            fn pack_type() -> String {
                format!("({})", Self::pack_entries().join(", "))
            }

            #[allow(unused_mut)]
            fn pack_entries() -> Vec<String> {
                let mut out: Vec<String> = Vec::new();
                $(out.extend($A::pack_entries());)*
                out
            }

            #[allow(unused_variables)]
            fn declare_types(vm: &mut rhue_vm::Vm) {
                $($A::declare_types(vm);)*
            }
        }
    };
}
tuples!();
tuples!(A);
tuples!(A, B);
tuples!(A, B, C);
tuples!(A, B, C, D);
tuples!(A, B, C, D, E);
tuples!(A, B, C, D, E, F);
tuples!(A, B, C, D, E, F, G);
tuples!(A, B, C, D, E, F, G, H);
