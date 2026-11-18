//! Converting between Rhue values and Rust types with `serde` (the
//! `serde` feature).
//!
//! The mapping is the one JSON uses, in Lua's terms:
//!
//! | Rust | Rhue |
//! |---|---|
//! | `bool` | boolean |
//! | integers, floats | number (an integer read back must be whole and in range) |
//! | `char`, `String`, `&str` | string |
//! | `None`, `()`, unit structs | `nil` |
//! | `Some(x)`, newtype structs | `x` |
//! | sequences, tuples, tuple structs | an array table (`{1, 2, 3}`) |
//! | maps, structs | a table with those keys |
//! | bytes (`serde_bytes`) | a buffer |
//! | (reading only) | a vector reads as a sequence of its three components |
//! | unit variants | their name, as a string |
//! | other variants | `{ Variant = contents }` |
//!
//! A table reads back as a sequence when its keys are exactly 1 to n, and
//! as a map otherwise. Functions, threads and userdata don't convert.
//!
//! ```
//! # use serde::{Deserialize, Serialize};
//! #[derive(Serialize, Deserialize, Debug, PartialEq)]
//! struct Config { name: String, retries: u32, hosts: Vec<String> }
//!
//! let mut engine = rhue::Engine::new();
//! let mut cx = engine.context();
//! let config = Config { name: "api".into(), retries: 3, hosts: vec!["a".into()] };
//! let value = rhue::serde::to_value(&mut cx, &config).unwrap();
//! cx.set_global("config", value);
//! drop(cx);
//! engine.eval("config.retries += 1").unwrap();
//! let value = engine.global("config");
//! let back: Config = rhue::serde::from_value(&mut engine.context(), value).unwrap();
//! assert_eq!(back.retries, 4);
//! ```

use std::fmt;

use serde::de::{self, DeserializeSeed, IntoDeserializer, MapAccess, SeqAccess, Visitor};
use serde::ser::{self, Serialize};

use crate::{Context, ScriptError, Table, Value};

/// Why a conversion failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl ser::Error for Error {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Error(msg.to_string())
    }
}

impl de::Error for Error {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Error(msg.to_string())
    }
}

impl From<Error> for ScriptError {
    fn from(e: Error) -> Self {
        ScriptError::new(e.0)
    }
}

/// `value` as a Rhue value (tables are new tables in `cx`'s engine).
pub fn to_value<T: Serialize + ?Sized>(cx: &mut Context<'_>, value: &T) -> Result<Value, Error> {
    value.serialize(Serializer { cx })
}

/// A `T` read out of a Rhue value.
pub fn from_value<T: de::DeserializeOwned>(cx: &mut Context<'_>, value: Value) -> Result<T, Error> {
    T::deserialize(Deserializer { cx, value })
}

// --- to Rhue ----------------------------------------------------------------------

struct Serializer<'a, 'b> {
    cx: &'a mut Context<'b>,
}

/// A table being filled: sequence items by index, or entries by key.
struct TableBuilder<'a, 'b> {
    cx: &'a mut Context<'b>,
    table: Table,
    next: usize,
    /// For a map: the key whose value comes next.
    key: Option<Value>,
    /// For a variant: the table to wrap this one in, under its name.
    wrap: Option<&'static str>,
}

impl<'a, 'b> TableBuilder<'a, 'b> {
    fn new(cx: &'a mut Context<'b>, wrap: Option<&'static str>) -> Self {
        let table = cx.new_table();
        TableBuilder {
            cx,
            table,
            next: 1,
            key: None,
            wrap,
        }
    }

    fn push<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        let v = value.serialize(Serializer { cx: self.cx })?;
        let i = self.next;
        self.next += 1;
        self.set(Value::Number(i as f64), v)
    }

    fn set(&mut self, key: Value, value: Value) -> Result<(), Error> {
        self.cx
            .set(&self.table, key, value)
            .map_err(|e| Error(e.message()))
    }

    fn finish(self) -> Result<Value, Error> {
        let value = Value::Table(self.table);
        match self.wrap {
            None => Ok(value),
            Some(variant) => {
                let outer = self.cx.new_table();
                self.cx
                    .set(&outer, variant, value)
                    .map_err(|e| Error(e.message()))?;
                Ok(Value::Table(outer))
            }
        }
    }
}

impl<'a, 'b> ser::Serializer for Serializer<'a, 'b> {
    type Ok = Value;
    type Error = Error;
    type SerializeSeq = TableBuilder<'a, 'b>;
    type SerializeTuple = TableBuilder<'a, 'b>;
    type SerializeTupleStruct = TableBuilder<'a, 'b>;
    type SerializeTupleVariant = TableBuilder<'a, 'b>;
    type SerializeMap = TableBuilder<'a, 'b>;
    type SerializeStruct = TableBuilder<'a, 'b>;
    type SerializeStructVariant = TableBuilder<'a, 'b>;

    fn serialize_bool(self, v: bool) -> Result<Value, Error> {
        Ok(Value::Bool(v))
    }
    fn serialize_i8(self, v: i8) -> Result<Value, Error> {
        Ok(Value::Number(v.into()))
    }
    fn serialize_i16(self, v: i16) -> Result<Value, Error> {
        Ok(Value::Number(v.into()))
    }
    fn serialize_i32(self, v: i32) -> Result<Value, Error> {
        Ok(Value::Number(v.into()))
    }
    fn serialize_i64(self, v: i64) -> Result<Value, Error> {
        Ok(Value::Number(v as f64))
    }
    fn serialize_u8(self, v: u8) -> Result<Value, Error> {
        Ok(Value::Number(v.into()))
    }
    fn serialize_u16(self, v: u16) -> Result<Value, Error> {
        Ok(Value::Number(v.into()))
    }
    fn serialize_u32(self, v: u32) -> Result<Value, Error> {
        Ok(Value::Number(v.into()))
    }
    fn serialize_u64(self, v: u64) -> Result<Value, Error> {
        Ok(Value::Number(v as f64))
    }
    fn serialize_f32(self, v: f32) -> Result<Value, Error> {
        Ok(Value::Number(v.into()))
    }
    fn serialize_f64(self, v: f64) -> Result<Value, Error> {
        Ok(Value::Number(v))
    }
    fn serialize_char(self, v: char) -> Result<Value, Error> {
        Ok(Value::String(v.to_string().into()))
    }
    fn serialize_str(self, v: &str) -> Result<Value, Error> {
        Ok(Value::String(v.into()))
    }
    fn serialize_bytes(self, v: &[u8]) -> Result<Value, Error> {
        Ok(Value::Buffer(self.cx.create_buffer(v)))
    }
    fn serialize_none(self) -> Result<Value, Error> {
        Ok(Value::Nil)
    }
    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<Value, Error> {
        value.serialize(self)
    }
    fn serialize_unit(self) -> Result<Value, Error> {
        Ok(Value::Nil)
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<Value, Error> {
        Ok(Value::Nil)
    }
    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
    ) -> Result<Value, Error> {
        Ok(Value::String(variant.into()))
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        value: &T,
    ) -> Result<Value, Error> {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<Value, Error> {
        let inner = value.serialize(Serializer { cx: self.cx })?;
        let outer = self.cx.new_table();
        self.cx
            .set(&outer, variant, inner)
            .map_err(|e| Error(e.message()))?;
        Ok(Value::Table(outer))
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Self::SerializeSeq, Error> {
        Ok(TableBuilder::new(self.cx, None))
    }
    fn serialize_tuple(self, _: usize) -> Result<Self::SerializeTuple, Error> {
        Ok(TableBuilder::new(self.cx, None))
    }
    fn serialize_tuple_struct(
        self,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleStruct, Error> {
        Ok(TableBuilder::new(self.cx, None))
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleVariant, Error> {
        Ok(TableBuilder::new(self.cx, Some(variant)))
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Self::SerializeMap, Error> {
        Ok(TableBuilder::new(self.cx, None))
    }
    fn serialize_struct(self, _: &'static str, _: usize) -> Result<Self::SerializeStruct, Error> {
        Ok(TableBuilder::new(self.cx, None))
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStructVariant, Error> {
        Ok(TableBuilder::new(self.cx, Some(variant)))
    }
}

macro_rules! sequence {
    ($($tr:ident :: $m:ident),*) => {$(
        impl ser::$tr for TableBuilder<'_, '_> {
            type Ok = Value;
            type Error = Error;
            fn $m<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
                self.push(value)
            }
            fn end(self) -> Result<Value, Error> {
                self.finish()
            }
        }
    )*};
}
sequence!(
    SerializeSeq::serialize_element,
    SerializeTuple::serialize_element,
    SerializeTupleStruct::serialize_field,
    SerializeTupleVariant::serialize_field
);

impl ser::SerializeMap for TableBuilder<'_, '_> {
    type Ok = Value;
    type Error = Error;
    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), Error> {
        self.key = Some(key.serialize(Serializer { cx: self.cx })?);
        Ok(())
    }
    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        let key = self
            .key
            .take()
            .ok_or_else(|| Error("a map value without a key".into()))?;
        if key.is_nil() {
            return Err(Error("a map key can't be nil".into()));
        }
        let v = value.serialize(Serializer { cx: self.cx })?;
        self.set(key, v)
    }
    fn end(self) -> Result<Value, Error> {
        self.finish()
    }
}

macro_rules! fields {
    ($($tr:ident),*) => {$(
        impl ser::$tr for TableBuilder<'_, '_> {
            type Ok = Value;
            type Error = Error;
            fn serialize_field<T: Serialize + ?Sized>(
                &mut self,
                key: &'static str,
                value: &T,
            ) -> Result<(), Error> {
                let v = value.serialize(Serializer { cx: self.cx })?;
                self.set(Value::String(key.into()), v)
            }
            fn end(self) -> Result<Value, Error> {
                self.finish()
            }
        }
    )*};
}
fields!(SerializeStruct, SerializeStructVariant);

// --- from Rhue --------------------------------------------------------------------

struct Deserializer<'a, 'b> {
    cx: &'a mut Context<'b>,
    value: Value,
}

/// A table's entries, as a sequence when its keys are exactly 1 to n.
fn entries(cx: &mut Context<'_>, t: &Table) -> (Vec<(Value, Value)>, bool) {
    let mut e = cx.pairs(t);
    let is_seq = e
        .iter()
        .all(|(k, _)| matches!(k, Value::Number(n) if n.fract() == 0.0 && *n >= 1.0 && *n <= e.len() as f64));
    if is_seq {
        e.sort_by(|a, b| {
            a.0.as_number()
                .unwrap_or(0.0)
                .total_cmp(&b.0.as_number().unwrap_or(0.0))
        });
    }
    (e, is_seq)
}

fn unexpected(v: &Value, wanted: &str) -> Error {
    Error(format!("expected {wanted}, got {}", v.type_name()))
}

impl<'de> de::Deserializer<'de> for Deserializer<'_, '_> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.value {
            Value::Nil => visitor.visit_unit(),
            Value::Bool(b) => visitor.visit_bool(b),
            Value::Number(n) => {
                if n.fract() == 0.0 && n.abs() < 9.007_199_254_740_992e15 {
                    visitor.visit_i64(n as i64)
                } else {
                    visitor.visit_f64(n)
                }
            }
            Value::String(s) => visitor.visit_str(&s),
            Value::Buffer(b) => visitor.visit_byte_buf(self.cx.buffer_bytes(&b)),
            // A vector reads as its three components.
            Value::Vector(v) => visitor.visit_seq(Seq {
                cx: self.cx,
                items: v.into_iter().map(|c| Value::Number(f64::from(c))),
            }),
            Value::Table(t) => {
                let (entries, is_seq) = entries(self.cx, &t);
                if is_seq && !entries.is_empty() {
                    visitor.visit_seq(Seq {
                        cx: self.cx,
                        items: entries.into_iter().map(|(_, v)| v),
                    })
                } else {
                    visitor.visit_map(Map {
                        cx: self.cx,
                        entries: entries.into_iter(),
                        value: None,
                    })
                }
            }
            ref other => Err(Error(format!(
                "can't convert a {} with serde",
                other.type_name()
            ))),
        }
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.value {
            Value::Nil => visitor.visit_none(),
            _ => visitor.visit_some(self),
        }
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        if let Value::Vector(_) = self.value {
            return de::Deserializer::deserialize_any(self, visitor);
        }
        let Value::Table(t) = &self.value else {
            return Err(unexpected(&self.value, "a table"));
        };
        let (entries, is_seq) = entries(self.cx, t);
        if !is_seq {
            return Err(Error("expected an array table (keys 1 to n)".into()));
        }
        visitor.visit_seq(Seq {
            cx: self.cx,
            items: entries.into_iter().map(|(_, v)| v),
        })
    }

    fn deserialize_tuple<V: Visitor<'de>>(self, _: usize, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_seq(visitor)
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        _: usize,
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.deserialize_seq(visitor)
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        let Value::Table(t) = &self.value else {
            return Err(unexpected(&self.value, "a table"));
        };
        let (entries, _) = entries(self.cx, t);
        visitor.visit_map(Map {
            cx: self.cx,
            entries: entries.into_iter(),
            value: None,
        })
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        _: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.deserialize_map(visitor)
    }

    fn deserialize_unit<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.value {
            Value::Nil => visitor.visit_unit(),
            ref other => Err(unexpected(other, "nil")),
        }
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.deserialize_unit(visitor)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_bytes<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.value {
            Value::Buffer(b) => visitor.visit_byte_buf(self.cx.buffer_bytes(&b)),
            Value::String(s) => visitor.visit_bytes(s.as_bytes()),
            ref other => Err(unexpected(other, "a buffer")),
        }
    }

    fn deserialize_byte_buf<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        self.deserialize_bytes(visitor)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _: &'static str,
        _: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        match self.value {
            Value::String(s) => visitor.visit_enum(s.to_string().into_deserializer()),
            Value::Table(t) => {
                let mut e = self.cx.pairs(&t);
                if e.len() != 1 {
                    return Err(Error(
                        "expected a table with one key, the variant's name".into(),
                    ));
                }
                let (k, v) = e.remove(0);
                let Value::String(variant) = k else {
                    return Err(unexpected(&k, "a variant name"));
                };
                visitor.visit_enum(Enum {
                    cx: self.cx,
                    variant: variant.to_string(),
                    value: v,
                })
            }
            ref other => Err(unexpected(other, "a string or a table")),
        }
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_unit()
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string identifier
    }
}

struct Seq<'a, 'b, I> {
    cx: &'a mut Context<'b>,
    items: I,
}

impl<'de, I: Iterator<Item = Value>> SeqAccess<'de> for Seq<'_, '_, I> {
    type Error = Error;
    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Error> {
        match self.items.next() {
            None => Ok(None),
            Some(value) => seed
                .deserialize(Deserializer { cx: self.cx, value })
                .map(Some),
        }
    }
}

struct Map<'a, 'b, I> {
    cx: &'a mut Context<'b>,
    entries: I,
    value: Option<Value>,
}

impl<'de, I: Iterator<Item = (Value, Value)>> MapAccess<'de> for Map<'_, '_, I> {
    type Error = Error;
    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        match self.entries.next() {
            None => Ok(None),
            Some((k, v)) => {
                self.value = Some(v);
                seed.deserialize(Deserializer {
                    cx: self.cx,
                    value: k,
                })
                .map(Some)
            }
        }
    }
    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        let value = self.value.take().unwrap_or(Value::Nil);
        seed.deserialize(Deserializer { cx: self.cx, value })
    }
}

struct Enum<'a, 'b> {
    cx: &'a mut Context<'b>,
    variant: String,
    value: Value,
}

impl<'de, 'a, 'b> de::EnumAccess<'de> for Enum<'a, 'b> {
    type Error = Error;
    type Variant = Deserializer<'a, 'b>;
    fn variant_seed<V: DeserializeSeed<'de>>(
        self,
        seed: V,
    ) -> Result<(V::Value, Self::Variant), Error> {
        let v = seed.deserialize(self.variant.into_deserializer())?;
        Ok((
            v,
            Deserializer {
                cx: self.cx,
                value: self.value,
            },
        ))
    }
}

impl<'de> de::VariantAccess<'de> for Deserializer<'_, '_> {
    type Error = Error;
    fn unit_variant(self) -> Result<(), Error> {
        Ok(())
    }
    fn newtype_variant_seed<T: DeserializeSeed<'de>>(self, seed: T) -> Result<T::Value, Error> {
        seed.deserialize(self)
    }
    fn tuple_variant<V: Visitor<'de>>(self, _: usize, visitor: V) -> Result<V::Value, Error> {
        de::Deserializer::deserialize_seq(self, visitor)
    }
    fn struct_variant<V: Visitor<'de>>(
        self,
        _: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        de::Deserializer::deserialize_map(self, visitor)
    }
}
