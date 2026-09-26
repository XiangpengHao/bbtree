//! Differential fuzzing (D5): arbitrary insert, lookup, removal and
//! iteration operations on a `BBTreeMap` and on std's `BTreeMap`, which must
//! answer every one alike.

#![no_main]

use std::collections::{BTreeMap, btree_map};
use std::fmt::Debug;
use std::ops::Bound;

use bbtree::BBTreeMap;
use bbtree::bbtree_map::Entry;
use libfuzzer_sys::arbitrary::{self, Arbitrary};
use libfuzzer_sys::fuzz_target;

/// A key from the input, standing for a map key in `0..KEY_SPACE`.
#[derive(Arbitrary, Debug, Clone, Copy)]
struct InputKey(u16);

impl InputKey {
    /// Small enough for inserts to collide, large enough for every node kind
    /// to split.
    const KEY_SPACE: u16 = 4096;

    fn map_key(self) -> u64 {
        u64::from(self.0 % Self::KEY_SPACE)
    }

    /// The key `offset` places after this one, wrapping around the key space.
    fn after(self, offset: u16) -> Self {
        Self(self.0.wrapping_add(offset) % Self::KEY_SPACE)
    }
}

/// A range bound from the input.
#[derive(Arbitrary, Debug, Clone, Copy)]
enum InputBound {
    Included(InputKey),
    Excluded(InputKey),
    Unbounded,
}

impl InputBound {
    fn map_bound(self) -> Bound<u64> {
        match self {
            Self::Included(key) => Bound::Included(key.map_key()),
            Self::Excluded(key) => Bound::Excluded(key.map_key()),
            Self::Unbounded => Bound::Unbounded,
        }
    }
}

/// Which end each step of a walk takes from: bit `i % 32` for step `i`, set
/// for the back.
#[derive(Arbitrary, Debug, Clone, Copy)]
struct Ends(u32);

impl Ends {
    fn from_back(self, step: usize) -> bool {
        self.0 >> (step % 32) & 1 == 1
    }
}

/// One operation on both maps.
#[derive(Arbitrary, Debug)]
enum MapOperation {
    Insert {
        key: InputKey,
        value: u16,
    },
    /// Inserts `count` consecutive keys from `first_key`: tall trees from
    /// short inputs.
    InsertRun {
        first_key: InputKey,
        count: u8,
        value: u16,
    },
    Get {
        key: InputKey,
    },
    GetMut {
        key: InputKey,
        value: u16,
    },
    ContainsKey {
        key: InputKey,
    },
    Len,
    Remove {
        key: InputKey,
    },
    RemoveEntry {
        key: InputKey,
    },
    /// Removes `count` consecutive keys from `first_key`: trees that shrink
    /// by many levels from short inputs.
    RemoveRun {
        first_key: InputKey,
        count: u8,
    },
    PopFirst,
    PopLast,
    FirstAndLast,
    Clear,
    /// Walks the whole maps from both ends.
    Iterate {
        ends: Ends,
    },
    /// Walks a range from both ends; bounds out of order, for which both
    /// maps panic, are skipped.
    Range {
        start: InputBound,
        end: InputBound,
        ends: Ends,
    },
    /// Walks the whole maps mutably from both ends, bumping every value.
    IterateMut {
        ends: Ends,
        amount: u16,
    },
    /// One use of the entry API, picked by `choice`, writing `value` through
    /// whatever reference it hands back.
    Entry {
        key: InputKey,
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
        key: InputKey,
        append_back: bool,
    },
    /// Appends a map made of these entries to both maps.
    Append {
        entries: Vec<(InputKey, u16)>,
    },
    /// Keeps the entries whose bit of `keep` (by key, mod 64) is set,
    /// bumping every value on the way.
    Retain {
        keep: u64,
    },
    /// Extracts the entries in a range whose bit of `extract` (by key, mod
    /// 64) is set, taking at most `take` of them; bounds out of order are
    /// skipped.
    ExtractIf {
        start: InputBound,
        end: InputBound,
        extract: u64,
        take: u8,
    },
    /// Replaces both maps by their clones.
    Clone,
    /// Replaces both maps by maps collected from these entries.
    Collect {
        entries: Vec<(InputKey, u16)>,
    },
    /// Extends both maps with these entries.
    Extend {
        entries: Vec<(InputKey, u16)>,
    },
    /// Takes both maps by value and walks `count` entries from both ends,
    /// then drops the rest: the maps start over empty.
    IntoIter {
        ends: Ends,
        count: u8,
    },
    /// Walks a range mutably from both ends, bumping every value; bounds out
    /// of order are skipped.
    RangeMut {
        start: InputBound,
        end: InputBound,
        ends: Ends,
        amount: u16,
    },
}

/// The value type of the maps under test, made from fuzzer input.
trait FuzzValue: Clone + PartialEq + Debug {
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

/// With `u64` keys, 12 entries fill a 4 KB node (D13).
type TwelvePerNode = [u64; 40];

impl FuzzValue for TwelvePerNode {
    fn from_input(input: u16) -> Self {
        std::array::from_fn(|index| u64::from(input) + index as u64)
    }

    fn bump(&mut self, amount: u16) {
        self[0] = self[0].wrapping_add(u64::from(amount));
    }
}

/// Which maps the operations run on.
#[derive(Arbitrary, Debug)]
enum FuzzInput {
    /// `u64` to `u64`: 255 entries per leaf, 170 per internal node.
    Words(Vec<MapOperation>),
    /// `u64` to 320-byte values: 12 entries per node.
    TwelvePerNode(Vec<MapOperation>),
}

/// Our map and std's, fed the same operations.
struct MirroredMaps<V> {
    ours: BBTreeMap<u64, V>,
    theirs: BTreeMap<u64, V>,
}

impl<V: FuzzValue> MirroredMaps<V> {
    /// Runs `operations` on a fresh pair of maps, comparing every answer and,
    /// at the end, every entry.
    fn run(operations: &[MapOperation]) {
        let mut maps = Self {
            ours: BBTreeMap::new(),
            theirs: BTreeMap::new(),
        };
        for operation in operations {
            maps.apply(operation);
        }
        maps.assert_same_entries();
    }

    fn apply(&mut self, operation: &MapOperation) {
        match *operation {
            MapOperation::Insert { key, value } => {
                self.insert(key.map_key(), V::from_input(value));
            }
            MapOperation::InsertRun {
                first_key,
                count,
                value,
            } => {
                for offset in 0..u16::from(count) {
                    let key = first_key.after(offset).map_key();
                    self.insert(key, V::from_input(value.wrapping_add(offset)));
                }
            }
            MapOperation::Get { key } => {
                let key = key.map_key();
                assert_eq!(self.ours.get(&key), self.theirs.get(&key));
            }
            MapOperation::GetMut { key, value } => {
                let key = key.map_key();
                match (self.ours.get_mut(&key), self.theirs.get_mut(&key)) {
                    (Some(ours), Some(theirs)) => {
                        assert_eq!(ours, theirs);
                        *ours = V::from_input(value);
                        *theirs = V::from_input(value);
                    }
                    (ours, theirs) => assert_eq!(ours, theirs),
                }
            }
            MapOperation::ContainsKey { key } => {
                let key = key.map_key();
                assert_eq!(self.ours.contains_key(&key), self.theirs.contains_key(&key));
            }
            MapOperation::Len => {
                assert_eq!(self.ours.len(), self.theirs.len());
                assert_eq!(self.ours.is_empty(), self.theirs.is_empty());
            }
            MapOperation::Remove { key } => {
                let key = key.map_key();
                assert_eq!(self.ours.remove(&key), self.theirs.remove(&key));
            }
            MapOperation::RemoveEntry { key } => {
                let key = key.map_key();
                assert_eq!(self.ours.remove_entry(&key), self.theirs.remove_entry(&key));
            }
            MapOperation::RemoveRun { first_key, count } => {
                for offset in 0..u16::from(count) {
                    let key = first_key.after(offset).map_key();
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
            MapOperation::Iterate { ends } => {
                assert_eq!(self.ours.iter().len(), self.theirs.iter().len());
                Self::assert_same_walk(self.ours.iter(), self.theirs.iter(), ends);
            }
            MapOperation::Entry { key, choice, value } => {
                let key = key.map_key();
                let (ours, theirs) = (self.ours.entry(key), self.theirs.entry(key));
                assert_eq!(ours.key(), theirs.key());
                match (ours, theirs, choice % 4) {
                    (ours, theirs, 0) => {
                        let (ours, theirs) = (
                            ours.or_insert(V::from_input(value)),
                            theirs.or_insert(V::from_input(value)),
                        );
                        assert_eq!(ours, theirs);
                        ours.bump(1);
                        theirs.bump(1);
                    }
                    (ours, theirs, 1) => {
                        let ours = ours
                            .and_modify(|value| value.bump(2))
                            .or_insert(V::from_input(value));
                        let theirs = theirs
                            .and_modify(|value| value.bump(2))
                            .or_insert(V::from_input(value));
                        assert_eq!(ours, theirs);
                    }
                    (Entry::Occupied(ours), btree_map::Entry::Occupied(theirs), 2) => {
                        assert_eq!(ours.remove_entry(), theirs.remove_entry());
                    }
                    (Entry::Occupied(mut ours), btree_map::Entry::Occupied(mut theirs), _) => {
                        assert_eq!(
                            ours.insert(V::from_input(value)),
                            theirs.insert(V::from_input(value))
                        );
                    }
                    (Entry::Vacant(ours), btree_map::Entry::Vacant(theirs), _) => {
                        let (ours, theirs) = (
                            ours.insert_entry(V::from_input(value)),
                            theirs.insert_entry(V::from_input(value)),
                        );
                        assert_eq!((ours.key(), ours.get()), (theirs.key(), theirs.get()));
                    }
                    _ => panic!("one entry is occupied, the other vacant"),
                }
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
                        assert_eq!(ours.remove_entry(), theirs.remove_entry())
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
                let key = key.map_key();
                let (mut ours, mut theirs) =
                    (self.ours.split_off(&key), self.theirs.split_off(&key));
                assert!(ours.iter().eq(theirs.iter()));
                self.assert_same_entries();
                if append_back {
                    self.ours.append(&mut ours);
                    self.theirs.append(&mut theirs);
                    assert!(ours.is_empty() && theirs.is_empty());
                }
            }
            MapOperation::Append { ref entries } => {
                let entries = || {
                    entries
                        .iter()
                        .map(|&(key, value)| (key.map_key(), V::from_input(value)))
                };
                let mut ours: BBTreeMap<u64, V> = entries().collect();
                let mut theirs: BTreeMap<u64, V> = entries().collect();
                self.ours.append(&mut ours);
                self.theirs.append(&mut theirs);
                assert!(ours.is_empty() && theirs.is_empty());
            }
            MapOperation::Retain { keep } => {
                let keep = |key: &u64, value: &mut V| {
                    value.bump(1);
                    keep >> (key % 64) & 1 == 1
                };
                self.ours.retain(keep);
                self.theirs.retain(keep);
            }
            MapOperation::ExtractIf {
                start,
                end,
                extract,
                take,
            } => {
                let bounds = (start.map_bound(), end.map_bound());
                if !Self::out_of_order(bounds) {
                    let extract = |key: &u64, _: &mut V| extract >> (key % 64) & 1 == 1;
                    let ours: Vec<_> = self
                        .ours
                        .extract_if(bounds, extract)
                        .take(usize::from(take))
                        .collect();
                    let theirs: Vec<_> = self
                        .theirs
                        .extract_if(bounds, extract)
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
                let entries = || {
                    entries
                        .iter()
                        .map(|&(key, value)| (key.map_key(), V::from_input(value)))
                };
                self.ours = entries().collect();
                self.theirs = entries().collect();
                self.assert_same_entries();
            }
            MapOperation::Extend { ref entries } => {
                let entries = || {
                    entries
                        .iter()
                        .map(|&(key, value)| (key.map_key(), V::from_input(value)))
                };
                self.ours.extend(entries());
                self.theirs.extend(entries());
            }
            MapOperation::IntoIter { ends, count } => {
                let mut ours = std::mem::take(&mut self.ours).into_iter();
                let mut theirs = std::mem::take(&mut self.theirs).into_iter();
                for step in 0..usize::from(count) {
                    assert_eq!(ours.len(), theirs.len());
                    let entries = if ends.from_back(step) {
                        (ours.next_back(), theirs.next_back())
                    } else {
                        (ours.next(), theirs.next())
                    };
                    assert_eq!(entries.0, entries.1);
                }
            }
            MapOperation::IterateMut { ends, amount } => {
                Self::assert_same_walk_mut(
                    self.ours.iter_mut(),
                    self.theirs.iter_mut(),
                    ends,
                    amount,
                );
            }
            MapOperation::RangeMut {
                start,
                end,
                ends,
                amount,
            } => {
                let bounds = (start.map_bound(), end.map_bound());
                if !Self::out_of_order(bounds) {
                    let (ours, theirs) =
                        (self.ours.range_mut(bounds), self.theirs.range_mut(bounds));
                    Self::assert_same_walk_mut(ours, theirs, ends, amount);
                }
            }
            MapOperation::Range { start, end, ends } => {
                let bounds = (start.map_bound(), end.map_bound());
                if !Self::out_of_order(bounds) {
                    Self::assert_same_walk(
                        self.ours.range(bounds),
                        self.theirs.range(bounds),
                        ends,
                    );
                }
            }
        }
        assert_eq!(self.ours.len(), self.theirs.len());
    }

    /// Whether both maps panic for these range bounds.
    fn out_of_order(bounds: (Bound<u64>, Bound<u64>)) -> bool {
        match bounds {
            (Bound::Excluded(start), Bound::Excluded(end)) => start >= end,
            (
                Bound::Included(start) | Bound::Excluded(start),
                Bound::Included(end) | Bound::Excluded(end),
            ) => start > end,
            _ => false,
        }
    }

    /// Takes every item of both mutable iterators, each step from the end
    /// `ends` picks, checking that they match and bumping each value.
    fn assert_same_walk_mut<'a>(
        mut ours: impl DoubleEndedIterator<Item = (&'a u64, &'a mut V)>,
        mut theirs: impl DoubleEndedIterator<Item = (&'a u64, &'a mut V)>,
        ends: Ends,
        amount: u16,
    ) where
        V: 'a,
    {
        for step in 0.. {
            let entries = if ends.from_back(step) {
                (ours.next_back(), theirs.next_back())
            } else {
                (ours.next(), theirs.next())
            };
            let (Some((our_key, our_value)), Some((their_key, their_value))) = entries else {
                assert!(entries.0.is_none() && entries.1.is_none());
                return;
            };
            assert_eq!((our_key, &*our_value), (their_key, &*their_value));
            our_value.bump(amount);
            their_value.bump(amount);
        }
    }

    /// Takes every item of both iterators, each step from the end `ends`
    /// picks, checking that they match.
    fn assert_same_walk<'a>(
        mut ours: impl DoubleEndedIterator<Item = (&'a u64, &'a V)>,
        mut theirs: impl DoubleEndedIterator<Item = (&'a u64, &'a V)>,
        ends: Ends,
    ) where
        V: 'a,
    {
        for step in 0.. {
            let (ours_item, theirs_item) = if ends.from_back(step) {
                (ours.next_back(), theirs.next_back())
            } else {
                (ours.next(), theirs.next())
            };
            assert_eq!(ours_item, theirs_item);
            if theirs_item.is_none() {
                return;
            }
        }
    }

    fn insert(&mut self, key: u64, value: V) {
        let ours = self.ours.insert(key, value.clone());
        assert_eq!(ours, self.theirs.insert(key, value));
        assert_eq!(self.ours.len(), self.theirs.len());
    }

    fn assert_same_entries(&self) {
        assert_eq!(self.ours.len(), self.theirs.len());
        assert_eq!(self.ours.first_key_value(), self.theirs.first_key_value());
        assert_eq!(self.ours.last_key_value(), self.theirs.last_key_value());
        for (key, value) in &self.theirs {
            assert_eq!(self.ours.get(key), Some(value));
        }
    }
}

fuzz_target!(|input: FuzzInput| match input {
    FuzzInput::Words(operations) => MirroredMaps::<u64>::run(&operations),
    FuzzInput::TwelvePerNode(operations) => MirroredMaps::<TwelvePerNode>::run(&operations),
});
