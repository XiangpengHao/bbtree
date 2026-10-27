//! Extraction's local successor positioning at leaf ends and across repairs.

#![forbid(unsafe_code)]

use core::fmt::Debug;
use core::ops::Bound::{Excluded, Included};
use core::ops::Range;
use core::panic::AssertUnwindSafe;
use std::panic::catch_unwind;
use std::vec::Vec;

use crate::inline_key::Auto;
use crate::tree::node::Node;
use crate::tree::node::leaf::Leaf;
use crate::unsync::BBTreeMap;

/// Consecutive integer keys and values: `u64` gets inline keys under Auto,
/// `u32` does not. Insert individually so the boundary fixtures use splits.
struct ExtractionFixture<K> {
    map: BBTreeMap<K, u32>,
    count: u32,
}

impl<K: Ord + From<u32> + Debug> ExtractionFixture<K> {
    fn new(count: u32) -> Self {
        let mut map = BBTreeMap::new();
        for number in 0..count {
            map.insert(K::from(number), number);
        }
        Self { map, count }
    }

    fn two_leaves() -> Self {
        let fixture = Self::new(Leaf::<K, u32, Auto>::capacity() as u32 + 1);
        assert_eq!(fixture.map.leaf_depth(), 1);
        let Node::Internal(root) = fixture.map.tree.root() else {
            panic!("the root leaf split");
        };
        assert_eq!(root.len(), 1);
        fixture
    }

    fn left_leaf_len(&self) -> u32 {
        let Node::Internal(root) = self.map.tree.root() else {
            panic!("the fixture has two leaves");
        };
        root.child(0).expect_leaf().len() as u32
    }

    /// Assert the walk neither repeats nor skips a key, including the key
    /// after a removal that changes the tree's shape.
    fn extract_and_check(&mut self, removed: Range<u32>) {
        let mut visited = Vec::new();
        let entries: Vec<_> = self
            .map
            .extract_if(
                K::from(removed.start)..K::from(removed.end),
                |key, value| {
                    assert_eq!(key, &K::from(*value));
                    visited.push(*value);
                    true
                },
            )
            .map(|(key, value)| {
                assert_eq!(key, K::from(value));
                value
            })
            .collect();
        assert!(entries.iter().copied().eq(removed.clone()));
        assert_eq!(visited, entries);
        assert_eq!(self.map.len(), (self.count - removed.len() as u32) as usize);
        assert!(
            self.map
                .iter()
                .map(|(key, &value)| {
                    assert_eq!(key, &K::from(value));
                    value
                })
                .eq((0..self.count).filter(|number| !removed.contains(number)))
        );
        self.map.check_invariants();
    }

    fn consecutive_inside_leaf(mut self) {
        self.extract_and_check(3..7);
        assert_eq!(self.map.leaf_depth(), 1);
    }

    fn across_leaf_end_and_separator(mut self) {
        let separator = self.left_leaf_len();
        self.extract_and_check(separator - 1..separator + 2);
        assert_eq!(self.map.leaf_depth(), 1);
    }

    fn last_entry(mut self) {
        self.extract_and_check(self.count - 1..self.count);
        let mut extraction = self.map.extract_if(.., |_, _| false);
        assert_eq!(extraction.next(), None);
        assert_eq!(extraction.next(), None);
    }

    fn merge_and_lower_root(mut self) {
        let minimum = (Leaf::<K, u32, Auto>::capacity() / 8).max(4) as u32;
        self.extract_and_check(0..self.left_leaf_len() - minimum + 1);
        assert_eq!(self.map.leaf_depth(), 0, "the underfull leaf merged");
    }

    fn borrow_from_right(mut self) {
        let capacity = Leaf::<K, u32, Auto>::capacity() as u32;
        let left_len = self.left_leaf_len();
        while self.count < left_len + 1 + capacity {
            self.map.insert(K::from(self.count), self.count);
            self.count += 1;
        }
        let minimum = (capacity / 8).max(4);
        self.extract_and_check(0..left_len - minimum + 1);
        let Node::Internal(root) = self.map.tree.root() else {
            panic!("borrowing must not lower the root");
        };
        assert_eq!(root.len(), 1);
        assert_eq!(root.child(0).expect_leaf().len(), minimum as usize);
        assert_eq!(root.child(1).expect_leaf().len(), (capacity - 1) as usize);
    }

    fn empty_root_leaf(mut self) {
        assert_eq!(self.map.leaf_depth(), 0);
        self.extract_and_check(0..self.count);
        assert!(self.map.is_empty());
        let mut extraction = self.map.extract_if(.., |_, _| panic!("empty map"));
        assert_eq!(extraction.next(), None);
        assert_eq!(extraction.next(), None);
        drop(extraction);
        self.map.insert(K::from(0), 0);
        assert_eq!(self.map.get(&K::from(0)), Some(&0));
        self.map.check_invariants();
    }
}

#[test]
fn extract_if_consecutive_entries_stay_at_the_successor() {
    ExtractionFixture::<u64>::two_leaves().consecutive_inside_leaf();
    ExtractionFixture::<u32>::two_leaves().consecutive_inside_leaf();
}

#[test]
fn extract_if_leaf_end_visits_separator_and_next_leaf_once() {
    ExtractionFixture::<u64>::two_leaves().across_leaf_end_and_separator();
    ExtractionFixture::<u32>::two_leaves().across_leaf_end_and_separator();
}

#[test]
fn extract_if_last_leaf_entry_exhausts_the_walk() {
    ExtractionFixture::<u64>::two_leaves().last_entry();
    ExtractionFixture::<u32>::two_leaves().last_entry();
}

#[test]
fn extract_if_merge_lowers_root_and_keeps_the_successor() {
    ExtractionFixture::<u64>::two_leaves().merge_and_lower_root();
    ExtractionFixture::<u32>::two_leaves().merge_and_lower_root();
}

#[test]
fn extract_if_borrow_keeps_the_successor() {
    ExtractionFixture::<u64>::two_leaves().borrow_from_right();
    ExtractionFixture::<u32>::two_leaves().borrow_from_right();
}

#[test]
fn extract_if_root_leaf_can_be_emptied_and_reused() {
    for count in [0, 1, 16] {
        ExtractionFixture::<u64>::new(count).empty_root_leaf();
        ExtractionFixture::<u32>::new(count).empty_root_leaf();
    }
}

#[test]
fn extract_if_is_lazy_and_early_drop_leaves_unvisited_values_untouched() {
    let mut map = ExtractionFixture::<u64>::new(32).map;
    drop(map.extract_if(.., |_, _| panic!("unconsumed iterator")));
    assert_eq!(map.len(), 32);
    let mut visited = Vec::new();
    let entries: Vec<_> = map
        .extract_if(.., |&key, value| {
            visited.push(key);
            *value += 100;
            key % 2 == 1
        })
        .take(3)
        .collect();
    assert_eq!(entries, [(1, 101), (3, 103), (5, 105)]);
    assert_eq!(visited, [0, 1, 2, 3, 4, 5]);
    assert_eq!(map.len(), 29);
    assert!(
        map.iter()
            .map(|(&key, &value)| (key, value))
            .eq([(0, 100), (2, 102), (4, 104)]
                .into_iter()
                .chain((6..32).map(|key| (key, key as u32))))
    );
    map.check_invariants();
}

#[test]
fn extract_if_does_not_call_predicate_past_range_end() {
    let mut map = ExtractionFixture::<u64>::new(32).map;
    let mut visited = Vec::new();
    let entries: Vec<_> = map
        .extract_if((Excluded(2), Included(6)), |&key, value| {
            visited.push(key);
            *value += 10;
            true
        })
        .collect();
    assert_eq!(entries, [(3, 13), (4, 14), (5, 15), (6, 16)]);
    assert_eq!(visited, [3, 4, 5, 6]);
    assert_eq!(map.len(), 28);
    assert!(
        map.iter()
            .all(|(&key, &value)| !(3..=6).contains(&key) && value == key as u32)
    );
    map.check_invariants();
}

#[test]
#[cfg_attr(panic = "abort", ignore = "needs unwinding")]
fn extract_if_predicate_panic_keeps_current_and_unvisited_entries() {
    let mut map = ExtractionFixture::<u64>::new(32).map;
    let mut visited = Vec::new();
    assert!(
        catch_unwind(AssertUnwindSafe(|| {
            map.extract_if(.., |&key, value| {
                visited.push(key);
                *value += 10;
                assert_ne!(key, 4, "predicate panics before selecting this entry");
                key % 2 == 0
            })
            .for_each(drop);
        }))
        .is_err()
    );
    assert_eq!(visited, [0, 1, 2, 3, 4]);
    assert_eq!(map.len(), 30);
    assert!(
        map.iter()
            .map(|(&key, &value)| (key, value))
            .eq([(1, 11), (3, 13), (4, 14)]
                .into_iter()
                .chain((5..32).map(|key| (key, key as u32))))
    );
    map.check_invariants();
}
