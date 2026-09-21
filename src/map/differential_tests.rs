//! Differential tests: the map against std's `BTreeMap`, fed the same
//! operations, for ordinary, heap-owning, wide, zero-sized and over-aligned
//! keys and values, including every key that ends up as a separator in an
//! internal node, with its value in a leaf's head (D17).

#![forbid(unsafe_code)]

use core::fmt::Debug;
use core::ptr;
use std::collections::BTreeMap;
use std::format;
use std::string::String;
use std::vec::Vec;

use super::BBTreeMap;
use crate::node::test_support::{CacheLineAligned, PageAligned, TestRng};

/// A key or value made from a number. Different numbers make different
/// values as far as the type allows, and keys from ascending numbers ascend.
trait FromNumber {
    fn from_number(number: usize) -> Self;
}

impl FromNumber for u64 {
    fn from_number(number: usize) -> Self {
        number as u64
    }
}

impl FromNumber for u8 {
    fn from_number(number: usize) -> Self {
        number as u8
    }
}

impl FromNumber for () {
    fn from_number(_: usize) -> Self {}
}

impl FromNumber for String {
    fn from_number(number: usize) -> Self {
        format!("{number:010}")
    }
}

/// With `u64` values, 64-byte keys fill a 1 KB node with 14 entries in both
/// kinds (D13, D15, D17): tall trees from a few hundred entries, with little
/// data per entry for Miri to track. Arrays order lexicographically, so keys
/// ascend with their first element.
type WideKey = [u64; 8];

impl FromNumber for WideKey {
    fn from_number(number: usize) -> Self {
        core::array::from_fn(|index| (number as u64).wrapping_mul(41) + index as u64)
    }
}

impl FromNumber for CacheLineAligned {
    fn from_number(number: usize) -> Self {
        Self::new(number as u64)
    }
}

impl FromNumber for PageAligned {
    fn from_number(number: usize) -> Self {
        Self::new(number as u64)
    }
}

/// Our map and std's, fed the same operations: every answer must match.
struct MirroredMaps<K, V> {
    ours: BBTreeMap<K, V>,
    theirs: BTreeMap<K, V>,
}

impl<K, V> MirroredMaps<K, V>
where
    K: Ord + Clone + Debug + FromNumber,
    V: Clone + PartialEq + Debug + FromNumber,
{
    fn new() -> Self {
        Self {
            ours: BBTreeMap::new(),
            theirs: BTreeMap::new(),
        }
    }

    fn insert(&mut self, key: K, value: V) {
        let ours = self.ours.insert(key.clone(), value.clone());
        assert_eq!(ours, self.theirs.insert(key, value));
        self.assert_same_len();
    }

    fn get(&self, key: &K) {
        assert_eq!(self.ours.get(key), self.theirs.get(key));
        assert_eq!(self.ours.get_key_value(key), self.theirs.get_key_value(key));
        assert_eq!(self.ours.contains_key(key), self.theirs.contains_key(key));
    }

    /// Replaces the value of `key` through `get_mut`, if the key is there.
    fn update(&mut self, key: &K, value: V) {
        match (self.ours.get_mut(key), self.theirs.get_mut(key)) {
            (Some(ours), Some(theirs)) => {
                assert_eq!(ours, theirs);
                *ours = value.clone();
                *theirs = value;
            }
            (ours, theirs) => assert_eq!(ours, theirs),
        }
    }

    fn assert_same_len(&self) {
        assert_eq!(self.ours.len(), self.theirs.len());
        assert_eq!(self.ours.is_empty(), self.theirs.is_empty());
    }

    /// Checks the map's invariants, and that it holds exactly std's entries.
    fn assert_same_entries(&self) {
        self.ours.check_invariants();
        self.assert_same_len();
        for (key, value) in &self.theirs {
            assert_eq!(self.ours.get_key_value(key), Some((key, value)));
        }
    }

    /// Gives every key that lives in an internal node a new value, through
    /// `insert` or `get_mut` at random: each time, the value replaced is a
    /// leaf's head (D17).
    fn overwrite_separators(&mut self, rng: &TestRng) {
        let separators: Vec<K> = self.ours.separator_keys().into_iter().cloned().collect();
        assert_eq!(separators.is_empty(), self.ours.height() == 0);
        for key in separators {
            let value = V::from_number(rng.below(1 << 30));
            if rng.below(2) == 0 {
                self.insert(key, value);
            } else {
                self.update(&key, value);
            }
        }
        self.assert_same_entries();
    }

    /// `steps` random operations on keys made from `0..key_space`.
    fn run_random_operations(&mut self, rng: &TestRng, steps: usize, key_space: usize) {
        for _ in 0..steps {
            let key = K::from_number(rng.below(key_space));
            match rng.below(8) {
                0..=3 => self.insert(key, V::from_number(rng.below(1 << 30))),
                4 | 5 => self.get(&key),
                6 => self.update(&key, V::from_number(rng.below(1 << 30))),
                _ => self.assert_same_len(),
            }
        }
        self.assert_same_entries();
    }

    /// Inserts the keys made from the even `numbers`, in their order, then
    /// looks up every key from 0 to the largest number plus one: the even
    /// ones are there, the odd ones fall between them.
    fn insert_evens_in_order(&mut self, numbers: impl IntoIterator<Item = usize>) {
        let mut largest = 0;
        for number in numbers {
            assert!(number.is_multiple_of(2));
            largest = largest.max(number);
            self.insert(K::from_number(number), V::from_number(number));
        }
        self.assert_same_entries();
        for number in 0..=largest + 1 {
            self.get(&K::from_number(number));
        }
    }
}

/// How much work one key and value type gets: large natively, small under
/// Miri.
struct Workload {
    random_steps: usize,
    key_space: usize,
    ordered_entries: usize,
    /// The height ascending and descending inserts must reach. They leave
    /// the nodes they split about half full, so their height is predictable;
    /// shuffled inserts fill nodes fuller and may stay a level lower.
    min_sequential_height: usize,
}

impl Workload {
    /// Random operations, then ascending, descending and shuffled inserts,
    /// each on a fresh pair of maps, and each followed by overwriting every
    /// separator's value.
    fn run<K, V>(&self, seed: u64)
    where
        K: Ord + Clone + Debug + FromNumber,
        V: Clone + PartialEq + Debug + FromNumber,
    {
        let rng = TestRng::new(seed);
        let mut maps = MirroredMaps::<K, V>::new();
        maps.run_random_operations(&rng, self.random_steps, self.key_space);
        maps.overwrite_separators(&rng);

        let evens = || (0..self.ordered_entries).map(|index| 2 * index);
        let sequential_orders: [Vec<usize>; 2] = [evens().collect(), evens().rev().collect()];
        for numbers in sequential_orders {
            let mut maps = MirroredMaps::<K, V>::new();
            maps.insert_evens_in_order(numbers);
            let height = maps.ours.height();
            assert!(
                height >= self.min_sequential_height,
                "height {height} is below {}",
                self.min_sequential_height
            );
            maps.overwrite_separators(&rng);
        }
        let mut shuffled: Vec<usize> = evens().collect();
        rng.shuffle(&mut shuffled);
        let mut maps = MirroredMaps::<K, V>::new();
        maps.insert_evens_in_order(shuffled);
        maps.overwrite_separators(&rng);
    }
}

#[test]
fn u64_to_u64_matches_std() {
    let workload = if cfg!(miri) {
        Workload {
            random_steps: 300,
            key_space: 400,
            ordered_entries: 150,
            min_sequential_height: 1,
        }
    } else {
        Workload {
            random_steps: 500_000,
            key_space: 1 << 18,
            ordered_entries: 200_000,
            min_sequential_height: 2,
        }
    };
    workload.run::<u64, u64>(1);
}

#[test]
fn string_to_string_matches_std() {
    let workload = if cfg!(miri) {
        Workload {
            random_steps: 150,
            key_space: 150,
            ordered_entries: 60,
            min_sequential_height: 1,
        }
    } else {
        Workload {
            random_steps: 100_000,
            key_space: 1 << 16,
            ordered_entries: 50_000,
            min_sequential_height: 2,
        }
    };
    workload.run::<String, String>(2);
}

#[test]
fn wide_keys_match_std() {
    let workload = if cfg!(miri) {
        // Height 2 already has an internal node split into a parent with
        // room; height 3 only adds depth, which the native run covers.
        // Ascending inserts leave half-full nodes: height 2 needs about 130.
        Workload {
            random_steps: 300,
            key_space: 300,
            ordered_entries: 200,
            min_sequential_height: 2,
        }
    } else {
        Workload {
            random_steps: 40_000,
            key_space: 1 << 14,
            ordered_entries: 10_000,
            min_sequential_height: 3,
        }
    };
    workload.run::<WideKey, u64>(3);
}

#[test]
fn zero_sized_keys_and_values_match_std() {
    let mut map = BBTreeMap::new();
    assert_eq!(map.insert((), ()), None);
    assert_eq!(map.insert((), ()), Some(()));
    assert_eq!(map.len(), 1);
    assert_eq!(map.get(&()), Some(&()));
    map.check_invariants();

    let rng = TestRng::new(4);
    MirroredMaps::<(), ()>::new().run_random_operations(&rng, 100, 1);
    MirroredMaps::<(), u64>::new().run_random_operations(&rng, 100, 1);
    let workload = Workload {
        random_steps: if cfg!(miri) { 200 } else { 20_000 },
        key_space: 1 << 12,
        // Leaves of `u64` keys and `()` values hold 127 entries.
        ordered_entries: if cfg!(miri) { 150 } else { 5000 },
        min_sequential_height: 1,
    };
    workload.run::<u64, ()>(5);
}

#[test]
fn byte_keys_with_zero_sized_values_match_std() {
    let rng = TestRng::new(6);
    let steps = if cfg!(miri) { 500 } else { 10_000 };
    let mut maps = MirroredMaps::<u8, ()>::new();
    maps.run_random_operations(&rng, steps, 256);
    for byte in 0..=u8::MAX {
        maps.insert(byte, ());
    }
    maps.assert_same_entries();
    assert_eq!(maps.ours.len(), 256);
}

#[test]
fn over_aligned_values_match_std() {
    Workload {
        random_steps: if cfg!(miri) { 150 } else { 20_000 },
        key_space: 1 << 12,
        ordered_entries: if cfg!(miri) { 60 } else { 3000 },
        min_sequential_height: 1,
    }
    .run::<u64, CacheLineAligned>(7);
    Workload {
        random_steps: if cfg!(miri) { 60 } else { 2000 },
        key_space: 1 << 10,
        ordered_entries: if cfg!(miri) { 30 } else { 600 },
        min_sequential_height: 1,
    }
    .run::<u64, PageAligned>(8);

    let count = if cfg!(miri) { 30 } else { 100 };
    let mut map = BBTreeMap::new();
    for key in 0..count {
        map.insert(key, PageAligned::new(key));
    }
    for key in 0..count {
        let value = map.get(&key).expect("every key is there");
        assert!(ptr::from_ref(value).is_aligned());
        assert_eq!(value.get(), key);
    }
}
