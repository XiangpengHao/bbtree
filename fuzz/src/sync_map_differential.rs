//! Differential fuzzing of the sync map, `bbtree::BBTreeMap`, which threads
//! share (`doc/09-concurrency.md`): operations from the input run on one
//! thread on it and on std's `BTreeMap`, which must answer every one alike,
//! and its tree must stay well formed after each (`check_invariants`).
//!
//! Its nodes are the unsync map's, which the map targets fuzz row by row
//! (`map_differential.rs`). What only it has is the layer over them: the
//! optimistic descent, lookups that copy a value out of a leaf without its
//! latch and check the leaf's version after, latched writes, scans,
//! `range_cloned`'s ends and the batches. On one thread every version check
//! holds, so each of those paths answers from what it read; how threads
//! interleave is the shuttle tests' (`src/map/shuttle_tests.rs`). So one
//! target covers every row with inline keys, which the input picks
//! ([`SyncMapRow`]).

use std::borrow::Borrow;
use std::collections::BTreeMap;
use std::fmt::Debug;
use std::ops::{Bound, ControlFlow};

use bbtree::{Auto, BBTreeMap, InlineKeyStrategy, WithInlineKey};
use libfuzzer_sys::arbitrary::{self, Arbitrary};

use crate::key_recipe::{BorrowedLookup, FuzzKey, KeyRecipe, Stems, UserKey};
use crate::map_differential::{BulkyValue, Fanout, FuzzStrategy, FuzzValue, InputBound, Steps};
use crate::operation_list::Operations;

/// The rows of the node plan's table that the sync map takes: every row
/// with inline keys, which it needs, as the map targets of the same names
/// (`MapTarget`).
#[derive(Arbitrary, Debug, Clone, Copy)]
enum SyncMapRow {
    HeapByteStrings,
    Integers,
    UserInlineKeys,
    ByteArrays,
}

/// One input of the sync map target.
#[derive(Arbitrary, Debug)]
pub struct SyncMapInput {
    row: SyncMapRow,
    fanout: Fanout,
    stems: Stems,
    operations: Operations<SyncMapOperation>,
}

impl SyncMapInput {
    /// Runs the operations on the map of the input's row and fanout. Wide
    /// maps hold `Copy` values, which lookups copy out of leaves without
    /// their latches: `u128` beside byte strings, whose latched leaves have
    /// room for length tags (D36) only beside 16-byte-aligned values, and
    /// `u64` beside the other keys.
    pub fn run(&self) {
        match (self.row, self.fanout) {
            (SyncMapRow::HeapByteStrings, Fanout::Wide) => {
                MirroredSyncMaps::<Vec<u8>, u128>::run(self);
            }
            (SyncMapRow::HeapByteStrings, Fanout::Narrow) => {
                MirroredSyncMaps::<Vec<u8>, BulkyValue>::run(self);
            }
            (SyncMapRow::Integers, Fanout::Wide) => MirroredSyncMaps::<u64, u64>::run(self),
            (SyncMapRow::Integers, Fanout::Narrow) => {
                MirroredSyncMaps::<u64, BulkyValue>::run(self);
            }
            (SyncMapRow::UserInlineKeys, Fanout::Wide) => {
                MirroredSyncMaps::<UserKey, u64, WithInlineKey>::run(self);
            }
            (SyncMapRow::UserInlineKeys, Fanout::Narrow) => {
                MirroredSyncMaps::<UserKey, BulkyValue, WithInlineKey>::run(self);
            }
            (SyncMapRow::ByteArrays, Fanout::Wide) => {
                MirroredSyncMaps::<[u8; 16], u64>::run(self);
            }
            (SyncMapRow::ByteArrays, Fanout::Narrow) => {
                MirroredSyncMaps::<[u8; 16], BulkyValue>::run(self);
            }
        }
    }
}

/// One operation on both maps.
#[derive(Arbitrary, Debug)]
enum SyncMapOperation {
    Insert {
        key: KeyRecipe,
        value: u16,
    },
    /// Inserts `count` keys of consecutive numbers from `first_key`: tall
    /// trees and runs of ascending inserts from short inputs.
    InsertRun {
        first_key: KeyRecipe,
        count: u8,
        value: u16,
    },
    /// `insert_many`.
    InsertBatch {
        entries: [(KeyRecipe, u16); 8],
    },
    /// `get_copied` (or `get_cloned`, for values that aren't `Copy`),
    /// `get_cloned`, `contains_key` and `read`.
    Get {
        key: KeyRecipe,
    },
    /// The same, by the key's borrowed form.
    GetBorrowed {
        key: KeyRecipe,
    },
    /// `get_many_copied` (or `get_many_cloned`), `get_many_cloned` and
    /// `read_many`.
    GetBatch {
        keys: [KeyRecipe; 8],
    },
    /// `update`, bumping the value by `amount`.
    Update {
        key: KeyRecipe,
        amount: u16,
    },
    /// `get_mut`, through the owner's `&mut`.
    GetMut {
        key: KeyRecipe,
        value: u16,
    },
    GetOrInsert {
        key: KeyRecipe,
        value: u16,
    },
    Remove {
        key: KeyRecipe,
    },
    RemoveBorrowed {
        key: KeyRecipe,
    },
    RemoveEntry {
        key: KeyRecipe,
    },
    /// `remove_if`, whose condition holds if `holds`.
    RemoveIf {
        key: KeyRecipe,
        holds: bool,
    },
    /// Removes `count` keys of consecutive numbers from `first_key`: trees
    /// that shrink by levels from short inputs.
    RemoveRun {
        first_key: KeyRecipe,
        count: u8,
    },
    /// `scan`, or `scan_rev` if `reverse`, breaking at entry `stop_at` of
    /// the range if it has one; bounds out of order are skipped.
    Scan {
        start: InputBound,
        end: InputBound,
        reverse: bool,
        stop_at: Option<u8>,
    },
    /// `range_cloned`: its first `steps`, then the rest from the front, or
    /// from the back if `rest_from_back`; bounds out of order are skipped.
    RangeCloned {
        start: InputBound,
        end: InputBound,
        steps: Steps,
        rest_from_back: bool,
    },
    /// `iter_mut`: its first `steps`, then the rest by a fold, bumping every
    /// value it reaches.
    IterMut {
        steps: Steps,
        amount: u16,
    },
    Clear,
    Extend {
        entries: Vec<(KeyRecipe, u16)>,
    },
    /// Replaces both maps by maps collected from these entries.
    Collect {
        entries: Vec<(KeyRecipe, u16)>,
    },
    /// Takes both maps by value and takes `count` entries, then drops the
    /// rest: the maps start over empty.
    IntoIter {
        count: u8,
    },
}

/// A value type of the sync maps under test, and how a lookup takes it out:
/// a `Copy` value by the copies that read leaves without their latches
/// (`get_copied`), which only `Copy` values have; any other by a clone.
trait SyncFuzzValue: FuzzValue {
    /// `key`'s value: `get_copied`'s if `Self` is `Copy`, else
    /// `get_cloned`'s.
    fn get<K, S, Q>(map: &BBTreeMap<K, Self, S>, key: &Q) -> Option<Self>
    where
        K: Borrow<Q>,
        S: InlineKeyStrategy<K>,
        Q: Ord + ?Sized;

    /// Each key's value, taken out as [`Self::get`] takes it, in one batch.
    fn get_many<K, S, Q, const N: usize>(
        map: &BBTreeMap<K, Self, S>,
        keys: [&Q; N],
    ) -> [Option<Self>; N]
    where
        K: Borrow<Q>,
        S: InlineKeyStrategy<K>,
        Q: Ord + ?Sized;
}

impl<T: FuzzValue + Copy> SyncFuzzValue for T {
    fn get<K, S, Q>(map: &BBTreeMap<K, Self, S>, key: &Q) -> Option<Self>
    where
        K: Borrow<Q>,
        S: InlineKeyStrategy<K>,
        Q: Ord + ?Sized,
    {
        map.get_copied(key)
    }

    fn get_many<K, S, Q, const N: usize>(
        map: &BBTreeMap<K, Self, S>,
        keys: [&Q; N],
    ) -> [Option<Self>; N]
    where
        K: Borrow<Q>,
        S: InlineKeyStrategy<K>,
        Q: Ord + ?Sized,
    {
        map.get_many_copied(keys)
    }
}

impl SyncFuzzValue for BulkyValue {
    fn get<K, S, Q>(map: &BBTreeMap<K, Self, S>, key: &Q) -> Option<Self>
    where
        K: Borrow<Q>,
        S: InlineKeyStrategy<K>,
        Q: Ord + ?Sized,
    {
        map.get_cloned(key)
    }

    fn get_many<K, S, Q, const N: usize>(
        map: &BBTreeMap<K, Self, S>,
        keys: [&Q; N],
    ) -> [Option<Self>; N]
    where
        K: Borrow<Q>,
        S: InlineKeyStrategy<K>,
        Q: Ord + ?Sized,
    {
        map.get_many_cloned(keys)
    }
}

/// The sync map, with strategy `S`, and std's, fed the same operations.
struct MirroredSyncMaps<'i, K, V, S: InlineKeyStrategy<K> = Auto> {
    stems: &'i Stems,
    ours: BBTreeMap<K, V, S>,
    theirs: BTreeMap<K, V>,
}

impl<'i, K, V, S> MirroredSyncMaps<'i, K, V, S>
where
    K: FuzzKey,
    V: SyncFuzzValue,
    S: FuzzStrategy<K>,
{
    /// Runs the input's operations on a fresh pair of maps, comparing every
    /// answer and checking our tree after each, and at the end every entry.
    fn run(input: &'i SyncMapInput) {
        let mut maps = Self {
            stems: &input.stems,
            ours: BBTreeMap::with_strategy(S::default()),
            theirs: BTreeMap::new(),
        };
        for operation in &input.operations {
            maps.apply(operation);
            assert_eq!(maps.ours.is_empty(), maps.theirs.is_empty());
            maps.ours.check_invariants(maps.theirs.len());
        }
        maps.assert_same_entries();
    }

    fn key(&self, recipe: KeyRecipe) -> K {
        K::from_recipe(recipe, self.stems)
    }

    fn key_range(&self, start: InputBound, end: InputBound) -> Option<(Bound<K>, Bound<K>)> {
        InputBound::key_range(start, end, self.stems)
    }

    fn entries(&self, entries: &[(KeyRecipe, u16)]) -> Vec<(K, V)> {
        (entries.iter())
            .map(|&(key, value)| (self.key(key), V::from_input(value)))
            .collect()
    }

    fn apply(&mut self, operation: &SyncMapOperation) {
        match *operation {
            SyncMapOperation::Insert { key, value } => {
                self.insert(self.key(key), V::from_input(value));
            }
            SyncMapOperation::InsertRun {
                first_key,
                count,
                value,
            } => {
                for offset in 0..u16::from(count) {
                    let key = self.key(first_key.after(offset));
                    self.insert(key, V::from_input(value.wrapping_add(offset)));
                }
            }
            SyncMapOperation::InsertBatch { entries } => {
                let entries = entries.map(|(key, value)| (self.key(key), V::from_input(value)));
                let expected = entries
                    .clone()
                    .map(|(key, value)| self.theirs.insert(key, value));
                assert_eq!(self.ours.insert_many(entries), expected);
            }
            SyncMapOperation::Get { key } => self.assert_same_lookup(&self.key(key)),
            SyncMapOperation::GetBorrowed { key } => {
                self.key(key).look_up_borrowed(SyncBorrowedGet(self));
            }
            SyncMapOperation::GetBatch { keys } => self.assert_same_batch(keys),
            SyncMapOperation::Update { key, amount } => {
                let key = self.key(key);
                let ours = self.ours.update(&key, |key, value| {
                    value.bump(amount);
                    (key.clone(), value.clone())
                });
                let theirs = self.theirs.get_mut(&key).map(|value| {
                    value.bump(amount);
                    (key.clone(), value.clone())
                });
                assert_eq!(ours, theirs);
            }
            SyncMapOperation::GetMut { key, value } => {
                let key = self.key(key);
                match (self.ours.get_mut(&key), self.theirs.get_mut(&key)) {
                    (Some(ours), Some(theirs)) => {
                        assert_eq!(ours, theirs);
                        *ours = V::from_input(value);
                        *theirs = V::from_input(value);
                    }
                    (ours, theirs) => assert_eq!(ours, theirs),
                }
            }
            SyncMapOperation::GetOrInsert { key, value } => {
                let key = self.key(key);
                let ours =
                    self.ours
                        .get_or_insert(key.clone(), V::from_input(value), |key, value| {
                            (key.clone(), value.clone())
                        });
                let theirs = self
                    .theirs
                    .entry(key.clone())
                    .or_insert(V::from_input(value));
                assert_eq!(ours, (key, theirs.clone()));
            }
            SyncMapOperation::Remove { key } => {
                let key = self.key(key);
                assert_eq!(self.ours.remove(&key), self.theirs.remove(&key));
            }
            SyncMapOperation::RemoveBorrowed { key } => {
                self.key(key).look_up_borrowed(SyncBorrowedRemove(self));
            }
            SyncMapOperation::RemoveEntry { key } => {
                let key = self.key(key);
                assert_eq!(self.ours.remove_entry(&key), self.theirs.remove_entry(&key));
            }
            SyncMapOperation::RemoveIf { key, holds } => self.remove_if(self.key(key), holds),
            SyncMapOperation::RemoveRun { first_key, count } => {
                for offset in 0..u16::from(count) {
                    let key = self.key(first_key.after(offset));
                    assert_eq!(self.ours.remove(&key), self.theirs.remove(&key));
                }
            }
            SyncMapOperation::Scan {
                start,
                end,
                reverse,
                stop_at,
            } => {
                if let Some(range) = self.key_range(start, end) {
                    self.assert_same_scan(range, reverse, stop_at);
                }
            }
            SyncMapOperation::RangeCloned {
                start,
                end,
                steps,
                rest_from_back,
            } => {
                if let Some(range) = self.key_range(start, end) {
                    let mut ours = self.ours.range_cloned(range.clone());
                    let mut theirs =
                        (self.theirs.range(range)).map(|(key, value)| (key.clone(), value.clone()));
                    steps.assert_same_steps(&mut ours, &mut theirs, |_| {});
                    if rest_from_back {
                        assert_eq!(
                            ours.rev().collect::<Vec<_>>(),
                            theirs.rev().collect::<Vec<_>>()
                        );
                    } else {
                        assert_eq!(ours.collect::<Vec<_>>(), theirs.collect::<Vec<_>>());
                    }
                }
            }
            SyncMapOperation::IterMut { steps, amount } => {
                let (mut ours, mut theirs) = (self.ours.iter_mut(), self.theirs.iter_mut());
                let bump = |(_, value): &mut (&K, &mut V)| value.bump(amount);
                steps.assert_same_steps(&mut ours, &mut theirs, bump);
                let collect = |mut items: Vec<_>, item| {
                    items.push(item);
                    items
                };
                let (mut ours, mut theirs) =
                    (ours.fold(vec![], collect), theirs.fold(vec![], collect));
                assert_eq!(ours, theirs);
                ours.iter_mut().for_each(bump);
                theirs.iter_mut().for_each(bump);
            }
            SyncMapOperation::Clear => {
                self.ours.clear();
                self.theirs.clear();
            }
            SyncMapOperation::Extend { ref entries } => {
                let entries = self.entries(entries);
                self.ours.extend(entries.iter().cloned());
                self.theirs.extend(entries);
            }
            SyncMapOperation::Collect { ref entries } => {
                let entries = self.entries(entries);
                self.ours = S::collect_sync(entries.clone());
                self.theirs = entries.into_iter().collect();
                self.assert_same_entries();
            }
            SyncMapOperation::IntoIter { count } => {
                let empty = BBTreeMap::with_strategy(S::default());
                let mut ours = std::mem::replace(&mut self.ours, empty).into_iter();
                let mut theirs = std::mem::take(&mut self.theirs).into_iter();
                for _ in 0..count {
                    assert_eq!(ours.next(), theirs.next());
                }
            }
        }
    }

    fn insert(&mut self, key: K, value: V) {
        let ours = self.ours.insert(key.clone(), value.clone());
        assert_eq!(ours, self.theirs.insert(key, value));
    }

    fn assert_same_lookup<Q: Ord + ?Sized>(&self, key: &Q)
    where
        K: Borrow<Q>,
    {
        let theirs = self.theirs.get_key_value(key);
        let their_value = theirs.map(|(_, value)| value);
        assert_eq!(V::get(&self.ours, key).as_ref(), their_value);
        assert_eq!(self.ours.get_cloned(key).as_ref(), their_value);
        assert_eq!(self.ours.contains_key(key), theirs.is_some());
        assert_eq!(
            self.ours
                .read(key, |key, value| (key.clone(), value.clone())),
            theirs.map(|(key, value)| (key.clone(), value.clone()))
        );
    }

    fn assert_same_batch(&self, keys: [KeyRecipe; 8]) {
        let keys = keys.map(|key| self.key(key));
        let queries = keys.each_ref();
        let theirs = queries.map(|key| self.theirs.get_key_value(key));
        let their_values = theirs.map(|entry| entry.map(|(_, value)| value.clone()));
        assert_eq!(V::get_many(&self.ours, queries), their_values);
        assert_eq!(self.ours.get_many_cloned(queries), their_values);
        assert_eq!(
            self.ours
                .read_many(queries, |key, value| (key.clone(), value.clone())),
            theirs.map(|entry| entry.map(|(key, value)| (key.clone(), value.clone())))
        );
    }

    /// `remove_if` for `key`, whose condition must see `key`'s entry, and
    /// only if the map holds one.
    fn remove_if(&mut self, key: K, holds: bool) {
        let entry = self.theirs.get_key_value(&key);
        let entry = entry.map(|(key, value)| (key.clone(), value.clone()));
        let ours = self.ours.remove_if(&key, |key, value| {
            assert_eq!(
                Some((key, value)),
                entry.as_ref().map(|(key, value)| (key, value))
            );
            holds
        });
        let theirs = if holds {
            self.theirs.remove_entry(&key)
        } else {
            None
        };
        assert_eq!(ours, theirs);
    }

    /// `scan`, or `scan_rev` if `reverse`, over `range`, breaking with the
    /// key of its entry `stop_at`: it must visit what std's range does, up
    /// to that entry.
    fn assert_same_scan(&self, range: (Bound<K>, Bound<K>), reverse: bool, stop_at: Option<u8>) {
        let stop_at = stop_at.map(usize::from);
        let mut visited = Vec::new();
        let mut visit = |key: &K, value: &V| {
            visited.push((key.clone(), value.clone()));
            if stop_at == Some(visited.len() - 1) {
                ControlFlow::Break(key.clone())
            } else {
                ControlFlow::Continue(())
            }
        };
        let stopped_at = if reverse {
            self.ours.scan_rev(range.clone(), &mut visit)
        } else {
            self.ours.scan(range.clone(), &mut visit)
        };
        let taken = stop_at.map_or(usize::MAX, |stop_at| stop_at + 1);
        let theirs = self.theirs.range(range);
        let expected: Vec<(K, V)> = if reverse {
            (theirs.rev().take(taken))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        } else {
            (theirs.take(taken))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        };
        let expected_stop = stop_at.and_then(|stop_at| expected.get(stop_at));
        assert_eq!(visited, expected);
        assert_eq!(stopped_at.as_ref(), expected_stop.map(|(key, _)| key));
    }

    fn assert_same_entries(&self) {
        let ours: Vec<(K, V)> = self.ours.range_cloned::<K, _>(..).collect();
        assert!(
            ours.iter()
                .map(|(key, value)| (key, value))
                .eq(&self.theirs)
        );
        for (key, value) in &self.theirs {
            assert_eq!(V::get(&self.ours, key).as_ref(), Some(value));
        }
    }
}

/// The lookups of [`SyncMapOperation::Get`] through a borrowed form of the
/// key, in both maps.
struct SyncBorrowedGet<'m, 'i, K, V, S: InlineKeyStrategy<K>>(&'m MirroredSyncMaps<'i, K, V, S>);

impl<K: FuzzKey, V: SyncFuzzValue, S: FuzzStrategy<K>> BorrowedLookup<K>
    for SyncBorrowedGet<'_, '_, K, V, S>
{
    fn look_up<Q: Ord + Debug + ?Sized>(self, key: &Q)
    where
        K: Borrow<Q>,
    {
        self.0.assert_same_lookup(key);
    }
}

/// `remove` through a borrowed form of the key, in both maps.
struct SyncBorrowedRemove<'m, 'i, K, V, S: InlineKeyStrategy<K>>(
    &'m mut MirroredSyncMaps<'i, K, V, S>,
);

impl<K: FuzzKey, V: SyncFuzzValue, S: FuzzStrategy<K>> BorrowedLookup<K>
    for SyncBorrowedRemove<'_, '_, K, V, S>
{
    fn look_up<Q: Ord + Debug + ?Sized>(self, key: &Q)
    where
        K: Borrow<Q>,
    {
        assert_eq!(self.0.ours.remove(key), self.0.theirs.remove(key));
    }
}
