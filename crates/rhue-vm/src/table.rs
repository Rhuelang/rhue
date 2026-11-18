//! Lua tables: an array part for keys `1..=n` and an insertion-ordered hash
//! part for everything else.
//!
//! **Array part.** Holds keys `1..=array.len()`. Setting `n + 1` appends and
//! then pulls any following integer keys (`n + 2`, ...) over from the hash
//! part, so a table filled in order stays entirely in the array. Setting the
//! last element to nil pops it (and any nils before it), so `#t` is just
//! `array.len()` unless the array has holes in the middle; then a binary
//! search finds a border, as Lua's `#` allows.
//!
//! **Hash part.** A `HashPart`: insertion-ordered and indexable, so `next`
//! can resume from any key in O(1) (small parts are a vector searched
//! linearly, larger ones an index map).
//! Assigning nil to an existing key leaves a tombstone instead of removing
//! it. That keeps the traversal order stable, so clearing fields while
//! iterating with `next`/`pairs` works, as Lua guarantees. Tombstones are
//! compacted away when a *new* key is inserted (inserting during traversal
//! is undefined in Lua anyway).

use std::cell::Cell;

use crate::hashpart::HashPart;

use crate::Value;
use crate::heap::{BufRef, FnRef, StrRef, TableRef, ThreadRef, UdRef};

/// A table key: any value except nil and NaN, with numbers normalised so
/// `-0` and `0` are the same key.
// `PartialEq` is written out for speed but compares exactly what the
// derived one would, so it agrees with the derived `Hash`.
#[allow(clippy::derived_hash_with_manual_eq)]
#[derive(Clone, Copy, Debug, Eq, Hash)]
pub enum Key {
    Bool(bool),
    Number(u64),
    String(StrRef),
    Table(TableRef),
    Function(FnRef),
    UserData(UdRef),
    Thread(ThreadRef),
    Buffer(BufRef),
    /// A vector, by its components' bits (`-0` as `0`).
    Vector([u32; 3]),
}

impl PartialEq for Key {
    /// String keys (fields) inline, the rest out of line: the derived
    /// comparison, once a variant held an array, slowed table lookups and
    /// the interpreter around them.
    #[inline]
    fn eq(&self, other: &Key) -> bool {
        if std::mem::discriminant(self) != std::mem::discriminant(other) {
            return false;
        }
        match (self, other) {
            (Key::String(a), Key::String(b)) => a == b,
            _ => self.eq_other(other),
        }
    }
}

impl Key {
    #[inline(never)]
    fn eq_other(&self, other: &Key) -> bool {
        match (self, other) {
            (Key::Number(a), Key::Number(b)) => a == b,
            (Key::Bool(a), Key::Bool(b)) => a == b,
            (Key::Table(a), Key::Table(b)) => a == b,
            (Key::Function(a), Key::Function(b)) => a == b,
            (Key::UserData(a), Key::UserData(b)) => a == b,
            (Key::Thread(a), Key::Thread(b)) => a == b,
            (Key::Buffer(a), Key::Buffer(b)) => a == b,
            (Key::Vector(a), Key::Vector(b)) => a == b,
            _ => false,
        }
    }
}

impl Key {
    /// `Ok(None)` for nil (a lookup that can only miss); `Err` for NaN.
    #[inline]
    pub fn from_value(v: Value) -> Result<Option<Key>, KeyError> {
        Ok(Some(match v {
            Value::Nil => return Ok(None),
            Value::Bool(b) => Key::Bool(b),
            Value::Number(n) if n.is_nan() => return Err(KeyError::NaN),
            Value::Number(n) => Key::Number(if n == 0.0 {
                0f64.to_bits()
            } else {
                n.to_bits()
            }),
            Value::String(s) => Key::String(s),
            Value::Table(t) => Key::Table(t),
            Value::Function(f) => Key::Function(f),
            Value::UserData(u) => Key::UserData(u),
            Value::Thread(t) => Key::Thread(t),
            Value::Buffer(b) => Key::Buffer(b),
            Value::Vector(v) if v.iter().any(|c| c.is_nan()) => return Err(KeyError::NaN),
            Value::Vector(v) => Key::Vector(v.map(|c| if c == 0.0 { 0 } else { c.to_bits() })),
        }))
    }

    pub fn to_value(self) -> Value {
        match self {
            Key::Bool(b) => Value::Bool(b),
            Key::Number(bits) => Value::Number(f64::from_bits(bits)),
            Key::String(s) => Value::String(s),
            Key::Table(t) => Value::Table(t),
            Key::Function(f) => Value::Function(f),
            Key::UserData(u) => Value::UserData(u),
            Key::Thread(t) => Value::Thread(t),
            Key::Buffer(b) => Value::Buffer(b),
            Key::Vector(bits) => Value::Vector(bits.map(f32::from_bits)),
        }
    }
}

/// `next` was given a key that isn't in the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidKey;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyError {
    Nil,
    NaN,
    /// The table is frozen (`table.freeze`).
    Frozen,
}

impl KeyError {
    pub fn message(self) -> &'static str {
        match self {
            KeyError::Nil => "table index is nil",
            KeyError::NaN => "table index is NaN",
            KeyError::Frozen => "attempt to modify a readonly table",
        }
    }
}

#[cold]
#[inline(never)]
fn frozen() -> Result<(), KeyError> {
    Err(KeyError::Frozen)
}

/// `n` as an array index (1-based) if it's a positive integer.
fn array_index(v: Value) -> Option<usize> {
    match v {
        Value::Number(n) if n >= 1.0 && n.fract() == 0.0 && n <= (usize::MAX >> 1) as f64 => {
            Some(n as usize)
        }
        _ => None,
    }
}

#[derive(Debug, Default, Clone)]
pub struct Table {
    array: Vec<Value>,
    hash: HashPart,
    tombstones: usize,
    /// The metatable set with `setmetatable`, if any.
    meta: Option<TableRef>,
    /// Set by `table.freeze`: every write fails from then on.
    frozen: bool,
}

/// Approximate heap bytes, for GC accounting.
pub const TABLE_BASE_BYTES: usize = 64;
const ARRAY_SLOT_BYTES: usize = 16;
const HASH_SLOT_BYTES: usize = 48;

impl Table {
    pub fn with_capacity(array: usize, hash: usize) -> Self {
        Table {
            array: Vec::with_capacity(array),
            hash: HashPart::with_capacity(hash),
            tombstones: 0,
            meta: None,
            frozen: false,
        }
    }

    /// A table shaped by a constructor's template (`DUPTABLE`): room for
    /// `array` and `hash` entries, with `keys` already present as
    /// tombstones (nil), in order. The keys must be distinct.
    #[inline(never)]
    pub fn from_template(
        array: usize,
        hash: usize,
        keys: impl ExactSizeIterator<Item = StrRef>,
    ) -> Self {
        Table {
            array: Vec::with_capacity(array),
            tombstones: keys.len(),
            hash: HashPart::with_nil_keys(hash, keys.map(Key::String)),
            meta: None,
            frozen: false,
        }
    }

    /// The metatable, if one is set.
    #[inline]
    pub fn meta(&self) -> Option<TableRef> {
        self.meta
    }

    pub fn set_meta(&mut self, meta: Option<TableRef>) {
        self.meta = meta;
    }

    /// Makes the table read-only for good (`table.freeze`).
    pub fn freeze(&mut self) {
        self.frozen = true;
    }

    pub fn is_frozen(&self) -> bool {
        self.frozen
    }

    /// A copy starts writable (`table.clone`).
    pub(crate) fn unfreeze_copy(&mut self) {
        self.frozen = false;
    }

    /// Removes every entry, keeping the metatable (`table.clear`).
    pub fn clear(&mut self) {
        let meta = self.meta;
        *self = Table::default();
        self.meta = meta;
    }

    pub fn bytes(&self) -> usize {
        TABLE_BASE_BYTES
            + self.array.capacity() * ARRAY_SLOT_BYTES
            + self.hash.capacity() * HASH_SLOT_BYTES
    }

    pub fn get(&self, key: Value) -> Value {
        if let Some(i) = array_index(key)
            && i <= self.array.len()
        {
            return self.array[i - 1];
        }
        match Key::from_value(key) {
            Ok(Some(k)) => self.hash.get(&k).copied().unwrap_or(Value::Nil),
            _ => Value::Nil,
        }
    }

    pub fn get_str(&self, key: StrRef) -> Value {
        self.hash
            .get(&Key::String(key))
            .copied()
            .unwrap_or(Value::Nil)
    }

    /// `t[key]` for a string key, trying the hash slot in `hint` first and
    /// updating it after a miss (see `rhue_bytecode::InlineCache`).
    #[inline]
    pub fn get_str_hinted(&self, key: StrRef, hint: &Cell<u32>) -> Value {
        let k = Key::String(key);
        if let Some((found, v)) = self.hash.get_index(hint.get() as usize)
            && *found == k
        {
            return *v;
        }
        match self.hash.get_index_of(&k) {
            Some(i) => {
                hint.set(i as u32);
                *self.hash.get_index(i).expect("index just found").1
            }
            None => Value::Nil,
        }
    }

    /// `t[key] = value` for a string key that is already in the hash part
    /// (a tombstone counts), using and updating `hint`. Returns `false`,
    /// changing nothing, when the key isn't there: the caller then does a
    /// full [`set`](Self::set), which may grow the table.
    #[inline]
    ///
    /// With a metatable, a key whose value is nil counts as absent too
    /// (assigning it may have to go to `__newindex`).
    pub fn set_existing_str(&mut self, key: StrRef, value: Value, hint: &Cell<u32>) -> bool {
        if self.frozen {
            // `set` reports it.
            return false;
        }
        let k = Key::String(key);
        let at = match self.hash.get_index(hint.get() as usize) {
            Some((found, _)) if *found == k => hint.get() as usize,
            _ => match self.hash.get_index_of(&k) {
                Some(i) => {
                    hint.set(i as u32);
                    i
                }
                None => return false,
            },
        };
        let has_meta = self.meta.is_some();
        let (_, slot) = self.hash.get_index_mut(at).expect("index just found");
        if has_meta && *slot == Value::Nil {
            return false;
        }
        match (*slot == Value::Nil, value == Value::Nil) {
            (false, true) => self.tombstones += 1,
            (true, false) => self.tombstones -= 1,
            _ => {}
        }
        *slot = value;
        true
    }

    pub fn get_int(&self, i: usize) -> Value {
        if i >= 1 && i <= self.array.len() {
            self.array[i - 1]
        } else {
            self.get(Value::Number(i as f64))
        }
    }

    /// `t[key] = value`. Nil and NaN keys are errors (assigning nil to them
    /// is too, as in Lua).
    #[inline]
    pub fn set(&mut self, key: Value, value: Value) -> Result<(), KeyError> {
        if self.frozen {
            return frozen();
        }
        if let Some(i) = array_index(key) {
            let n = self.array.len();
            if i <= n {
                self.array[i - 1] = value;
                if i == n && value == Value::Nil {
                    while self.array.last() == Some(&Value::Nil) {
                        self.array.pop();
                    }
                }
                return Ok(());
            }
            if i == n + 1 && value != Value::Nil {
                // It may also be in the hash part (set before the array
                // reached it); drop that copy.
                if let Some(k) = Key::from_value(key)?
                    && let Some(old) = self.hash.get_mut(&k)
                {
                    if *old != Value::Nil {
                        self.tombstones += 1;
                    }
                    *old = Value::Nil;
                }
                self.array.push(value);
                self.migrate_from_hash();
                return Ok(());
            }
        }
        let Some(k) = Key::from_value(key)? else {
            return Err(KeyError::Nil);
        };
        self.set_hash(k, value);
        Ok(())
    }

    fn set_hash(&mut self, k: Key, value: Value) {
        match self.hash.get_mut(&k) {
            Some(slot) => {
                match (*slot == Value::Nil, value == Value::Nil) {
                    (false, true) => self.tombstones += 1,
                    (true, false) => self.tombstones -= 1,
                    _ => {}
                }
                *slot = value;
            }
            None if value == Value::Nil => {}
            None => {
                if self.tombstones > 8 && self.tombstones * 2 > self.hash.len() {
                    self.hash.retain(|_, v| *v != Value::Nil);
                    self.tombstones = 0;
                }
                self.hash.insert_new(k, value);
            }
        }
    }

    /// `n` copies of `value` at `1..=n` (`table.create`).
    pub fn filled(n: usize, value: Value) -> Self {
        if value == Value::Nil {
            return Table::with_capacity(n, 0);
        }
        Table {
            array: vec![value; n],
            ..Table::default()
        }
    }

    /// `t[at + i] = values[i]` for each value, raw and in order, as
    /// `table.move` does. The part that lands in the array is one copy.
    pub fn set_run(&mut self, at: i64, values: &[Value]) -> Result<(), KeyError> {
        if self.frozen {
            return frozen();
        }
        let n = self.array.len();
        let inside = if at >= 1 && at as usize <= n {
            (n - at as usize + 1).min(values.len())
        } else {
            0
        };
        if inside > 0 {
            let start = at as usize - 1;
            self.array[start..start + inside].copy_from_slice(&values[..inside]);
        }
        if inside > 0 && at as usize - 1 + inside == n {
            // The copy reached the end: drop trailing nils, as `set` would.
            while self.array.last() == Some(&Value::Nil) {
                self.array.pop();
            }
        }
        for (i, &v) in values[inside..].iter().enumerate() {
            self.set(Value::Number((at + (inside + i) as i64) as f64), v)?;
        }
        Ok(())
    }

    /// `t[from..from + len]` (raw), from the array part when it's there.
    pub fn get_run(&self, from: i64, len: usize) -> Vec<Value> {
        if from >= 1 && from as usize - 1 + len <= self.array.len() {
            return self.array[from as usize - 1..][..len].to_vec();
        }
        (0..len)
            .map(|i| self.get(Value::Number((from + i as i64) as f64)))
            .collect()
    }

    /// After the array grew, pull `n + 1`, `n + 2`, ... out of the hash.
    fn migrate_from_hash(&mut self) {
        loop {
            let next = Value::Number((self.array.len() + 1) as f64);
            let Ok(Some(k)) = Key::from_value(next) else {
                return;
            };
            match self.hash.get_mut(&k) {
                Some(v) if *v != Value::Nil => {
                    let moved = std::mem::replace(v, Value::Nil);
                    self.tombstones += 1;
                    self.array.push(moved);
                }
                _ => return,
            }
        }
    }

    /// `#t`: a border (`t[n] ~= nil` and `t[n + 1] == nil`, or 0).
    pub fn len(&self) -> usize {
        let n = self.array.len();
        if n == 0 {
            // Keys can sit in the hash part only if `1` itself is missing.
            return 0;
        }
        if self.array[n - 1] != Value::Nil {
            return n;
        }
        // Binary search for a border inside the array: array[lo-1] is
        // non-nil (or lo = 0), array[hi-1] is nil.
        let (mut lo, mut hi) = (0usize, n);
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if self.array[mid - 1] == Value::Nil {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        lo
    }

    pub fn is_empty(&self) -> bool {
        self.array.iter().all(|v| *v == Value::Nil) && self.hash.values().all(|v| *v == Value::Nil)
    }

    /// Lua's `next`: the entry after `key` (nil = the first), or `None` at
    /// the end. `Err` if `key` isn't in the table.
    pub fn next(&self, key: Value) -> Result<Option<(Value, Value)>, InvalidKey> {
        let mut start_hash = 0;
        let array_from = match key {
            Value::Nil => Some(0),
            k => match array_index(k) {
                Some(i) if i <= self.array.len() => Some(i),
                _ => {
                    let Ok(Some(hk)) = Key::from_value(k) else {
                        return Err(InvalidKey);
                    };
                    start_hash = self.hash.get_index_of(&hk).ok_or(InvalidKey)? + 1;
                    None
                }
            },
        };
        if let Some(from) = array_from {
            for (i, v) in self.array.iter().enumerate().skip(from) {
                if *v != Value::Nil {
                    return Ok(Some((Value::Number((i + 1) as f64), *v)));
                }
            }
        }
        for i in start_hash..self.hash.len() {
            let (k, v) = self.hash.get_index(i).expect("index in range");
            if *v != Value::Nil {
                return Ok(Some((k.to_value(), *v)));
            }
        }
        Ok(None)
    }

    /// Every value the table holds (keys and values), for the GC.
    pub fn for_each_ref(&self, mut f: impl FnMut(Value)) {
        if let Some(m) = self.meta {
            f(Value::Table(m));
        }
        for v in &self.array {
            f(*v);
        }
        for (k, v) in self.hash.iter() {
            if *v != Value::Nil {
                f(k.to_value());
                f(*v);
            }
        }
    }

    /// Appends at `#t + 1` (`table.insert(t, v)`).
    pub fn push(&mut self, value: Value) -> Result<(), KeyError> {
        let n = self.len();
        self.set(Value::Number((n + 1) as f64), value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::HashMap;

    fn n(x: f64) -> Value {
        Value::Number(x)
    }

    #[test]
    fn sequential_inserts_stay_in_the_array() {
        let mut t = Table::default();
        for i in 1..=100 {
            t.set(n(i as f64), n(i as f64 * 10.0)).unwrap();
        }
        assert_eq!(t.array.len(), 100);
        assert_eq!(t.hash.len(), 0);
        assert_eq!(t.len(), 100);
        assert_eq!(t.get(n(42.0)), n(420.0));
    }

    #[test]
    fn out_of_order_inserts_migrate_into_the_array() {
        let mut t = Table::default();
        t.set(n(3.0), n(30.0)).unwrap();
        t.set(n(2.0), n(20.0)).unwrap();
        assert_eq!(t.len(), 0, "1 is missing");
        t.set(n(1.0), n(10.0)).unwrap();
        assert_eq!(t.array.len(), 3);
        assert_eq!(t.len(), 3);
    }

    #[test]
    fn negative_zero_and_zero_are_the_same_key() {
        let mut t = Table::default();
        t.set(n(0.0), n(1.0)).unwrap();
        assert_eq!(t.get(n(-0.0)), n(1.0));
    }

    #[test]
    fn nil_and_nan_keys() {
        let mut t = Table::default();
        assert_eq!(t.set(Value::Nil, n(1.0)), Err(KeyError::Nil));
        assert_eq!(t.set(n(f64::NAN), n(1.0)), Err(KeyError::NaN));
        assert_eq!(t.get(Value::Nil), Value::Nil);
    }

    #[test]
    fn clearing_during_traversal_is_safe() {
        let mut t = Table::default();
        for i in 0..20 {
            t.set(n(i as f64 + 0.5), n(1.0)).unwrap();
        }
        let mut key = Value::Nil;
        let mut seen = 0;
        while let Some((k, _)) = t.next(key).unwrap() {
            t.set(k, Value::Nil).unwrap();
            seen += 1;
            key = k;
        }
        assert_eq!(seen, 20);
        assert!(t.is_empty());
    }

    #[derive(Clone, Debug)]
    enum Op {
        Set(i8, Option<i8>),
        SetFloat(i8, Option<i8>),
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            (-3i8..40, prop::option::weighted(0.8, any::<i8>())).prop_map(|(k, v)| Op::Set(k, v)),
            (-3i8..40, prop::option::weighted(0.8, any::<i8>()))
                .prop_map(|(k, v)| Op::SetFloat(k, v)),
        ]
    }

    proptest! {
        /// The table behaves like a plain map from keys to non-nil values:
        /// get agrees, `next` visits exactly the live entries once, and `#`
        /// is a valid border.
        #[test]
        fn behaves_like_a_map(ops in prop::collection::vec(op(), 0..200)) {
            let mut t = Table::default();
            let mut model: HashMap<u64, f64> = HashMap::new();
            for o in &ops {
                let (k, v) = match *o {
                    Op::Set(k, v) => (k as f64, v),
                    Op::SetFloat(k, v) => (k as f64 + 0.5, v),
                };
                let val = v.map_or(Value::Nil, |v| n(v as f64));
                t.set(n(k), val).unwrap();
                match v {
                    Some(v) => { model.insert(k.to_bits(), v as f64); }
                    None => { model.remove(&k.to_bits()); }
                }
            }
            for (&bits, &v) in &model {
                prop_assert_eq!(t.get(n(f64::from_bits(bits))), n(v));
            }
            let mut seen = HashMap::new();
            let mut key = Value::Nil;
            while let Some((k, v)) = t.next(key).unwrap() {
                let Value::Number(kn) = k else { panic!() };
                prop_assert!(seen.insert(kn.to_bits(), v).is_none(), "key visited twice");
                key = k;
            }
            prop_assert_eq!(seen.len(), model.len());
            let border = t.len();
            prop_assert!(border == 0 || model.contains_key(&(border as f64).to_bits()));
            prop_assert!(!model.contains_key(&((border + 1) as f64).to_bits()));
        }
    }
}
