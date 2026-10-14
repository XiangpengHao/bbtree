//! A comparator may change its mind between the two searches of an insert.
//! Replaying must handle both an existing key and a different fitting leaf.

#![forbid(unsafe_code)]

use core::cell::Cell;
use core::cmp::Ordering;

use crate::map::BBTreeMap;
use crate::tree::node::Node;
use crate::tree::node::inline_keys::SearchKey;
use crate::tree::node::leaf::Leaf;

/// Changes the query's number after one search's comparisons. Its size
/// selects ordinary binary search, which calls the query's comparator.
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

    fn after_first_search(mut self, map: &mut BBTreeMap<Self, u64>, number: u64) -> Self {
        self.remaining.set(usize::MAX);
        let root = map.tree.root_mut();
        let Err((leaf, _)) = root.search_for_insert(&SearchKey::new(&self)) else {
            panic!("the first search must be vacant");
        };
        assert_eq!(
            leaf.len(),
            Leaf::<Self, u64, crate::inline_key::Auto>::capacity()
        );
        let comparisons = usize::MAX - self.remaining.get();
        assert!(comparisons > 0);
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
    let mut count = 0;
    loop {
        map.insert(RedirectedKey::new(count, &remaining), count);
        count += 1;
        let Node::Leaf(leaf) = map.tree.root() else {
            panic!("stop before the first split");
        };
        if leaf.len() == Leaf::<RedirectedKey<'_>, u64, crate::inline_key::Auto>::capacity() {
            break;
        }
    }
    let target = count / 2;
    let query = RedirectedKey::new(count, &remaining).after_first_search(&mut map, target);
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
            let Node::Leaf(right) = root.child(root.len()) else {
                panic!("stop before a second level grows");
            };
            if right.len() == Leaf::<RedirectedKey<'_>, u64, crate::inline_key::Auto>::capacity() {
                break;
            }
        }
    }
    assert_eq!(map.remove(&RedirectedKey::new(0, &remaining)), Some(0));
    let target = RedirectedKey::new(1, &remaining);
    let Err((leaf, _)) = map
        .tree
        .root_mut()
        .search_for_insert(&SearchKey::new(&target))
    else {
        panic!("the replay destination must be vacant");
    };
    assert!(leaf.len() < Leaf::<RedirectedKey<'_>, u64, crate::inline_key::Auto>::capacity());
    let query = RedirectedKey::new(2 * count, &remaining).after_first_search(&mut map, 1);
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

/// Builds through public insertion, so every armed hint comes from a real
/// rightmost split rather than a test-only setter.
struct AscendingInserts {
    map: BBTreeMap<u64, u64>,
    next: u64,
    leaf_capacity: usize,
}

impl AscendingInserts {
    fn new() -> Self {
        Self {
            map: BBTreeMap::new(),
            next: 0,
            leaf_capacity: 0,
        }
    }

    fn is_armed(&self) -> bool {
        self.map.tree.remembers_rightmost_leaf()
    }

    fn insert_next(&mut self) {
        assert_eq!(self.map.insert(self.next, self.next), None);
        self.next += 1;
        if self.leaf_capacity == 0 && self.map.leaf_depth() > 0 {
            self.leaf_capacity = self.map.len() - 1;
        }
    }

    fn arm(&mut self) {
        for _ in 0..128 {
            if self.is_armed() {
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
                Node::Leaf(leaf) => return leaf.len() == self.leaf_capacity,
                Node::Internal(internal) => node = internal.child(internal.len()),
            }
        }
    }
}

#[test]
fn rightmost_hint_arms_after_split_and_rearms_after_full_or_miss() {
    let mut inserts = AscendingInserts::new();
    inserts.insert_next();
    assert!(!inserts.is_armed());
    inserts.arm();
    assert_eq!(inserts.map.leaf_depth(), 1);
    inserts.insert_next();
    assert!(inserts.is_armed());
    while !inserts.is_rightmost_leaf_full() {
        inserts.insert_next();
    }
    inserts.insert_next();
    assert!(inserts.is_armed());
    assert!(!inserts.is_rightmost_leaf_full());
    // Equality and a smaller key both use ordinary replacement semantics.
    assert_eq!(
        inserts.map.insert(inserts.next - 1, 99),
        Some(inserts.next - 1)
    );
    assert!(!inserts.is_armed());
    inserts.arm();
    assert_eq!(inserts.map.insert(0, 100), Some(0));
    assert!(!inserts.is_armed());
    inserts.arm();
    inserts.map.check_invariants();
}

#[test]
fn an_unused_rightmost_append_token_leaves_the_tree_unchanged() {
    let mut inserts = AscendingInserts::new();
    inserts.arm();
    let len = inserts.map.len();
    {
        let _append = inserts
            .map
            .tree
            .rightmost_append(&inserts.next)
            .expect("the armed leaf has room for the next key");
    }
    assert!(!inserts.is_armed());
    assert_eq!(inserts.map.len(), len);
    assert!(inserts.map.iter().all(|(&key, &value)| key == value));
    inserts.map.check_invariants();
    inserts.insert_next();
    assert_eq!(inserts.map.len(), len + 1);
    inserts.map.check_invariants();
}

#[test]
fn rightmost_hint_is_cleared_before_general_mutation_access() {
    for operation in 0..7 {
        let mut inserts = AscendingInserts::new();
        inserts.arm();
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
            5 => {
                let upper = inserts.map.split_off(&(inserts.next / 2));
                drop(upper); // The remembered rightmost allocation is gone.
            }
            6 => inserts.map.clear(),
            _ => unreachable!(),
        }
        assert!(!inserts.is_armed());
        inserts.insert_next();
        inserts.map.check_invariants();
    }
}

#[test]
fn an_entry_that_splits_never_publishes_a_rightmost_hint() {
    let mut inserts = AscendingInserts::new();
    inserts.arm();
    while !inserts.is_rightmost_leaf_full() {
        inserts.insert_next();
    }
    let crate::map::Entry::Vacant(entry) = inserts.map.entry(inserts.next) else {
        panic!("the next key is absent");
    };
    // This retained path can remove the inserted key immediately, and may
    // merge/release nodes. Only plain insertion consumes it to publish.
    let occupied = entry.insert_entry(inserts.next);
    assert_eq!(occupied.remove_entry(), (inserts.next, inserts.next));
    assert!(!inserts.is_armed());
    for _ in 0..64 {
        inserts.map.pop_last();
    }
    inserts.insert_next();
    inserts.map.check_invariants();
}

#[test]
fn rightmost_hints_follow_whole_owners_but_not_clones_or_cut_subtrees() {
    let mut inserts = AscendingInserts::new();
    inserts.arm();
    let mut cloned = inserts.map.clone();
    assert!(!cloned.tree.remembers_rightmost_leaf());
    let mut moved = BBTreeMap::new();
    moved.append(&mut inserts.map); // Empty-destination append swaps owners.
    assert!(moved.tree.remembers_rightmost_leaf());
    drop(moved);
    assert_eq!(cloned.insert(inserts.next, inserts.next), None);
    let mut other = AscendingInserts::new();
    other.arm();
    cloned.append(&mut other.map);
    cloned.insert(inserts.next + 1, inserts.next + 1);
    cloned.check_invariants();
    assert!(other.map.is_empty());
}

#[test]
fn interior_splits_do_not_arm_a_local_last_leaf() {
    let mut map = BBTreeMap::new();
    for key in (0..4000).step_by(2) {
        map.insert(key, key);
    }
    // Entry acquisition disarms the earlier ascending build.
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
        assert!(!self.control.inline.get(), "summary panic");
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
    use core::panic::AssertUnwindSafe;
    use std::panic::catch_unwind;

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
