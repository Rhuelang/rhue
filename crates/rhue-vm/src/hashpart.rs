//! A table's hash part, insertion-ordered and indexable by position.
//!
//! Most tables with string keys are small (objects with a handful of fields),
//! and for those hashing costs more than it saves. Up to [`SMALL_MAX`] entries
//! live in a plain vector searched linearly, one more and the part turns into
//! an [`FxIndexMap`]. Both keep insertion order and give every entry a stable
//! position, which `next` and the VM's inline caches rely on (a position only
//! changes when the table compacts its tombstones).

use crate::Value;
use crate::hash::FxIndexMap;
use crate::table::Key;

/// The most entries kept in the linear form.
pub const SMALL_MAX: usize = 8;

#[derive(Debug, Clone)]
pub enum HashPart {
    Small(Vec<(Key, Value)>),
    Large(FxIndexMap<Key, Value>),
}

impl Default for HashPart {
    fn default() -> Self {
        HashPart::Small(Vec::new())
    }
}

impl HashPart {
    pub fn with_capacity(n: usize) -> Self {
        if n <= SMALL_MAX {
            HashPart::Small(Vec::with_capacity(n))
        } else {
            HashPart::Large(FxIndexMap::with_capacity_and_hasher(n, Default::default()))
        }
    }

    pub fn len(&self) -> usize {
        match self {
            HashPart::Small(v) => v.len(),
            HashPart::Large(m) => m.len(),
        }
    }

    pub fn capacity(&self) -> usize {
        match self {
            HashPart::Small(v) => v.capacity(),
            HashPart::Large(m) => m.capacity(),
        }
    }

    #[inline]
    pub fn get_index_of(&self, k: &Key) -> Option<usize> {
        match self {
            HashPart::Small(v) => v.iter().position(|(key, _)| key == k),
            HashPart::Large(m) => m.get_index_of(k),
        }
    }

    #[inline]
    pub fn get(&self, k: &Key) -> Option<&Value> {
        match self {
            HashPart::Small(v) => v.iter().find(|(key, _)| key == k).map(|(_, v)| v),
            HashPart::Large(m) => m.get(k),
        }
    }

    #[inline]
    pub fn get_mut(&mut self, k: &Key) -> Option<&mut Value> {
        match self {
            HashPart::Small(v) => v.iter_mut().find(|(key, _)| key == k).map(|(_, v)| v),
            HashPart::Large(m) => m.get_mut(k),
        }
    }

    /// The entry at position `i`.
    #[inline]
    pub fn get_index(&self, i: usize) -> Option<(&Key, &Value)> {
        match self {
            HashPart::Small(v) => v.get(i).map(|(k, v)| (k, v)),
            HashPart::Large(m) => m.get_index(i),
        }
    }

    #[inline]
    pub fn get_index_mut(&mut self, i: usize) -> Option<(&Key, &mut Value)> {
        match self {
            HashPart::Small(v) => v.get_mut(i).map(|(k, v)| (&*k, v)),
            HashPart::Large(m) => m.get_index_mut(i),
        }
    }

    /// A part holding `keys` (all different) with nil values, with room for
    /// `capacity` entries.
    pub fn with_nil_keys(capacity: usize, keys: impl ExactSizeIterator<Item = Key>) -> Self {
        let capacity = capacity.max(keys.len());
        if capacity <= SMALL_MAX {
            let mut v = Vec::with_capacity(capacity);
            v.extend(keys.map(|k| (k, Value::Nil)));
            HashPart::Small(v)
        } else {
            let mut m = FxIndexMap::with_capacity_and_hasher(capacity, Default::default());
            m.extend(keys.map(|k| (k, Value::Nil)));
            HashPart::Large(m)
        }
    }

    /// Adds a key that isn't there yet, at the end.
    pub fn insert_new(&mut self, k: Key, value: Value) {
        match self {
            HashPart::Small(v) if v.len() < SMALL_MAX => v.push((k, value)),
            HashPart::Small(v) => {
                let mut m = FxIndexMap::with_capacity_and_hasher(v.len() * 2, Default::default());
                m.extend(v.drain(..));
                m.insert(k, value);
                *self = HashPart::Large(m);
            }
            HashPart::Large(m) => {
                m.insert(k, value);
            }
        }
    }

    pub fn retain(&mut self, mut keep: impl FnMut(&Key, &Value) -> bool) {
        match self {
            HashPart::Small(v) => v.retain(|(k, v)| keep(k, v)),
            HashPart::Large(m) => m.retain(|k, v| keep(k, v)),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Key, &Value)> {
        (0..self.len()).map(|i| self.get_index(i).expect("index in range"))
    }

    pub fn values(&self) -> impl Iterator<Item = &Value> {
        self.iter().map(|(_, v)| v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(n: u64) -> Key {
        Key::Number((n as f64).to_bits())
    }

    #[test]
    #[expect(clippy::cast_possible_truncation, reason = "test indices are small")]
    fn growing_past_the_small_limit_keeps_order_and_positions() {
        let mut h = HashPart::default();
        for i in 0..20u64 {
            assert!(h.get(&k(i)).is_none());
            h.insert_new(k(i), Value::Number(i as f64));
            assert_eq!(matches!(h, HashPart::Small(_)), i < SMALL_MAX as u64);
        }
        for i in 0..20u64 {
            assert_eq!(h.get_index_of(&k(i)), Some(i as usize));
            assert_eq!(h.get_index(i as usize).map(|(key, _)| *key), Some(k(i)));
            assert_eq!(h.get(&k(i)), Some(&Value::Number(i as f64)));
        }
        h.retain(|_, v| *v != Value::Number(3.0));
        assert_eq!(h.len(), 19);
        assert_eq!(h.get_index_of(&k(4)), Some(3));
    }
}
