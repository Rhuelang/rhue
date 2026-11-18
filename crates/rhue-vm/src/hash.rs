//! A fast hasher for keys that are VM handles and number bits.
//!
//! Table keys and global names are small fixed-size values (a handle's
//! index and generation, or an f64's bits), so SipHash's resistance to
//! chosen byte strings buys little and costs a lot on every `t.x` and
//! global lookup. This is the multiply-rotate "Fx" hash used by rustc.
//! Like Lua's own number hashing it is deterministic, so a script that
//! crafts colliding number keys can slow its own tables down; string
//! *contents* (the intern map) still use SipHash.

use std::hash::{BuildHasher, Hasher};

#[derive(Clone, Copy, Debug, Default)]
pub struct FxBuildHasher;

impl BuildHasher for FxBuildHasher {
    type Hasher = FxHasher;

    #[inline]
    fn build_hasher(&self) -> FxHasher {
        FxHasher { hash: 0 }
    }
}

pub struct FxHasher {
    hash: u64,
}

const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

impl FxHasher {
    #[inline]
    fn add(&mut self, word: u64) {
        self.hash = (self.hash.rotate_left(5) ^ word).wrapping_mul(SEED);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut word = [0u8; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            self.add(u64::from_le_bytes(word));
        }
    }

    #[inline]
    fn write_u8(&mut self, n: u8) {
        self.add(n as u64);
    }

    #[inline]
    fn write_u32(&mut self, n: u32) {
        self.add(n as u64);
    }

    #[inline]
    fn write_u64(&mut self, n: u64) {
        self.add(n);
    }

    #[inline]
    fn write_usize(&mut self, n: usize) {
        self.add(n as u64);
    }

    #[inline]
    fn write_isize(&mut self, n: isize) {
        self.add(n as u64);
    }

    #[inline]
    fn finish(&self) -> u64 {
        // Fold the high bits down: hash tables index with the low bits,
        // and the multiply pushes most of the mixing upwards.
        self.hash ^ (self.hash >> 29)
    }
}

pub type FxHashMap<K, V> = std::collections::HashMap<K, V, FxBuildHasher>;
pub type FxIndexMap<K, V> = indexmap::IndexMap<K, V, FxBuildHasher>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::Hash;

    fn hash(v: impl Hash) -> u64 {
        FxBuildHasher.hash_one(v)
    }

    #[test]
    fn deterministic_and_spread() {
        assert_eq!(hash(42u64), hash(42u64));
        // Consecutive handles land in different low-bit buckets.
        let buckets: std::collections::HashSet<u64> = (0u32..64).map(|i| hash(i) & 63).collect();
        assert!(buckets.len() > 40, "{}", buckets.len());
    }

    #[test]
    fn byte_writes_cover_partial_words() {
        assert_ne!(hash("abc"), hash("abd"));
        assert_ne!(hash([1u8; 9]), hash([1u8; 8]));
    }
}
