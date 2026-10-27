//! The latched map's operations whose compiled code `check.py` checks, each
//! out of line under a symbol of its own. Each probe holds what its
//! operation inlines: the descent, the leaf's read, the walk that settles a
//! tie, and the latched way back.

use bbtree::BBTreeMap;

/// A lookup that copies the leaf's entry unlatched, for keys compared as
/// copies.
#[unsafe(no_mangle)]
#[inline(never)]
pub fn probe_get_u64(map: &BBTreeMap<u64, u64>, key: u64) -> Option<u64> {
    map.get_copied(&key)
}

/// A lookup of keys that aren't compared as copies.
#[unsafe(no_mangle)]
#[inline(never)]
pub fn probe_get_string(map: &BBTreeMap<String, u64>, key: &str) -> Option<u64> {
    map.get_copied(key)
}

/// Lookups in lanes (doc 12).
#[unsafe(no_mangle)]
#[inline(never)]
pub fn probe_get_many_u64(map: &BBTreeMap<u64, u64>, keys: &[u64; 16]) -> [Option<u64>; 16] {
    map.get_many_copied(core::array::from_fn(|lane| &keys[lane]))
}

/// An insert: the descent, then the leaf latched exclusively.
#[unsafe(no_mangle)]
#[inline(never)]
pub fn probe_insert_u64(map: &BBTreeMap<u64, u64>, key: u64, value: u64) -> bool {
    matches!(map.insert(key, value), Ok(None))
}
