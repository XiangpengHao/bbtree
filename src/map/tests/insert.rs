//! Inserts (`tree/insert.rs`) where the map's API alone cannot say what
//! happened. A comparator may change its mind between the two searches of
//! an insert into a full leaf: the replay must handle both an existing key
//! and a different leaf with room, and a comparator that panics in it.
//! Ascending inserts remember the rightmost leaf after a split, and append
//! to it (D45): what remembers it, what forgets it, and an append that never
//! happens.

#![forbid(unsafe_code)]

use core::cell::Cell;
use core::cmp::Ordering;
use core::panic::AssertUnwindSafe;
use std::panic::catch_unwind;

use super::drops::BudgetedKey;
use crate::inline_key::Auto;
use crate::map::{BBTreeMap, Entry};
use crate::tree::node::Node;
use crate::tree::node::inline_keys::SearchKey;
use crate::tree::node::leaf::Leaf;

impl<K: Ord> BBTreeMap<K, u64> {
    /// Inserts the keys `key` makes from 0, 1, 2 and on, each with its
    /// number as its value, until the root leaf is full; returns how many.
    fn fill_root_leaf(&mut self, key: impl Fn(u64) -> K) -> u64 {
        let mut count = 0;
        loop {
            self.insert(key(count), count);
            count += 1;
            if self.tree.root().expect_leaf().len() == Leaf::<K, u64, Auto>::capacity() {
                return count;
            }
        }
    }

    /// How many comparisons the search for `query`'s insert makes, which
    /// `query`'s `Ord` counts down from a full `budget`: the search must end
    /// in a full leaf, finding no equal key.
    fn comparisons_to_a_full_leaf(&self, query: &K, budget: &Cell<usize>) -> usize {
        budget.set(usize::MAX);
        let Err((leaf, _)) = self.tree.root().search_for_insert(&SearchKey::new(query)) else {
            panic!("the search finds no equal key");
        };
        assert_eq!(leaf.len(), Leaf::<K, u64, Auto>::capacity());
        let comparisons = usize::MAX - budget.get();
        assert!(comparisons > 0);
        comparisons
    }
}

/// A key whose number changes to `replay_number` once comparisons have spent
/// `remaining`. Its 24 bytes select the early-exit search
/// (`KeySearch::EarlyExit`), which calls the query's `cmp`, not the stored
/// keys'.
struct RedirectedKey<'a> {
    number: Cell<u64>,
    remaining: &'a Cell<usize>,
    replay_number: u64,
}

impl<'a> RedirectedKey<'a> {
    fn new(number: u64, remaining: &'a Cell<usize>) -> Self {
        Self {
            number: Cell::new(number),
            remaining,
            replay_number: number,
        }
    }

    /// This query, which turns into `number` once it has made as many
    /// comparisons as one search for its insert into `map`: the insert's
    /// second search, its replay, goes elsewhere.
    fn redirected_after_one_search(mut self, map: &BBTreeMap<Self, u64>, number: u64) -> Self {
        let comparisons = map.comparisons_to_a_full_leaf(&self, self.remaining);
        self.remaining.set(comparisons);
        self.replay_number = number;
        self
    }
}

impl PartialEq for RedirectedKey<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for RedirectedKey<'_> {}

impl PartialOrd for RedirectedKey<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RedirectedKey<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        let remaining = self.remaining.get();
        if remaining == 0 {
            self.number.set(self.replay_number);
        } else {
            self.remaining.set(remaining - 1);
        }
        self.number.get().cmp(&other.number.get())
    }
}

#[test]
fn full_leaf_replay_can_find_an_existing_key() {
    let remaining = Cell::new(usize::MAX);
    let mut map = BBTreeMap::new();
    let count = map.fill_root_leaf(|number| RedirectedKey::new(number, &remaining));
    let target = count / 2;
    let query = RedirectedKey::new(count, &remaining).redirected_after_one_search(&map, target);
    assert_eq!(map.insert(query, u64::MAX), Some(target));
    remaining.set(usize::MAX);
    assert_eq!(map.len(), count as usize);
    assert_eq!(map.leaf_depth(), 0);
    map.check_invariants();
    assert!(map.iter().all(|(key, &value)| value
        == if key.number.get() == target {
            u64::MAX
        } else {
            key.number.get()
        }));
}

#[test]
fn full_leaf_replay_can_find_a_different_fitting_leaf() {
    let remaining = Cell::new(usize::MAX);
    let mut map = BBTreeMap::new();
    let mut count = 0;
    loop {
        map.insert(RedirectedKey::new(2 * count, &remaining), 2 * count);
        count += 1;
        if let Node::Internal(root) = map.tree.root() {
            let right = root.child(root.len()).expect_leaf();
            if right.len() == Leaf::<RedirectedKey<'_>, u64, Auto>::capacity() {
                break;
            }
        }
    }
    assert_eq!(map.remove(&RedirectedKey::new(0, &remaining)), Some(0));
    let target = RedirectedKey::new(1, &remaining);
    let Err((leaf, _)) = map.tree.root().search_for_insert(&SearchKey::new(&target)) else {
        panic!("the replay destination must be vacant");
    };
    assert!(leaf.len() < Leaf::<RedirectedKey<'_>, u64, Auto>::capacity());
    let query = RedirectedKey::new(2 * count, &remaining).redirected_after_one_search(&map, 1);
    assert_eq!(map.insert(query, 1), None);
    remaining.set(usize::MAX);
    assert_eq!(map.len(), count as usize);
    assert_eq!(map.leaf_depth(), 1);
    map.check_invariants();
    assert!(
        map.iter()
            .map(|(key, &value)| (key.number.get(), value))
            .eq(core::iter::once((1, 1)).chain((1..count).map(|number| (2 * number, 2 * number))))
    );
}

#[test]
#[cfg_attr(panic = "abort", ignore = "needs unwinding")]
fn a_panicking_full_leaf_replay_leaves_the_map_unchanged() {
    let budget = Cell::new(usize::MAX);
    let mut map = BBTreeMap::new();
    let count = map.fill_root_leaf(|number| BudgetedKey::new(number, &budget));
    let query = BudgetedKey::new(count, &budget);
    budget.set(map.comparisons_to_a_full_leaf(&query, &budget));
    assert!(catch_unwind(AssertUnwindSafe(|| map.insert(query, count))).is_err());
    assert_eq!(budget.get(), 0, "the second search must have started");
    budget.set(usize::MAX);
    map.check_invariants();
    assert_eq!(map.len(), count as usize);
    assert_eq!(map.leaf_depth(), 0);
    assert!(
        map.iter()
            .map(|(key, &value)| (key.number(), value))
            .eq((0..count).map(|number| (number, number)))
    );
}

/// A map that ascending inserts fill through the public API, so that every
/// rightmost leaf it remembers comes from a real split (D45).
struct AscendingInserts {
    map: BBTreeMap<u64, u64>,
    next: u64,
}

impl AscendingInserts {
    fn new() -> Self {
        Self {
            map: BBTreeMap::new(),
            next: 0,
        }
    }

    fn remembers_rightmost_leaf(&self) -> bool {
        self.map.tree.remembers_rightmost_leaf()
    }

    fn insert_next(&mut self) {
        assert_eq!(self.map.insert(self.next, self.next), None);
        self.next += 1;
    }

    /// Inserts the next keys until the map remembers its rightmost leaf.
    fn insert_until_rightmost_leaf_remembered(&mut self) {
        for _ in 0..128 {
            if self.remembers_rightmost_leaf() {
                return;
            }
            self.insert_next();
        }
        panic!("ascending inserts must reach a rightmost split");
    }

    fn is_rightmost_leaf_full(&self) -> bool {
        let mut node = self.map.tree.root();
        loop {
            match node {
                Node::Leaf(leaf) => return leaf.len() == Leaf::<u64, u64, Auto>::capacity(),
                Node::Internal(internal) => node = internal.child(internal.len()),
            }
        }
    }
}

/// A split at the right end remembers the new rightmost leaf, even after
/// filling the one remembered before; a key not past the last forgets it.
#[test]
fn rightmost_splits_remember_the_leaf_and_keys_not_past_the_last_forget_it() {
    let mut inserts = AscendingInserts::new();
    inserts.insert_next();
    assert!(!inserts.remembers_rightmost_leaf());
    inserts.insert_until_rightmost_leaf_remembered();
    assert_eq!(inserts.map.leaf_depth(), 1);
    inserts.insert_next();
    assert!(inserts.remembers_rightmost_leaf());
    while !inserts.is_rightmost_leaf_full() {
        inserts.insert_next();
    }
    inserts.insert_next();
    assert!(inserts.remembers_rightmost_leaf());
    assert!(!inserts.is_rightmost_leaf_full());
    // An equal key and a smaller one go the ordinary way, replacing values.
    assert_eq!(
        inserts.map.insert(inserts.next - 1, 99),
        Some(inserts.next - 1)
    );
    assert!(!inserts.remembers_rightmost_leaf());
    inserts.insert_until_rightmost_leaf_remembered();
    assert_eq!(inserts.map.insert(0, 100), Some(0));
    assert!(!inserts.remembers_rightmost_leaf());
    inserts.insert_until_rightmost_leaf_remembered();
    inserts.map.check_invariants();
}

#[test]
fn a_dropped_rightmost_append_forgets_the_leaf_and_changes_nothing() {
    let mut inserts = AscendingInserts::new();
    inserts.insert_until_rightmost_leaf_remembered();
    let len = inserts.map.len();
    {
        let _append = inserts
            .map
            .tree
            .rightmost_append(inserts.next)
            .expect("the remembered leaf has room for the next key");
    }
    assert!(!inserts.remembers_rightmost_leaf());
    assert_eq!(inserts.map.len(), len);
    assert!(inserts.map.iter().all(|(&key, &value)| key == value));
    inserts.map.check_invariants();
    inserts.insert_next();
    assert_eq!(inserts.map.len(), len + 1);
    inserts.map.check_invariants();
}

#[test]
fn mutable_access_forgets_the_rightmost_leaf() {
    for operation in 0..7 {
        let mut inserts = AscendingInserts::new();
        inserts.insert_until_rightmost_leaf_remembered();
        match operation {
            0 => *inserts.map.get_mut(&0).unwrap() += 1,
            1 => *inserts.map.iter_mut().next_back().unwrap().1 += 1,
            2 => *inserts.map.range_mut(0..).next().unwrap().1 += 1,
            3 => {
                inserts.map.entry(0).and_modify(|value| *value += 1);
            }
            4 => {
                assert_eq!(inserts.map.last_entry().unwrap().remove(), inserts.next - 1);
            }
            // The remembered leaf goes with the part cut off, and is freed.
            5 => drop(inserts.map.split_off(&(inserts.next / 2))),
            6 => inserts.map.clear(),
            _ => unreachable!(),
        }
        assert!(!inserts.remembers_rightmost_leaf());
        inserts.insert_next();
        inserts.map.check_invariants();
    }
}

#[test]
fn an_insert_through_an_entry_never_remembers_the_rightmost_leaf() {
    let mut inserts = AscendingInserts::new();
    inserts.insert_until_rightmost_leaf_remembered();
    while !inserts.is_rightmost_leaf_full() {
        inserts.insert_next();
    }
    let Entry::Vacant(entry) = inserts.map.entry(inserts.next) else {
        panic!("the next key is absent");
    };
    // The entry it hands back can take the key out again, which may merge
    // nodes and free pages: only a plain insert remembers the leaf.
    let occupied = entry.insert_entry(inserts.next);
    assert_eq!(occupied.remove_entry(), (inserts.next, inserts.next));
    assert!(!inserts.remembers_rightmost_leaf());
    for _ in 0..64 {
        inserts.map.pop_last();
    }
    inserts.insert_next();
    inserts.map.check_invariants();
}

/// A clone starts without the remembered leaf. A move keeps it: an append
/// into an empty map, which swaps the trees. An append into a map with
/// entries forgets both maps' leaves.
#[test]
fn clones_forget_and_moves_keep_the_rightmost_leaf() {
    let mut inserts = AscendingInserts::new();
    inserts.insert_until_rightmost_leaf_remembered();
    let mut cloned = inserts.map.clone();
    assert!(!cloned.tree.remembers_rightmost_leaf());
    let mut moved = BBTreeMap::new();
    moved.append(&mut inserts.map);
    assert!(moved.tree.remembers_rightmost_leaf());
    drop(moved);
    assert_eq!(cloned.insert(inserts.next, inserts.next), None);
    let mut other = AscendingInserts::new();
    other.insert_until_rightmost_leaf_remembered();
    cloned.append(&mut other.map);
    assert!(!cloned.tree.remembers_rightmost_leaf());
    assert!(!other.remembers_rightmost_leaf());
    cloned.insert(inserts.next + 1, inserts.next + 1);
    cloned.check_invariants();
    assert!(other.map.is_empty());
}

#[test]
fn splits_inside_the_tree_never_remember_a_leaf() {
    let mut map = BBTreeMap::new();
    for key in (0..4000).step_by(2) {
        map.insert(key, key);
    }
    // Taking an entry forgets the leaf the ascending inserts remembered.
    map.entry(0).and_modify(|value| *value = 0);
    for key in (1..2000).step_by(2) {
        map.insert(key, key);
        assert!(!map.tree.remembers_rightmost_leaf());
    }
    map.check_invariants();
}

struct AppendPanicControl {
    compare: Cell<bool>,
    inline: Cell<bool>,
}

struct AppendPanicKey<'a> {
    number: u64,
    control: &'a AppendPanicControl,
    panic_on_drop: bool,
}

impl<'a> AppendPanicKey<'a> {
    fn new(number: u64, control: &'a AppendPanicControl) -> Self {
        Self {
            number,
            control,
            panic_on_drop: false,
        }
    }
}

impl PartialEq for AppendPanicKey<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.number == other.number
    }
}
impl Eq for AppendPanicKey<'_> {}
impl PartialOrd for AppendPanicKey<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for AppendPanicKey<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        assert!(!self.control.compare.get(), "comparison panic");
        self.number.cmp(&other.number)
    }
}
impl crate::InlineKey for AppendPanicKey<'_> {
    const WIDTH: crate::InlineWidth = crate::InlineWidth::Two;
    fn inline_key_at(&self, offset: usize) -> crate::InlineWord {
        assert!(!self.control.inline.get(), "inline key panic");
        self.number.inline_key_at(offset)
    }
}
impl Drop for AppendPanicKey<'_> {
    fn drop(&mut self) {
        assert!(!self.panic_on_drop, "key drop panic");
    }
}

#[test]
#[cfg_attr(panic = "abort", ignore = "needs unwinding")]
fn cached_append_clears_before_callbacks_and_preserves_drop_order() {
    let control = AppendPanicControl {
        compare: Cell::new(false),
        inline: Cell::new(false),
    };
    let mut map = BBTreeMap::with_strategy(crate::WithInlineKey);
    let mut next = 0;
    for callback in [&control.compare, &control.inline] {
        while !map.tree.remembers_rightmost_leaf() {
            map.insert(AppendPanicKey::new(next, &control), next);
            next += 1;
        }
        let len = map.len();
        callback.set(true);
        assert!(
            catch_unwind(AssertUnwindSafe(|| {
                map.insert(AppendPanicKey::new(next, &control), next);
            }))
            .is_err()
        );
        callback.set(false);
        assert!(!map.tree.remembers_rightmost_leaf());
        assert_eq!(map.len(), len);
        assert_eq!(map.get(&AppendPanicKey::new(next, &control)), None);
        map.check_invariants();
    }
    while !map.tree.remembers_rightmost_leaf() {
        map.insert(AppendPanicKey::new(next, &control), next);
        next += 1;
    }
    let mut equal = AppendPanicKey::new(next - 1, &control);
    equal.panic_on_drop = true;
    assert!(catch_unwind(AssertUnwindSafe(|| map.insert(equal, u64::MAX))).is_err());
    assert_eq!(
        map.get(&AppendPanicKey::new(next - 1, &control)),
        Some(&(next - 1))
    );
    assert!(!map.tree.remembers_rightmost_leaf());
    map.check_invariants();
}
