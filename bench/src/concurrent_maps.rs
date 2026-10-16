//! The maps threads share that `bbtree-scaling` measures (`scaling.rs`), each
//! as its crate ships it: `BCTreeMap`; `BBTreeMap` behind a `RwLock`, the way
//! to share it without one; other crates' concurrent ordered maps:
//! crossbeam's `SkipMap`, scc's `TreeIndex`, surrealdb's artmap and
//! concurrent-map; and DashMap, a hash map, which keeps no order and so
//! scans nothing. Values are `u64`; keys any [`ScalingKey`].

use std::hash::Hash;
use std::ops::{Bound, ControlFlow};
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use artmap::{ArtMap, AsBytes};
use bbtree::{BBTreeMap, BCTreeMap};
use concurrent_map::{ConcurrentMap, Minimum};
use crossbeam_skiplist::SkipMap;
use dashmap::DashMap;
use scc::{Guard, TreeIndex};

/// A key the maps are measured with: `u64`, `u128` or `String`.
pub(crate) trait ScalingKey: Ord + Hash + Clone + Minimum + Send + Sync + 'static {
    /// The key as artmap keeps it: bytes in the key's order, big-endian
    /// for an integer.
    type ArtKey: AsBytes + Send + Sync + 'static;

    fn into_art_key(self) -> Self::ArtKey;

    /// Runs `f` on the bytes of [`Self::into_art_key`], without making it.
    fn with_art_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R;
}

impl ScalingKey for u64 {
    type ArtKey = [u8; 8];

    fn into_art_key(self) -> [u8; 8] {
        self.to_be_bytes()
    }

    fn with_art_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        f(&self.to_be_bytes())
    }
}

impl ScalingKey for u128 {
    type ArtKey = [u8; 16];

    fn into_art_key(self) -> [u8; 16] {
        self.to_be_bytes()
    }

    fn with_art_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        f(&self.to_be_bytes())
    }
}

impl ScalingKey for String {
    type ArtKey = String;

    fn into_art_key(self) -> String {
        self
    }

    fn with_art_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        f(self.as_bytes())
    }
}

/// A map from `K` to `u64` that threads share, each worker through a handle
/// of its own: a reference for most, a clone for a map whose reclamation
/// keeps state per handle.
pub(crate) trait SharedMap<K>: Sized {
    const NAME: &'static str;

    /// Whether it keeps its keys in order, to scan a range of them.
    const ORDERED: bool = true;

    /// What one worker reads and writes the map through.
    type Handle<'m>: MapHandle<K> + Send
    where
        Self: 'm;

    fn new() -> Self;

    fn handle(&self) -> Self::Handle<'_>;
}

/// One worker's way into a shared map.
pub(crate) trait MapHandle<K> {
    fn insert(&self, key: K, value: u64);

    fn get(&self, key: &K) -> Option<u64>;

    /// The values of `keys`, in one batch where the map has a batch API,
    /// by separate lookups otherwise.
    fn get_batch<const N: usize>(&self, keys: [&K; N]) -> [Option<u64>; N] {
        keys.map(|key| self.get(key))
    }

    /// Adds one to the value of `key`, if the map holds it.
    fn bump(&self, key: &K);

    /// Removes `key`'s entry; whether the map held one.
    fn remove(&self, key: &K) -> bool;

    /// The first `count` entries from `start` on, in key order: how many
    /// there were, and their values added up.
    fn scan(&self, start: &K, count: usize) -> (usize, u64);
}

impl<K, T: MapHandle<K>> MapHandle<K> for &T {
    fn insert(&self, key: K, value: u64) {
        (**self).insert(key, value);
    }

    fn get(&self, key: &K) -> Option<u64> {
        (**self).get(key)
    }

    fn get_batch<const N: usize>(&self, keys: [&K; N]) -> [Option<u64>; N] {
        (**self).get_batch(keys)
    }

    fn bump(&self, key: &K) {
        (**self).bump(key);
    }

    fn remove(&self, key: &K) -> bool {
        (**self).remove(key)
    }

    fn scan(&self, start: &K, count: usize) -> (usize, u64) {
        (**self).scan(start, count)
    }
}

/// Implements [`SharedMap`] for a map that threads share by reference,
/// named `$name`, for every [`ScalingKey`].
macro_rules! shared_by_reference {
    ($map:ty, $name:literal, $new:expr) => {
        impl<K: ScalingKey> SharedMap<K> for $map {
            const NAME: &'static str = $name;

            type Handle<'m> = &'m Self;

            fn new() -> Self {
                $new
            }

            fn handle(&self) -> &Self {
                self
            }
        }
    };
}

/// How many of `entries` there are, up to `count`, and their values added
/// up.
fn sum_of_first(entries: impl Iterator<Item = u64>, count: usize) -> (usize, u64) {
    (entries.take(count)).fold((0, 0), |(visited, sum), value| {
        (visited + 1, sum.wrapping_add(value))
    })
}

shared_by_reference!(BCTreeMap<K, u64>, "bctree", BCTreeMap::new());

impl<K: ScalingKey> MapHandle<K> for BCTreeMap<K, u64> {
    fn insert(&self, key: K, value: u64) {
        BCTreeMap::insert(self, key, value);
    }

    fn get(&self, key: &K) -> Option<u64> {
        self.read(key, |_, &value| value)
    }

    fn get_batch<const N: usize>(&self, keys: [&K; N]) -> [Option<u64>; N] {
        self.read_many(keys, |_, &value| value)
    }

    fn bump(&self, key: &K) {
        self.update(key, |_, value| *value += 1);
    }

    fn remove(&self, key: &K) -> bool {
        BCTreeMap::remove(self, key).is_some()
    }

    fn scan(&self, start: &K, count: usize) -> (usize, u64) {
        let (mut visited, mut sum) = (0, 0_u64);
        BCTreeMap::scan(self, start.., |_, &value| {
            sum = sum.wrapping_add(value);
            visited += 1;
            if visited == count {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        });
        (visited, sum)
    }
}

shared_by_reference!(
    RwLock<BBTreeMap<K, u64>>,
    "bbtree-rwlock",
    RwLock::new(BBTreeMap::new())
);

impl<K: ScalingKey> MapHandle<K> for RwLock<BBTreeMap<K, u64>> {
    fn insert(&self, key: K, value: u64) {
        self.write().expect("no thread panics").insert(key, value);
    }

    fn get_batch<const N: usize>(&self, keys: [&K; N]) -> [Option<u64>; N] {
        let map = self.read().expect("no thread panics");
        map.get_many(keys).map(Option::<&u64>::copied)
    }

    fn get(&self, key: &K) -> Option<u64> {
        self.read().expect("no thread panics").get(key).copied()
    }

    fn bump(&self, key: &K) {
        if let Some(value) = self.write().expect("no thread panics").get_mut(key) {
            *value += 1;
        }
    }

    fn remove(&self, key: &K) -> bool {
        self.write()
            .expect("no thread panics")
            .remove(key)
            .is_some()
    }

    fn scan(&self, start: &K, count: usize) -> (usize, u64) {
        let map = self.read().expect("no thread panics");
        sum_of_first(map.range(start..).map(|(_, &value)| value), count)
    }
}

// crossbeam's lock-free skiplist. It hands out values shared only, and
// replaces an entry by removing it and inserting another, which a lookup
// meanwhile misses: values are atomics, which an update adds to.
shared_by_reference!(
    SkipMap<K, AtomicU64>,
    "crossbeam-skipmap",
    SkipMap::new()
);

impl<K: ScalingKey> MapHandle<K> for SkipMap<K, AtomicU64> {
    fn insert(&self, key: K, value: u64) {
        SkipMap::insert(self, key, AtomicU64::new(value));
    }

    fn get(&self, key: &K) -> Option<u64> {
        SkipMap::get(self, key).map(|entry| entry.value().load(Ordering::Relaxed))
    }

    fn bump(&self, key: &K) {
        if let Some(entry) = SkipMap::get(self, key) {
            entry.value().fetch_add(1, Ordering::Relaxed);
        }
    }

    fn remove(&self, key: &K) -> bool {
        SkipMap::remove(self, key).is_some()
    }

    fn scan(&self, start: &K, count: usize) -> (usize, u64) {
        let entries = self
            .range(start..)
            .map(|entry| entry.value().load(Ordering::Relaxed));
        sum_of_first(entries, count)
    }
}

// scc's B+-tree, read-optimized, which hands out values shared only:
// values are atomics, as for the skiplist. Lookups peek, which takes no
// lock; updates read, which sees every change to a value.
shared_by_reference!(
    TreeIndex<K, AtomicU64>,
    "scc-treeindex",
    TreeIndex::new()
);

impl<K: ScalingKey> MapHandle<K> for TreeIndex<K, AtomicU64> {
    fn insert(&self, key: K, value: u64) {
        self.upsert_sync(key, AtomicU64::new(value));
    }

    fn get(&self, key: &K) -> Option<u64> {
        self.peek_with(key, |_, value| value.load(Ordering::Relaxed))
    }

    fn bump(&self, key: &K) {
        self.read_sync(key, |_, value| value.fetch_add(1, Ordering::Relaxed));
    }

    fn remove(&self, key: &K) -> bool {
        self.remove_sync(key)
    }

    fn scan(&self, start: &K, count: usize) -> (usize, u64) {
        let guard = Guard::new();
        let entries = self
            .range::<K, _>(start.., &guard)
            .map(|(_, value)| value.load(Ordering::Relaxed));
        sum_of_first(entries, count)
    }
}

// surrealdb's artmap, an adaptive radix tree with optimistic lock coupling
// and epoch-based reclamation, which orders keys by their bytes: integers
// go in big-endian. Each call pins an epoch, as a call that keeps no guard.
// It hands out values shared only: values are atomics, as for the skiplist.

/// artmap's map, keyed by the bytes of a [`ScalingKey`].
pub(crate) struct ArtMapOf<K: ScalingKey>(ArtMap<K::ArtKey, AtomicU64>);

impl<K: ScalingKey> SharedMap<K> for ArtMapOf<K> {
    const NAME: &'static str = "artmap";

    type Handle<'m> = &'m Self;

    fn new() -> Self {
        Self(ArtMap::new())
    }

    fn handle(&self) -> &Self {
        self
    }
}

impl<K: ScalingKey> MapHandle<K> for ArtMapOf<K> {
    fn insert(&self, key: K, value: u64) {
        self.0.insert(key.into_art_key(), AtomicU64::new(value));
    }

    fn get(&self, key: &K) -> Option<u64> {
        key.with_art_bytes(|bytes| {
            (self.0.get_by_slice(bytes)).map(|entry| entry.value().load(Ordering::Relaxed))
        })
    }

    fn bump(&self, key: &K) {
        key.with_art_bytes(|bytes| {
            if let Some(entry) = self.0.get_by_slice(bytes) {
                entry.value().fetch_add(1, Ordering::Relaxed);
            }
        });
    }

    fn remove(&self, key: &K) -> bool {
        key.with_art_bytes(|bytes| self.0.remove_by_slice(bytes).is_some())
    }

    fn scan(&self, start: &K, count: usize) -> (usize, u64) {
        let (mut visited, mut sum) = (0, 0_u64);
        start.with_art_bytes(|bytes| {
            let from = (Bound::Included(bytes), Bound::Unbounded);
            self.0.scan::<_, [u8], _>(from, |_, value| {
                sum = sum.wrapping_add(value.load(Ordering::Relaxed));
                visited += 1;
                visited < count
            });
        });
        (visited, sum)
    }
}

// sled's lock-free B+-tree. Its reclamation keeps state per handle, which
// isn't `Sync`: each worker clones the map.
impl<K: ScalingKey> SharedMap<K> for ConcurrentMap<K, u64> {
    const NAME: &'static str = "concurrent-map";

    type Handle<'m> = Self;

    fn new() -> Self {
        ConcurrentMap::default()
    }

    fn handle(&self) -> Self {
        self.clone()
    }
}

impl<K: ScalingKey> MapHandle<K> for ConcurrentMap<K, u64> {
    fn insert(&self, key: K, value: u64) {
        ConcurrentMap::insert(self, key, value);
    }

    fn get(&self, key: &K) -> Option<u64> {
        ConcurrentMap::get(self, key)
    }

    fn bump(&self, key: &K) {
        self.fetch_and_update(key.clone(), |value| value.map(|value| value + 1));
    }

    fn remove(&self, key: &K) -> bool {
        ConcurrentMap::remove(self, key).is_some()
    }

    fn scan(&self, start: &K, count: usize) -> (usize, u64) {
        sum_of_first(self.range(start..).map(|(_, value)| value), count)
    }
}

// DashMap, a hash map in shards, each behind a reader-writer lock, with its
// default hasher. It keeps no order.
impl<K: ScalingKey> SharedMap<K> for DashMap<K, u64> {
    const NAME: &'static str = "dashmap";
    const ORDERED: bool = false;

    type Handle<'m> = &'m Self;

    fn new() -> Self {
        DashMap::new()
    }

    fn handle(&self) -> &Self {
        self
    }
}

impl<K: ScalingKey> MapHandle<K> for DashMap<K, u64> {
    fn insert(&self, key: K, value: u64) {
        DashMap::insert(self, key, value);
    }

    fn get(&self, key: &K) -> Option<u64> {
        DashMap::get(self, key).map(|value| *value)
    }

    fn bump(&self, key: &K) {
        if let Some(mut value) = self.get_mut(key) {
            *value += 1;
        }
    }

    fn remove(&self, key: &K) -> bool {
        DashMap::remove(self, key).is_some()
    }

    fn scan(&self, _start: &K, _count: usize) -> (usize, u64) {
        unreachable!("a hash map keeps no order to scan")
    }
}
