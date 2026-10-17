//! The maps that `bbtree-scaling` measures (`scaling.rs`), each as its crate
//! ships it. Maps threads share: `BCTreeMap`; `BBTreeMap` and std's
//! `BTreeMap` behind a `RwLock`, the way to share them without one; other
//! crates' concurrent ordered maps: crossbeam's `SkipMap`, scc's
//! `TreeIndex`, surrealdb's artmap and
//! concurrent-map; and DashMap, a hash map, which keeps no order and so
//! scans nothing. Maps one thread owns, measured at one thread: `BBTreeMap`
//! and std's `BTreeMap` as they are used without threads (`Owned`), and
//! std's `HashMap` beside them, which keeps no order. Values are `u64`; keys
//! any [`ScalingKey`].

use std::cell::{Cell, UnsafeCell};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::hash::Hash;
use std::marker::PhantomData;
use std::ops::{Bound, ControlFlow};
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use artmap::{ArtMap, AsBytes};
use bbtree::{BBTreeMap, BCTreeMap};
use concurrent_map::{ConcurrentMap, Minimum};
use crossbeam_skiplist::SkipMap;
use dashmap::DashMap;
use scc::{Guard, TreeIndex};

/// A key the maps are measured with: `u64`, `u128`, `[u8; 16]` or `String`.
pub(crate) trait ScalingKey: Ord + Hash + Clone + Minimum + Send + Sync + 'static {
    /// The key as artmap keeps it: bytes in the key's order, big-endian
    /// for an integer.
    type ArtKey: AsBytes + Send + Sync + 'static;

    fn into_art_key(self) -> Self::ArtKey;

    /// Runs `f` on the bytes of [`Self::into_art_key`], without making it.
    fn with_art_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R;

    /// Runs `f` on [`Self::into_art_key`]'s key, without allocating: the
    /// key itself, or its bytes on the stack.
    fn with_art_key<R>(&self, f: impl FnOnce(&Self::ArtKey) -> R) -> R;

    /// Appends the key to a key dump (`--dump-keys`) as a line of its own:
    /// an integer in decimal, a UUID in hex, a string as it is.
    fn dump_line(&self, dump: &mut String);
}

impl ScalingKey for u64 {
    type ArtKey = [u8; 8];

    fn into_art_key(self) -> [u8; 8] {
        self.to_be_bytes()
    }

    fn with_art_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        f(&self.to_be_bytes())
    }

    fn with_art_key<R>(&self, f: impl FnOnce(&Self::ArtKey) -> R) -> R {
        f(&self.to_be_bytes())
    }

    fn dump_line(&self, dump: &mut String) {
        let _ = writeln!(dump, "{self}");
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

    fn with_art_key<R>(&self, f: impl FnOnce(&Self::ArtKey) -> R) -> R {
        f(&self.to_be_bytes())
    }

    fn dump_line(&self, dump: &mut String) {
        let _ = writeln!(dump, "{self}");
    }
}

impl ScalingKey for [u8; 16] {
    type ArtKey = [u8; 16];

    fn into_art_key(self) -> [u8; 16] {
        self
    }

    fn with_art_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        f(self)
    }

    fn with_art_key<R>(&self, f: impl FnOnce(&Self::ArtKey) -> R) -> R {
        f(self)
    }

    fn dump_line(&self, dump: &mut String) {
        for byte in self {
            let _ = write!(dump, "{byte:02x}");
        }
        dump.push('\n');
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

    fn with_art_key<R>(&self, f: impl FnOnce(&Self::ArtKey) -> R) -> R {
        f(self)
    }

    fn dump_line(&self, dump: &mut String) {
        assert!(
            !self.contains('\n'),
            "a key with a newline can't go in a dump"
        );
        dump.push_str(self);
        dump.push('\n');
    }
}

/// A map from `K` to `u64` that threads share, each worker through a handle
/// of its own: a reference for most, a clone for a map whose reclamation
/// keeps state per handle; or that one worker owns ([`Owned`]).
pub(crate) trait SharedMap<K>: Sized {
    const NAME: &'static str;

    /// Whether it keeps its keys in order, to scan a range of them.
    const ORDERED: bool = true;

    /// Whether threads share it. A map one thread owns runs at one thread
    /// only, and that thread builds it.
    const SHARED: bool = true;

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

    /// Inserts `entries`, in one batch where the map has a batch API, by
    /// separate inserts otherwise.
    fn insert_batch<const N: usize>(&self, entries: [(K, u64); N]) {
        for (key, value) in entries {
            self.insert(key, value);
        }
    }

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

    fn insert_batch<const N: usize>(&self, entries: [(K, u64); N]) {
        (**self).insert_batch(entries);
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

    fn insert_batch<const N: usize>(&self, entries: [(K, u64); N]) {
        self.write().expect("no thread panics").insert_many(entries);
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

    /// One guard for the batch, scc's way to share its cost.
    fn get_batch<const N: usize>(&self, keys: [&K; N]) -> [Option<u64>; N] {
        let guard = Guard::new();
        keys.map(|key| (self.peek(key, &guard)).map(|value| value.load(Ordering::Relaxed)))
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
// go in big-endian. Each call pins an epoch, as a call that keeps no guard;
// a batch pins once.
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

    /// One pin for the batch, artmap's way to share its cost.
    fn get_batch<const N: usize>(&self, keys: [&K; N]) -> [Option<u64>; N] {
        let guard = self.0.pin();
        keys.map(|key| {
            key.with_art_key(|key| {
                (self.0.get_with_guard(key, &guard))
                    .map(|entry| entry.value().load(Ordering::Relaxed))
            })
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

// std's B-tree behind a reader-writer lock, as `BBTreeMap` above: the way to
// share a map that has no concurrent version.
shared_by_reference!(
    RwLock<BTreeMap<K, u64>>,
    "std-btree-rwlock",
    RwLock::new(BTreeMap::new())
);

impl<K: ScalingKey> MapHandle<K> for RwLock<BTreeMap<K, u64>> {
    fn insert(&self, key: K, value: u64) {
        self.write().expect("no thread panics").insert(key, value);
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

/// A map one worker owns, as a map is used without threads: no lock, no
/// atomics. It hands out one handle at a time, which is then the only way
/// into the map, and the harness runs it at one thread.
pub(crate) struct Owned<M> {
    map: UnsafeCell<M>,
    handed_out: AtomicBool,
}

// SAFETY: the map is reached only through an `OwnedHandle`, of which at most
// one exists at a time (`only_handle` panics otherwise), and a handle isn't
// `Sync`: one thread at a time reaches the map, as if it owned it, which a
// `Send` map allows.
unsafe impl<M: Send> Sync for Owned<M> {}

impl<M> Owned<M> {
    fn new(map: M) -> Self {
        Self {
            map: UnsafeCell::new(map),
            handed_out: AtomicBool::new(false),
        }
    }

    /// The map's one handle, while no other is out.
    fn only_handle(&self) -> OwnedHandle<'_, M> {
        let other_out = self.handed_out.swap(true, Ordering::Acquire);
        assert!(!other_out, "a map one thread owns has one handle at a time");
        OwnedHandle {
            owner: self,
            one_thread: PhantomData,
        }
    }
}

/// The one way into an [`Owned`] map while it lives.
pub(crate) struct OwnedHandle<'m, M> {
    owner: &'m Owned<M>,
    /// Keeps the handle from being `Sync`: one thread uses it.
    one_thread: PhantomData<Cell<()>>,
}

// SAFETY: the handle is the map's only way in while it lives, so sending it
// moves the map's use to another thread, as sending the map would.
unsafe impl<M: Send> Send for OwnedHandle<'_, M> {}

impl<M> OwnedHandle<'_, M> {
    /// Runs `f` on the map.
    #[inline(always)]
    fn with<R>(&self, f: impl FnOnce(&mut M) -> R) -> R {
        // SAFETY: this handle is the only one (`Owned::only_handle`), it
        // isn't `Sync`, and nothing `f` gets calls back into it: no other
        // reference to the map exists while `f` runs.
        f(unsafe { &mut *self.owner.map.get() })
    }
}

impl<M> Drop for OwnedHandle<'_, M> {
    fn drop(&mut self) {
        self.owner.handed_out.store(false, Ordering::Release);
    }
}

impl<K: ScalingKey> SharedMap<K> for Owned<BBTreeMap<K, u64>> {
    const NAME: &'static str = "bbtree";
    const SHARED: bool = false;

    type Handle<'m> = OwnedHandle<'m, BBTreeMap<K, u64>>;

    fn new() -> Self {
        Owned::new(BBTreeMap::new())
    }

    fn handle(&self) -> Self::Handle<'_> {
        self.only_handle()
    }
}

impl<K: ScalingKey> MapHandle<K> for OwnedHandle<'_, BBTreeMap<K, u64>> {
    fn insert(&self, key: K, value: u64) {
        self.with(|map| map.insert(key, value));
    }

    fn insert_batch<const N: usize>(&self, entries: [(K, u64); N]) {
        self.with(|map| map.insert_many(entries));
    }

    fn get(&self, key: &K) -> Option<u64> {
        self.with(|map| map.get(key).copied())
    }

    fn get_batch<const N: usize>(&self, keys: [&K; N]) -> [Option<u64>; N] {
        self.with(|map| map.get_many(keys).map(Option::<&u64>::copied))
    }

    fn bump(&self, key: &K) {
        self.with(|map| {
            if let Some(value) = map.get_mut(key) {
                *value += 1;
            }
        });
    }

    fn remove(&self, key: &K) -> bool {
        self.with(|map| map.remove(key).is_some())
    }

    fn scan(&self, start: &K, count: usize) -> (usize, u64) {
        self.with(|map| sum_of_first(map.range(start..).map(|(_, &value)| value), count))
    }
}

impl<K: ScalingKey> SharedMap<K> for Owned<BTreeMap<K, u64>> {
    const NAME: &'static str = "std-btree";
    const SHARED: bool = false;

    type Handle<'m> = OwnedHandle<'m, BTreeMap<K, u64>>;

    fn new() -> Self {
        Owned::new(BTreeMap::new())
    }

    fn handle(&self) -> Self::Handle<'_> {
        self.only_handle()
    }
}

impl<K: ScalingKey> MapHandle<K> for OwnedHandle<'_, BTreeMap<K, u64>> {
    fn insert(&self, key: K, value: u64) {
        self.with(|map| map.insert(key, value));
    }

    fn get(&self, key: &K) -> Option<u64> {
        self.with(|map| map.get(key).copied())
    }

    fn bump(&self, key: &K) {
        self.with(|map| {
            if let Some(value) = map.get_mut(key) {
                *value += 1;
            }
        });
    }

    fn remove(&self, key: &K) -> bool {
        self.with(|map| map.remove(key).is_some())
    }

    fn scan(&self, start: &K, count: usize) -> (usize, u64) {
        self.with(|map| sum_of_first(map.range(start..).map(|(_, &value)| value), count))
    }
}

// std's hash map with its default hasher (SipHash-1-3), as a program
// without threads uses it: the map an ordered one is held up against,
// which keeps no order and so scans nothing.
impl<K: ScalingKey> SharedMap<K> for Owned<HashMap<K, u64>> {
    const NAME: &'static str = "std-hash";
    const ORDERED: bool = false;
    const SHARED: bool = false;

    type Handle<'m> = OwnedHandle<'m, HashMap<K, u64>>;

    fn new() -> Self {
        Owned::new(HashMap::new())
    }

    fn handle(&self) -> Self::Handle<'_> {
        self.only_handle()
    }
}

impl<K: ScalingKey> MapHandle<K> for OwnedHandle<'_, HashMap<K, u64>> {
    fn insert(&self, key: K, value: u64) {
        self.with(|map| map.insert(key, value));
    }

    fn get(&self, key: &K) -> Option<u64> {
        self.with(|map| map.get(key).copied())
    }

    fn bump(&self, key: &K) {
        self.with(|map| {
            if let Some(value) = map.get_mut(key) {
                *value += 1;
            }
        });
    }

    fn remove(&self, key: &K) -> bool {
        self.with(|map| map.remove(key).is_some())
    }

    fn scan(&self, _start: &K, _count: usize) -> (usize, u64) {
        unreachable!("a hash map keeps no order to scan")
    }
}
