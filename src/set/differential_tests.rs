//! Differential tests: the set against std's `BTreeSet`, fed the same
//! operations. The set's tree is a map's, of zero-sized values, which the
//! map's tests cover (`map/differential_tests.rs`); these check that each of
//! the set's methods reaches the right one of the map's and gives std's
//! answers, down to which of equal elements stays and what each iterator
//! prints.

#![forbid(unsafe_code)]

use core::cmp::Ordering;
use core::fmt::Debug;
use core::hash::{Hash, Hasher};
use core::mem;
use core::ops::Bound;
use std::collections::BTreeSet;
use std::format;
use std::hash::DefaultHasher;
use std::string::String;
use std::vec::Vec;

use super::{BBTreeSet, IntoIter, Iter, Range};
use crate::inline_key::{Auto, InlineKeyStrategy};
use crate::node::test_support::{FromNumber, TestRng};

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
        self.assert_same_iteration(rng);
        self.assert_same_ranges(rng, 64, key_space);
        self.bulk_changes(rng, key_space);
        self.assert_same_iteration(rng);
        self.assert_clones_compare_alike(rng, key_space);
        self.ours.clear();
        self.theirs.clear();
        self.assert_same_contents();
        self.run_random_operations(rng, steps / 4, key_space);
        self.consume(rng);
    }

    /// `steps` random point operations on elements made from
    /// `0..key_space`. Inserts are as frequent as removals and pops
    /// together, which keeps about a third of the key space in the sets.
    fn run_random_operations(&mut self, rng: &TestRng, steps: usize, key_space: usize) {
        for _ in 0..steps {
            let value = T::from_number(rng.below(key_space));
            match rng.below(10) {
                0..4 => assert_eq!(self.ours.insert(value.clone()), self.theirs.insert(value)),
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
            let bounds = Self::random_range_bounds(rng, key_space);
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

    /// Random bounds in order, at elements made from `0..key_space`, of
    /// every kind; half of them narrow.
    fn random_range_bounds(rng: &TestRng, key_space: usize) -> (Bound<T>, Bound<T>) {
        let low_number = rng.below(key_space);
        let high_number = if rng.below(2) == 0 {
            low_number + rng.below(48)
        } else {
            rng.below(key_space)
        };
        let (mut low, mut high) = (T::from_number(low_number), T::from_number(high_number));
        if low > high {
            mem::swap(&mut low, &mut high);
        }
        let random_bound = |value| match rng.below(5) {
            0 => Bound::Unbounded,
            1 | 2 => Bound::Included(value),
            _ => Bound::Excluded(value),
        };
        let mut start = random_bound(low);
        let end = random_bound(high);
        if let (Bound::Excluded(start_value), Bound::Excluded(end_value)) = (&start, &end)
            && start_value == end_value
        {
            start = Bound::Included(start_value.clone());
        }
        (start, end)
    }

    /// `retain` by the same random choices; a split at a random element and
    /// an `append` that puts the halves back together; and an `append` of a
    /// random set that overlaps, made by `extend`.
    fn bulk_changes(&mut self, rng: &TestRng, key_space: usize) {
        let seed = rng.below(1 << 30) as u64;
        let (ours_choices, theirs_choices) = (TestRng::new(seed), TestRng::new(seed));
        self.ours.retain(|_| ours_choices.below(8) != 0);
        self.theirs.retain(|_| theirs_choices.below(8) != 0);
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
        let hash = |set: &dyn Fn(&mut DefaultHasher)| {
            let mut hasher = DefaultHasher::new();
            set(&mut hasher);
            hasher.finish()
        };
        assert_eq!(self.ours == ours_clone, self.theirs == theirs_clone);
        assert_eq!(
            self.ours.partial_cmp(&ours_clone),
            self.theirs.partial_cmp(&theirs_clone)
        );
        assert_eq!(self.ours.cmp(&ours_clone), self.theirs.cmp(&theirs_clone));
        assert_eq!(
            hash(&|hasher| ours_clone.hash(hasher)),
            hash(&|hasher| theirs_clone.hash(hasher))
        );
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
    let (steps, key_space) = if cfg!(miri) {
        (400, 600)
    } else {
        (100_000, 1 << 14)
    };
    MirroredSets::<u64>::new().run(&rng, steps, key_space);
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
}

/// An element ordered by its key alone, whose tag, which it prints, tells
/// equal elements apart: which of them a set keeps.
#[derive(Clone, Debug)]
struct Tagged {
    key: u64,
    #[expect(dead_code, reason = "read by `Debug` alone, which the tests compare")]
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
/// `extend` and `append` keep the one already in; `get` and `take` hand
/// back the one in: all as std's do.
#[test]
fn which_of_equal_elements_stays_matches_std() {
    let rng = TestRng::new(24);
    let (trials, max_len) = if cfg!(miri) { (20, 60) } else { (300, 3000) };
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
        for element in elements.iter().take(16) {
            same_print(&ours.get(element), &theirs.get(element));
            same_print(&ours_taken.take(element), &theirs_taken.take(element));
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
