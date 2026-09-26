//! Differential fuzzing (D5): arbitrary insert, lookup and removal
//! operations on a `BBTreeMap` and on std's `BTreeMap`, which must answer
//! every one alike.

#![no_main]

use std::collections::BTreeMap;
use std::fmt::Debug;

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
        }
        assert_eq!(self.ours.len(), self.theirs.len());
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
