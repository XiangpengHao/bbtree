//! Differential tests: the set against std's `BTreeSet`, fed the same
//! operations. The set's tree is a map's, of zero-sized values, which the
//! map's tests cover (`map/tests/differential.rs`); these check that each of
//! the set's methods reaches the right one of the map's and gives std's
//! answers, down to which of equal elements stays and what each iterator
//! prints. The operations on two sets run on pairs of every shape that
//! decides their strategy: apart, touching at an end, one inside the other,
//! and lengths on both sides of where looking up beats walking.
//!
//! Type-level properties shared with std's `BTreeSet` and its iterators:
//! variance, size, and a `const` constructor. The auto traits are the map's
//! (`map/tests/type_properties.rs`), whose values here are a unit struct.
//! And the allocator a set operator's result takes its nodes from.

#![forbid(unsafe_code)]

use core::cmp::Ordering;
use core::fmt::Debug;
use core::hash::Hash;
use core::mem;
use std::collections::BTreeSet;
use std::format;
use std::string::String;
use std::vec::Vec;

use super::{BBTreeSet, IntoIter, Iter, Range};
use crate::inline_key::{Auto, InlineKeyStrategy, WithInlineKey};
use crate::tree::node::page::TestPages;
use crate::tree::test_support::{FromNumber, TestRng, hash_of};
use crate::unsync::map::BBTreeMap;

/// Sets of at most this many elements, and iterators with at most this many
/// left, are printed and the prints compared.
const PRINTED_LEN: usize = 64;

/// Our set, with inline-key strategy `S`, and std's, fed the same
/// operations: every answer must match.
struct MirroredSets<T, S: InlineKeyStrategy<T> = Auto> {
    ours: BBTreeSet<T, S>,
    theirs: BTreeSet<T>,
}

impl<T, S> MirroredSets<T, S>
where
    T: Ord + Clone + Debug + Hash + FromNumber,
    S: InlineKeyStrategy<T> + Default,
{
    fn new() -> Self {
        Self {
            ours: BBTreeSet::with_strategy(S::default()),
            theirs: BTreeSet::new(),
        }
    }

    /// Random point operations, then iterating, ranges, bulk changes,
    /// clones and comparisons, on elements made from `0..key_space`; then
    /// clearing, and consuming what more point operations put in.
    fn run(&mut self, rng: &TestRng, steps: usize, key_space: usize) {
        self.run_random_operations(rng, steps, key_space);
        self.ours.map.check_invariants();
        self.assert_same_iteration(rng);
        self.assert_same_ranges(rng, 64, key_space);
        self.bulk_changes(rng, key_space);
        self.ours.map.check_invariants();
        self.assert_same_iteration(rng);
        self.assert_clones_compare_alike(rng, key_space);
        self.ours.clear();
        self.theirs.clear();
        self.assert_same_contents();
        self.run_random_operations(rng, steps / 4, key_space);
        self.consume(rng);
    }

    /// `steps` random point operations on elements made from
    /// `0..key_space`. Inserts and replacements are as frequent as removals
    /// and pops together, which keeps about a third of the key space in the
    /// sets.
    fn run_random_operations(&mut self, rng: &TestRng, steps: usize, key_space: usize) {
        for _ in 0..steps {
            let value = T::from_number(rng.below(key_space));
            match rng.below(10) {
                0..3 => assert_eq!(self.ours.insert(value.clone()), self.theirs.insert(value)),
                3 => assert_eq!(self.ours.replace(value.clone()), self.theirs.replace(value)),
                4 => assert_eq!(self.ours.remove(&value), self.theirs.remove(&value)),
                5 => assert_eq!(self.ours.take(&value), self.theirs.take(&value)),
                6 => assert_eq!(self.ours.pop_first(), self.theirs.pop_first()),
                7 => assert_eq!(self.ours.pop_last(), self.theirs.pop_last()),
                8 => {
                    assert_eq!(self.ours.contains(&value), self.theirs.contains(&value));
                    assert_eq!(self.ours.get(&value), self.theirs.get(&value));
                }
                _ => {
                    assert_eq!(self.ours.first(), self.theirs.first());
                    assert_eq!(self.ours.last(), self.theirs.last());
                }
            }
            assert_eq!(self.ours.len(), self.theirs.len());
            assert_eq!(self.ours.is_empty(), self.theirs.is_empty());
        }
    }

    /// The same elements, in order, and the same print while small.
    fn assert_same_contents(&self) {
        assert_eq!(self.ours.len(), self.theirs.len());
        assert!(self.ours.iter().eq(&self.theirs));
        if self.theirs.len() <= PRINTED_LEN {
            assert_eq!(format!("{:?}", self.ours), format!("{:?}", self.theirs));
        }
    }

    /// Walks the whole sets from both ends at random until the ends meet,
    /// and once more each way by `fold`; checks the shortcuts to the ends,
    /// and the prints of small sets' iterators.
    fn assert_same_iteration(&self, rng: &TestRng) {
        let steps = self.theirs.len() + 1;
        rng.assert_same_items(self.ours.iter(), self.theirs.iter(), steps);
        let ours = IntoIterator::into_iter(&self.ours);
        rng.assert_same_items(ours, self.theirs.iter(), steps);
        Self::assert_same_folds(self.ours.iter(), self.theirs.iter());
        assert_eq!(self.ours.iter().last(), self.theirs.iter().last());
        assert_eq!(self.ours.iter().min(), self.theirs.iter().min());
        assert_eq!(self.ours.iter().max(), self.theirs.iter().max());
        if self.theirs.len() <= PRINTED_LEN {
            let (mut ours, mut theirs) = (self.ours.iter(), self.theirs.iter());
            assert_eq!(ours.next(), theirs.next());
            assert_eq!(format!("{ours:?}"), format!("{theirs:?}"));
        }
    }

    /// The items of `ours` and `theirs` by `fold`, and backwards by `rfold`.
    fn assert_same_folds<'a>(
        ours: impl DoubleEndedIterator<Item = &'a T> + Clone,
        theirs: impl DoubleEndedIterator<Item = &'a T> + Clone,
    ) where
        T: 'a,
    {
        assert_eq!(Self::folded(ours.clone()), Self::folded(theirs.clone()));
        assert_eq!(Self::folded(ours.rev()), Self::folded(theirs.rev()));
    }

    /// The items of `items`, by `for_each`, which folds.
    fn folded<'a>(items: impl Iterator<Item = &'a T>) -> Vec<&'a T> {
        let mut folded = Vec::new();
        items.for_each(|item| folded.push(item));
        folded
    }

    /// Checks `count` ranges between random bounds in order, from both ends,
    /// by `fold`, and by the shortcuts to the ends; and the prints of small
    /// ones.
    fn assert_same_ranges(&self, rng: &TestRng, count: usize, key_space: usize) {
        for _ in 0..count {
            let bounds = rng.range_bounds(key_space, T::from_number);
            let bounds = (bounds.0.as_ref(), bounds.1.as_ref());
            let ours = self.ours.range::<T, _>(bounds);
            let theirs = self.theirs.range::<T, _>(bounds);
            rng.assert_same_items(ours.clone(), theirs.clone(), PRINTED_LEN);
            Self::assert_same_folds(ours.clone(), theirs.clone());
            assert_eq!(ours.clone().last(), theirs.clone().last());
            assert_eq!(ours.clone().min(), theirs.clone().min());
            assert_eq!(ours.clone().max(), theirs.clone().max());
            if theirs.clone().count() <= PRINTED_LEN {
                assert_eq!(format!("{ours:?}"), format!("{theirs:?}"));
            }
        }
    }

    /// `retain`, and `extract_if` over a random range stopped partway, by
    /// the same random choices; a split at a random element and an `append`
    /// that puts the halves back together; and an `append` of a random set
    /// that overlaps, made by `extend`.
    fn bulk_changes(&mut self, rng: &TestRng, key_space: usize) {
        let seed = rng.below(1 << 30) as u64;
        let (ours_choices, theirs_choices) = (TestRng::new(seed), TestRng::new(seed));
        self.ours.retain(|_| ours_choices.below(8) != 0);
        self.theirs.retain(|_| theirs_choices.below(8) != 0);
        self.assert_same_contents();

        let bounds = rng.range_bounds(key_space, T::from_number);
        let mut ours = self
            .ours
            .extract_if(bounds.clone(), |_| ours_choices.below(4) == 0);
        let mut theirs = self
            .theirs
            .extract_if(bounds, |_| theirs_choices.below(4) == 0);
        for _ in 0..rng.below(key_space / 8 + 2) {
            assert_eq!(ours.size_hint(), theirs.size_hint());
            if theirs.size_hint().1 <= Some(PRINTED_LEN) {
                assert_eq!(format!("{ours:?}"), format!("{theirs:?}"));
            }
            let extracted = theirs.next();
            assert_eq!(ours.next(), extracted);
            if extracted.is_none() {
                break;
            }
        }
        drop((ours, theirs));
        self.assert_same_contents();

        let at = T::from_number(rng.below(key_space));
        let mut ours_high = self.ours.split_off(&at);
        let mut theirs_high = self.theirs.split_off(&at);
        assert!(ours_high.iter().eq(&theirs_high));
        self.assert_same_contents();
        self.ours.append(&mut ours_high);
        self.theirs.append(&mut theirs_high);
        assert!(ours_high.is_empty());
        self.assert_same_contents();

        let numbers: Vec<usize> = (0..rng.below(key_space / 4 + 1))
            .map(|_| rng.below(key_space))
            .collect();
        let mut ours_other = BBTreeSet::with_strategy(S::default());
        ours_other.extend(numbers.iter().map(|&number| T::from_number(number)));
        let mut theirs_other = BTreeSet::new();
        theirs_other.extend(numbers.iter().map(|&number| T::from_number(number)));
        assert!(ours_other.iter().eq(&theirs_other));
        self.ours.append(&mut ours_other);
        self.theirs.append(&mut theirs_other);
        assert!(ours_other.is_empty());
        self.assert_same_contents();
    }

    /// Clones both sets, changes at most one element of the clones, and
    /// compares, orders and hashes the sets against their clones.
    fn assert_clones_compare_alike(&self, rng: &TestRng, key_space: usize) {
        let mut ours_clone = self.ours.clone();
        let mut theirs_clone = self.theirs.clone();
        assert!(ours_clone.iter().eq(&theirs_clone));
        let value = T::from_number(rng.below(key_space));
        match rng.below(3) {
            0 => assert_eq!(ours_clone.insert(value.clone()), theirs_clone.insert(value)),
            1 => assert_eq!(ours_clone.remove(&value), theirs_clone.remove(&value)),
            _ => {}
        }
        assert_eq!(self.ours == ours_clone, self.theirs == theirs_clone);
        assert_eq!(
            self.ours.partial_cmp(&ours_clone),
            self.theirs.partial_cmp(&theirs_clone)
        );
        assert_eq!(self.ours.cmp(&ours_clone), self.theirs.cmp(&theirs_clone));
        assert_eq!(hash_of(&ours_clone), hash_of(&theirs_clone));
    }

    /// Sets of the elements made from `numbers`.
    fn from_numbers(numbers: impl Iterator<Item = usize> + Clone) -> Self {
        let mut sets = Self::new();
        sets.ours.extend(numbers.clone().map(T::from_number));
        sets.theirs.extend(numbers.map(T::from_number));
        sets
    }

    /// Combines these sets with `other`, both ways, and with themselves, by
    /// every operation on two sets, lazy and building a set, against std's.
    fn assert_same_set_operations(&self, other: &Self, rng: &TestRng) {
        for (a, b) in [(self, other), (other, self), (self, self)] {
            let (ours, theirs) = ((&a.ours, &b.ours), (&a.theirs, &b.theirs));
            let difference = (ours.0.difference(ours.1), theirs.0.difference(theirs.1));
            Self::assert_same_combination(rng, difference.0, difference.1);
            let intersection = (ours.0.intersection(ours.1), theirs.0.intersection(theirs.1));
            Self::assert_same_combination(rng, intersection.0, intersection.1);
            let union = (ours.0.union(ours.1), theirs.0.union(theirs.1));
            Self::assert_same_combination(rng, union.0, union.1);
            let symmetric_difference = (
                ours.0.symmetric_difference(ours.1),
                theirs.0.symmetric_difference(theirs.1),
            );
            Self::assert_same_combination(rng, symmetric_difference.0, symmetric_difference.1);
            assert_eq!(ours.0.is_subset(ours.1), theirs.0.is_subset(theirs.1));
            assert_eq!(ours.0.is_superset(ours.1), theirs.0.is_superset(theirs.1));
            assert_eq!(ours.0.is_disjoint(ours.1), theirs.0.is_disjoint(theirs.1));
            let built = [
                (ours.0 - ours.1, theirs.0 - theirs.1),
                (ours.0 & ours.1, theirs.0 & theirs.1),
                (ours.0 | ours.1, theirs.0 | theirs.1),
                (ours.0 ^ ours.1, theirs.0 ^ theirs.1),
            ];
            for (ours, theirs) in built {
                ours.map.check_invariants();
                assert_eq!(ours.len(), theirs.len());
                assert!(ours.iter().eq(&theirs));
            }
        }
    }

    /// The operations that look this set's elements up in `large`, which
    /// has at least 16 times as many, against std's.
    fn assert_same_lookups_in(&self, large: &Self, rng: &TestRng) {
        let (ours, theirs) = ((&self.ours, &large.ours), (&self.theirs, &large.theirs));
        for (ours, theirs) in [
            (ours.0.intersection(ours.1), theirs.0.intersection(theirs.1)),
            (ours.1.intersection(ours.0), theirs.1.intersection(theirs.0)),
        ] {
            Self::assert_same_combination(rng, ours, theirs);
        }
        let difference = (ours.0.difference(ours.1), theirs.0.difference(theirs.1));
        Self::assert_same_combination(rng, difference.0, difference.1);
        assert_eq!(ours.0.is_subset(ours.1), theirs.0.is_subset(theirs.1));
        assert_eq!(ours.1.is_superset(ours.0), theirs.1.is_superset(theirs.0));
        assert_eq!(ours.0.is_disjoint(ours.1), theirs.0.is_disjoint(theirs.1));
        for (ours, theirs) in [
            (ours.0 & ours.1, theirs.0 & theirs.1),
            (ours.0 - ours.1, theirs.0 - theirs.1),
        ] {
            assert!(ours.iter().eq(&theirs));
        }
    }

    /// Steps `ours` and `theirs`, over the same combination of two sets, alike
    /// to their ends: the same items and size hints. Their `min` and `fold`
    /// from the start, and clones taken at a random step, folded and stepped,
    /// go on alike too.
    fn assert_same_combination<'a>(
        rng: &TestRng,
        mut ours: impl Iterator<Item = &'a T> + Clone,
        mut theirs: impl Iterator<Item = &'a T> + Clone,
    ) where
        T: 'a,
    {
        assert_eq!(ours.clone().min(), theirs.clone().min());
        assert_eq!(Self::folded(ours.clone()), Self::folded(theirs.clone()));
        let mut clones = None;
        loop {
            assert_eq!(ours.size_hint(), theirs.size_hint());
            if clones.is_none() && rng.below(8) == 0 {
                clones = Some((ours.clone(), theirs.clone()));
            }
            let item = theirs.next();
            assert_eq!(ours.next(), item);
            if item.is_none() {
                assert_eq!(ours.next(), None);
                break;
            }
        }
        if let Some((ours, theirs)) = clones {
            let (ours_rest, theirs_rest) = (ours.clone(), theirs.clone());
            assert_eq!(Self::folded(ours), Self::folded(theirs));
            assert!(ours_rest.eq(theirs_rest));
        }
    }

    /// Takes both sets by value and walks them from both ends at random,
    /// through about half of their elements, then drops what is left of
    /// both. A small set's iterators print the same first.
    fn consume(&mut self, rng: &TestRng) {
        let ours = mem::replace(&mut self.ours, BBTreeSet::with_strategy(S::default()));
        let (ours, theirs) = (ours.into_iter(), mem::take(&mut self.theirs).into_iter());
        if theirs.len() <= PRINTED_LEN {
            assert_eq!(format!("{ours:?}"), format!("{theirs:?}"));
        }
        let steps = theirs.len() / 2 + 1;
        rng.assert_same_items(ours, theirs, steps);
    }
}

#[test]
fn u64_set_matches_std() {
    let rng = TestRng::new(21);
    MirroredSets::<u64>::new().run(&rng, 100_000, 1 << 14);
    // Small sets, whose sets and iterators are printed.
    MirroredSets::<u64>::new().run(&rng, 200, 100);
}

#[test]
fn string_set_matches_std() {
    let rng = TestRng::new(22);
    let (steps, key_space) = if cfg!(miri) {
        (200, 300)
    } else {
        (50_000, 1 << 13)
    };
    MirroredSets::<String>::new().run(&rng, steps, key_space);
}

#[test]
fn zero_sized_set_matches_std() {
    let rng = TestRng::new(23);
    MirroredSets::<()>::new().run(&rng, 100, 1);
    let (empty, full) = (
        MirroredSets::<()>::new(),
        MirroredSets::<()>::from_numbers(0..1),
    );
    empty.assert_same_set_operations(&full, &rng);
    full.assert_same_set_operations(&full, &rng);
}

/// Lengths of pairs of sets: about where looking up beats walking, 16
/// times as many elements, on both sides of it, and far from it.
const PAIR_LENS: [(usize, usize); 13] = [
    (0, 0),
    (0, 7),
    (1, 1),
    (2, 31),
    (2, 32),
    (10, 10),
    (10, 159),
    (10, 160),
    (10, 161),
    (100, 1599),
    (100, 1600),
    (500, 2000),
    (20, 6000),
];

impl MirroredSets<u64> {
    /// Every operation on two sets, a set of `a_len` elements in each shape
    /// against one of `b_len`: overlapping, apart, touching at one end, and
    /// inside it with or without its ends.
    fn assert_same_operations_of_every_shape(a_len: usize, b_len: usize, rng: &TestRng) {
        let universe = 2 * (a_len + b_len) + 2;
        let b_numbers = rng.distinct_numbers(b_len, universe);
        let b = MirroredSets::<u64>::from_numbers(b_numbers.iter().copied());
        let overlapping = rng.distinct_numbers(a_len, universe);
        let apart = overlapping.iter().map(|number| number + universe);
        // Below `b` but for its first element, which is the top of these.
        let b_min = b_numbers.iter().copied().min().unwrap_or(0);
        let touching = overlapping
            .iter()
            .map(|number| b_min - number % (b_min + 1));
        let inside = b_numbers.iter().copied().take(a_len);
        let mut inside_without_ends = b_numbers.clone();
        inside_without_ends.sort_unstable();
        let inner = inside_without_ends.len().saturating_sub(1).max(1);
        let inside_without_ends = inside_without_ends
            .into_iter()
            .take(inner)
            .skip(1)
            .take(a_len);
        let shapes = [
            MirroredSets::from_numbers(overlapping.iter().copied()),
            MirroredSets::from_numbers(apart),
            MirroredSets::from_numbers(touching.chain(b_numbers.first().map(|_| b_min))),
            MirroredSets::from_numbers(inside),
            MirroredSets::from_numbers(inside_without_ends),
        ];
        for a in &shapes {
            a.assert_same_set_operations(&b, rng);
        }
    }
}

/// Every operation on two sets, on pairs of each length in [`PAIR_LENS`],
/// and of each shape: overlapping, apart, touching at one end, and one
/// inside the other with or without its ends.
#[test]
fn set_operations_match_std() {
    let rng = TestRng::new(25);
    for (a_len, b_len) in PAIR_LENS {
        MirroredSets::<u64>::assert_same_operations_of_every_shape(a_len, b_len, &rng);
    }
    // Strings, with inline keys of another width.
    let a = MirroredSets::<String>::from_numbers(rng.distinct_numbers(300, 1000).into_iter());
    let b = MirroredSets::<String>::from_numbers(rng.distinct_numbers(600, 1000).into_iter());
    a.assert_same_set_operations(&b, &rng);
}

/// The operations that look a small set's elements up in a large one, on
/// a set large enough to look them up a batch at a time (`lookups.rs`):
/// whole batches and a partial one, all found or some.
#[test]
fn set_operations_with_batched_lookups_match_std() {
    let rng = TestRng::new(26);
    let universe = 400_000;
    let large_numbers = rng.distinct_numbers(130_000, universe);
    let large = MirroredSets::<u64>::from_numbers(large_numbers.iter().copied());
    assert!(large.ours.map.batches(), "large enough to batch");
    for small_len in [33, 2000] {
        let overlapping = rng.distinct_numbers(small_len, universe);
        let inside = large_numbers.iter().copied().step_by(7).take(small_len);
        for small in [
            MirroredSets::from_numbers(overlapping.iter().copied()),
            MirroredSets::from_numbers(inside),
        ] {
            small.assert_same_lookups_in(&large, &rng);
        }
    }
}

/// An element ordered by its key alone, whose tag, which it prints, tells
/// equal elements apart: which of them a set keeps.
#[derive(Clone, Debug)]
struct Tagged {
    key: u64,
    tag: usize,
}

impl PartialEq for Tagged {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl Eq for Tagged {}

impl PartialOrd for Tagged {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Tagged {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key.cmp(&other.key)
    }
}

/// Of equal elements, `collect` and `from` keep the last; `insert`,
/// `extend` and `append` keep the one already in; `replace` swaps in the
/// new one; `get` and `take` hand back the one in; the operations on two
/// sets hand out one set's or the other's: all as std's do.
#[test]
fn which_of_equal_elements_stays_matches_std() {
    let rng = TestRng::new(24);
    let (trials, max_len) = if cfg!(miri) { (20, 60) } else { (30, 3000) };
    let same_print = |ours: &dyn Debug, theirs: &dyn Debug| {
        assert_eq!(format!("{ours:?}"), format!("{theirs:?}"));
    };
    for _ in 0..trials {
        let len = rng.below(max_len);
        let elements: Vec<Tagged> = (0..len)
            .map(|tag| Tagged {
                key: rng.below(len + 1) as u64,
                tag,
            })
            .collect();
        let ours: BBTreeSet<Tagged> = elements.iter().cloned().collect();
        let theirs: BTreeSet<Tagged> = elements.iter().cloned().collect();
        same_print(&ours, &theirs);

        let mut ours_extended = BBTreeSet::new();
        ours_extended.extend(elements.iter().cloned());
        let mut theirs_extended = BTreeSet::new();
        theirs_extended.extend(elements.iter().cloned());
        same_print(&ours_extended, &theirs_extended);

        let (mut ours_appended, mut ours_other) = (ours_extended.clone(), ours.clone());
        let (mut theirs_appended, mut theirs_other) = (theirs_extended.clone(), theirs.clone());
        ours_appended.append(&mut ours_other);
        theirs_appended.append(&mut theirs_other);
        same_print(&ours_appended, &theirs_appended);

        let (mut ours_taken, mut theirs_taken) = (ours.clone(), theirs.clone());
        let (mut ours_replaced, mut theirs_replaced) = (ours.clone(), theirs.clone());
        for element in elements.iter().take(16) {
            same_print(&ours.get(element), &theirs.get(element));
            same_print(&ours_taken.take(element), &theirs_taken.take(element));
            let replacement = Tagged {
                key: element.key,
                tag: len + element.tag,
            };
            same_print(
                &ours_replaced.replace(replacement.clone()),
                &theirs_replaced.replace(replacement),
            );
        }
        same_print(&ours_replaced, &theirs_replaced);
        ours_replaced.map.check_invariants();

        // `ours` holds the last of equal elements and `ours_extended` the
        // first, so the sets they make tell which side an operation hands
        // out: with a part of about a 16th, on both sides of where looking
        // up beats walking.
        let part: Vec<bool> = (0..ours.len()).map(|_| rng.below(16) == 0).collect();
        let ours_part: BBTreeSet<Tagged> = ours_extended
            .iter()
            .zip(&part)
            .filter(|&(_, &taken)| taken)
            .map(|(element, _)| element.clone())
            .collect();
        let theirs_part: BTreeSet<Tagged> = theirs_extended
            .iter()
            .zip(&part)
            .filter(|&(_, &taken)| taken)
            .map(|(element, _)| element.clone())
            .collect();
        for (ours, theirs) in [
            ((&ours, &ours_part), (&theirs, &theirs_part)),
            ((&ours_part, &ours), (&theirs_part, &theirs)),
        ] {
            for (ours, theirs) in [
                (
                    ours.0.intersection(ours.1).collect::<Vec<_>>(),
                    theirs.0.intersection(theirs.1).collect::<Vec<_>>(),
                ),
                (
                    ours.0.union(ours.1).collect(),
                    theirs.0.union(theirs.1).collect(),
                ),
                (
                    ours.0.difference(ours.1).collect(),
                    theirs.0.difference(theirs.1).collect(),
                ),
            ] {
                same_print(&ours, &theirs);
            }
            same_print(&(ours.0 & ours.1), &(theirs.0 & theirs.1));
            same_print(&(ours.0 | ours.1), &(theirs.0 | theirs.1));
        }
    }
    let array = [(3, 0), (1, 1), (3, 2), (2, 3), (1, 4)].map(|(key, tag)| Tagged { key, tag });
    same_print(&BBTreeSet::from(array.clone()), &BTreeSet::from(array));
    assert!(BBTreeSet::<u8>::from([]).is_empty());
}

#[test]
fn extending_by_reference_matches_std() {
    let numbers = [5_u64, 3, 5, 9, 1, 3];
    let mut ours = BBTreeSet::from([4, 5]);
    let mut theirs = BTreeSet::from([4, 5]);
    ours.extend(&numbers);
    theirs.extend(&numbers);
    assert!(ours.iter().eq(&theirs));
}

/// The operations on two sets print what each side has left.
#[test]
fn set_operations_print_what_is_left() {
    let (a, b) = (BBTreeSet::from([1, 2, 3]), BBTreeSet::from([2, 4]));
    let mut union = a.union(&b);
    union.next();
    assert_eq!(
        format!("{union:?}"),
        "Union(MergeSides { left: [2, 3], right: [2, 4] })"
    );
    assert_eq!(
        format!("{:?}", a.intersection(&b)),
        "Intersection(Stitch(MergeSides { left: [1, 2, 3], right: [2, 4] }))"
    );
}

/// Iterators over nothing, made by `Default` or over an empty set, print as
/// std's do.
#[test]
fn empty_iterators_print_like_std() {
    let same_print = |ours: &dyn Debug, theirs: &dyn Debug| {
        assert_eq!(format!("{ours:?}"), format!("{theirs:?}"));
    };
    same_print(
        &Iter::<'_, u8>::default(),
        &std::collections::btree_set::Iter::<'_, u8>::default(),
    );
    same_print(
        &Range::<'_, u8>::default(),
        &std::collections::btree_set::Range::<'_, u8>::default(),
    );
    same_print(
        &IntoIter::<u8>::default(),
        &std::collections::btree_set::IntoIter::<u8>::default(),
    );
    let (ours, theirs) = (BBTreeSet::<u8>::new(), BTreeSet::<u8>::new());
    same_print(&ours, &theirs);
    same_print(&ours.iter(), &theirs.iter());
    same_print(&ours.range(1..), &theirs.range(1..));
    same_print(&ours.into_iter(), &theirs.into_iter());
}

/// Compiles only if the set is covariant in its elements, as std's is.
fn shorten_lifetimes<'a>(set: BBTreeSet<&'static str>) -> BBTreeSet<&'a str> {
    set
}

#[test]
fn covariant_in_elements() {
    let local = String::from("local");
    let mut set = BBTreeSet::new();
    set.insert("static");
    let mut set = shorten_lifetimes(set);
    set.insert(local.as_str());
    assert!(set.contains("static"));
    assert!(set.contains("local"));
}

/// Our iterators over a set whose elements live for `'k`.
type StrIterators<'a, 'k> = (Iter<'a, &'k str>, Range<'a, &'k str>);

/// Compiles only if the iterators are covariant in their elements, as std's
/// are.
fn shorten_iterator_lifetimes<'a>(iterators: StrIterators<'a, 'static>) -> StrIterators<'a, 'a> {
    iterators
}

#[test]
fn iterators_are_covariant_in_elements() {
    let set = BBTreeSet::from(["static"]);
    let iterators = (set.iter(), set.range::<&str, _>(..));
    let (iter, range) = shorten_iterator_lifetimes(iterators);
    assert_eq!(iter.count() + range.count(), 2);
}

/// The map's three words, its values taking no space.
#[test]
#[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
fn three_words_like_the_map() {
    assert_eq!(size_of::<BBTreeSet<u64>>(), 24);
    assert_eq!(size_of::<BBTreeSet<u64>>(), size_of::<BBTreeMap<u64, ()>>());
    assert_eq!(size_of::<BBTreeSet<String, WithInlineKey>>(), 24);
}

#[test]
fn new_is_const() {
    const EMPTY: BBTreeSet<String> = BBTreeSet::new();
    let set = EMPTY;
    assert!(set.is_empty());
    assert_eq!(set.len(), 0);
    assert!(!set.contains("absent"));
    assert_eq!(set.first(), None);
}

/// A set operator's result takes its nodes from a clone of the left set's
/// allocator.
#[test]
fn set_operators_build_from_the_left_sets_allocator() {
    let (left_pages, right_pages) = (TestPages::<u64, ()>::new(), TestPages::<u64, ()>::new());
    {
        let mut left = BBTreeSet::new_in(&left_pages);
        let mut right = BBTreeSet::new_in(&right_pages);
        left.extend(0..300);
        right.extend(100..400);
        let in_use = left_pages.in_use_count();
        let difference = &left - &right;
        assert!(difference.iter().copied().eq(0..100));
        assert!(left_pages.in_use_count() > in_use);
    }
    left_pages.assert_all_released();
    right_pages.assert_all_released();
}
