//! The maps a kernel would use, each operation in a function of its own, so
//! that building the crate builds every search the maps pick for the target:
//! on `x86_64-unknown-none`, those without SSE.

#![no_std]

extern crate alloc;

use alloc::boxed::Box;

use bbtree::{Auto, BBTreeMap, Global, unsync};

/// How many of `keys` an unsync map of them holds, looked up one at a time
/// and in a batch, and how many lie in a range.
pub fn integer_keys(keys: &[u64]) -> usize {
    let mut map = unsync::BBTreeMap::new();
    for &key in keys {
        map.insert(key, key);
    }
    let _ = map.try_insert(u64::MAX, 0);
    let found = keys.iter().filter(|&key| map.get(key).is_some()).count();
    let batch = map.get_many([&1, &2, &3, &4]).iter().flatten().count();
    let ranged = map.range(3..9).count();
    map.remove(&3);
    found + batch + ranged
}

/// How many of `keys` an unsync map of byte strings holds.
pub fn byte_string_keys(keys: &[&[u8]]) -> usize {
    let mut map: unsync::BBTreeMap<Box<[u8]>, u64> = unsync::BBTreeMap::new();
    for &key in keys {
        map.insert(key.into(), 1);
    }
    keys.iter().filter(|&&key| map.get(key).is_some()).count()
}

/// How many entries a map built from `sorted`, on the global allocator,
/// holds.
pub fn sorted_entries(sorted: &[(u64, u64)]) -> usize {
    unsync::BBTreeMap::from_sorted_in(sorted.iter().copied(), Auto, Global).len()
}

/// How many of `keys` the shared map of them holds, looked up one at a time
/// and in a batch.
pub fn shared_integer_keys(keys: &[u64]) -> usize {
    let map = BBTreeMap::new();
    for &key in keys {
        let _ = map.insert(key, key);
    }
    let found = keys
        .iter()
        .filter(|&key| map.get_copied(key).is_some())
        .count();
    found + map.get_many_copied([&1, &2]).iter().flatten().count()
}
