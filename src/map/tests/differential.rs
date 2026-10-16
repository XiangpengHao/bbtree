//! Differential tests: the map against std's `BTreeMap`, fed the same
//! operations, for ordinary, heap-owning, wide, zero-sized and over-aligned
//! keys and values, including every separator key, whose value is a leaf's
//! head (D17), and removals down to an empty map, which merge nodes and move
//! entries between them (D22). Iterators and ranges walk the maps from both
//! ends at once (D23), with bounds at separators too. The tests of inline
//! keys (`inline_key.rs`) use the same harness.

#![forbid(unsafe_code)]

use core::any::Any;
use core::borrow::Borrow;
use core::fmt::Debug;
use core::mem;
use core::ops::Bound;
use core::panic::AssertUnwindSafe;
use core::ptr;
use std::boxed::Box;
use std::collections::{BTreeMap, btree_map};
use std::format;
use std::panic::catch_unwind;
use std::string::String;
use std::vec::Vec;

use crate::inline_key::{Auto, InlineKeyStrategy};
use crate::map::{BBTreeMap, Entry, OccupiedEntry};
use crate::tree::test_support::{
    CacheLineAligned, FromNumber, PageAligned, StepEitherEnd, TestRng, hash_of,
};

/// With `u64` values, 64-byte keys fill a 1 KB node with 14 entries in both
/// kinds (D13, D15, D17): tall trees from a few hundred entries. Arrays order
/// lexicographically, so keys ascend with their first element.
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

/// Our map, with inline-key strategy `S`, and std's, fed the same
/// operations: every answer must match.
pub(super) struct MirroredMaps<K, V, S: InlineKeyStrategy<K> = Auto> {
    ours: BBTreeMap<K, V, S>,
    theirs: BTreeMap<K, V>,
}

impl<K, V, S> MirroredMaps<K, V, S>
where
    K: Ord + Clone + Debug + FromNumber,
    V: Clone + PartialEq + Debug + FromNumber,
    S: InlineKeyStrategy<K> + Default,
{
    pub(super) fn new() -> Self {
        Self {
            ours: BBTreeMap::with_strategy(S::default()),
            theirs: BTreeMap::new(),
        }
    }

    /// Our map.
    pub(super) fn ours(&self) -> &BBTreeMap<K, V, S> {
        &self.ours
    }

    /// Looks `key` up by a borrowed form of the keys in both maps.
    pub(super) fn get_borrowed<Q>(&self, key: &Q)
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        assert_eq!(self.ours.get(key), self.theirs.get(key));
        assert_eq!(self.ours.get_key_value(key), self.theirs.get_key_value(key));
        assert_eq!(self.ours.contains_key(key), self.theirs.contains_key(key));
    }

    /// Removes `key` by a borrowed form of the keys from both maps.
    pub(super) fn remove_borrowed<Q>(&mut self, key: &Q)
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        assert_eq!(self.ours.remove_entry(key), self.theirs.remove_entry(key));
        self.assert_same_len();
    }

    /// Checks a range by a borrowed form of the keys in both maps: its
    /// entries from both ends at random, or the same panic.
    pub(super) fn range_borrowed<Q>(&self, rng: &TestRng, bounds: (Bound<&Q>, Bound<&Q>))
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let ours = catch_unwind(AssertUnwindSafe(|| self.ours.range::<Q, _>(bounds)));
        let theirs = catch_unwind(AssertUnwindSafe(|| self.theirs.range::<Q, _>(bounds)));
        match (ours, theirs) {
            (Ok(ours), Ok(theirs)) => {
                rng.assert_same_items(ours.clone(), theirs.clone(), 64);
                if rng.below(32) == 0 {
                    assert!(ours.eq(theirs), "the whole ranges differ");
                }
            }
            (Err(ours), Err(theirs)) => {
                assert_eq!(panic_message(&ours), panic_message(&theirs));
            }
            (ours, theirs) => panic!(
                "one range panicked, not the other: ours {}, std's {}",
                ours.is_err(),
                theirs.is_err()
            ),
        }
    }

    /// The keys in both maps, in order.
    pub(super) fn keys(&self) -> impl Iterator<Item = &K> {
        self.theirs.keys()
    }

    /// Walks the whole maps from both ends at random, step by step, until
    /// the ends meet; and the other iterators, which share its walk, for a
    /// while.
    fn assert_same_iteration(&self, rng: &TestRng) {
        let steps = self.theirs.len() + 1;
        rng.assert_same_items(self.ours.iter(), self.theirs.iter(), steps);
        let steps = steps.min(1024);
        rng.assert_same_items(self.ours.keys(), self.theirs.keys(), steps);
        rng.assert_same_items(self.ours.values(), self.theirs.values(), steps);
        let (ours, theirs) = (IntoIterator::into_iter(&self.ours), self.theirs.iter());
        rng.assert_same_items(ours, theirs, steps);
    }

    /// Checks `count` ranges against std's, between random bounds in order
    /// ([`Self::random_range_bounds`]).
    fn assert_same_ranges(&self, rng: &TestRng, count: usize, key_space: usize) {
        let separators: Vec<K> = self.ours.separator_keys().into_iter().cloned().collect();
        for _ in 0..count {
            let (start, end) = Self::random_range_bounds(rng, &separators, key_space);
            self.range_borrowed(rng, (start.as_ref(), end.as_ref()));
        }
    }

    /// Random bounds in order ([`TestRng::range_bounds`]), at keys made from
    /// `0..key_space` or, a third of them, at `separators` (D17).
    fn random_range_bounds(
        rng: &TestRng,
        separators: &[K],
        key_space: usize,
    ) -> (Bound<K>, Bound<K>) {
        rng.range_bounds(key_space, |number| match separators.len() {
            len if len > 0 && rng.below(3) == 0 => separators[rng.below(len)].clone(),
            _ => K::from_number(number),
        })
    }

    /// Takes both maps by value and walks them from both ends at random, by
    /// `into_iter`, `into_keys` or `into_values`, through about half of
    /// their entries, then drops what is left of both. A small map's
    /// iterators print the same first.
    fn consume(&mut self, rng: &TestRng) {
        let ours = mem::replace(&mut self.ours, BBTreeMap::with_strategy(S::default()));
        let theirs = mem::take(&mut self.theirs);
        let steps = theirs.len() / 2 + 1;
        match rng.below(3) {
            0 => {
                let (ours, theirs) = (ours.into_iter(), theirs.into_iter());
                if theirs.len() <= 64 {
                    assert_eq!(format!("{ours:?}"), format!("{theirs:?}"));
                }
                rng.assert_same_items(ours, theirs, steps);
            }
            1 => rng.assert_same_items(ours.into_keys(), theirs.into_keys(), steps),
            _ => rng.assert_same_items(ours.into_values(), theirs.into_values(), steps),
        }
    }

    /// Takes entries from both ends of `iter_mut` at random until they meet,
    /// giving each the same new value in both maps, and printing what is left
    /// of both while little is.
    fn mutate_by_iter_mut(&mut self, rng: &TestRng) {
        let mut ours = self.ours.iter_mut();
        let mut theirs = self.theirs.iter_mut();
        loop {
            assert_eq!(ours.size_hint(), theirs.size_hint());
            if theirs.len() <= 8 {
                assert_eq!(format!("{ours:?}"), format!("{theirs:?}"));
            }
            if !Self::mutate_at_random_end(rng, &mut ours, &mut theirs) {
                break;
            }
        }
    }

    /// Takes the next entry of `ours` and `theirs` alike, from the front or
    /// the back at random: the same entry, which then gets the same new value
    /// in both. Returns whether there was one.
    fn mutate_at_random_end<'a>(
        rng: &TestRng,
        ours: &mut impl DoubleEndedIterator<Item = (&'a K, &'a mut V)>,
        theirs: &mut impl DoubleEndedIterator<Item = (&'a K, &'a mut V)>,
    ) -> bool
    where
        K: 'a,
        V: 'a,
    {
        let back = rng.below(2) != 0;
        let entries = (ours.step(back), theirs.step(back));
        let (Some((our_key, our_value)), Some((their_key, their_value))) = entries else {
            assert!(entries.0.is_none() && entries.1.is_none());
            return false;
        };
        assert_eq!((our_key, &*our_value), (their_key, &*their_value));
        let value = V::from_number(rng.below(1 << 30));
        *our_value = value.clone();
        *their_value = value;
        true
    }

    /// Takes every value out of `values_mut`, from both ends at random,
    /// before writing to any, so that all are borrowed at once: the walk hands
    /// out each value once, and no two alias, which Miri checks
    /// (`mutable_walks_of_a_small_tree_match_std`).
    fn mutate_all_at_once(&mut self, rng: &TestRng) {
        let from_back: Vec<bool> = (0..self.theirs.len()).map(|_| rng.below(2) == 0).collect();
        let mut ours = self.ours.values_mut();
        let ours: Vec<&mut V> = from_back
            .iter()
            .map(|&back| ours.step(back))
            .collect::<Option<_>>()
            .expect("as many values as std's");
        let mut theirs = self.theirs.values_mut();
        let theirs: Vec<&mut V> = from_back
            .iter()
            .map(|&back| theirs.step(back))
            .collect::<Option<_>>()
            .expect("as many values as the map holds");
        for (our_value, their_value) in ours.into_iter().zip(theirs) {
            assert_eq!(our_value, their_value);
            let value = V::from_number(rng.below(1 << 30));
            *our_value = value.clone();
            *their_value = value;
        }
    }

    /// Takes entries from both ends of `count` random ranges at random, up
    /// to 64 per range, giving each the same new value in both maps.
    fn mutate_ranges(&mut self, rng: &TestRng, count: usize, key_space: usize) {
        let separators: Vec<K> = self.ours.separator_keys().into_iter().cloned().collect();
        for _ in 0..count {
            let (start, end) = Self::random_range_bounds(rng, &separators, key_space);
            let bounds = (start.as_ref(), end.as_ref());
            let mut ours = self.ours.range_mut::<K, _>(bounds);
            let mut theirs = self.theirs.range_mut::<K, _>(bounds);
            for _ in 0..64 {
                if !Self::mutate_at_random_end(rng, &mut ours, &mut theirs) {
                    break;
                }
            }
        }
    }

    pub(super) fn insert(&mut self, key: K, value: V) {
        let ours = self.ours.insert(key.clone(), value.clone());
        assert_eq!(ours, self.theirs.insert(key, value));
        self.assert_same_len();
    }

    /// Looks `key` up in both maps, as [`Self::get_borrowed`] does, and by
    /// `Index` if it is there.
    fn get(&self, key: &K) {
        self.get_borrowed(key);
        if self.theirs.contains_key(key) {
            assert_eq!(&self.ours[key], &self.theirs[key]);
        }
    }

    /// One random use of the entry API for `key`, alike on both maps: both
    /// entries are occupied or both vacant, print the same, and hand out
    /// the same values, which then get the same new value through the
    /// references handed out.
    fn use_entry(&mut self, rng: &TestRng, key: K) {
        let value = V::from_number(rng.below(1 << 30));
        let new_value = V::from_number(rng.below(1 << 30));
        let (ours, theirs) = (self.ours.entry(key.clone()), self.theirs.entry(key.clone()));
        assert_eq!(ours.key(), theirs.key());
        assert_eq!(format!("{ours:?}"), format!("{theirs:?}"));
        if rng.below(2) == 0 {
            let (ours, theirs) = match rng.below(5) {
                0 => (ours.or_insert(value.clone()), theirs.or_insert(value)),
                1 => (
                    ours.or_insert_with(|| value.clone()),
                    theirs.or_insert_with(|| value),
                ),
                2 => {
                    let ours = ours.or_insert_with_key(|entry_key| {
                        assert!(*entry_key == key);
                        value.clone()
                    });
                    (ours, theirs.or_insert_with_key(|_| value))
                }
                3 => {
                    let modified = new_value.clone();
                    let ours = ours.and_modify(|value| *value = modified);
                    let theirs = theirs.and_modify(|value| *value = new_value.clone());
                    (ours.or_insert(value.clone()), theirs.or_insert(value))
                }
                _ => {
                    let (ours, theirs) =
                        (ours.insert_entry(value.clone()), theirs.insert_entry(value));
                    Self::assert_same_occupied(&ours, &theirs);
                    (ours.into_mut(), theirs.into_mut())
                }
            };
            assert_eq!(ours, theirs);
            *ours = new_value.clone();
            *theirs = new_value;
        } else {
            match (ours, theirs) {
                (Entry::Occupied(mut ours), btree_map::Entry::Occupied(mut theirs)) => {
                    Self::assert_same_occupied(&ours, &theirs);
                    match rng.below(5) {
                        0 => assert_eq!(ours.remove_entry(), theirs.remove_entry()),
                        1 => assert_eq!(ours.remove(), theirs.remove()),
                        2 => assert_eq!(ours.insert(value.clone()), theirs.insert(value)),
                        3 => {
                            *ours.get_mut() = value.clone();
                            *theirs.get_mut() = value;
                            Self::assert_same_occupied(&ours, &theirs);
                        }
                        _ => {
                            let (ours, theirs) = (ours.into_mut(), theirs.into_mut());
                            assert_eq!(ours, theirs);
                            *ours = value.clone();
                            *theirs = value;
                        }
                    }
                }
                (Entry::Vacant(ours), btree_map::Entry::Vacant(theirs)) => match rng.below(3) {
                    0 => {
                        let (ours, theirs) = (ours.insert(value.clone()), theirs.insert(value));
                        assert_eq!(ours, theirs);
                        *ours = new_value.clone();
                        *theirs = new_value;
                    }
                    1 => {
                        let (ours, theirs) =
                            (ours.insert_entry(value.clone()), theirs.insert_entry(value));
                        Self::assert_same_occupied(&ours, &theirs);
                    }
                    _ => assert_eq!(ours.into_key(), theirs.into_key()),
                },
                _ => panic!("one entry is occupied, the other vacant"),
            }
        }
        self.assert_same_len();
    }

    /// The first or the last entry of both maps, read, changed or removed.
    fn use_end_entry(&mut self, rng: &TestRng) {
        let last = rng.below(2) == 0;
        let (ours, theirs) = if last {
            (self.ours.last_entry(), self.theirs.last_entry())
        } else {
            (self.ours.first_entry(), self.theirs.first_entry())
        };
        match (ours, theirs) {
            (Some(mut ours), Some(mut theirs)) => {
                Self::assert_same_occupied(&ours, &theirs);
                if rng.below(2) == 0 {
                    assert_eq!(ours.remove_entry(), theirs.remove_entry());
                } else {
                    let value = V::from_number(rng.below(1 << 30));
                    assert_eq!(ours.insert(value.clone()), theirs.insert(value));
                }
            }
            (ours, theirs) => assert_eq!(ours.is_none(), theirs.is_none()),
        }
        self.assert_same_len();
    }

    fn assert_same_occupied(
        ours: &OccupiedEntry<'_, K, V, S>,
        theirs: &btree_map::OccupiedEntry<'_, K, V>,
    ) {
        assert_eq!((ours.key(), ours.get()), (theirs.key(), theirs.get()));
        assert_eq!(format!("{ours:?}"), format!("{theirs:?}"));
    }

    /// Bulk changes on both maps, alike: a split at a random key, the parts
    /// checked and appended back; a small map appended; `retain` and
    /// `extract_if` with the same decisions for both, on random ranges,
    /// changing values on the way, some walks dropped early. Each is O(n):
    /// a large map gets one round of each, a small one four.
    fn bulk_changes(&mut self, rng: &TestRng, key_space: usize) {
        let rounds = if self.theirs.len() > 20_000 { 1 } else { 4 };
        let separators: Vec<K> = self.ours.separator_keys().into_iter().cloned().collect();
        for _ in 0..rounds {
            let key = match separators.len() {
                len if len > 0 && rng.below(2) == 0 => separators[rng.below(len)].clone(),
                _ => K::from_number(rng.below(key_space + 2)),
            };
            let mut upper = Self {
                ours: self.ours.split_off(&key),
                theirs: self.theirs.split_off(&key),
            };
            self.assert_same_contents();
            upper.assert_same_contents();
            if rng.below(2) == 0 {
                self.ours.append(&mut upper.ours);
                self.theirs.append(&mut upper.theirs);
            } else {
                upper.ours.append(&mut self.ours);
                upper.theirs.append(&mut self.theirs);
                mem::swap(self, &mut upper);
            }
            upper.assert_same_contents();
            self.assert_same_contents();
        }

        let mut other = Self::new();
        for _ in 0..rng.below(64) {
            other.insert(
                K::from_number(rng.below(key_space)),
                V::from_number(rng.below(1 << 30)),
            );
        }
        self.ours.append(&mut other.ours);
        self.theirs.append(&mut other.theirs);
        other.assert_same_contents();
        self.assert_same_contents();

        let decisions: Vec<bool> = (0..self.theirs.len()).map(|_| rng.below(8) != 0).collect();
        let new_value = V::from_number(rng.below(1 << 30));
        let keep = |decisions: &mut core::slice::Iter<'_, bool>, value: &mut V| {
            *value = new_value.clone();
            *decisions.next().expect("a decision per entry")
        };
        let mut our_decisions = decisions.iter();
        self.ours.retain(|_, value| keep(&mut our_decisions, value));
        let mut their_decisions = decisions.iter();
        self.theirs
            .retain(|_, value| keep(&mut their_decisions, value));
        self.assert_same_contents();

        for _ in 0..rounds {
            let (start, end) = Self::random_range_bounds(rng, &separators, key_space);
            let range = (start, end);
            let decisions: Vec<bool> = (0..self.theirs.len()).map(|_| rng.below(3) == 0).collect();
            let limit = if rng.below(2) == 0 {
                usize::MAX
            } else {
                rng.below(16)
            };
            let mut our_decisions = decisions.iter();
            let mut ours = self.ours.extract_if(range.clone(), |_, _| {
                *our_decisions.next().expect("a decision")
            });
            let mut their_decisions = decisions.iter();
            let mut theirs = self
                .theirs
                .extract_if(range, |_, _| *their_decisions.next().expect("a decision"));
            for _ in 0..limit {
                assert_eq!(format!("{ours:?}"), format!("{theirs:?}"));
                let (our_entry, their_entry) = (ours.next(), theirs.next());
                let done = their_entry.is_none();
                assert_eq!(our_entry, their_entry);
                if done {
                    break;
                }
            }
            drop(ours);
            drop(theirs);
            self.assert_same_contents();
        }
    }

    /// Checks the map's invariants, and that it holds exactly std's entries,
    /// by iterating both ways.
    fn assert_same_contents(&self) {
        self.ours.check_invariants();
        self.assert_same_len();
        assert!(self.ours.iter().eq(&self.theirs));
        assert!(self.ours.iter().rev().eq(self.theirs.iter().rev()));
    }

    /// Extends both maps with the same few entries, some of them with keys
    /// already there.
    fn extend(&mut self, rng: &TestRng, key_space: usize) {
        let entries: Vec<(K, V)> = (0..rng.below(8))
            .map(|_| {
                let key = K::from_number(rng.below(key_space));
                (key, V::from_number(rng.below(1 << 30)))
            })
            .collect();
        self.ours.extend(entries.clone());
        self.theirs.extend(entries);
        self.assert_same_len();
    }

    /// Clones both maps and checks the copies: the same entries, and ours
    /// with the same shape as its source. Then changes the copies, and checks
    /// that the sources still match: a copy shares nothing.
    fn assert_clones_match(&self, rng: &TestRng, key_space: usize) {
        let mut copies = Self {
            ours: self.ours.clone(),
            theirs: self.theirs.clone(),
        };
        copies.assert_same_contents();
        assert_eq!(copies.ours.leaf_depth(), self.ours.leaf_depth());
        assert_eq!(copies.ours.separator_keys(), self.ours.separator_keys());
        for _ in 0..64 {
            let key = K::from_number(rng.below(key_space));
            copies.insert(key, V::from_number(rng.below(1 << 30)));
            copies.remove(&K::from_number(rng.below(key_space)));
            copies.pop_first();
        }
        self.assert_same_contents();
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

    fn remove(&mut self, key: &K) {
        assert_eq!(self.ours.remove(key), self.theirs.remove(key));
        self.assert_same_len();
    }

    fn pop_first(&mut self) {
        assert_eq!(self.ours.pop_first(), self.theirs.pop_first());
        self.assert_same_len();
    }

    fn pop_last(&mut self) {
        assert_eq!(self.ours.pop_last(), self.theirs.pop_last());
        self.assert_same_len();
    }

    fn first_and_last(&self) {
        assert_eq!(self.ours.first_key_value(), self.theirs.first_key_value());
        assert_eq!(self.ours.last_key_value(), self.theirs.last_key_value());
    }

    fn clear(&mut self) {
        self.ours.clear();
        self.theirs.clear();
        self.assert_same_entries();
    }

    fn assert_same_len(&self) {
        assert_eq!(self.ours.len(), self.theirs.len());
        assert_eq!(self.ours.is_empty(), self.theirs.is_empty());
    }

    /// [`Self::assert_same_contents`], and a lookup of each of std's
    /// entries; a small map prints the same.
    pub(super) fn assert_same_entries(&self) {
        self.assert_same_contents();
        for (key, value) in &self.theirs {
            assert_eq!(self.ours.get_key_value(key), Some((key, value)));
        }
        if self.theirs.len() <= 64 {
            assert_eq!(format!("{:?}", self.ours), format!("{:?}", self.theirs));
        }
    }

    /// Gives every separator key a new value, through `insert` or `get_mut`
    /// at random: each time, the value replaced is a leaf's head (D17).
    fn overwrite_separators(&mut self, rng: &TestRng) {
        let separators: Vec<K> = self.ours.separator_keys().into_iter().cloned().collect();
        assert_eq!(separators.is_empty(), self.ours.leaf_depth() == 0);
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

    /// `steps` random operations on keys made from `0..key_space`. Inserts
    /// outnumber removals: the map grows to about two thirds of the key
    /// space.
    pub(super) fn run_random_operations(&mut self, rng: &TestRng, steps: usize, key_space: usize) {
        for _ in 0..steps {
            let key = K::from_number(rng.below(key_space));
            match rng.below(20) {
                0..=5 => self.insert(key, V::from_number(rng.below(1 << 30))),
                6..=8 => self.use_entry(rng, key),
                9 => self.use_end_entry(rng),
                10 => self.extend(rng, key_space),
                11 | 12 => self.get(&key),
                13 => self.update(&key, V::from_number(rng.below(1 << 30))),
                14 | 15 => self.remove(&key),
                16 => self.remove_borrowed(&key),
                17 => self.pop_first(),
                18 => self.pop_last(),
                _ => self.first_and_last(),
            }
        }
        self.assert_same_entries();
    }

    /// Removes every entry, in `order`, checking the invariants and a few
    /// ranges between keys made from `0..key_space` as the map shrinks:
    /// whenever its leaves rise a level, and about 32 times besides.
    fn remove_all(&mut self, order: RemovalOrder, rng: &TestRng, key_space: usize) {
        let mut keys: Vec<K> = self.theirs.keys().cloned().collect();
        rng.shuffle(&mut keys);
        let check_every = (keys.len() / 32).max(1);
        let mut leaf_depth = self.ours.leaf_depth();
        for (step, key) in keys.iter().enumerate() {
            match order {
                RemovalOrder::Ascending => self.pop_first(),
                RemovalOrder::Descending => self.pop_last(),
                RemovalOrder::Random => match rng.below(8) {
                    0 => self.pop_first(),
                    1 => self.pop_last(),
                    2 => self.remove_borrowed(key),
                    _ => self.remove(key),
                },
            }
            self.first_and_last();
            if self.ours.leaf_depth() != leaf_depth || step % check_every == 0 {
                leaf_depth = self.ours.leaf_depth();
                self.ours.check_invariants();
                self.assert_same_ranges(rng, 4, key_space);
            }
        }
        // A pop may have taken a key before its turn, leaving another behind.
        while !self.theirs.is_empty() {
            self.pop_first();
        }
        self.assert_same_entries();
        assert_eq!(self.ours.leaf_depth(), 0);
    }

    /// Iterates over both maps, shared and mutably, and checks `ranges`
    /// ranges between keys made from `0..key_space`, shared and mutably.
    fn walk(&mut self, rng: &TestRng, key_space: usize, ranges: usize) {
        self.assert_same_iteration(rng);
        self.assert_clones_match(rng, key_space);
        self.mutate_by_iter_mut(rng);
        self.mutate_all_at_once(rng);
        self.assert_same_ranges(rng, ranges, key_space);
        self.mutate_ranges(rng, ranges, key_space);
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
            if number.is_multiple_of(4) {
                self.insert(K::from_number(number), V::from_number(number));
            } else {
                // Through a vacant entry, writing through the reference it
                // hands back: that must be the new entry's, wherever the
                // splits took it.
                let key = K::from_number(number);
                let ours = self.ours.entry(key.clone()).or_insert(V::from_number(0));
                *ours = V::from_number(number);
                *self.theirs.entry(key).or_insert(V::from_number(0)) = V::from_number(number);
                self.assert_same_len();
            }
        }
        self.assert_same_entries();
        for number in 0..=largest + 1 {
            self.get(&K::from_number(number));
        }
    }
}

/// The order in which [`MirroredMaps::remove_all`] removes entries.
#[derive(Clone, Copy)]
enum RemovalOrder {
    /// By `pop_first`: always from the leftmost leaf, which has no left
    /// sibling.
    Ascending,
    /// By `pop_last`: always from the rightmost leaf, which has no right
    /// sibling.
    Descending,
    /// By key, in random order, through `remove` and `remove_entry`, mixed
    /// with `pop_first` and `pop_last`: separators too (D17).
    Random,
}

/// The message of a panic from `panic!` with a literal or a format.
fn panic_message(payload: &Box<dyn Any + Send>) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .expect("a panic with a message")
}

/// How much work one key and value type gets.
pub(super) struct Workload {
    pub(super) random_steps: usize,
    pub(super) key_space: usize,
    pub(super) ordered_entries: usize,
    /// How deep ascending and descending inserts must push the leaves. They
    /// leave larger nodes densely filled and small ones about half full. The
    /// workloads keep enough entries to reach this depth under either
    /// policy.
    pub(super) min_sequential_leaf_depth: usize,
}

impl Workload {
    /// `random_steps` random operations on keys made from `0..key_space`,
    /// and ordered inserts of half as many keys, which push the leaves 2
    /// below the root.
    pub(super) fn new(random_steps: usize, key_space: usize) -> Self {
        Self {
            random_steps,
            key_space,
            ordered_entries: key_space / 2,
            min_sequential_leaf_depth: 2,
        }
    }

    /// Random operations, then ascending, descending and shuffled inserts,
    /// each on a fresh pair of maps whose ours has strategy `S`, and each
    /// followed by overwriting every separator's value, iterating and
    /// checking ranges; the random and the shuffled ones then go through bulk
    /// changes, whose rebuilds make any shape the same. Then each loses its
    /// entries: removed in random order (after which the map of random
    /// operations takes more of them, and is cleared), popped from the front
    /// or the back, or consumed by an owning iterator.
    pub(super) fn run<K, V, S>(&self, seed: u64)
    where
        K: Ord + Clone + Debug + FromNumber,
        V: Clone + PartialEq + Debug + FromNumber,
        S: InlineKeyStrategy<K> + Default,
    {
        let rng = TestRng::new(seed);
        let mut maps = MirroredMaps::<K, V, S>::new();
        maps.run_random_operations(&rng, self.random_steps, self.key_space);
        maps.overwrite_separators(&rng);
        maps.walk(&rng, self.key_space, self.ranges());
        maps.bulk_changes(&rng, self.key_space);
        maps.remove_all(RemovalOrder::Random, &rng, self.key_space);
        maps.run_random_operations(&rng, self.random_steps / 4, self.key_space);
        maps.clear();

        let evens = || (0..self.ordered_entries).map(|index| 2 * index);
        let sequential_orders: [(Vec<usize>, RemovalOrder); 2] = [
            (evens().collect(), RemovalOrder::Ascending),
            (evens().rev().collect(), RemovalOrder::Descending),
        ];
        for (numbers, removal_order) in sequential_orders {
            let mut maps = MirroredMaps::<K, V, S>::new();
            maps.insert_evens_in_order(numbers);
            let leaf_depth = maps.ours.leaf_depth();
            assert!(
                leaf_depth >= self.min_sequential_leaf_depth,
                "leaves at depth {leaf_depth}, above {}",
                self.min_sequential_leaf_depth
            );
            maps.overwrite_separators(&rng);
            maps.walk(&rng, self.ordered_key_space(), self.ranges());
            maps.remove_all(removal_order, &rng, self.ordered_key_space());
        }
        let mut shuffled: Vec<usize> = evens().collect();
        rng.shuffle(&mut shuffled);
        let mut maps = MirroredMaps::<K, V, S>::new();
        maps.insert_evens_in_order(shuffled);
        maps.overwrite_separators(&rng);
        maps.walk(&rng, self.ordered_key_space(), self.ranges());
        maps.bulk_changes(&rng, self.ordered_key_space());
        maps.consume(&rng);
    }

    /// The numbers the ordered inserts make keys from, the odd ones absent.
    fn ordered_key_space(&self) -> usize {
        2 * self.ordered_entries + 2
    }

    /// How many ranges each walk checks: as many as the workload's size
    /// affords.
    fn ranges(&self) -> usize {
        (self.random_steps / 2000).clamp(16, 256)
    }
}

#[test]
#[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
fn u64_to_u64_matches_std() {
    Workload {
        random_steps: 500_000,
        key_space: 1 << 18,
        ordered_entries: 200_000,
        min_sequential_leaf_depth: 2,
    }
    .run::<u64, u64, Auto>(1);
}

#[test]
#[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
fn string_to_string_matches_std() {
    Workload {
        random_steps: 100_000,
        key_space: 1 << 16,
        ordered_entries: 50_000,
        min_sequential_leaf_depth: 2,
    }
    .run::<String, String, Auto>(2);
}

#[test]
#[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
fn wide_keys_match_std() {
    Workload {
        random_steps: 40_000,
        key_space: 1 << 14,
        ordered_entries: 10_000,
        min_sequential_leaf_depth: 3,
    }
    .run::<WideKey, u64, Auto>(3);
}

/// The mutable walks of a tree two levels tall, small enough for Miri, which
/// checks that they hand out each value once, and no two references that
/// alias.
#[test]
fn mutable_walks_of_a_small_tree_match_std() {
    let rng = TestRng::new(13);
    let mut maps = MirroredMaps::<u64, u64>::new();
    for number in 0..60 {
        maps.insert(number, number);
    }
    assert_eq!(maps.ours.leaf_depth(), 1);
    maps.mutate_by_iter_mut(&rng);
    maps.mutate_all_at_once(&rng);
    maps.mutate_ranges(&rng, 8, 60);
    maps.assert_same_contents();
}

/// Bounds out of order panic with std's messages, unless the map has never
/// had a tree; an emptied map still has one, as std's does.
#[test]
#[cfg_attr(panic = "abort", ignore = "needs unwinding")]
fn ranges_out_of_order_panic_like_std() {
    let rng = TestRng::new(10);
    let mut maps = MirroredMaps::<u64, u64>::new();
    let out_of_order: [(Bound<&u64>, Bound<&u64>); 4] = [
        (Bound::Included(&5), Bound::Included(&4)),
        (Bound::Excluded(&5), Bound::Excluded(&4)),
        (Bound::Excluded(&5), Bound::Excluded(&5)),
        (Bound::Included(&5), Bound::Excluded(&4)),
    ];
    let in_order: [(Bound<&u64>, Bound<&u64>); 3] = [
        (Bound::Included(&5), Bound::Excluded(&5)),
        (Bound::Excluded(&5), Bound::Included(&5)),
        (Bound::Unbounded, Bound::Excluded(&0)),
    ];
    for bounds in out_of_order.into_iter().chain(in_order) {
        maps.range_borrowed(&rng, bounds);
    }
    for number in 0..200 {
        maps.insert(number, number);
    }
    while !maps.theirs.is_empty() {
        maps.pop_first();
    }
    for bounds in out_of_order.into_iter().chain(in_order) {
        maps.range_borrowed(&rng, bounds);
    }
}

/// `collect` and `from` sort the entries, and keep the last of equal keys,
/// as std's do; a missing key in `Index` panics with std's message.
#[test]
#[cfg_attr(panic = "abort", ignore = "needs unwinding")]
fn collecting_and_indexing_match_std() {
    let rng = TestRng::new(12);
    let (trials, max_len) = if cfg!(miri) { (20, 60) } else { (300, 3000) };
    for _ in 0..trials {
        let len = rng.below(max_len);
        // Values number the entries, which tells which of equal keys stays.
        let entries: Vec<(u64, usize)> = (0..len)
            .map(|index| (rng.below(len + 1) as u64, index))
            .collect();
        let ours: BBTreeMap<u64, usize> = entries.iter().copied().collect();
        let theirs: BTreeMap<u64, usize> = entries.iter().copied().collect();
        ours.check_invariants();
        assert!(ours.iter().eq(&theirs));

        let mut extended: BBTreeMap<u64, usize> = BBTreeMap::new();
        extended.extend(entries.iter().map(|(key, value)| (key, value)));
        let mut std_extended: BTreeMap<u64, usize> = BTreeMap::new();
        std_extended.extend(entries.iter().map(|(key, value)| (key, value)));
        assert!(extended.iter().eq(&std_extended));
    }
    let array = [(3, 'a'), (1, 'b'), (3, 'c'), (2, 'd'), (1, 'e')];
    let ours = BBTreeMap::from(array);
    assert!(ours.iter().eq(&BTreeMap::from(array)));
    assert_eq!(ours[&3], 'c');
    assert!(BBTreeMap::<u8, u8>::from([]).is_empty());

    let missing = catch_unwind(AssertUnwindSafe(|| ours[&4]));
    let std_missing = catch_unwind(AssertUnwindSafe(|| BTreeMap::from(array)[&4]));
    let message = |payload| String::from(panic_message(&payload));
    assert_eq!(missing.map_err(message), std_missing.map_err(message));
}

/// Equality, ordering, hashing and printing go entry by entry in key order,
/// as std's do, with the same results, and the same hash.
#[test]
fn comparisons_and_hashes_match_std() {
    let rng = TestRng::new(11);
    let (trials, max_len) = if cfg!(miri) { (20, 80) } else { (400, 2000) };
    for _ in 0..trials {
        let len = rng.below(max_len);
        let entries: Vec<(u64, u64)> = (0..len)
            .map(|_| (rng.below(3 * max_len) as u64, rng.below(3) as u64))
            .collect();
        // The second map is the first with at most one change.
        let mut other_entries = entries.clone();
        match rng.below(4) {
            0 if len > 0 => other_entries[rng.below(len)].1 += 1,
            1 if len > 0 => drop(other_entries.swap_remove(rng.below(len))),
            2 => other_entries.push((rng.below(3 * max_len) as u64, 0)),
            _ => {}
        }
        let (mut ours, mut theirs) = (BBTreeMap::new(), BTreeMap::new());
        let (mut other_ours, mut other_theirs) = (BBTreeMap::new(), BTreeMap::new());
        for &(key, value) in &entries {
            ours.insert(key, value);
            theirs.insert(key, value);
        }
        for &(key, value) in &other_entries {
            other_ours.insert(key, value);
            other_theirs.insert(key, value);
        }
        assert_eq!(ours == other_ours, theirs == other_theirs);
        assert_eq!(
            ours.partial_cmp(&other_ours),
            theirs.partial_cmp(&other_theirs)
        );
        assert_eq!(ours.cmp(&other_ours), theirs.cmp(&other_theirs));
        assert_eq!(hash_of(&ours), hash_of(&theirs));
        assert_eq!(format!("{ours:?}"), format!("{theirs:?}"));
    }
}

#[test]
#[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
fn zero_sized_keys_and_values_match_std() {
    let mut map = BBTreeMap::new();
    assert_eq!(map.insert((), ()), None);
    assert_eq!(map.insert((), ()), Some(()));
    assert_eq!(map.len(), 1);
    assert_eq!(map.get(&()), Some(&()));
    map.check_invariants();
    assert_eq!(map.remove_entry(&()), Some(((), ())));
    assert_eq!(map.pop_first(), None);
    assert!(map.is_empty());
    map.check_invariants();

    let rng = TestRng::new(4);
    MirroredMaps::<(), ()>::new().run_random_operations(&rng, 100, 1);
    MirroredMaps::<(), u64>::new().run_random_operations(&rng, 100, 1);
    Workload {
        random_steps: 20_000,
        key_space: 1 << 12,
        ordered_entries: 5000,
        min_sequential_leaf_depth: 1,
    }
    .run::<u64, (), Auto>(5);
}

#[test]
#[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
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
    maps.assert_same_iteration(&rng);
    maps.assert_same_ranges(&rng, 64, 256);
    maps.remove_all(RemovalOrder::Random, &rng, 256);
}

#[test]
#[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
fn over_aligned_values_match_std() {
    Workload {
        random_steps: 20_000,
        key_space: 1 << 12,
        ordered_entries: 3000,
        min_sequential_leaf_depth: 1,
    }
    .run::<u64, CacheLineAligned, Auto>(7);
    Workload {
        random_steps: 2000,
        key_space: 1 << 10,
        ordered_entries: 600,
        min_sequential_leaf_depth: 1,
    }
    .run::<u64, PageAligned, Auto>(8);

    let count = 100;
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

/// `split_off` at every position of a tree a few levels tall: before the
/// first key, at each key and between each two, after the last, and so at
/// every separator. Both halves match std's and keep the invariants, in a
/// tree of half-full small nodes, from ascending inserts, and in one thinned by
/// removals toward the minimum fill, whose borders have little to spare.
#[test]
#[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
fn split_off_at_every_position_matches_std() {
    // Ascending inserts leave these small nodes about half full: 1500 entries
    // put the leaves 3 below the root.
    let (count, leaf_depth) = (1500, 3);
    for thinned in [false, true] {
        let rng = TestRng::new(31);
        let mut maps = MirroredMaps::<WideKey, u64>::new();
        // Keys at even numbers, so that odd ones fall between them.
        for number in (1..=count).map(|number| 2 * number) {
            maps.insert(WideKey::from_number(number), u64::from_number(number));
        }
        if thinned {
            for number in (1..=count).map(|number| 2 * number) {
                if rng.below(4) != 0 {
                    let key = WideKey::from_number(number);
                    assert_eq!(maps.ours.remove(&key), maps.theirs.remove(&key));
                }
            }
        } else {
            assert!(
                maps.ours.leaf_depth() >= leaf_depth,
                "the tree is a few levels tall"
            );
        }
        maps.assert_same_contents();
        for number in 0..=2 * count + 1 {
            let key = WideKey::from_number(number);
            let mut lower = maps.ours.clone();
            let mut theirs_lower = maps.theirs.clone();
            let upper = lower.split_off(&key);
            let theirs_upper = theirs_lower.split_off(&key);
            for (ours, theirs) in [(&lower, &theirs_lower), (&upper, &theirs_upper)] {
                ours.check_invariants();
                assert_eq!(ours.len(), theirs.len());
                assert!(ours.iter().eq(theirs));
            }
        }
    }
}
