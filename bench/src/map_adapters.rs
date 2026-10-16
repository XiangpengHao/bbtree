//! The maps under test, behind one trait. Each adapter lists the operations
//! its map's API lacks, whose cells are reported as `n/a`.

use std::collections::{BTreeMap, HashMap};

use bbtree::{BBTreeMap, BCTreeMap, InlineKey, InlineKeyStrategy, WithInlineKey};

use crate::command_line::Named;
use crate::keys::{BenchKey, KeyKind};
use crate::operations::Operation;
use crate::values::BenchValue;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MapKind {
    /// `BBTreeMap` with the strategy its users would pick for the key type
    /// (`BenchKey::measure_on_bbtree`): `WithInlineKey` for memtable keys,
    /// `Auto` for the others.
    BBTree,
    /// `BBTreeMap` with the `WithInlineKey` strategy: keys with inline keys
    /// through their own `InlineKey` impl. Runs only when `--map` names it,
    /// and only on the keys that `bbtree` gives none, the integers.
    BBTreeInline,
    /// `BCTreeMap`, the map threads share, with the strategy `bbtree` uses,
    /// on one thread. Runs only when `--map` names it, on keys with inline
    /// keys.
    BCTree,
    StdBTree,
    /// hashbrown's `HashMap` with its default hasher (foldhash).
    Hashbrown,
    /// std's `HashMap` with its default hasher (SipHash-1-3).
    StdHash,
}

impl Named for MapKind {
    const ALL: &'static [Self] = &[
        Self::BBTree,
        Self::BBTreeInline,
        Self::BCTree,
        Self::StdBTree,
        Self::Hashbrown,
        Self::StdHash,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::BBTree => "bbtree",
            Self::BBTreeInline => "bbtree-inline",
            Self::BCTree => "bctree",
            Self::StdBTree => "std-btree",
            Self::Hashbrown => "hashbrown",
            Self::StdHash => "std-hash",
        }
    }
}

impl MapKind {
    /// The column title in reports.
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::BBTree => "bbtree",
            Self::BBTreeInline => "bbtree, inline keys",
            Self::BCTree => "bctree",
            Self::StdBTree => "std BTreeMap",
            Self::Hashbrown => "hashbrown",
            Self::StdHash => "std HashMap",
        }
    }

    pub(crate) fn supports(self, operation: Operation) -> bool {
        !self.unsupported_operations().contains(&operation)
    }

    /// Whether the map runs on keys of `key`: `bbtree-inline` only on those
    /// whose type has an `InlineKey` impl that `bbtree` doesn't use, and
    /// `bctree` on those with inline keys.
    pub(crate) fn supports_key(self, key: KeyKind) -> bool {
        match self {
            Self::BBTreeInline => key.is_inline_keyed_integer(),
            Self::BCTree => key != KeyKind::U32,
            Self::BBTree | Self::StdBTree | Self::Hashbrown | Self::StdHash => true,
        }
    }

    /// Whether the map runs only when `--map` names it.
    pub(crate) fn is_opt_in(self) -> bool {
        matches!(self, Self::BBTreeInline | Self::BCTree)
    }

    /// The adapter's list, read at `u64 → u64`: the lists don't depend on
    /// the value type, nor on the key type unless it picks another adapter,
    /// which `CellRun::measure` checks.
    fn unsupported_operations(self) -> &'static [Operation] {
        match self {
            Self::BBTree => <BBTreeMap<u64, u64> as MapUnderTest<u64, u64>>::UNSUPPORTED,
            Self::BBTreeInline => {
                <BBTreeMap<u64, u64, WithInlineKey> as MapUnderTest<u64, u64>>::UNSUPPORTED
            }
            Self::BCTree => <CountedBCTree<u64, u64> as MapUnderTest<u64, u64>>::UNSUPPORTED,
            Self::StdBTree => <BTreeMap<u64, u64> as MapUnderTest<u64, u64>>::UNSUPPORTED,
            Self::Hashbrown => {
                <hashbrown::HashMap<u64, u64> as MapUnderTest<u64, u64>>::UNSUPPORTED
            }
            Self::StdHash => <HashMap<u64, u64> as MapUnderTest<u64, u64>>::UNSUPPORTED,
        }
    }
}

/// A map under test.
///
/// The required methods are the API every map has. The provided ones are
/// operations that some maps lack: an adapter overrides the ones its map has
/// and lists the others in `UNSUPPORTED`, which keeps them from running. So
/// enabling an operation is one change in one adapter: implement its method
/// and take it off the list. `get`, which hands out a reference, is one: a
/// map that hands out none says so ([`MapUnderTest::LENDS_REFERENCES`]) and
/// looks up through [`MapUnderTest::lookup_fingerprint`].
pub(crate) trait MapUnderTest<K: BenchKey, V: BenchValue>: Sized {
    const UNSUPPORTED: &'static [Operation];

    fn new() -> Self;

    fn entry_count(&self) -> usize;

    fn insert(&mut self, key: K, value: V);

    fn insert_batch<const N: usize>(&mut self, entries: [(K, V); N]) {
        for (key, value) in entries {
            self.insert(key, value);
        }
    }

    fn extend_entries(&mut self, entries: impl Iterator<Item = (K, V)>);

    /// Whether the map hands out references to its values ([`Self::get`]);
    /// if not, lookups read through [`Self::lookup_fingerprint`].
    const LENDS_REFERENCES: bool = true;

    fn get(&self, _key: &K) -> Option<&V> {
        unreachable!("a map that lends no references looks up through lookup_fingerprint")
    }

    /// The fingerprint of `key`'s value, if the map holds `key`, for a map
    /// that lends no references.
    fn lookup_fingerprint(&self, key: &K) -> Option<u64> {
        self.get(key).map(BenchValue::fingerprint)
    }

    /// A batch on maps that have it, separate lookups on the others.
    fn get_batch<const N: usize>(&self, keys: [&K; N]) -> [Option<&V>; N] {
        keys.map(|key| self.get(key))
    }

    /// Visits every entry, in key order for ordered maps, and returns their
    /// checksum.
    fn checksum_all(&self) -> u64 {
        Self::lacks(Operation::Iterate)
    }

    /// Visits up to `count` entries in key order, from the first key at or
    /// after `start`, and returns their checksum.
    fn checksum_range(&self, _start: &K, _count: usize) -> u64 {
        Self::lacks(Operation::Range64)
    }

    fn remove(&mut self, _key: &K) -> Option<V> {
        Self::lacks(Operation::RandomRemove)
    }

    /// The counter-increment pattern: `*map.entry(key).or_default() += 1`.
    fn bump_count(&mut self, _key: K) {
        Self::lacks(Operation::EntryUpsert)
    }

    fn pop_first(&mut self) -> Option<(K, V)> {
        Self::lacks(Operation::PopFirst)
    }

    /// `FromIterator`, from entries sorted by key.
    fn from_sorted_entries(_entries: Vec<(K, V)>) -> Self {
        Self::lacks(Operation::BulkBuild)
    }

    /// The checksum of visited entries, which reads every key and value.
    fn checksum<'a>(entries: impl Iterator<Item = (&'a K, &'a V)>) -> u64
    where
        K: 'a,
        V: 'a,
    {
        entries.fold(0, |sum, (key, value)| {
            sum.wrapping_add(key.fingerprint() ^ value.fingerprint())
        })
    }

    fn lacks(operation: Operation) -> ! {
        unreachable!(
            "{} is listed as unsupported, so it never runs",
            operation.name()
        )
    }
}

impl<K: BenchKey, V: BenchValue> MapUnderTest<K, V> for BBTreeMap<K, V> {
    /// Not written yet.
    const UNSUPPORTED: &'static [Operation] = &[];

    fn new() -> Self {
        BBTreeMap::new()
    }

    fn extend_entries(&mut self, entries: impl Iterator<Item = (K, V)>) {
        self.extend(entries);
    }

    fn entry_count(&self) -> usize {
        BBTreeMap::len(self)
    }

    fn insert(&mut self, key: K, value: V) {
        BBTreeMap::insert(self, key, value);
    }

    fn insert_batch<const N: usize>(&mut self, entries: [(K, V); N]) {
        self.insert_many(entries);
    }

    fn get(&self, key: &K) -> Option<&V> {
        BBTreeMap::get(self, key)
    }

    fn get_batch<const N: usize>(&self, keys: [&K; N]) -> [Option<&V>; N] {
        self.get_many(keys)
    }

    fn checksum_all(&self) -> u64 {
        Self::checksum(self.iter())
    }

    fn checksum_range(&self, start: &K, count: usize) -> u64 {
        Self::checksum(self.range::<K, _>(start..).take(count))
    }

    fn remove(&mut self, key: &K) -> Option<V> {
        BBTreeMap::remove(self, key)
    }

    fn pop_first(&mut self) -> Option<(K, V)> {
        BBTreeMap::pop_first(self)
    }

    fn from_sorted_entries(entries: Vec<(K, V)>) -> Self {
        entries.into_iter().collect()
    }

    fn bump_count(&mut self, key: K) {
        self.entry(key).or_default().bump();
    }
}

/// `collect` makes maps of the default strategy only.
impl<K: BenchKey + InlineKey, V: BenchValue> MapUnderTest<K, V> for BBTreeMap<K, V, WithInlineKey> {
    const UNSUPPORTED: &'static [Operation] = &[Operation::BulkBuild];

    fn new() -> Self {
        BBTreeMap::with_strategy(WithInlineKey)
    }

    fn extend_entries(&mut self, entries: impl Iterator<Item = (K, V)>) {
        self.extend(entries);
    }

    fn entry_count(&self) -> usize {
        BBTreeMap::len(self)
    }

    fn insert(&mut self, key: K, value: V) {
        BBTreeMap::insert(self, key, value);
    }

    fn insert_batch<const N: usize>(&mut self, entries: [(K, V); N]) {
        self.insert_many(entries);
    }

    fn get(&self, key: &K) -> Option<&V> {
        BBTreeMap::get(self, key)
    }

    fn get_batch<const N: usize>(&self, keys: [&K; N]) -> [Option<&V>; N] {
        self.get_many(keys)
    }

    fn checksum_all(&self) -> u64 {
        Self::checksum(self.iter())
    }

    fn checksum_range(&self, start: &K, count: usize) -> u64 {
        Self::checksum(self.range::<K, _>(start..).take(count))
    }

    fn remove(&mut self, key: &K) -> Option<V> {
        BBTreeMap::remove(self, key)
    }

    fn pop_first(&mut self) -> Option<(K, V)> {
        BBTreeMap::pop_first(self)
    }

    fn bump_count(&mut self, key: K) {
        self.entry(key).or_default().bump();
    }
}

impl<K: BenchKey, V: BenchValue> MapUnderTest<K, V> for BTreeMap<K, V> {
    const UNSUPPORTED: &'static [Operation] = &[];

    fn new() -> Self {
        BTreeMap::new()
    }

    fn extend_entries(&mut self, entries: impl Iterator<Item = (K, V)>) {
        self.extend(entries);
    }

    fn entry_count(&self) -> usize {
        BTreeMap::len(self)
    }

    fn insert(&mut self, key: K, value: V) {
        BTreeMap::insert(self, key, value);
    }

    fn get(&self, key: &K) -> Option<&V> {
        BTreeMap::get(self, key)
    }

    fn checksum_all(&self) -> u64 {
        Self::checksum(self.iter())
    }

    fn checksum_range(&self, start: &K, count: usize) -> u64 {
        Self::checksum(self.range::<K, _>(start..).take(count))
    }

    fn remove(&mut self, key: &K) -> Option<V> {
        BTreeMap::remove(self, key)
    }

    fn bump_count(&mut self, key: K) {
        self.entry(key).or_default().bump();
    }

    fn pop_first(&mut self) -> Option<(K, V)> {
        BTreeMap::pop_first(self)
    }

    fn from_sorted_entries(entries: Vec<(K, V)>) -> Self {
        entries.into_iter().collect()
    }
}

/// Hash maps have no order: no range scans and no `pop_first`.
impl<K: BenchKey, V: BenchValue> MapUnderTest<K, V> for hashbrown::HashMap<K, V> {
    const UNSUPPORTED: &'static [Operation] = &[Operation::Range64, Operation::PopFirst];

    fn new() -> Self {
        hashbrown::HashMap::new()
    }

    fn extend_entries(&mut self, entries: impl Iterator<Item = (K, V)>) {
        self.extend(entries);
    }

    fn entry_count(&self) -> usize {
        hashbrown::HashMap::len(self)
    }

    fn insert(&mut self, key: K, value: V) {
        hashbrown::HashMap::insert(self, key, value);
    }

    fn get(&self, key: &K) -> Option<&V> {
        hashbrown::HashMap::get(self, key)
    }

    fn checksum_all(&self) -> u64 {
        Self::checksum(self.iter())
    }

    fn remove(&mut self, key: &K) -> Option<V> {
        hashbrown::HashMap::remove(self, key)
    }

    fn bump_count(&mut self, key: K) {
        self.entry(key).or_default().bump();
    }

    fn from_sorted_entries(entries: Vec<(K, V)>) -> Self {
        entries.into_iter().collect()
    }
}

/// Hash maps have no order: no range scans and no `pop_first`.
impl<K: BenchKey, V: BenchValue> MapUnderTest<K, V> for HashMap<K, V> {
    const UNSUPPORTED: &'static [Operation] = &[Operation::Range64, Operation::PopFirst];

    fn new() -> Self {
        HashMap::new()
    }

    fn extend_entries(&mut self, entries: impl Iterator<Item = (K, V)>) {
        self.extend(entries);
    }

    fn entry_count(&self) -> usize {
        HashMap::len(self)
    }

    fn insert(&mut self, key: K, value: V) {
        HashMap::insert(self, key, value);
    }

    fn get(&self, key: &K) -> Option<&V> {
        HashMap::get(self, key)
    }

    fn checksum_all(&self) -> u64 {
        Self::checksum(self.iter())
    }

    fn remove(&mut self, key: &K) -> Option<V> {
        HashMap::remove(self, key)
    }

    fn bump_count(&mut self, key: K) {
        self.entry(key).or_default().bump();
    }

    fn from_sorted_entries(entries: Vec<(K, V)>) -> Self {
        entries.into_iter().collect()
    }
}

/// `BCTreeMap` with strategy `S`, and the count of entries it has taken,
/// which the map itself doesn't keep: a shared counter would cost every
/// insert an atomic. Lookups read through a closure, as the map hands out no
/// references.
pub(crate) struct CountedBCTree<K, V, S: InlineKeyStrategy<K> = bbtree::Auto> {
    map: BCTreeMap<K, V, S>,
    entries: usize,
}

impl<K: BenchKey, V: BenchValue, S: InlineKeyStrategy<K> + Default> MapUnderTest<K, V>
    for CountedBCTree<K, V, S>
{
    /// Phase 1 of `BCTreeMap`: inserts and lookups only.
    const UNSUPPORTED: &'static [Operation] = &[
        Operation::BulkBuild,
        Operation::Iterate,
        Operation::Range64,
        Operation::RandomRemove,
        Operation::EntryUpsert,
        Operation::PopFirst,
        Operation::BatchGetHit32,
        Operation::BatchGetMiss32,
        Operation::BatchGetHit64,
        Operation::BatchGetMiss64,
        Operation::BatchRandomInsert,
        Operation::BatchSequentialInsert,
        Operation::ExtendRandom,
        Operation::ExtendSequential,
    ];

    const LENDS_REFERENCES: bool = false;

    fn new() -> Self {
        Self {
            map: BCTreeMap::with_strategy(S::default()),
            entries: 0,
        }
    }

    fn entry_count(&self) -> usize {
        self.entries
    }

    fn insert(&mut self, key: K, value: V) {
        if self.map.insert(key, value).is_none() {
            self.entries += 1;
        }
    }

    fn extend_entries(&mut self, entries: impl Iterator<Item = (K, V)>) {
        for (key, value) in entries {
            self.insert(key, value);
        }
    }

    fn lookup_fingerprint(&self, key: &K) -> Option<u64> {
        self.map.read(key, |_, value| value.fingerprint())
    }
}
