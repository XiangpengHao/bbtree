//! Differential fuzzing of maps (D5): operations from the input run on an
//! `unsync::BBTreeMap` and on std's `BTreeMap`, which must answer every one
//! alike, and our tree must stay well formed after each
//! (`check_invariants`).
//!
//! Every map operation reaches the same node code: slots, splits, merges,
//! moves between siblings, cuts, joins, bulk builds and walks. What sets key
//! types apart is the node plan (`src/node/node_plan.rs`): which inline
//! keys and hash tags nodes keep, and how they search. So
//! there is one target per row of the plan's table ([`MapTarget`]), each on
//! two maps: one with nodes as wide as the key type gives, and one with
//! narrow nodes, whose trees grow tall from short inputs ([`Fanout`]).

use std::borrow::Borrow;
use std::collections::{BTreeMap, btree_map};
use std::fmt::Debug;
use std::ops::Bound;

use bbtree::unsync::BBTreeMap;
use bbtree::unsync::bbtree_map::Entry;
use bbtree::{Auto, BBTreeMap as SyncMap, InlineKey, InlineKeyStrategy, WithInlineKey};
use libfuzzer_sys::arbitrary::{self, Arbitrary};

use crate::key_recipe::{BorrowedLookup, FuzzKey, KeyRecipe, Stems, UserKey, WideKey};
use crate::operation_list::Operations;
use crate::position_mask::PositionMask;

/// The map targets: one per row of the node plan's table.
#[derive(Clone, Copy, Debug)]
pub enum MapTarget {
    /// Std heap byte strings (`Vec<u8>`): 8-byte inline keys in leaves and
    /// 4-byte ones in internal nodes, read again from keys when handed out
    /// (D43), and hash tags in leaves (D36). The most code of any row.
    HeapByteStrings,
    /// Std integers (`u64`): 2-byte inline keys past the bytes a node's keys
    /// share (D31); the most common keys with inline keys.
    Integers,
    /// A user's byte-string key with its own `InlineKey` impl, under
    /// `WithInlineKey`, at the default width of 8 bytes: prefixes kept
    /// without calling the encoding while keys move (D26), and lookups by a
    /// borrowed form the nodes can't encode, by `Ord` alone (D19).
    UserInlineKeys,
    /// Std byte arrays (`[u8; 16]`): 2-byte inline keys, keyless internal
    /// nodes (D49), and a lookup's one candidate compared whole (D38).
    ByteArrays,
    /// Keys without inline keys: `u32`, searched branchless (D19), and in
    /// the narrow map 64-byte arrays, whose trees reach three levels from a
    /// few hundred entries.
    Plain,
}

impl MapTarget {
    pub const ALL: [Self; 5] = [
        Self::HeapByteStrings,
        Self::Integers,
        Self::UserInlineKeys,
        Self::ByteArrays,
        Self::Plain,
    ];

    pub fn run(self, input: &MapInput) {
        match (self, input.fanout) {
            (Self::HeapByteStrings, Fanout::Wide) => MirroredMaps::<Vec<u8>, u64>::run(input),
            (Self::HeapByteStrings, Fanout::Narrow) => {
                MirroredMaps::<Vec<u8>, BulkyValue>::run(input)
            }
            (Self::Integers, Fanout::Wide) => MirroredMaps::<u64, u64>::run(input),
            (Self::Integers, Fanout::Narrow) => MirroredMaps::<u64, BulkyValue>::run(input),
            (Self::UserInlineKeys, Fanout::Wide) => {
                MirroredMaps::<UserKey, u64, WithInlineKey>::run(input)
            }
            (Self::UserInlineKeys, Fanout::Narrow) => {
                MirroredMaps::<UserKey, BulkyValue, WithInlineKey>::run(input)
            }
            (Self::ByteArrays, Fanout::Wide) => MirroredMaps::<[u8; 16], u64>::run(input),
            (Self::ByteArrays, Fanout::Narrow) => MirroredMaps::<[u8; 16], BulkyValue>::run(input),
            (Self::Plain, Fanout::Wide) => MirroredMaps::<u32, u64>::run(input),
            (Self::Plain, Fanout::Narrow) => MirroredMaps::<WideKey, u64>::run(input),
        }
    }
}

/// One input of a map target.
#[derive(Arbitrary, Debug)]
pub struct MapInput {
    fanout: Fanout,
    stems: Stems,
    operations: Operations<MapOperation>,
}

/// Which of a target's two maps an input runs on.
#[derive(Arbitrary, Debug, Clone, Copy)]
pub(crate) enum Fanout {
    /// Nodes as wide as the key type gives, with values of a word or two.
    Wide,
    /// Nodes of about a dozen entries, the minimum (D13), in leaves, or in
    /// both kinds where the key type allows.
    Narrow,
}

/// A range bound from the input.
#[derive(Arbitrary, Debug, Clone, Copy)]
pub(crate) enum InputBound {
    Included(KeyRecipe),
    Excluded(KeyRecipe),
    Unbounded,
}

impl InputBound {
    /// The range from `start` to `end`, or `None` if they are out of order,
    /// for which every map under test panics, as std's does.
    pub(crate) fn key_range<K: FuzzKey>(
        start: Self,
        end: Self,
        stems: &Stems,
    ) -> Option<(Bound<K>, Bound<K>)> {
        let range = (start.key_bound(stems), end.key_bound(stems));
        let out_of_order = match &range {
            (Bound::Excluded(start), Bound::Excluded(end)) => start >= end,
            (
                Bound::Included(start) | Bound::Excluded(start),
                Bound::Included(end) | Bound::Excluded(end),
            ) => start > end,
            _ => false,
        };
        (!out_of_order).then_some(range)
    }

    fn key_bound<K: FuzzKey>(self, stems: &Stems) -> Bound<K> {
        match self {
            Self::Included(key) => Bound::Included(K::from_recipe(key, stems)),
            Self::Excluded(key) => Bound::Excluded(K::from_recipe(key, stems)),
            Self::Unbounded => Bound::Unbounded,
        }
    }
}

/// Which end each step of a walk takes from: bit `i % 32` for step `i`, set
/// for the back.
#[derive(Arbitrary, Debug, Clone, Copy)]
pub(crate) struct Ends(u32);

impl Ends {
    fn takes_back(self, step: usize) -> bool {
        self.0 >> (step % 32) & 1 == 1
    }
}

/// The iterators that walk a map by shared reference: each has its own
/// folds, over the walk's per-leaf loops (`node/walk.rs`).
#[derive(Arbitrary, Debug, Clone, Copy)]
enum WalkSource {
    Entries,
    Keys,
    Values,
    Range { start: InputBound, end: InputBound },
}

/// The iterators that walk a map by mutable reference.
#[derive(Arbitrary, Debug, Clone, Copy)]
enum WalkMutSource {
    Entries,
    Values,
    Range { start: InputBound, end: InputBound },
}

/// How a walk takes what is left after its steps.
#[derive(Arbitrary, Debug, Clone, Copy)]
enum WalkFinish {
    Fold,
    Rfold,
    Count,
    Last,
    Min,
    Max,
    /// Walks a clone of the iterator to the end, then the iterator itself
    /// from the back.
    Cloned,
}

/// One operation on both maps.
#[derive(Arbitrary, Debug)]
enum MapOperation {
    GetBatch {
        keys: [KeyRecipe; 8],
    },
    InsertBatch {
        entries: [(KeyRecipe, u16); 8],
    },
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
    /// `get`, `get_key_value` and `contains_key`.
    Get {
        key: KeyRecipe,
    },
    /// The same, by the key's borrowed form.
    GetBorrowed {
        key: KeyRecipe,
    },
    GetMut {
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
    /// Removes `count` keys of consecutive numbers from `first_key`: trees
    /// that shrink by levels from short inputs.
    RemoveRun {
        first_key: KeyRecipe,
        count: u8,
    },
    PopFirst,
    PopLast,
    FirstAndLast,
    Clear,
    /// Takes `steps` items, each from the end `ends` picks, then the rest
    /// as `finish` says; ranges whose bounds are out of order, for which
    /// both maps panic, are skipped.
    Walk {
        source: WalkSource,
        steps: u8,
        ends: Ends,
        finish: WalkFinish,
    },
    /// Takes `steps` items, each from the end `ends` picks, then the rest by
    /// a fold, bumping every value it reaches.
    WalkMut {
        source: WalkMutSource,
        steps: u8,
        ends: Ends,
        amount: u16,
    },
    /// One use of the entry API, picked by `choice`.
    Entry {
        key: KeyRecipe,
        choice: u8,
        value: u16,
    },
    /// The first or last entry, read, changed or removed.
    EndEntry {
        last: bool,
        remove: bool,
        value: u16,
    },
    /// Splits both maps at `key`, compares the parts, and appends the upper
    /// part back, or drops it.
    SplitOff {
        key: KeyRecipe,
        append_back: bool,
    },
    /// Appends a map made of these entries to both maps.
    Append {
        entries: Vec<(KeyRecipe, u16)>,
    },
    /// Keeps the entries that `keep` picks, bumping every value on the way.
    Retain {
        keep: PositionMask,
    },
    /// Extracts the entries in a range that `extract` picks, taking at most
    /// `take` of them; bounds out of order are skipped.
    ExtractIf {
        start: InputBound,
        end: InputBound,
        extract: PositionMask,
        take: u8,
    },
    /// Replaces both maps by their clones.
    Clone,
    /// Replaces both maps by maps collected from these entries.
    Collect {
        entries: Vec<(KeyRecipe, u16)>,
    },
    Extend {
        entries: Vec<(KeyRecipe, u16)>,
    },
    /// Takes both maps by value and takes `count` entries, each from the end
    /// `ends` picks, then drops the rest: the maps start over empty.
    IntoIter {
        ends: Ends,
        count: u8,
    },
}

/// The value type of the maps under test, made from fuzzer input.
pub(crate) trait FuzzValue: Clone + Ord + Debug {
    fn from_input(input: u16) -> Self;

    /// Changes the value by `amount`, the same way every time.
    fn bump(&mut self, amount: u16);
}

impl FuzzValue for u64 {
    fn from_input(input: u16) -> Self {
        u64::from(input)
    }

    fn bump(&mut self, amount: u16) {
        *self = self.wrapping_add(u64::from(amount));
    }
}

impl FuzzValue for u128 {
    fn from_input(input: u16) -> Self {
        u128::from(input)
    }

    fn bump(&mut self, amount: u16) {
        *self = self.wrapping_add(u128::from(amount));
    }
}

/// A 328-byte value that owns heap memory: leaves of about a dozen entries,
/// the minimum (D13), and a value the map loses or drops twice is a leak or
/// a double free, which AddressSanitizer reports.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct BulkyValue {
    words: [u64; 40],
    owned: Box<u16>,
}

impl FuzzValue for BulkyValue {
    fn from_input(input: u16) -> Self {
        Self {
            words: std::array::from_fn(|index| u64::from(input) + index as u64),
            owned: Box::new(input),
        }
    }

    fn bump(&mut self, amount: u16) {
        self.words[0] = self.words[0].wrapping_add(u64::from(amount));
        *self.owned = self.owned.wrapping_add(amount);
    }
}

/// A strategy the map targets run, and how they make a map of it from
/// entries, the unsync map and the sync one alike: as `collect` does, which
/// std's API offers for the default strategy only (`map/from_entries.rs`),
/// or by `extend`.
pub(crate) trait FuzzStrategy<K: Ord>: InlineKeyStrategy<K> + Default {
    fn collect<V>(entries: Vec<(K, V)>) -> BBTreeMap<K, V, Self>;

    fn collect_sync<V>(entries: Vec<(K, V)>) -> SyncMap<K, V, Self>;
}

impl<K: Ord> FuzzStrategy<K> for Auto {
    fn collect<V>(entries: Vec<(K, V)>) -> BBTreeMap<K, V> {
        entries.into_iter().collect()
    }

    fn collect_sync<V>(entries: Vec<(K, V)>) -> SyncMap<K, V> {
        entries.into_iter().collect()
    }
}

impl<K: Ord + InlineKey> FuzzStrategy<K> for WithInlineKey {
    fn collect<V>(entries: Vec<(K, V)>) -> BBTreeMap<K, V, Self> {
        let mut map = BBTreeMap::with_strategy(Self);
        map.extend(entries);
        map
    }

    fn collect_sync<V>(entries: Vec<(K, V)>) -> SyncMap<K, V, Self> {
        let mut map = SyncMap::with_strategy(Self);
        map.extend(entries);
        map
    }
}

/// Our map, with strategy `S`, and std's, fed the same operations.
struct MirroredMaps<'i, K, V, S: InlineKeyStrategy<K> = Auto> {
    stems: &'i Stems,
    ours: BBTreeMap<K, V, S>,
    theirs: BTreeMap<K, V>,
}

impl<'i, K, V, S> MirroredMaps<'i, K, V, S>
where
    K: FuzzKey,
    V: FuzzValue,
    S: FuzzStrategy<K>,
{
    /// Runs the input's operations on a fresh pair of maps, comparing every
    /// answer and checking our tree after each, and at the end every entry.
    fn run(input: &'i MapInput) {
        let mut maps = Self {
            stems: &input.stems,
            ours: BBTreeMap::with_strategy(S::default()),
            theirs: BTreeMap::new(),
        };
        for operation in &input.operations {
            maps.apply(operation);
            assert_eq!(maps.ours.len(), maps.theirs.len());
            maps.ours.check_invariants();
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

    fn apply(&mut self, operation: &MapOperation) {
        match *operation {
            MapOperation::GetBatch { keys } => {
                let keys = keys.map(|key| self.key(key));
                let queries = keys.each_ref();
                assert_eq!(
                    self.ours.get_many(queries),
                    queries.map(|key| self.theirs.get(key))
                );
            }
            MapOperation::InsertBatch { entries } => {
                let entries = entries.map(|(key, value)| (self.key(key), V::from_input(value)));
                let mut individual = self.ours.clone();
                let expected = entries
                    .clone()
                    .map(|(key, value)| self.theirs.insert(key, value));
                let sequential = entries
                    .clone()
                    .map(|(key, value)| individual.insert(key, value));
                assert_eq!(self.ours.insert_many(entries), expected);
                assert_eq!(sequential, expected);
                self.ours.check_same_nodes(&individual);
                assert!(self.ours.iter().eq(individual.iter()));
            }
            MapOperation::Insert { key, value } => {
                self.insert(self.key(key), V::from_input(value));
            }
            MapOperation::InsertRun {
                first_key,
                count,
                value,
            } => {
                for offset in 0..u16::from(count) {
                    let key = self.key(first_key.after(offset));
                    self.insert(key, V::from_input(value.wrapping_add(offset)));
                }
            }
            MapOperation::Get { key } => self.assert_same_lookup(&self.key(key)),
            MapOperation::GetBorrowed { key } => {
                self.key(key).look_up_borrowed(BorrowedGet(self));
            }
            MapOperation::GetMut { key, value } => {
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
            MapOperation::Remove { key } => {
                let key = self.key(key);
                assert_eq!(self.ours.remove(&key), self.theirs.remove(&key));
            }
            MapOperation::RemoveBorrowed { key } => {
                self.key(key).look_up_borrowed(BorrowedRemove(self));
            }
            MapOperation::RemoveEntry { key } => {
                let key = self.key(key);
                assert_eq!(self.ours.remove_entry(&key), self.theirs.remove_entry(&key));
            }
            MapOperation::RemoveRun { first_key, count } => {
                for offset in 0..u16::from(count) {
                    let key = self.key(first_key.after(offset));
                    assert_eq!(self.ours.remove(&key), self.theirs.remove(&key));
                }
            }
            MapOperation::PopFirst => {
                assert_eq!(self.ours.pop_first(), self.theirs.pop_first());
            }
            MapOperation::PopLast => {
                assert_eq!(self.ours.pop_last(), self.theirs.pop_last());
            }
            MapOperation::FirstAndLast => {
                assert_eq!(self.ours.first_key_value(), self.theirs.first_key_value());
                assert_eq!(self.ours.last_key_value(), self.theirs.last_key_value());
            }
            MapOperation::Clear => {
                self.ours.clear();
                self.theirs.clear();
            }
            MapOperation::Walk {
                source,
                steps,
                ends,
                finish,
            } => self.walk(source, Steps { steps, ends }, finish),
            MapOperation::WalkMut {
                source,
                steps,
                ends,
                amount,
            } => self.walk_mut(source, Steps { steps, ends }, amount),
            MapOperation::Entry { key, choice, value } => {
                self.use_entry(self.key(key), choice, V::from_input(value));
            }
            MapOperation::EndEntry {
                last,
                remove,
                value,
            } => {
                let (ours, theirs) = if last {
                    (self.ours.last_entry(), self.theirs.last_entry())
                } else {
                    (self.ours.first_entry(), self.theirs.first_entry())
                };
                match (ours, theirs) {
                    (Some(ours), Some(theirs)) if remove => {
                        assert_eq!(ours.remove_entry(), theirs.remove_entry());
                    }
                    (Some(mut ours), Some(mut theirs)) => {
                        assert_eq!(
                            ours.insert(V::from_input(value)),
                            theirs.insert(V::from_input(value))
                        );
                    }
                    (ours, theirs) => assert_eq!(ours.is_none(), theirs.is_none()),
                }
            }
            MapOperation::SplitOff { key, append_back } => {
                let key = self.key(key);
                let (mut ours, mut theirs) =
                    (self.ours.split_off(&key), self.theirs.split_off(&key));
                ours.check_invariants();
                assert!(ours.iter().eq(theirs.iter()));
                self.assert_same_entries();
                if append_back {
                    self.ours.append(&mut ours);
                    self.theirs.append(&mut theirs);
                    assert!(ours.is_empty() && theirs.is_empty());
                }
            }
            MapOperation::Append { ref entries } => {
                let entries = self.entries(entries);
                let mut ours = S::collect(entries.clone());
                let mut theirs: BTreeMap<K, V> = entries.into_iter().collect();
                self.ours.append(&mut ours);
                self.theirs.append(&mut theirs);
                assert!(ours.is_empty() && theirs.is_empty());
            }
            MapOperation::Retain { keep } => {
                let (mut ours, mut theirs) = (keep.picker(), keep.picker());
                self.ours.retain(|_, value| {
                    value.bump(1);
                    ours()
                });
                self.theirs.retain(|_, value| {
                    value.bump(1);
                    theirs()
                });
            }
            MapOperation::ExtractIf {
                start,
                end,
                extract,
                take,
            } => {
                if let Some(bounds) = self.key_range(start, end) {
                    let (mut ours, mut theirs) = (extract.picker(), extract.picker());
                    let ours: Vec<_> = (self.ours.extract_if(bounds.clone(), |_, _| ours()))
                        .take(usize::from(take))
                        .collect();
                    let theirs: Vec<_> = (self.theirs.extract_if(bounds, |_, _| theirs()))
                        .take(usize::from(take))
                        .collect();
                    assert_eq!(ours, theirs);
                }
            }
            MapOperation::Clone => {
                self.ours = self.ours.clone();
                self.theirs = self.theirs.clone();
                self.assert_same_entries();
            }
            MapOperation::Collect { ref entries } => {
                let entries = self.entries(entries);
                self.ours = S::collect(entries.clone());
                self.theirs = entries.into_iter().collect();
                self.assert_same_entries();
            }
            MapOperation::Extend { ref entries } => {
                let entries = self.entries(entries);
                let mut individual = self.ours.clone();
                for (key, value) in entries.iter().cloned() {
                    individual.insert(key, value);
                }
                self.ours.extend(entries.iter().cloned());
                self.theirs.extend(entries);
                self.ours.check_same_nodes(&individual);
                assert!(self.ours.iter().eq(individual.iter()));
            }
            MapOperation::IntoIter { ends, count } => {
                let empty = BBTreeMap::with_strategy(S::default());
                let mut ours = std::mem::replace(&mut self.ours, empty).into_iter();
                let mut theirs = std::mem::take(&mut self.theirs).into_iter();
                for step in 0..usize::from(count) {
                    assert_eq!(ours.len(), theirs.len());
                    let entries = if ends.takes_back(step) {
                        (ours.next_back(), theirs.next_back())
                    } else {
                        (ours.next(), theirs.next())
                    };
                    assert_eq!(entries.0, entries.1);
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
        assert_eq!(self.ours.get(key), self.theirs.get(key));
        assert_eq!(self.ours.get_key_value(key), self.theirs.get_key_value(key));
        assert_eq!(self.ours.contains_key(key), self.theirs.contains_key(key));
    }

    fn walk(&self, source: WalkSource, steps: Steps, finish: WalkFinish) {
        match source {
            WalkSource::Entries => {
                steps.assert_same_walk(self.ours.iter(), self.theirs.iter(), finish)
            }
            WalkSource::Keys => {
                steps.assert_same_walk(self.ours.keys(), self.theirs.keys(), finish)
            }
            WalkSource::Values => {
                steps.assert_same_walk(self.ours.values(), self.theirs.values(), finish);
            }
            WalkSource::Range { start, end } => {
                if let Some(bounds) = self.key_range(start, end) {
                    let (ours, theirs) =
                        (self.ours.range(bounds.clone()), self.theirs.range(bounds));
                    steps.assert_same_walk(ours, theirs, finish);
                }
            }
        }
    }

    fn walk_mut(&mut self, source: WalkMutSource, steps: Steps, amount: u16) {
        match source {
            WalkMutSource::Entries => {
                let (ours, theirs) = (self.ours.iter_mut(), self.theirs.iter_mut());
                steps.assert_same_walk_mut(ours, theirs, |(_, value)| value.bump(amount));
            }
            WalkMutSource::Values => {
                let (ours, theirs) = (self.ours.values_mut(), self.theirs.values_mut());
                steps.assert_same_walk_mut(ours, theirs, |value| value.bump(amount));
            }
            WalkMutSource::Range { start, end } => {
                if let Some(bounds) = self.key_range(start, end) {
                    let ours = self.ours.range_mut(bounds.clone());
                    let theirs = self.theirs.range_mut(bounds);
                    steps.assert_same_walk_mut(ours, theirs, |(_, value)| value.bump(amount));
                }
            }
        }
    }

    /// One use of the entry API for `key` in both maps, picked by `choice`:
    /// the same entries come back, and the same values go in.
    fn use_entry(&mut self, key: K, choice: u8, value: V) {
        let (ours, theirs) = (self.ours.entry(key.clone()), self.theirs.entry(key));
        assert_eq!(ours.key(), theirs.key());
        match (ours, theirs, choice % 5) {
            (ours, theirs, 0) => {
                let (ours, theirs) = (ours.or_insert(value.clone()), theirs.or_insert(value));
                assert_eq!(ours, theirs);
                ours.bump(1);
                theirs.bump(1);
            }
            (ours, theirs, 1) => {
                let ours =
                    (ours.and_modify(|value| value.bump(2))).or_insert_with_key(|_| value.clone());
                let theirs =
                    (theirs.and_modify(|value| value.bump(2))).or_insert_with_key(|_| value);
                assert_eq!(ours, theirs);
            }
            (Entry::Occupied(ours), btree_map::Entry::Occupied(theirs), 2) => {
                assert_eq!(ours.remove_entry(), theirs.remove_entry());
            }
            (Entry::Vacant(ours), btree_map::Entry::Vacant(theirs), 2) => {
                assert_eq!(ours.into_key(), theirs.into_key());
            }
            (Entry::Occupied(mut ours), btree_map::Entry::Occupied(mut theirs), 3) => {
                assert_eq!(ours.insert(value.clone()), theirs.insert(value));
            }
            (Entry::Occupied(mut ours), btree_map::Entry::Occupied(mut theirs), _) => {
                ours.get_mut().bump(3);
                theirs.get_mut().bump(3);
                assert_eq!(ours.into_mut(), theirs.into_mut());
            }
            (Entry::Vacant(ours), btree_map::Entry::Vacant(theirs), _) => {
                let (ours, theirs) = (ours.insert_entry(value.clone()), theirs.insert_entry(value));
                assert_eq!((ours.key(), ours.get()), (theirs.key(), theirs.get()));
            }
            _ => panic!("one entry is occupied, the other vacant"),
        }
    }

    fn assert_same_entries(&self) {
        assert_eq!(self.ours.len(), self.theirs.len());
        assert!(self.ours.iter().eq(self.theirs.iter()));
        for (key, value) in &self.theirs {
            assert_eq!(self.ours.get(key), Some(value));
        }
    }
}

/// `get`, `get_key_value` and `contains_key` through a borrowed form of the
/// key, in both maps.
struct BorrowedGet<'m, 'i, K, V, S: InlineKeyStrategy<K>>(&'m MirroredMaps<'i, K, V, S>);

impl<K: FuzzKey, V: FuzzValue, S: FuzzStrategy<K>> BorrowedLookup<K>
    for BorrowedGet<'_, '_, K, V, S>
{
    fn look_up<Q: Ord + Debug + ?Sized>(self, key: &Q)
    where
        K: Borrow<Q>,
    {
        self.0.assert_same_lookup(key);
    }
}

/// `remove` through a borrowed form of the key, in both maps.
struct BorrowedRemove<'m, 'i, K, V, S: InlineKeyStrategy<K>>(&'m mut MirroredMaps<'i, K, V, S>);

impl<K: FuzzKey, V: FuzzValue, S: FuzzStrategy<K>> BorrowedLookup<K>
    for BorrowedRemove<'_, '_, K, V, S>
{
    fn look_up<Q: Ord + Debug + ?Sized>(self, key: &Q)
    where
        K: Borrow<Q>,
    {
        assert_eq!(self.0.ours.remove(key), self.0.theirs.remove(key));
    }
}

/// How a walk takes its first items: `steps` of them, each from the end
/// `ends` picks.
#[derive(Arbitrary, Debug, Clone, Copy)]
pub(crate) struct Steps {
    steps: u8,
    ends: Ends,
}

impl Steps {
    /// Takes the first items of both iterators, each from the end `ends`
    /// picks, until they run out: they must match, and `bump` changes each
    /// one, in both. Their size hints aren't compared: the sync map's
    /// iterators give none.
    pub(crate) fn assert_same_steps<T: PartialEq + Debug>(
        self,
        ours: &mut impl DoubleEndedIterator<Item = T>,
        theirs: &mut impl DoubleEndedIterator<Item = T>,
        mut bump: impl FnMut(&mut T),
    ) {
        for step in 0..usize::from(self.steps) {
            let items = if self.ends.takes_back(step) {
                (ours.next_back(), theirs.next_back())
            } else {
                (ours.next(), theirs.next())
            };
            let (Some(mut our_item), Some(mut their_item)) = items else {
                assert!(items.0.is_none() && items.1.is_none());
                return;
            };
            assert_eq!(our_item, their_item);
            bump(&mut our_item);
            bump(&mut their_item);
        }
    }

    /// Takes the first items of both iterators, then the rest as `finish`
    /// says: they must match all along, and so must their size hints.
    fn assert_same_walk<I, J>(self, mut ours: I, mut theirs: J, finish: WalkFinish)
    where
        I: DoubleEndedIterator + Clone,
        J: DoubleEndedIterator<Item = I::Item> + Clone,
        I::Item: Ord + Debug,
    {
        for step in 0..usize::from(self.steps) {
            assert_eq!(ours.size_hint(), theirs.size_hint());
            if self.ends.takes_back(step) {
                assert_eq!(ours.next_back(), theirs.next_back());
            } else {
                assert_eq!(ours.next(), theirs.next());
            }
        }
        assert_eq!(ours.size_hint(), theirs.size_hint());
        let collect = |mut items: Vec<I::Item>, item| {
            items.push(item);
            items
        };
        match finish {
            WalkFinish::Fold => {
                assert_eq!(ours.fold(vec![], collect), theirs.fold(vec![], collect))
            }
            WalkFinish::Rfold => {
                assert_eq!(ours.rfold(vec![], collect), theirs.rfold(vec![], collect));
            }
            WalkFinish::Count => assert_eq!(ours.count(), theirs.count()),
            WalkFinish::Last => assert_eq!(ours.last(), theirs.last()),
            WalkFinish::Min => assert_eq!(ours.min(), theirs.min()),
            WalkFinish::Max => assert_eq!(ours.max(), theirs.max()),
            WalkFinish::Cloned => {
                assert!(ours.clone().eq(theirs.clone()));
                assert!(ours.rev().eq(theirs.rev()));
            }
        }
    }

    /// Takes the first items of both mutable iterators, then the rest by a
    /// fold: they must match all along. `bump` changes each item's value.
    fn assert_same_walk_mut<T: PartialEq + Debug>(
        self,
        mut ours: impl DoubleEndedIterator<Item = T>,
        mut theirs: impl DoubleEndedIterator<Item = T>,
        mut bump: impl FnMut(&mut T),
    ) {
        for step in 0..usize::from(self.steps) {
            assert_eq!(ours.size_hint(), theirs.size_hint());
            let items = if self.ends.takes_back(step) {
                (ours.next_back(), theirs.next_back())
            } else {
                (ours.next(), theirs.next())
            };
            let (Some(mut our_item), Some(mut their_item)) = items else {
                assert!(items.0.is_none() && items.1.is_none());
                return;
            };
            assert_eq!(our_item, their_item);
            bump(&mut our_item);
            bump(&mut their_item);
        }
        let collect = |mut items: Vec<T>, item| {
            items.push(item);
            items
        };
        let (mut ours, mut theirs) = (ours.fold(vec![], collect), theirs.fold(vec![], collect));
        assert_eq!(ours, theirs);
        ours.iter_mut().for_each(&mut bump);
        theirs.iter_mut().for_each(&mut bump);
    }
}
