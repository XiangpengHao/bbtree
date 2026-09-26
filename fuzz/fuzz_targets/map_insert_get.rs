//! Differential fuzzing (D5): arbitrary insert, lookup, removal and
//! iteration operations on a `BBTreeMap` and on std's `BTreeMap`, which must
//! answer every one alike.

#![no_main]

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::ops::Bound;

use bbtree::BBTreeMap;
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
}

/// The value type of the maps under test, made from fuzzer input.
trait FuzzValue: Clone + PartialEq + Debug {
    fn from_input(input: u16) -> Self;
}

impl FuzzValue for u64 {
    fn from_input(input: u16) -> Self {
        u64::from(input)
    }
}

/// With `u64` keys, 12 entries fill a 4 KB node (D13).
type TwelvePerNode = [u64; 40];

impl FuzzValue for TwelvePerNode {
    fn from_input(input: u16) -> Self {
        std::array::from_fn(|index| u64::from(input) + index as u64)
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
            MapOperation::Range { start, end, ends } => {
                let bounds = (start.map_bound(), end.map_bound());
                let out_of_order = match bounds {
                    (Bound::Excluded(start), Bound::Excluded(end)) => start >= end,
                    (
                        Bound::Included(start) | Bound::Excluded(start),
                        Bound::Included(end) | Bound::Excluded(end),
                    ) => start > end,
                    _ => false,
                };
                if !out_of_order {
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
