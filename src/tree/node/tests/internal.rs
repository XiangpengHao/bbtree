//! Internal-node operations (`tree/node/internal.rs`, `siblings.rs`) against
//! a model: inserts that fit or split, levels, and balancing between
//! siblings.

use core::panic::AssertUnwindSafe;
use std::panic::catch_unwind;
use std::vec::Vec;

use crate::inline_key::{Auto, InlineKeyStrategy, WithInlineKey};
use crate::tree::invariants::NodeChecks;
use crate::tree::node::inline_keys::{InlinedKey, Separator};
use crate::tree::node::internal::Internal;
use crate::tree::node::leaf::Leaf;
use crate::tree::node::page::TestPages;
use crate::tree::node::siblings::StealError;
use crate::tree::node::{Child, Insertion, Node, Split};
use crate::tree::root::Root;
use crate::tree::shared_prefix::SharedPrefix;
use crate::tree::test_support::{Bulky, DropLedger, FromTracked, TestRng, TestTree, Tracked};

/// A leaf holding one tracked entry, named by the id of its key.
fn named_leaf<'l, K: FromTracked<'l>, S: InlineKeyStrategy<K>>(
    pages: &TestPages<K, Tracked<'l>, S>,
    ledger: &'l DropLedger,
) -> (Leaf<K, Tracked<'l>, S>, usize) {
    let leaf = Leaf::new(pages.fresh_leaf());
    let key = K::from_tracked(ledger.track());
    let name = key.tracked_id();
    leaf.insert_fit(0, InlinedKey::new(key, 0), ledger.track());
    (leaf, name)
}

/// A named leaf for the right of separator `key`, with a head to hold the
/// separator's value (D17), and its key where separator keys are heads
/// (D49). Returns the leaf, its name, the id of its head, and the separator,
/// as its parent takes it.
fn child_leaf<'l, K: FromTracked<'l>, S: InlineKeyStrategy<K>>(
    pages: &TestPages<K, Tracked<'l>, S>,
    ledger: &'l DropLedger,
    key: K,
) -> (Node<K, Tracked<'l>, S>, usize, usize, Separator<K, S>) {
    let (leaf, name) = named_leaf(pages, ledger);
    let head = ledger.track();
    let head_id = head.id();
    let separator = leaf.put_separator_head(InlinedKey::new(key, 0), head);
    (Node::Leaf(leaf), name, head_id, separator)
}

/// The names of the leaf children of an internal node, in order.
fn child_names<'l, K: FromTracked<'l>, V, S: InlineKeyStrategy<K>>(
    node: Internal<K, V, S>,
) -> Vec<usize> {
    (0..=node.len())
        .map(|index| node.leaf_child(index).keys()[0].tracked_id())
        .collect()
}

/// The root of a tree that must be internal.
fn internal_root<K, V, S>(tree: &Root<K, V, S>) -> Internal<K, V, S> {
    match tree.top() {
        Node::Internal(root) => root,
        Node::Leaf(_) => panic!("the root is a leaf"),
    }
}

/// A root with no keys above one named leaf, the leftmost, which has no
/// head; and the leaf's name.
fn root_above_one_leaf<'l, K: FromTracked<'l>, S: InlineKeyStrategy<K>>(
    pages: &TestPages<K, Tracked<'l>, S>,
    ledger: &'l DropLedger,
) -> (Root<K, Tracked<'l>, S>, usize) {
    let (leaf, name) = named_leaf(pages, ledger);
    let mut tree = Root::new(Node::Leaf(leaf), SharedPrefix::none());
    tree.push_internal_level(pages.fresh());
    (tree, name)
}

/// Random operations on one internal node with leaf children, checked
/// against a model of its separators, with their values from the
/// children's heads, and of its children's names; and its inline keys, if
/// it has them, against its keys.
fn random_operations<'l, K: FromTracked<'l>, S: InlineKeyStrategy<K>>(
    ledger: &'l DropLedger,
    steps: usize,
) {
    let pages = TestPages::<K, Tracked<'l>, S>::new();
    let rng = TestRng::new(2);
    let (tree, first_name) = root_above_one_leaf(&pages, ledger);
    let mut separators: Vec<(usize, usize)> = Vec::new();
    let mut children = std::vec![first_name];

    for _ in 0..steps {
        let len = separators.len();
        let root = internal_root(&tree);
        match rng.below(10) {
            0..=3 => {
                let index = rng.below(len + 1);
                if root.len() < Internal::<K, Tracked<'l>, S>::capacity() {
                    let key = K::from_tracked(ledger.track());
                    let key_id = key.tracked_id();
                    let (child, name, head_id, separator) = child_leaf(&pages, ledger, key);
                    separators.insert(index, (key_id, head_id));
                    children.insert(index + 1, name);
                    root.insert_fit(index, separator, child);
                }
            }
            4 | 5 if len > 0 => {
                let index = rng.below(len);
                let (separator, child) = root.remove_with_right_child(index);
                let head_id = child.leftmost_head_id();
                assert_eq!(
                    (
                        child.separator_key_id(&separator),
                        head_id.expect("the child has a head")
                    ),
                    separators.remove(index)
                );
                assert_eq!(child.height(), 0);
                let Node::Leaf(child_leaf) = child else {
                    panic!("the root's children are leaves");
                };
                let name = child_leaf.keys()[0].tracked_id();
                assert_eq!(name, children.remove(index + 1));
                drop(separator);
                child.destroy(|page| pages.release(page));
            }
            6 if len > 0 => {
                let index = rng.below(len);
                let key = K::from_tracked(ledger.track());
                let new_id = key.tracked_id();
                let old_key = root.replace_key(index, InlinedKey::new(key, 0));
                assert_eq!(old_key.tracked_id(), separators[index].0);
                separators[index].0 = new_id;
            }
            7 if len > 0 => {
                let index = rng.below(len);
                let value = ledger.track();
                let new_id = value.id();
                let head = root.head_leaf(index);
                let old_value = core::mem::replace(head.head_value_mut(), value);
                assert_eq!(old_value.id(), separators[index].1);
                separators[index].1 = new_id;
            }
            8 => {
                let index = rng.below(len + 1);
                let child = root.leaf_child(index);
                assert_eq!(child.keys()[0].tracked_id(), children[index]);
                if index > 0 {
                    assert!(root.head_leaf(index - 1).node_ptr().is(child.node_ptr()));
                }
            }
            9 => {
                let index = rng.below(len + 1);
                let child = root.child(index);
                assert_eq!(child.height(), 0);
                let Node::Leaf(child) = child else {
                    panic!("the children are leaves");
                };
                assert_eq!(child.key(0).tracked_id(), children[index]);
                drop(core::mem::replace(child.value_mut(0), ledger.track()));
            }
            _ => {}
        }
        let root = internal_root(&tree);
        assert_eq!(root.tracked_ids(), separators);
        assert_eq!(child_names(root), children);
        root.assert_inline_keys_match();
    }
    tree.destroy(|page| pages.release(page));
    pages.assert_all_released();
}

#[test]
fn random_operations_match_a_model() {
    let ledger = DropLedger::new();
    let steps = if cfg!(miri) { 200 } else { 10_000 };
    random_operations::<Bulky<'_>, Auto>(&ledger, steps);
    random_operations::<Tracked<'_>, Auto>(&ledger, steps);
    random_operations::<Tracked<'_>, WithInlineKey>(&ledger, steps);
    ledger.assert_all_dropped_once();
}

/// Builds a full internal node with named leaf children, inserts one more
/// separator and child at `position`, and checks the split against the
/// model: keys, with their inline keys, and edges move, and every
/// separator keeps its value.
fn split_at<'l, K: FromTracked<'l>, S: InlineKeyStrategy<K>>(
    ledger: &'l DropLedger,
    position: usize,
) {
    let pages = TestPages::<K, Tracked<'l>, S>::new();
    let (tree, first_name) = root_above_one_leaf(&pages, ledger);
    let left = internal_root(&tree);
    let mut separators = Vec::new();
    let mut children = std::vec![first_name];
    while left.len() < Internal::<K, Tracked<'l>, S>::capacity() {
        let key = K::from_tracked(ledger.track());
        let key_id = key.tracked_id();
        let (child, name, head_id, separator) = child_leaf(&pages, ledger, key);
        separators.push((key_id, head_id));
        children.push(name);
        left.insert_fit(left.len(), separator, child);
    }
    let key = K::from_tracked(ledger.track());
    let key_id = key.tracked_id();
    let (child, name, head_id, separator) = child_leaf(&pages, ledger, key);
    separators.insert(position, (key_id, head_id));
    children.insert(position + 1, name);

    let Insertion::Split(Split {
        separator, right, ..
    }) = left.insert(position, separator, child, || pages.fresh())
    else {
        panic!("a full node splits");
    };
    let below_separator = right.child(0);
    let separator_value = below_separator.leftmost_head_id();
    let mut halves = left.tracked_ids();
    halves.push((
        below_separator.separator_key_id(&separator),
        separator_value.expect("the right half's leftmost leaf has a head"),
    ));
    halves.extend(right.tracked_ids());
    assert_eq!(halves, separators);
    let mut halves_children = child_names(left);
    halves_children.extend(child_names(right));
    assert_eq!(halves_children, children);
    assert!(!left.is_underfull());
    assert!(!right.is_underfull());
    assert_eq!((left.height(), right.height()), (1, 1));
    left.assert_inline_keys_match();
    right.assert_inline_keys_match();
    drop(separator);
    Node::Internal(right).destroy(|page| pages.release(page));
    tree.destroy(|page| pages.release(page));
    pages.assert_all_released();
}

#[test]
fn split_and_insert_at_every_position() {
    fn every_position<'l, K: FromTracked<'l>, S: InlineKeyStrategy<K>>(ledger: &'l DropLedger) {
        for position in 0..=Internal::<K, Tracked<'l>, S>::capacity() {
            split_at::<K, S>(ledger, position);
        }
    }
    let ledger = DropLedger::new();
    every_position::<Bulky<'_>, Auto>(&ledger);
    if !cfg!(miri) {
        every_position::<Tracked<'_>, Auto>(&ledger);
        every_position::<Tracked<'_>, WithInlineKey>(&ledger);
    }
    ledger.assert_all_dropped_once();
}

#[test]
#[cfg_attr(panic = "abort", ignore = "needs unwinding")]
#[cfg_attr(
    miri,
    ignore = "the rejected child is leaked on purpose, which Miri reports"
)]
fn insert_rejects_a_child_of_the_wrong_height() {
    let ledger = DropLedger::new();
    let pages = TestPages::<Tracked<'_>, Tracked<'_>>::new();
    let (tree, first_name) = root_above_one_leaf(&pages, &ledger);
    let (child, _, _, separator) = child_leaf(&pages, &ledger, ledger.track());
    let too_tall = Node::Internal(Internal::new(pages.fresh(), child));

    let root = internal_root(&tree);
    let rejected = catch_unwind(AssertUnwindSafe(|| root.insert_fit(0, separator, too_tall)));
    assert!(rejected.is_err());
    assert_eq!(root.len(), 0);
    assert_eq!(child_names(root), [first_name]);
    tree.destroy(|page| pages.release(page));
    assert_eq!(pages.in_use(), 2, "only the rejected child is leaked");
}

/// A separator over a leaf without a head is a broken tree: balancing it
/// panics instead of moving an empty head slot.
#[test]
#[cfg_attr(panic = "abort", ignore = "needs unwinding")]
fn balancing_refuses_a_headless_right_leaf() {
    let ledger = DropLedger::new();
    let pages = TestPages::<Tracked<'_>, Tracked<'_>>::new();
    let (tree, _) = root_above_one_leaf(&pages, &ledger);
    let (headless, _) = named_leaf(&pages, &ledger);
    let root = internal_root(&tree);
    let key = InlinedKey::new(ledger.track(), 0);
    root.insert_fit(0, Separator::with_key(key), Node::Leaf(headless));
    let siblings = catch_unwind(AssertUnwindSafe(|| {
        root.siblings::<Leaf<Tracked<'_>, Tracked<'_>, Auto>>(0)
            .left_len()
    }));
    assert!(siblings.is_err());
    // Taking the tree apart needs the head the separator's value is.
    root.leaf_child(1).put_head(None, ledger.track());
    tree.destroy(|page| pages.release(page));
    pages.assert_all_released();
    ledger.assert_all_dropped_once();
}

#[test]
#[cfg_attr(panic = "abort", ignore = "needs unwinding")]
fn push_and_pop_levels() {
    let ledger = DropLedger::new();
    let pages = TestPages::<Tracked<'_>, Tracked<'_>>::new();
    let mut tree = Root::new_leaf(pages.fresh());
    for _ in 0..3 {
        let Node::Leaf(leaf) = tree.top() else {
            panic!("the root is a leaf");
        };
        leaf.insert_fit(
            leaf.len(),
            InlinedKey::new(ledger.track(), 0),
            ledger.track(),
        );
    }
    let ids = tree.top().tracked_ids_under();

    for height in 1..=3 {
        let root = tree.push_internal_level(pages.fresh());
        assert_eq!((root.len(), root.height()), (0, height));
        assert_eq!(tree.height(), height);
    }
    let mut node = tree.top();
    while let Node::Internal(internal) = node {
        assert_eq!(internal.len(), 0);
        node = internal.child(0);
    }
    assert_eq!(node.height(), 0);
    assert_eq!(node.tracked_ids_under(), ids);

    for height in (0..3).rev() {
        pages.release(tree.pop_internal_level());
        assert_eq!(tree.height(), height);
    }
    let popped_leaf = catch_unwind(AssertUnwindSafe(|| tree.pop_internal_level()));
    assert!(popped_leaf.is_err(), "a leaf root has no level to pop");
    assert_eq!(tree.top().tracked_ids_under(), ids);
    tree.destroy(|page| pages.release(page));
    pages.assert_all_released();
    ledger.assert_all_dropped_once();
}

/// Bulky keys: 12 entries in either kind of node.
type BulkyTree<'p, 'l> = TestTree<'p, Bulky<'l>, Tracked<'l>>;

/// Leaves of a [`BulkyTree`].
type BulkyLeaf<'l> = Leaf<Bulky<'l>, Tracked<'l>, Auto>;

/// A tree of `count` entries with ascending keys.
fn ascending_tree<'p, 'l>(
    pages: &'p TestPages<Bulky<'l>, Tracked<'l>>,
    ledger: &'l DropLedger,
    count: usize,
) -> BulkyTree<'p, 'l> {
    let mut tree = TestTree::new(pages);
    for key in ledger.track_many(count) {
        tree.insert(Bulky::from_tracked(key), ledger.track());
    }
    tree
}

fn entry_ids<'l, K: FromTracked<'l>, S: InlineKeyStrategy<K>>(
    tree: &TestTree<'_, K, Tracked<'l>, S>,
) -> Vec<(usize, usize)> {
    tree.entries(|key, value| (key.tracked_id(), value.id()))
}

#[test]
fn steals_and_merges_of_leaves() {
    let ledger = DropLedger::new();
    let pages = TestPages::new();
    // 13 entries in 12-entry leaves: one split, two leaves of 6 around
    // a separator, whose value is the right leaf's head.
    let mut tree = ascending_tree(&pages, &ledger, 13);
    let mut model = entry_ids(&tree);
    assert_eq!(tree.height(), 1);

    let root = internal_root(tree.root());
    let mut siblings = root.siblings::<BulkyLeaf<'_>>(0);
    assert_eq!((siblings.left_len(), siblings.right_len()), (6, 6));
    assert!(
        !siblings.can_merge(),
        "6 + 1 + 6 entries don't fit in one leaf"
    );
    assert_eq!(siblings.steal_left(), Ok(()));
    assert_eq!((siblings.left_len(), siblings.right_len()), (5, 7));
    assert_eq!(siblings.steal_right(), Ok(()));
    assert_eq!(siblings.steal_right(), Ok(()));
    assert_eq!((siblings.left_len(), siblings.right_len()), (7, 5));
    assert_eq!(root.key(0).tracked_id(), model[7].0);
    assert_eq!(entry_ids(&tree), model);
    assert_eq!(root.leaf_child(0).head_id(), None);
    assert_eq!(root.leaf_child(1).head_id(), Some(model[7].1));

    let (key, value) = root.leaf_child(0).remove(3);
    assert_eq!((key.tracked_id(), value.id()), model.remove(3));
    drop((key, value));
    let siblings = root.siblings::<BulkyLeaf<'_>>(0);
    assert!(siblings.can_merge(), "6 + 1 + 5 entries fit in one leaf");
    pages.release(siblings.merge());
    assert_eq!(root.len(), 0);
    let merged_leaf = root.leaf_child(0);
    assert_eq!(merged_leaf.len(), 12);
    assert!(!merged_leaf.has_head(), "the leftmost leaf has no head");
    tree.pop_empty_roots();
    assert_eq!(tree.height(), 0);
    assert_eq!(entry_ids(&tree), model);

    tree.destroy();
    pages.assert_all_released();
    ledger.assert_all_dropped_once();
}

#[test]
fn steals_refuse_an_empty_donor_or_a_full_receiver() {
    let ledger = DropLedger::new();
    let pages = TestPages::new();
    // Two leaves of 6, then 6 more entries fill the right one.
    let tree = ascending_tree(&pages, &ledger, 19);
    assert_eq!(tree.height(), 1);
    let model = entry_ids(&tree);

    let root = internal_root(tree.root());
    let mut siblings = root.siblings::<BulkyLeaf<'_>>(0);
    assert_eq!((siblings.left_len(), siblings.right_len()), (6, 12));
    assert_eq!(siblings.steal_left(), Err(StealError::ReceiverFull));
    assert_eq!((siblings.left_len(), siblings.right_len()), (6, 12));
    let left = siblings.left();
    while left.len() > 0 {
        left.remove(0);
    }

    let mut siblings = root.siblings::<BulkyLeaf<'_>>(0);
    assert_eq!(siblings.steal_left(), Err(StealError::DonorEmpty));
    assert_eq!(siblings.steal_right(), Ok(()));
    assert_eq!((siblings.left_len(), siblings.right_len()), (1, 11));
    let right = siblings.right();
    while right.len() > 0 {
        right.remove(right.len() - 1);
    }
    let mut siblings = root.siblings::<BulkyLeaf<'_>>(0);
    assert_eq!(siblings.steal_right(), Err(StealError::DonorEmpty));
    assert_eq!(entry_ids(&tree), [model[6], model[7]]);

    tree.destroy();
    pages.assert_all_released();
    ledger.assert_all_dropped_once();
}

#[test]
#[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
fn steals_and_merges_of_internal_nodes() {
    type BulkyInternal<'l> = Internal<Bulky<'l>, Tracked<'l>, Auto>;
    let ledger = DropLedger::new();
    let pages = TestPages::new();
    let mut tree = ascending_tree(&pages, &ledger, 100);
    assert_eq!(tree.height(), 2);
    let mut model = entry_ids(&tree);

    let root = internal_root(tree.root());
    let mut siblings = root.siblings::<BulkyInternal<'_>>(0);
    let (left_len, right_len) = (siblings.left_len(), siblings.right_len());
    assert_eq!(siblings.steal_left(), Ok(()));
    assert_eq!(siblings.steal_right(), Ok(()));
    assert_eq!(siblings.steal_right(), Ok(()));
    assert_eq!(
        (siblings.left_len(), siblings.right_len()),
        (left_len + 1, right_len - 1)
    );
    assert_eq!(siblings.left().height(), 1);
    assert_eq!(entry_ids(&tree), model);

    // Shrink the left child until both children fit in one node.
    loop {
        let siblings = root.siblings::<BulkyInternal<'_>>(0);
        if siblings.can_merge() {
            break;
        }
        let (separator, grandchild) = siblings.left().remove_with_right_child(0);
        let value_id = grandchild.leftmost_head_id();
        let key_id = grandchild.separator_key_id(&separator);
        let mut removed = std::vec![(key_id, value_id.expect("a head"))];
        removed.extend(grandchild.tracked_ids_under());
        model.retain(|entry| !removed.contains(entry));
        drop(separator);
        grandchild.destroy(|page| pages.release(page));
    }
    let siblings = root.siblings::<BulkyInternal<'_>>(0);
    pages.release(siblings.merge());
    assert_eq!(root.child(0).height(), 1);
    tree.pop_empty_roots();
    assert_eq!(entry_ids(&tree), model);

    tree.destroy();
    pages.assert_all_released();
    ledger.assert_all_dropped_once();
}

/// A random separator of a random internal node, if the walk down meets
/// one with keys: the node, and the separator's index.
fn random_separator<K, V, S: InlineKeyStrategy<K>>(
    mut node: Node<K, V, S>,
    rng: &TestRng,
) -> Option<(Internal<K, V, S>, usize)> {
    loop {
        let Node::Internal(internal) = node else {
            return None;
        };
        let len = internal.len();
        if len > 0 && (internal.height() == 1 || rng.below(2) == 0) {
            return Some((internal, rng.below(len)));
        }
        node = internal.child(rng.below(len + 1));
    }
}

/// Removes a random entry of a random leaf, if the leaf has one.
fn remove_random_leaf_entry<K, V, S: InlineKeyStrategy<K>>(
    mut node: Node<K, V, S>,
    rng: &TestRng,
) -> Option<(K, V)> {
    loop {
        match node {
            Node::Leaf(leaf) => {
                let len = leaf.len();
                return (len > 0).then(|| leaf.remove(rng.below(len)));
            }
            Node::Internal(internal) => {
                let len = internal.len();
                node = internal.child(rng.below(len + 1));
            }
        }
    }
}

/// Random removals, steals and merges anywhere in a tree, checked against
/// a model of its entries, and its inline keys, if it has them, against
/// its keys.
fn random_balancing<'l, K: FromTracked<'l> + Ord, S: InlineKeyStrategy<K>>(
    ledger: &'l DropLedger,
    seed: u64,
) {
    let pages = TestPages::<K, Tracked<'l>, S>::new();
    let rng = TestRng::new(seed);
    let count = if cfg!(miri) { 80 } else { 800 };
    let mut keys = ledger.track_many(count);
    rng.shuffle(&mut keys);
    let mut tree = TestTree::new(&pages);
    for key in keys {
        tree.insert(K::from_tracked(key), ledger.track());
    }
    let mut model = entry_ids(&tree);

    let steps = if cfg!(miri) { 100 } else { 4000 };
    let (mut merges, mut steals, mut refusals) = (0, 0, 0);
    for _ in 0..steps {
        if rng.below(3) == 0 {
            if let Some((key, value)) = remove_random_leaf_entry(tree.root_mut().top_mut(), &rng) {
                model.retain(|&entry| entry != (key.tracked_id(), value.id()));
            }
        } else if let Some((node, index)) = random_separator(tree.root_mut().top_mut(), &rng) {
            let balanced = if node.has_leaf_children() {
                balance_randomly(
                    node.siblings::<Leaf<K, Tracked<'l>, S>>(index),
                    &rng,
                    &pages,
                )
            } else {
                balance_randomly(
                    node.siblings::<Internal<K, Tracked<'l>, S>>(index),
                    &rng,
                    &pages,
                )
            };
            match balanced {
                Balanced::Merged => merges += 1,
                Balanced::Stole => steals += 1,
                Balanced::Refused => refusals += 1,
            }
        }
        tree.pop_empty_roots();
        assert_eq!(entry_ids(&tree), model);
        tree.root().top().assert_inline_keys_match_under();
    }
    assert!(
        merges > 0 && steals > 0,
        "{merges} merges, {steals} steals, {refusals} refusals"
    );

    tree.destroy();
    pages.assert_all_released();
}

/// What a random balancing step did.
enum Balanced {
    Merged,
    Stole,
    Refused,
}

/// A random balancing step between `siblings`: a merge, half the times they
/// fit, or else a steal either way, which may be refused.
fn balance_randomly<
    'l,
    K: FromTracked<'l>,
    S: InlineKeyStrategy<K>,
    C: Child<K, Tracked<'l>, S>,
>(
    mut siblings: crate::tree::node::siblings::Siblings<K, Tracked<'l>, S, C>,
    rng: &TestRng,
    pages: &TestPages<K, Tracked<'l>, S>,
) -> Balanced {
    if siblings.can_merge() && rng.below(2) == 0 {
        pages.release(siblings.merge());
        return Balanced::Merged;
    }
    let (left_len, right_len) = (siblings.left_len(), siblings.right_len());
    let (stolen, donor_len, receiver_len) = if rng.below(2) == 0 {
        (siblings.steal_left(), left_len, right_len)
    } else {
        (siblings.steal_right(), right_len, left_len)
    };
    match stolen {
        Ok(()) => Balanced::Stole,
        Err(StealError::DonorEmpty) => {
            assert_eq!(donor_len, 0);
            Balanced::Refused
        }
        Err(StealError::ReceiverFull) => {
            assert_eq!(receiver_len, C::capacity());
            Balanced::Refused
        }
    }
}

#[test]
fn random_balancing_keeps_every_entry_in_order() {
    let ledger = DropLedger::new();
    random_balancing::<Bulky<'_>, Auto>(&ledger, 5);
    random_balancing::<Tracked<'_>, WithInlineKey>(&ledger, 6);
    ledger.assert_all_dropped_once();
}
