//! Differential fuzzing of the operations on two sets (D5): union,
//! intersection, difference and symmetric difference, as iterators and as
//! operators, and the subset and disjointness tests, on a pair of
//! `unsync::BBTreeSet`s and a pair of std's `BTreeSet`s, fed the same changes.
//!
//! A set is a map of zero-sized values, whose own methods hand their work
//! to the map's, which the map targets fuzz. What is the set's own is the
//! code on two sets (`src/set/`): the strategy each operation picks from the
//! sets' lengths and ends, the merge of two walks a leaf's run at a time
//! (`set/merge.rs`), and how its iterators print, rebuilding std's. The
//! changes here are only those that give the pair every shape: apart,
//! touching, nested, and of lengths on both sides of where looking up beats
//! walking.

use std::collections::BTreeSet;
use std::fmt::Debug;

use bbtree::unsync::BBTreeSet;
use libfuzzer_sys::arbitrary::{self, Arbitrary};

use crate::key_recipe::{FuzzKey, KeyRecipe, Stems};
use crate::operation_list::Operations;
use crate::position_mask::PositionMask;

/// One input of the set target.
#[derive(Arbitrary, Debug)]
pub struct SetInput {
    elements: Elements,
    stems: Stems,
    operations: Operations<SetOperation>,
}

impl SetInput {
    pub fn run(&self) {
        match self.elements {
            Elements::Integers => MirroredSets::<u64>::run(self),
            Elements::ByteStrings => MirroredSets::<Vec<u8>>::run(self),
        }
    }
}

/// The sets' element type.
#[derive(Arbitrary, Debug, Clone, Copy)]
enum Elements {
    /// `u64`: about a hundred elements to a leaf.
    Integers,
    /// `Vec<u8>`: about thirty to a leaf, and elements that own heap
    /// memory, which the operators clone.
    ByteStrings,
}

/// One set of the pair.
#[derive(Arbitrary, Debug, Clone, Copy)]
enum Side {
    Left,
    Right,
}

/// An operation on two sets, as an iterator or as an operator. The
/// differences are of the left set less the right one, and the reverse.
#[derive(Arbitrary, Debug, Clone, Copy)]
enum Combination {
    Union,
    Intersection,
    Difference,
    ReversedDifference,
    SymmetricDifference,
}

/// How a walk of a combination takes what is left after its steps.
#[derive(Arbitrary, Debug, Clone, Copy)]
enum CombinationFinish {
    Fold,
    Count,
    Last,
    Min,
    Max,
    /// Walks a clone of the iterator to the end, then the iterator itself.
    Cloned,
}

/// One operation on both pairs of sets.
#[derive(Arbitrary, Debug)]
enum SetOperation {
    ContainsBatch {
        side: Side,
        elements: [KeyRecipe; 8],
    },
    InsertBatch {
        side: Side,
        elements: [KeyRecipe; 8],
    },
    Extend {
        side: Side,
        elements: Vec<KeyRecipe>,
    },
    Insert {
        side: Side,
        element: KeyRecipe,
    },
    /// Inserts `count` elements of consecutive numbers from `first`.
    InsertRun {
        side: Side,
        first: KeyRecipe,
        count: u8,
    },
    Remove {
        side: Side,
        element: KeyRecipe,
    },
    /// Removes `count` elements of consecutive numbers from `first`.
    RemoveRun {
        side: Side,
        first: KeyRecipe,
        count: u8,
    },
    /// Keeps the elements that `keep` picks.
    Retain {
        side: Side,
        keep: PositionMask,
    },
    /// Keeps the part of the set below `at`, or the part from it on.
    SplitOff {
        side: Side,
        at: KeyRecipe,
        keep_upper: bool,
    },
    /// Makes the set a clone of the other one.
    CopyOther {
        side: Side,
    },
    /// Takes `steps` items of the combination's iterator, then the rest as
    /// `finish` says.
    Walk {
        combination: Combination,
        steps: u8,
        finish: CombinationFinish,
    },
    /// The subset, superset and disjointness tests, both ways, and the
    /// sets' comparison.
    Relate,
    /// Builds the combination's set by its operator.
    Build {
        combination: Combination,
    },
}

/// Our set and std's, fed the same changes.
struct SetPair<T> {
    ours: BBTreeSet<T>,
    theirs: BTreeSet<T>,
}

impl<T: FuzzKey> SetPair<T> {
    fn assert_same_elements(&self) {
        assert_eq!(self.ours.len(), self.theirs.len());
        assert!(self.ours.iter().eq(self.theirs.iter()));
    }
}

/// Two pairs of sets, left and right, fed the same operations.
struct MirroredSets<'i, T> {
    stems: &'i Stems,
    left: SetPair<T>,
    right: SetPair<T>,
}

impl<'i, T: FuzzKey> MirroredSets<'i, T> {
    /// Runs the input's operations on two fresh pairs of sets, comparing
    /// every answer and checking our trees after each change.
    fn run(input: &'i SetInput) {
        let mut sets = Self {
            stems: &input.stems,
            left: SetPair {
                ours: BBTreeSet::new(),
                theirs: BTreeSet::new(),
            },
            right: SetPair {
                ours: BBTreeSet::new(),
                theirs: BTreeSet::new(),
            },
        };
        for operation in &input.operations {
            sets.apply(operation);
        }
        sets.left.assert_same_elements();
        sets.right.assert_same_elements();
    }

    fn element(&self, recipe: KeyRecipe) -> T {
        T::from_recipe(recipe, self.stems)
    }

    fn side_mut(&mut self, side: Side) -> &mut SetPair<T> {
        match side {
            Side::Left => &mut self.left,
            Side::Right => &mut self.right,
        }
    }

    fn apply(&mut self, operation: &SetOperation) {
        match *operation {
            SetOperation::ContainsBatch { side, elements } => {
                let elements = elements.map(|element| self.element(element));
                let pair = self.side_mut(side);
                let queries = elements.each_ref();
                assert_eq!(
                    pair.ours.contains_many(queries),
                    queries.map(|element| pair.theirs.contains(element))
                );
            }
            SetOperation::InsertBatch { side, elements } => {
                let elements = elements.map(|element| self.element(element));
                self.change(side, |pair| {
                    let mut individual = pair.ours.clone();
                    let expected = elements.clone().map(|element| pair.theirs.insert(element));
                    assert_eq!(
                        elements.clone().map(|element| individual.insert(element)),
                        expected
                    );
                    assert_eq!(pair.ours.insert_many(elements), expected);
                    pair.ours.check_same_nodes(&individual);
                });
            }
            SetOperation::Extend { side, ref elements } => {
                let elements: Vec<_> = elements
                    .iter()
                    .map(|&element| self.element(element))
                    .collect();
                self.change(side, |pair| {
                    let mut individual = pair.ours.clone();
                    for element in elements.iter().cloned() {
                        individual.insert(element);
                    }
                    pair.ours.extend(elements.iter().cloned());
                    pair.theirs.extend(elements);
                    pair.ours.check_same_nodes(&individual);
                });
            }
            SetOperation::Insert { side, element } => {
                let element = self.element(element);
                self.change(side, |pair| {
                    assert_eq!(
                        pair.ours.insert(element.clone()),
                        pair.theirs.insert(element)
                    );
                });
            }
            SetOperation::InsertRun { side, first, count } => {
                let elements: Vec<T> = (0..u16::from(count))
                    .map(|offset| self.element(first.after(offset)))
                    .collect();
                self.change(side, |pair| {
                    for element in elements {
                        assert_eq!(
                            pair.ours.insert(element.clone()),
                            pair.theirs.insert(element)
                        );
                    }
                });
            }
            SetOperation::Remove { side, element } => {
                let element = self.element(element);
                self.change(side, |pair| {
                    assert_eq!(pair.ours.remove(&element), pair.theirs.remove(&element));
                });
            }
            SetOperation::RemoveRun { side, first, count } => {
                let elements: Vec<T> = (0..u16::from(count))
                    .map(|offset| self.element(first.after(offset)))
                    .collect();
                self.change(side, |pair| {
                    for element in &elements {
                        assert_eq!(pair.ours.remove(element), pair.theirs.remove(element));
                    }
                });
            }
            SetOperation::Retain { side, keep } => self.change(side, |pair| {
                let (mut ours, mut theirs) = (keep.picker(), keep.picker());
                pair.ours.retain(|_| ours());
                pair.theirs.retain(|_| theirs());
            }),
            SetOperation::SplitOff {
                side,
                at,
                keep_upper,
            } => {
                let at = self.element(at);
                self.change(side, |pair| {
                    let (ours, theirs) = (pair.ours.split_off(&at), pair.theirs.split_off(&at));
                    ours.check_invariants();
                    pair.ours.check_invariants();
                    assert!(ours.iter().eq(theirs.iter()));
                    if keep_upper {
                        (pair.ours, pair.theirs) = (ours, theirs);
                    }
                });
            }
            SetOperation::CopyOther { side } => {
                let (ours, theirs) = match side {
                    Side::Left => (self.right.ours.clone(), self.right.theirs.clone()),
                    Side::Right => (self.left.ours.clone(), self.left.theirs.clone()),
                };
                self.change(side, |pair| (pair.ours, pair.theirs) = (ours, theirs));
            }
            SetOperation::Walk {
                combination,
                steps,
                finish,
            } => self.walk(combination, steps, finish),
            SetOperation::Relate => {
                let (left, right) = (&self.left, &self.right);
                assert_eq!(
                    left.ours.is_subset(&right.ours),
                    left.theirs.is_subset(&right.theirs)
                );
                assert_eq!(
                    right.ours.is_subset(&left.ours),
                    right.theirs.is_subset(&left.theirs)
                );
                assert_eq!(
                    left.ours.is_superset(&right.ours),
                    left.theirs.is_superset(&right.theirs)
                );
                assert_eq!(
                    left.ours.is_disjoint(&right.ours),
                    left.theirs.is_disjoint(&right.theirs)
                );
                assert_eq!(
                    right.ours.is_disjoint(&left.ours),
                    right.theirs.is_disjoint(&left.theirs)
                );
                assert_eq!(left.ours == right.ours, left.theirs == right.theirs);
                assert_eq!(left.ours.cmp(&right.ours), left.theirs.cmp(&right.theirs));
            }
            SetOperation::Build { combination } => self.build(combination),
        }
    }

    /// Changes one pair by `change`, then checks our tree and that the sets
    /// agree.
    fn change(&mut self, side: Side, change: impl FnOnce(&mut SetPair<T>)) {
        let pair = self.side_mut(side);
        change(pair);
        pair.ours.check_invariants();
        assert_eq!(pair.ours.len(), pair.theirs.len());
    }

    fn walk(&self, combination: Combination, steps: u8, finish: CombinationFinish) {
        let (left, right) = (&self.left, &self.right);
        let walk = CombinationWalk { steps, finish };
        match combination {
            Combination::Union => walk.assert_same(
                left.ours.union(&right.ours),
                left.theirs.union(&right.theirs),
            ),
            Combination::Intersection => walk.assert_same(
                left.ours.intersection(&right.ours),
                left.theirs.intersection(&right.theirs),
            ),
            Combination::Difference => walk.assert_same(
                left.ours.difference(&right.ours),
                left.theirs.difference(&right.theirs),
            ),
            Combination::ReversedDifference => walk.assert_same(
                right.ours.difference(&left.ours),
                right.theirs.difference(&left.theirs),
            ),
            Combination::SymmetricDifference => walk.assert_same(
                left.ours.symmetric_difference(&right.ours),
                left.theirs.symmetric_difference(&right.theirs),
            ),
        }
    }

    fn build(&self, combination: Combination) {
        let (left, right) = (&self.left, &self.right);
        let (ours, theirs) = match combination {
            Combination::Union => (&left.ours | &right.ours, &left.theirs | &right.theirs),
            Combination::Intersection => (&left.ours & &right.ours, &left.theirs & &right.theirs),
            Combination::Difference => (&left.ours - &right.ours, &left.theirs - &right.theirs),
            Combination::ReversedDifference => {
                (&right.ours - &left.ours, &right.theirs - &left.theirs)
            }
            Combination::SymmetricDifference => {
                (&left.ours ^ &right.ours, &left.theirs ^ &right.theirs)
            }
        };
        ours.check_invariants();
        SetPair { ours, theirs }.assert_same_elements();
    }
}

/// How a walk of a combination goes: `steps` items, then the rest as
/// `finish` says.
struct CombinationWalk {
    steps: u8,
    finish: CombinationFinish,
}

impl CombinationWalk {
    /// Walks both iterators: they must match all along, and so must their size
    /// hints.
    fn assert_same<'a, T, I, J>(&self, mut ours: I, mut theirs: J)
    where
        T: Ord + Debug + 'a,
        I: Iterator<Item = &'a T> + Clone,
        J: Iterator<Item = &'a T> + Clone,
    {
        for _ in 0..self.steps {
            assert_eq!(ours.size_hint(), theirs.size_hint());
            assert_eq!(ours.next(), theirs.next());
        }
        assert_eq!(ours.size_hint(), theirs.size_hint());
        let collect = |mut items: Vec<&'a T>, item| {
            items.push(item);
            items
        };
        match self.finish {
            CombinationFinish::Fold => {
                assert_eq!(ours.fold(vec![], collect), theirs.fold(vec![], collect));
            }
            CombinationFinish::Count => assert_eq!(ours.count(), theirs.count()),
            CombinationFinish::Last => assert_eq!(ours.last(), theirs.last()),
            CombinationFinish::Min => assert_eq!(ours.min(), theirs.min()),
            CombinationFinish::Max => assert_eq!(ours.max(), theirs.max()),
            CombinationFinish::Cloned => {
                assert!(ours.clone().eq(theirs.clone()));
                assert!(ours.eq(theirs));
            }
        }
    }
}
