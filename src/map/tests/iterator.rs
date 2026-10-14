//! Fold traversal must preserve the same order and disjoint mutable
//! references as repeated `next`, including after both ends have moved.

#![forbid(unsafe_code)]

use core::fmt::Debug;
use core::ops::Bound::{Excluded, Included, Unbounded};
use core::panic::AssertUnwindSafe;
use std::collections::BTreeMap;
use std::format;
use std::panic::catch_unwind;
use std::vec::Vec;

use crate::BBTreeMap;
use crate::tree::test_support::StepEitherEnd;

/// Our map and std's, with the same entries, for the tests to walk alike
/// and to change through the walks.
struct FoldMaps {
    ours: BBTreeMap<u64, u64>,
    theirs: BTreeMap<u64, u64>,
}

/// `entries` with `entry` pushed: a fold that collects what it visits.
fn push_entry<T>(mut entries: Vec<T>, entry: T) -> Vec<T> {
    entries.push(entry);
    entries
}

#[test]
fn full_iteration_does_not_require_ordering() {
    struct NoOrd;

    let mut map: BBTreeMap<NoOrd, NoOrd> = BBTreeMap::new();
    assert_eq!(map.iter().count(), 0);
    assert!(map.iter_mut().next_back().is_none());
}

#[test]
#[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
fn borrowed_walk_counts_steps_from_both_ends() {
    for length in [0, 1, 27, 128, 8192] {
        let mut maps = FoldMaps::new(length);
        let mut ours = maps.ours.iter();
        let mut theirs = maps.theirs.iter();
        for index in 0..length + 2 {
            let back = index % 11 >= 7;
            assert_eq!(ours.step(back), theirs.step(back));
            assert_eq!(ours.len(), theirs.len());
            assert_eq!(ours.size_hint(), theirs.size_hint());
        }

        let mut ours = maps.ours.iter_mut();
        let mut theirs = maps.theirs.iter_mut();
        let mut held = Vec::new();
        for index in 0..113 {
            let back = index % 13 >= 5;
            let (actual, expected) = (ours.step(back), theirs.step(back));
            assert_eq!(actual.as_ref(), expected.as_ref());
            if let Some(entry) = actual {
                held.push(entry);
            }
            if let Some((&key, value)) = expected {
                *value += key + 1;
            }
            assert_eq!(ours.len(), theirs.len());
            assert_eq!(ours.size_hint(), theirs.size_hint());
        }
        let remaining = ours.fold(Vec::new(), push_entry);
        assert_eq!(remaining.len(), theirs.len());
        for (&key, value) in held.into_iter().chain(remaining) {
            *value += key + 1;
        }
        for (&key, value) in theirs {
            *value += key + 1;
        }
        assert!(maps.ours.iter().eq(&maps.theirs));
    }
}

impl FoldMaps {
    fn new(length: u64) -> Self {
        let entries = (0..length).map(|index| {
            let key = index * 4093 % length;
            (key, key * 3 + 7)
        });
        let mut ours = BBTreeMap::new();
        let mut theirs = BTreeMap::new();
        for (key, value) in entries {
            ours.insert(key, value);
            theirs.insert(key, value);
        }
        Self { ours, theirs }
    }

    fn assert_folds<'a>(
        mut ours: impl DoubleEndedIterator<Item = (&'a u64, &'a u64)> + Clone,
        mut theirs: impl DoubleEndedIterator<Item = (&'a u64, &'a u64)> + Clone,
        front: usize,
        back: usize,
    ) {
        for _ in 0..front {
            assert_eq!(ours.next(), theirs.next());
        }
        for _ in 0..back {
            assert_eq!(ours.next_back(), theirs.next_back());
        }
        assert_eq!(
            ours.clone().fold(Vec::new(), push_entry),
            theirs.clone().fold(Vec::new(), push_entry),
        );
        assert_eq!(
            ours.rfold(Vec::new(), push_entry),
            theirs.rfold(Vec::new(), push_entry),
        );
    }
}

/// Checks folds both ways over our map of `entries`, and a walk of its
/// `iter_mut` from both ends, against std's.
fn assert_folds_and_iter_mut_match_std<K: Ord + Clone + Debug, V: Clone + Debug + Eq>(
    entries: Vec<(K, V)>,
) {
    let mut ours: BBTreeMap<K, V> = entries.clone().into_iter().collect();
    let mut theirs: BTreeMap<K, V> = entries.into_iter().collect();
    assert_eq!(
        ours.iter().fold(Vec::new(), push_entry),
        theirs.iter().collect::<Vec<_>>(),
    );
    assert_eq!(
        ours.iter().rfold(Vec::new(), push_entry),
        theirs.iter().rev().collect::<Vec<_>>(),
    );
    let count = ours.len();
    let mut actual = ours.iter_mut();
    let mut expected = theirs.iter_mut();
    for index in 0..count + 2 {
        let back = index % 17 >= 10;
        assert_eq!(actual.step(back), expected.step(back));
        assert_eq!(actual.len(), expected.len());
    }
}

#[test]
#[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
fn folds_cover_different_leaf_strides() {
    #[derive(Clone, Debug, Eq, PartialEq)]
    #[repr(align(64))]
    struct AlignedValue(u64);

    assert_folds_and_iter_mut_match_std(
        (0..1024)
            .map(|key| (format!("key/{key:08}"), key))
            .collect(),
    );
    assert_folds_and_iter_mut_match_std((0..1024_u64).map(|key| (key, ())).collect());
    assert_folds_and_iter_mut_match_std(
        (0..1024)
            .map(|key| ([key, key + 1], AlignedValue(key)))
            .collect(),
    );
    assert_folds_and_iter_mut_match_std(
        (0..1024_u64).map(|key| (key, AlignedValue(key))).collect(),
    );
    assert_folds_and_iter_mut_match_std(std::vec![((), AlignedValue(7))]);
    assert_folds_and_iter_mut_match_std(std::vec![((), ())]);
}

#[test]
#[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
fn cached_runs_resume_after_try_fold_breaks() {
    use core::ops::ControlFlow;

    for reverse in [false, true] {
        let mut maps = FoldMaps::new(2048);
        let mut actual = maps.ours.range_mut(17..2007);
        let mut expected = maps.theirs.range_mut(17..2007);
        let mut held = Vec::new();
        for budget in [1, 7, 47, 64, 129] {
            let mut visit = |_, entry| {
                held.push(entry);
                if held.len() == budget {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            };
            if reverse {
                assert!(actual.try_rfold((), &mut visit).is_break());
            } else {
                assert!(actual.try_fold((), &mut visit).is_break());
            }
            for (&key, value) in held.drain(..) {
                let (&expected_key, expected_value) = if reverse {
                    expected.next_back().unwrap()
                } else {
                    expected.next().unwrap()
                };
                assert_eq!((key, *value), (expected_key, *expected_value));
                *value += 1;
                *expected_value += 1;
            }
            // Switching after an early return reconciles the cached pointer
            // before the opposite end can hand out another mutable value.
            assert_eq!(actual.next().as_ref(), expected.next().as_ref());
            assert_eq!(actual.next_back().as_ref(), expected.next_back().as_ref());
        }
        assert!(actual.eq(expected));
        assert!(maps.ours.iter().eq(&maps.theirs));
    }
}

#[test]
fn zero_stride_runs_count_logical_entries() {
    use crate::{InlineKey, InlineWord, WithInlineKey};
    use core::cmp::Ordering;

    // An inconsistent ordering lets a zero-sized key occupy many logical
    // slots. Iterator termination and count must remain structural even
    // when all of those slots have the same byte address.
    #[derive(Eq, PartialEq)]
    struct Repeated;
    impl Ord for Repeated {
        fn cmp(&self, _: &Self) -> Ordering {
            Ordering::Less
        }
    }
    impl PartialOrd for Repeated {
        fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
            Some(self.cmp(other))
        }
    }
    impl InlineKey for Repeated {
        fn inline_key_at(&self, _: usize) -> InlineWord {
            0
        }
    }
    let mut map = BBTreeMap::with_strategy(WithInlineKey);
    for _ in 0..2048 {
        map.insert(Repeated, ());
    }
    assert_eq!(map.len(), 2048);
    let mut iter = map.iter_mut();
    let mut held = Vec::new();
    for index in 0..2048 {
        held.push(if index % 3 == 0 {
            iter.next_back().unwrap()
        } else {
            iter.next().unwrap()
        });
        assert_eq!(iter.len(), 2047 - index);
    }
    assert!(iter.next().is_none());
    assert!(iter.next_back().is_none());
    assert_eq!(held.len(), 2048);
}

#[test]
#[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
fn folds_match_std_after_both_ends_move() {
    for length in [0, 1, 16, 256, 8192] {
        let maps = FoldMaps::new(length);
        for (front, back) in [(0, 0), (1, 0), (0, 1), (7, 13), (71, 83), (8192, 0)] {
            FoldMaps::assert_folds(maps.ours.iter(), maps.theirs.iter(), front, back);
            FoldMaps::assert_folds(maps.ours.range(..), maps.theirs.range(..), front, back);
            FoldMaps::assert_folds(
                maps.ours.range(length / 3..),
                maps.theirs.range(length / 3..),
                front,
                back,
            );
            FoldMaps::assert_folds(
                maps.ours.range(..length / 3),
                maps.theirs.range(..length / 3),
                front,
                back,
            );
            for bounds in [
                0..length,
                length / 4..3 * length / 4,
                length / 2..length / 2,
                0..length / 3,
            ] {
                FoldMaps::assert_folds(
                    maps.ours.range(bounds.clone()),
                    maps.theirs.range(bounds),
                    front,
                    back,
                );
            }
        }
        assert_eq!(
            maps.ours.keys().copied().sum::<u64>(),
            maps.theirs.keys().copied().sum::<u64>(),
        );
        assert_eq!(
            maps.ours.values().copied().sum::<u64>(),
            maps.theirs.values().copied().sum::<u64>(),
        );
        assert_eq!(
            maps.ours.keys().rev().copied().collect::<Vec<_>>(),
            maps.theirs.keys().rev().copied().collect::<Vec<_>>(),
        );
    }
}

#[test]
#[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
fn unbounded_ranges_can_switch_direction_at_exhaustion() {
    for length in [0, 1, 16, 8192] {
        for bounds in [
            (Unbounded, Unbounded),
            (Included(length), Unbounded),
            (Unbounded, Excluded(0)),
            (Included(length / 3), Unbounded),
            (Unbounded, Excluded(length / 3)),
        ] {
            for reverse in [false, true] {
                let mut maps = FoldMaps::new(length);
                let count = maps.theirs.range(bounds).count();
                let mut range = maps.ours.range_mut(bounds);
                let mut entries = Vec::new();
                // Stop exactly after the last item, before observing None:
                // the previously unused end must not hand it out again.
                for _ in 0..count {
                    entries.push(if reverse {
                        range.next_back().unwrap()
                    } else {
                        range.next().unwrap()
                    });
                }
                if reverse {
                    assert_eq!(range.next(), None);
                } else {
                    assert_eq!(range.next_back(), None);
                }
                assert_eq!(range.next(), None);
                assert_eq!(range.next_back(), None);
                for (&key, value) in entries {
                    *value += key;
                }
                for (&key, value) in maps.theirs.range_mut(bounds) {
                    *value += key;
                }
                assert!(maps.ours.iter().eq(&maps.theirs));
            }
        }
    }
}

#[test]
#[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
fn range_runs_preserve_mixed_ends_clones_and_mutable_reborrows() {
    for length in [1, 27, 128, 8192] {
        for bounds in [
            (Unbounded, Unbounded),
            (Included(length / 3), Unbounded),
            (Unbounded, Excluded(length / 2)),
            (Included(length / 3), Included(length / 3)),
            (Included(length / 4), Excluded(3 * length / 4)),
        ] {
            let mut maps = FoldMaps::new(length);
            let mut ours = maps.ours.range(bounds);
            let mut theirs = maps.theirs.range(bounds);
            for index in 0..length + 2 {
                let back = index % 11 >= 7;
                assert_eq!(ours.step(back), theirs.step(back));
                if index % 31 == 0 {
                    assert!(ours.clone().eq(theirs.clone()));
                    assert!(ours.clone().rev().eq(theirs.clone().rev()));
                }
            }
            assert_eq!(ours.next(), None);
            assert_eq!(ours.next_back(), None);

            let mut ours = maps.ours.range_mut(bounds);
            let mut theirs = maps.theirs.range_mut(bounds);
            let mut held = Vec::new();
            for index in 0..length + 2 {
                // Keep the yielded mutable references alive across every
                // switch, reborrow and leaf boundary. Their addresses and
                // keys must remain distinct until the range is exhausted.
                let back = index % 13 >= 5;
                let (actual, expected) = (ours.step(back), theirs.step(back));
                assert_eq!(actual.as_ref(), expected.as_ref());
                if let Some(entry) = actual {
                    held.push(entry);
                }
                if let Some((&key, value)) = expected {
                    *value += key + 1;
                }
                if index % 31 == 0 {
                    assert_eq!(format!("{ours:?}"), format!("{theirs:?}"));
                }
            }
            assert_eq!(ours.next(), None);
            assert_eq!(ours.next_back(), None);
            held.sort_by_key(|(key, _)| **key);
            for pair in held.windows(2) {
                assert!(pair[0].0 < pair[1].0);
                assert!(!core::ptr::eq(pair[0].1, pair[1].1));
            }
            for (&key, value) in held {
                *value += key + 1;
            }
            assert!(maps.ours.iter().eq(&maps.theirs));
        }
    }
}

#[test]
#[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
fn mutable_folds_keep_all_handed_out_references_disjoint() {
    for reverse in [false, true] {
        let mut maps = FoldMaps::new(8192);
        let mut iterator = maps.ours.iter_mut();
        let first = iterator.next().unwrap();
        let last = iterator.next_back().unwrap();
        let middle = if reverse {
            iterator.rfold(Vec::new(), push_entry)
        } else {
            iterator.fold(Vec::new(), push_entry)
        };
        for (key, value) in [first, last].into_iter().chain(middle) {
            *value += *key;
        }
        for (&key, value) in &mut maps.theirs {
            *value += key;
        }
        assert!(maps.ours.iter().eq(&maps.theirs));

        let mut range = maps.ours.range_mut(25..8010);
        let first = range.next().unwrap();
        let last = range.next_back().unwrap();
        let middle = if reverse {
            range.rfold(Vec::new(), push_entry)
        } else {
            range.fold(Vec::new(), push_entry)
        };
        for (key, value) in [first, last].into_iter().chain(middle) {
            *value += *key;
        }
        for (&key, value) in maps.theirs.range_mut(25..8010) {
            *value += key;
        }
        assert!(maps.ours.iter().eq(&maps.theirs));

        if reverse {
            maps.ours.values_mut().rfold((), |(), value| *value += 1);
        } else {
            maps.ours.values_mut().fold((), |(), value| *value += 1);
        }
        maps.theirs.values_mut().for_each(|value| *value += 1);
        assert!(maps.ours.iter().eq(&maps.theirs));
    }
}

#[test]
#[cfg_attr(panic = "abort", ignore = "needs unwinding")]
#[cfg_attr(miri, ignore = "takes over 3 minutes under Miri")]
fn a_panicking_fold_leaves_the_same_mutations_as_std() {
    for reverse in [false, true] {
        let mut maps = FoldMaps::new(8192);
        let mutate = |(), (&key, value): (&u64, &mut u64)| {
            assert_ne!(key, 1979, "stop inside a leaf");
            *value += key;
        };
        assert!(
            catch_unwind(AssertUnwindSafe(|| {
                if reverse {
                    maps.ours.range_mut(17..8000).rfold((), mutate);
                } else {
                    maps.ours.range_mut(17..8000).fold((), mutate);
                }
            }))
            .is_err()
        );
        assert!(
            catch_unwind(AssertUnwindSafe(|| {
                if reverse {
                    maps.theirs.range_mut(17..8000).rfold((), mutate);
                } else {
                    maps.theirs.range_mut(17..8000).fold((), mutate);
                }
            }))
            .is_err()
        );
        assert!(maps.ours.iter().eq(&maps.theirs));
        maps.ours.check_invariants();
    }
}
