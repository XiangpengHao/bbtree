//! Leaf operations (`tree/node/leaf.rs`) against a model: searches, inserts
//! that fit or split, and removals, for keys and values of every shape.

use core::cmp::Ordering;
use core::panic::AssertUnwindSafe;
use std::format;
use std::panic::catch_unwind;
use std::string::String;
use std::vec::Vec;

use crate::inline_key::{Auto, InlineKey, InlineKeyStrategy, InlineWord, WithInlineKey};
use crate::tree::invariants::NodeChecks;
use crate::tree::node::inline_keys::{InlinedKey, SearchKey, Separator};
use crate::tree::node::leaf::Leaf;
use crate::tree::node::page::{NodePtr, TestPages};
use crate::tree::node::{Insertion, Node, Split};
use crate::tree::root::Root;
use crate::tree::test_support::{
    Bulky, CacheLineAligned, DropLedger, FromTracked, PageAligned, TestRng, TestTree, Tracked,
};

/// Appends an entry to a leaf that has room.
fn push_back<K, V, S: InlineKeyStrategy<K>>(leaf: Leaf<K, V, S>, key: K, value: V) {
    assert!(leaf.len() < Leaf::<K, V, S>::capacity(), "the leaf is full");
    leaf.insert_fit(leaf.len(), InlinedKey::new(key, 0), value);
}

/// The root of a tree that must be a leaf.
fn root_leaf<K, V, S: InlineKeyStrategy<K>>(tree: &Root<K, V, S>) -> Leaf<K, V, S> {
    match tree.top() {
        Node::Leaf(leaf) => leaf,
        Node::Internal(_) => panic!("the root is internal"),
    }
}

/// Takes `leaf`, which the caller owns, apart, releasing its page to
/// `pages`.
fn destroy_leaf<K, V, S: InlineKeyStrategy<K>>(leaf: Leaf<K, V, S>, pages: &TestPages<K, V, S>) {
    Node::Leaf(leaf).destroy(|page| pages.release(page));
}

/// Where a search of `leaf` for `key` ends: `Ok` at an equal key, `Err`
/// at the edge where it belongs, as `slice::binary_search` says.
fn search_position<K: core::borrow::Borrow<Q>, V, S: InlineKeyStrategy<K>, Q: Ord + ?Sized>(
    leaf: Leaf<K, V, S>,
    key: &Q,
) -> Result<usize, usize> {
    leaf.search(&SearchKey::new(key))
}

#[test]
fn search_matches_slice_binary_search() {
    let pages = TestPages::<u64, u64>::new();
    let leaf = Leaf::new(pages.fresh_leaf());
    let keys: Vec<u64> = (0..40).map(|key| key * 2).collect();
    for &key in &keys {
        push_back(leaf, key, key + 1000);
    }
    for probe in 0..85 {
        if let Ok(index) = leaf.search(&SearchKey::new(&probe)) {
            assert_eq!(*leaf.key_value(index).1, probe + 1000);
        }
        assert_eq!(search_position(leaf, &probe), keys.binary_search(&probe));
    }
    destroy_leaf(leaf, &pages);
    pages.assert_all_released();
}

/// The same check for keys that take the plain binary search, or, with
/// inline keys, the search over them: whole short strings, which their
/// inline keys hold, and long ones that tie on them.
#[test]
fn string_search_matches_slice_binary_search() {
    for (prefix, width) in [("", 4), ("", 8), ("", 9), ("tied/prefix/", 4)] {
        let pages = TestPages::<String, u64>::new();
        assert!(NodePtr::<String, u64, Auto>::has_inline_keys());
        let leaf = Leaf::new(pages.fresh_leaf());
        let key = |number: usize| format!("{prefix}{number:0width$}");
        // A full leaf of every other number, probed with every number
        // from below its first key to past its last.
        let capacity = Leaf::<String, u64, Auto>::capacity();
        let keys: Vec<String> = (0..capacity).map(|number| key(number * 2)).collect();
        for (number, key) in keys.iter().enumerate() {
            push_back(leaf, key.clone(), number as u64);
        }
        leaf.assert_inline_keys_match();
        for probe in (0..2 * capacity + 3).map(key) {
            let expected = keys.binary_search(&probe);
            assert_eq!(search_position(leaf, probe.as_str()), expected);
            assert_eq!(search_position(leaf, &probe), expected);
        }
        // Shorter and longer keys with the same inline key.
        for probe in [key(4) + "\0", key(4) + "0", String::from(&key(4)[..1])] {
            let expected = keys.binary_search(&probe);
            assert_eq!(search_position(leaf, probe.as_str()), expected);
        }
        destroy_leaf(leaf, &pages);
        pages.assert_all_released();
    }
}

#[test]
fn search_borrows_keys() {
    let pages = TestPages::<String, ()>::new();
    let leaf = Leaf::new(pages.fresh_leaf());
    for key in ["apple", "banana", "cherry"] {
        push_back(leaf, String::from(key), ());
    }
    match leaf.search(&SearchKey::new("banana")) {
        Ok(index) => assert_eq!(leaf.key(index), "banana"),
        Err(_) => panic!("banana is there"),
    }
    match leaf.search(&SearchKey::new("blueberry")) {
        Ok(_) => panic!("blueberry is not there"),
        Err(edge) => assert_eq!(edge, 2),
    }
    destroy_leaf(leaf, &pages);
    pages.assert_all_released();
}

/// A key whose `Ord` and `InlineKey` answer at random. `PADDING` picks
/// the search path without inline keys: 0 keeps it small enough for the
/// branchless search, 32 makes it take the plain binary search.
struct Chaotic<'r, const PADDING: usize>(&'r TestRng, [u8; PADDING]);

impl<'r, const PADDING: usize> Chaotic<'r, PADDING> {
    fn new(rng: &'r TestRng) -> Self {
        Self(rng, [0; PADDING])
    }
}

impl<const PADDING: usize> PartialEq for Chaotic<'_, PADDING> {
    fn eq(&self, _: &Self) -> bool {
        self.0.below(2) == 0
    }
}

impl<const PADDING: usize> Eq for Chaotic<'_, PADDING> {}

impl<const PADDING: usize> PartialOrd for Chaotic<'_, PADDING> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<const PADDING: usize> Ord for Chaotic<'_, PADDING> {
    fn cmp(&self, _: &Self) -> Ordering {
        [Ordering::Less, Ordering::Equal, Ordering::Greater][self.0.below(3)]
    }
}

/// Few distinct values, so that runs of equal inline keys are common.
impl<const PADDING: usize> InlineKey for Chaotic<'_, PADDING> {
    fn inline_key_at(&self, _offset: usize) -> InlineWord {
        self.0.below(4) as InlineWord
    }
}

#[test]
fn search_stays_in_bounds_for_any_ord() {
    search_stays_in_bounds::<0, Auto>();
    search_stays_in_bounds::<32, Auto>();
    search_stays_in_bounds::<0, WithInlineKey>();
    search_stays_in_bounds::<32, WithInlineKey>();
}

fn search_stays_in_bounds<const PADDING: usize, S>()
where
    for<'r> S: InlineKeyStrategy<Chaotic<'r, PADDING>>,
{
    let rng = TestRng::new(7);
    let ledger = DropLedger::new();
    let pages = TestPages::<Chaotic<'_, PADDING>, Tracked<'_>, S>::new();
    let leaf = Leaf::new(pages.fresh_leaf());
    let searches = if cfg!(miri) { 60 } else { 1000 };
    for _ in 0..searches {
        let probe = Chaotic::new(&rng);
        let edge = match leaf.search(&SearchKey::new(&probe)) {
            Ok(index) => {
                assert!(index < leaf.len());
                let _ = leaf.key_value(index).1.id();
                index + 1
            }
            Err(edge) => edge,
        };
        assert!(edge <= leaf.len());
        if leaf.len() < Leaf::<Chaotic<'_, PADDING>, Tracked<'_>, S>::capacity() {
            leaf.insert_fit(edge, InlinedKey::new(Chaotic::new(&rng), 0), ledger.track());
        }
    }
    destroy_leaf(leaf, &pages);
    pages.assert_all_released();
    ledger.assert_all_dropped_once();
}

#[test]
#[cfg_attr(miri, ignore = "its Miri size misses the height it asserts")]
fn a_tree_stays_memory_safe_under_any_ord() {
    tree_stays_memory_safe::<0, Auto>();
    tree_stays_memory_safe::<32, Auto>();
    tree_stays_memory_safe::<0, WithInlineKey>();
    tree_stays_memory_safe::<32, WithInlineKey>();
}

fn tree_stays_memory_safe<const PADDING: usize, S>()
where
    for<'r> S: InlineKeyStrategy<Chaotic<'r, PADDING>>,
{
    let rng = TestRng::new(8);
    let ledger = DropLedger::new();
    let pages = TestPages::<Chaotic<'_, PADDING>, Bulky<'_>, S>::new();
    let mut tree = TestTree::new(&pages);
    let inserts = if cfg!(miri) { 120 } else { 3000 };
    for _ in 0..inserts {
        tree.insert(Chaotic::new(&rng), Bulky::from_tracked(ledger.track()));
    }
    assert!(tree.height() >= 1);
    let ids = tree.entries(|_, value| value.tracked_id());
    assert!(!ids.is_empty() && ids.len() <= inserts);
    tree.destroy();
    pages.assert_all_released();
    ledger.assert_all_dropped_once();
}

/// What one leaf should hold: the ids of its entries in order, and of its
/// head, if any, and of the head's key, where separator keys are heads
/// (D49).
struct LeafModel {
    entries: Vec<(usize, usize)>,
    head: Option<usize>,
    head_key: Option<usize>,
}

impl LeafModel {
    /// Checks the leaf's entries and head, and its inline keys, if its
    /// map has them, against its keys.
    fn assert_matches<'l, V: FromTracked<'l>, S: InlineKeyStrategy<Tracked<'l>>>(
        &self,
        leaf: Leaf<Tracked<'l>, V, S>,
    ) {
        assert_eq!(leaf.tracked_ids(), self.entries);
        assert_eq!(leaf.head_id(), self.head);
        assert_eq!(leaf.head_key_id(), self.head_key);
        leaf.assert_inline_keys_match();
    }

    /// Checks the halves of a split of the modelled leaf: the left keeps
    /// the head, the separator's value is the right half's head, and the
    /// halves both meet the minimum occupancy.
    fn assert_split_halves<'l, V: FromTracked<'l>, S: InlineKeyStrategy<Tracked<'l>>>(
        &self,
        left: Leaf<Tracked<'l>, V, S>,
        split: &Split<Tracked<'l>, S, Leaf<Tracked<'l>, V, S>>,
    ) {
        let right = split.right;
        let separator_value = right.head_id().expect("the right half has a head");
        // The separator's key goes up, or where separator keys are heads,
        // into the right half's head (D49).
        let separator_key = match split.separator.key() {
            Some(key) => key.id(),
            None => right
                .head_key_id()
                .expect("the right half's head has a key"),
        };
        let mut halves = left.tracked_ids();
        halves.push((separator_key, separator_value));
        halves.extend(right.tracked_ids());
        assert_eq!(halves, self.entries);
        assert_eq!(left.head_id(), self.head);
        assert_eq!(left.head_key_id(), self.head_key);
        assert!(!left.is_underfull());
        assert!(!right.is_underfull());
        left.assert_inline_keys_match();
        right.assert_inline_keys_match();
    }
}

/// Random operations on one leaf, its head included, checked against a
/// model after each. A split of the full leaf is put back together
/// through a parent. Returns how many times the leaf split.
fn random_operations<'l, V: FromTracked<'l>, S: InlineKeyStrategy<Tracked<'l>>>(
    ledger: &'l DropLedger,
    steps: usize,
) -> usize {
    let pages = TestPages::<Tracked<'l>, V, S>::new();
    let rng = TestRng::new(1);
    let mut tree = Root::new(pages.fresh());
    let mut model = LeafModel {
        entries: Vec::new(),
        head: None,
        head_key: None,
    };
    let mut splits = 0;

    for _ in 0..steps {
        let len = model.entries.len();
        match rng.below(11) {
            0..=3 => {
                let index = rng.below(len + 1);
                let (key, value) = (ledger.track(), V::from_tracked(ledger.track()));
                model.entries.insert(index, (key.id(), value.tracked_id()));
                let leaf = root_leaf(&tree);
                match leaf.insert(index, InlinedKey::new(key, 0), value, || pages.fresh()) {
                    Insertion::Fit(fitted) => assert_eq!(fitted, index),
                    Insertion::Split(split) => {
                        model.assert_split_halves(leaf, &split);
                        let parts = SplitLeaf {
                            separator: split.separator,
                            right: split.right,
                            left_len: leaf.len(),
                        };
                        tree = parts.reassemble(tree, &pages, &rng, &mut model.entries);
                        splits += 1;
                    }
                }
            }
            4 | 5 if len > 0 => {
                let index = rng.below(len);
                let (key, value) = root_leaf(&tree).remove(index);
                assert_eq!((key.id(), value.tracked_id()), model.entries.remove(index));
            }
            6 if len > 0 => {
                let index = rng.below(len);
                let (key, value) = (ledger.track(), V::from_tracked(ledger.track()));
                let new_ids = (key.id(), value.tracked_id());
                let (old_key, old_value) =
                    root_leaf(&tree).replace_key_value(index, InlinedKey::new(key, 0), value);
                assert_eq!((old_key.id(), old_value.tracked_id()), model.entries[index]);
                model.entries[index] = new_ids;
            }
            7 if len > 0 => {
                let index = rng.below(len);
                let value = V::from_tracked(ledger.track());
                model.entries[index].1 = value.tracked_id();
                let leaf = root_leaf(&tree);
                let old_value = core::mem::replace(leaf.value_mut(index), value);
                drop(old_value);
            }
            8 if len > 0 => {
                let leaf = root_leaf(&tree);
                let index = rng.below(len);
                assert_eq!(leaf.key(index).id(), model.entries[index].0);
                let (key, value) = leaf.key_value(index);
                assert_eq!((key.id(), value.tracked_id()), model.entries[index]);
                assert_eq!(leaf.keys().len(), len);
            }
            9 => {
                let leaf = root_leaf(&tree);
                match (model.head, rng.below(3)) {
                    (Some(head_id), 0) => {
                        let (key, value) = leaf.take_head();
                        assert_eq!(value.tracked_id(), head_id);
                        assert_eq!(key.as_ref().map(Tracked::id), model.head_key);
                        model.head = None;
                        model.head_key = None;
                    }
                    (Some(head_id), 1) => {
                        match leaf.take_head_and_first_key() {
                            Some(succession) => {
                                assert_eq!(succession.value.tracked_id(), head_id);
                                let removed_key = succession.removed_key.as_ref().map(Tracked::id);
                                assert_eq!(removed_key, model.head_key);
                                let (first_key_id, first_value_id) = model.entries.remove(0);
                                // The first key goes up, or where separator
                                // keys are heads, becomes the head's (D49).
                                match succession.successor.key() {
                                    Some(key) => assert_eq!(key.id(), first_key_id),
                                    None => model.head_key = Some(first_key_id),
                                }
                                model.head = Some(first_value_id);
                            }
                            None => assert!(model.entries.is_empty()),
                        }
                    }
                    (Some(_), _) => {}
                    (None, _) => {
                        let value = V::from_tracked(ledger.track());
                        model.head = Some(value.tracked_id());
                        let key = NodePtr::<Tracked<'l>, V, S>::has_separator_keys_in_heads()
                            .then(|| ledger.track());
                        model.head_key = key.as_ref().map(Tracked::id);
                        leaf.put_head(key, value);
                    }
                }
            }
            _ => {}
        }
        model.assert_matches(root_leaf(&tree));
    }
    tree.destroy(|page| pages.release(page));
    pages.assert_all_released();
    splits
}

/// The leaf operations without inline keys, and with them, on the same
/// operations.
#[test]
fn random_operations_match_a_model() {
    let ledger = DropLedger::new();
    let (bulky_steps, tracked_steps) = if cfg!(miri) {
        (200, 200)
    } else {
        (5_000, 20_000)
    };
    assert!(random_operations::<Bulky<'_>, Auto>(&ledger, bulky_steps) > 0);
    assert!(random_operations::<Bulky<'_>, WithInlineKey>(&ledger, bulky_steps) > 0);
    let tracked_splits = random_operations::<Tracked<'_>, Auto>(&ledger, tracked_steps);
    assert!(cfg!(miri) || tracked_splits > 0);
    let tracked_splits = random_operations::<Tracked<'_>, WithInlineKey>(&ledger, tracked_steps);
    assert!(cfg!(miri) || tracked_splits > 0);
    ledger.assert_all_dropped_once();
}

/// The parts of a root leaf that split, besides the leaf itself.
struct SplitLeaf<K, V, S> {
    separator: Separator<K, S>,
    right: Leaf<K, V, S>,
    left_len: usize,
}

impl<'l, V: FromTracked<'l>, S: InlineKeyStrategy<Tracked<'l>>> SplitLeaf<Tracked<'l>, V, S> {
    /// Puts the split root leaf `tree` back together in one leaf: hangs
    /// both halves under a new root, removes one random entry other than
    /// the separator so that they fit, merges them, which brings the
    /// separator back beside its value, and pops the root. `entries`
    /// loses the removed entry.
    fn reassemble(
        self,
        mut tree: Root<Tracked<'l>, V, S>,
        pages: &TestPages<Tracked<'l>, V, S>,
        rng: &TestRng,
        entries: &mut Vec<(usize, usize)>,
    ) -> Root<Tracked<'l>, V, S> {
        let root = tree.raise_in(pages);
        assert_eq!(root.len(), 0);
        root.insert_fit(0, self.separator, Node::Leaf(self.right));
        root.assert_inline_keys_match();

        let right_len = entries.len() - self.left_len - 1;
        let in_left = self.left_len > 0 && (right_len == 0 || rng.below(2) == 0);
        let child = root.leaf_child(usize::from(!in_left));
        let index = rng.below(child.len());
        let (key, value) = child.remove(index);
        let value = value.tracked_id();
        let model_index = if in_left {
            index
        } else {
            self.left_len + 1 + index
        };
        assert_eq!((key.id(), value), entries.remove(model_index));

        let siblings = root.siblings::<Leaf<Tracked<'l>, V, S>>(0);
        assert_eq!(
            siblings.left_len() + 1 + siblings.right_len(),
            entries.len()
        );
        assert!(siblings.can_merge(), "the halves fit again");
        pages.release(siblings.merge());
        assert_eq!(root.len(), 0);

        pages.release(tree.lower());
        assert_eq!(tree.height(), 0);
        tree
    }
}

/// Builds a full leaf of tracked entries, with a head if `with_head`,
/// inserts one more at `position`, and checks the split against the
/// model.
fn split_at<'l, V: FromTracked<'l>, S: InlineKeyStrategy<Tracked<'l>>>(
    ledger: &'l DropLedger,
    position: usize,
    with_head: bool,
) {
    let pages = TestPages::<Tracked<'l>, V, S>::new();
    let leaf = Leaf::new(pages.fresh_leaf());
    let mut model = LeafModel {
        entries: Vec::new(),
        head: None,
        head_key: None,
    };
    if with_head {
        let value = V::from_tracked(ledger.track());
        model.head = Some(value.tracked_id());
        let key =
            NodePtr::<Tracked<'l>, V, S>::has_separator_keys_in_heads().then(|| ledger.track());
        model.head_key = key.as_ref().map(Tracked::id);
        leaf.put_head(key, value);
    }
    while leaf.len() < Leaf::<Tracked<'l>, V, S>::capacity() {
        let (key, value) = (ledger.track(), V::from_tracked(ledger.track()));
        model.entries.push((key.id(), value.tracked_id()));
        leaf.insert_fit(leaf.len(), InlinedKey::new(key, 0), value);
    }
    let (key, value) = (ledger.track(), V::from_tracked(ledger.track()));
    model
        .entries
        .insert(position, (key.id(), value.tracked_id()));
    let Insertion::Split(split) =
        leaf.insert(position, InlinedKey::new(key, 0), value, || pages.fresh())
    else {
        panic!("a full leaf splits");
    };
    model.assert_split_halves(leaf, &split);
    let Split {
        separator, right, ..
    } = split;
    drop(separator);
    destroy_leaf(right, &pages);
    destroy_leaf(leaf, &pages);
    pages.assert_all_released();
}

#[test]
fn split_and_insert_at_every_position() {
    fn every_position<'l, V: FromTracked<'l>, S: InlineKeyStrategy<Tracked<'l>>>(
        ledger: &'l DropLedger,
        heads: &[bool],
    ) {
        for position in 0..=Leaf::<Tracked<'l>, V, S>::capacity() {
            for &with_head in heads {
                split_at::<V, S>(ledger, position, with_head);
            }
        }
    }
    let ledger = DropLedger::new();
    if cfg!(miri) {
        every_position::<Bulky<'_>, Auto>(&ledger, &[true]);
        every_position::<Bulky<'_>, WithInlineKey>(&ledger, &[false]);
    } else {
        every_position::<Bulky<'_>, Auto>(&ledger, &[false, true]);
        every_position::<Bulky<'_>, WithInlineKey>(&ledger, &[false, true]);
        every_position::<Tracked<'_>, Auto>(&ledger, &[false, true]);
        every_position::<Tracked<'_>, WithInlineKey>(&ledger, &[false, true]);
    }
    ledger.assert_all_dropped_once();
}

#[test]
fn boundary_insert_remove_keeps_split_headroom() {
    for at_front in [false, true] {
        let pages = TestPages::<u64, u64>::new();
        let mut tree = Root::new(pages.fresh());
        let mut length = 0;
        let capacity = Leaf::<u64, u64, Auto>::capacity() as u64;
        for key in 1..=capacity {
            tree.insert(key, key, || pages.fresh(), || pages.fresh(), &mut length);
        }
        let boundary_key = if at_front { 0 } else { capacity + 1 };
        tree.insert(
            boundary_key,
            boundary_key,
            || pages.fresh(),
            || pages.fresh(),
            &mut length,
        );
        assert_eq!(tree.height(), 1);
        let root = tree.top().node_ptr();
        let allocated = pages.allocated_count();
        let in_use = pages.in_use();

        for _ in 0..16 {
            let removed = if at_front {
                tree.pop_first(|page| pages.release(page), &mut length)
            } else {
                tree.pop_last(|page| pages.release(page), &mut length)
            };
            assert_eq!(removed, Some((boundary_key, boundary_key)));
            assert_eq!(tree.height(), 1, "deletion must not undo the split");
            assert!(tree.top().node_ptr().is(root));
            assert_eq!(pages.in_use(), in_use, "no node should be merged away");
            assert_eq!(
                tree.insert(
                    boundary_key,
                    boundary_key,
                    || panic!("boundary churn must not allocate a leaf"),
                    || panic!("boundary churn must not allocate an internal node"),
                    &mut length,
                ),
                None,
            );
            assert_eq!(pages.allocated_count(), allocated);
        }
        tree.destroy(|page| pages.release(page));
        pages.assert_all_released();
    }
}

#[test]
#[cfg_attr(panic = "abort", ignore = "needs unwinding")]
fn checked_positions_reject_out_of_bounds_indices() {
    let pages = TestPages::<u32, u64>::new();
    let leaf = Leaf::new(pages.fresh_leaf());
    assert!(catch_unwind(AssertUnwindSafe(|| *leaf.key(0))).is_err());
    push_back(leaf, 1, 10);
    push_back(leaf, 2, 20);
    assert!(catch_unwind(AssertUnwindSafe(|| leaf.remove(2))).is_err());
    assert!(catch_unwind(AssertUnwindSafe(|| *leaf.key_value(2).1)).is_err());
    assert!(
        catch_unwind(AssertUnwindSafe(
            || leaf.replace_key(2, InlinedKey::new(3, 0))
        ))
        .is_err()
    );
    assert_eq!(leaf.keys().iter().copied().collect::<Vec<_>>(), [1, 2]);
    assert!(leaf.inline_keys().is_none());
    destroy_leaf(leaf, &pages);
    pages.assert_all_released();
}

#[test]
fn zero_sized_keys_and_values() {
    let pages = TestPages::<(), ()>::new();
    let leaf = Leaf::new(pages.fresh_leaf());
    let mut len = 0;
    while leaf.len() < Leaf::<(), (), Auto>::capacity() {
        leaf.insert_fit(0, InlinedKey::new((), 0), ());
        len += 1;
    }
    assert_eq!(len, 1023);
    let Insertion::Split(split) =
        leaf.insert(len / 2, InlinedKey::new((), 0), (), || pages.fresh())
    else {
        panic!("a full leaf splits");
    };
    assert_eq!(leaf.len() + 1 + split.right.len(), len + 1);
    assert!(split.right.has_head());
    destroy_leaf(split.right, &pages);
    assert!(leaf.search(&SearchKey::new(&())).is_ok());
    destroy_leaf(leaf, &pages);
    pages.assert_all_released();
}

#[test]
fn over_aligned_keys_and_values() {
    let rng = TestRng::new(3);
    let count = if cfg!(miri) { 40 } else { 400 };
    let mut keys: Vec<u64> = (0..count).collect();
    rng.shuffle(&mut keys);

    let pages = TestPages::<CacheLineAligned, CacheLineAligned>::new();
    let mut tree = TestTree::new(&pages);
    for &key in &keys {
        let key = CacheLineAligned::new(key);
        assert!(
            tree.insert(key, CacheLineAligned::new(key.get() * 3))
                .is_none()
        );
    }
    let entries = tree.entries(|key, value| (key.get(), value.get()));
    assert_eq!(
        entries,
        (0..count).map(|key| (key, key * 3)).collect::<Vec<_>>()
    );
    assert!(tree.height() >= 1);
    tree.destroy();
    pages.assert_all_released();

    let pages = TestPages::<u64, PageAligned>::new();
    let mut tree = TestTree::new(&pages);
    let count = if cfg!(miri) { 20 } else { 100 };
    for &key in keys.iter().filter(|&&key| key < count) {
        tree.insert(key, PageAligned::new(key + 1));
    }
    let entries = tree.entries(|key, value| (*key, value.get()));
    assert_eq!(
        entries,
        (0..count).map(|key| (key, key + 1)).collect::<Vec<_>>()
    );
    assert!(tree.height() >= 1);
    tree.destroy();
    pages.assert_all_released();
}
