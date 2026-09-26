//! Internal nodes as the tree uses them: insert a child's separator and
//! right half, splitting a full node first. Also the tests of the
//! internal-node operations, levels and balancing in `handle.rs`.

#![forbid(unsafe_code)]

use super::handle::{Edge, EdgeRoom, Insertion, Internal, NodeMut, Subtree};
use super::inline_keys::InlinedKey;
use super::page::FreshPage;
use crate::inline_key::InlineKeyStrategy;

impl<'a, K, V, S: InlineKeyStrategy<K>> Edge<NodeMut<'a, K, V, S, Internal>> {
    /// Inserts separator `key`, with its inline key, at this edge with
    /// `right_child` as the edge to its right, splitting the node first if it
    /// is full, with the right half in a page from `new_page`. This is how a
    /// child's split lands in its parent; the separator's value is already in
    /// `right_child`, as the head of its leftmost leaf (D17).
    ///
    /// # Panics
    ///
    /// If `right_child` is not exactly one level below this node; the node is
    /// unchanged then.
    pub(crate) fn insert(
        self,
        key: InlinedKey<K, S>,
        right_child: Subtree<K, V, S>,
        new_page: impl FnOnce() -> FreshPage<K, V, S, Internal>,
    ) -> Insertion<'a, K, V, S, Internal> {
        match self.check_room() {
            EdgeRoom::Fits(edge) => Insertion::Fit(edge.insert_fit(key, right_child)),
            EdgeRoom::Full(edge) => {
                Insertion::Split(edge.split_and_insert(new_page(), key, right_child))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use core::panic::AssertUnwindSafe;
    use std::panic::catch_unwind;
    use std::vec::Vec;

    use crate::inline_key::{Auto, InlineKeyStrategy, WithInlineKey};
    use crate::node::handle::{
        EdgeRoom, ForceResult, Immut, Insertion, Internal, Kv, Leaf, LeafOrInternal, NodeMut,
        NodeRef, Split, StealError, Subtree,
    };
    use crate::node::inline_keys::InlinedKey;
    use crate::node::page::{NodePtr, TestPages};
    use crate::node::test_support::{Bulky, DropLedger, FromTracked, TestRng, TestTree, Tracked};

    /// A leaf holding one tracked entry, named by the id of its key.
    fn named_leaf<'l, K: FromTracked<'l>, S: InlineKeyStrategy<K>>(
        pages: &TestPages<K, Tracked<'l>, S>,
        ledger: &'l DropLedger,
    ) -> (Subtree<K, Tracked<'l>, S, Leaf>, usize) {
        let mut leaf = Subtree::new_leaf(pages.fresh());
        let key = K::from_tracked(ledger.track());
        let name = key.tracked_id();
        let EdgeRoom::Fits(edge) = leaf.borrow_mut().first_edge().check_room() else {
            panic!("an empty leaf has room");
        };
        edge.insert_fit(InlinedKey::new(key), ledger.track());
        (leaf, name)
    }

    /// A named leaf for the right of a separator, with a head to hold the
    /// separator's value (D17). Returns the leaf, its name, and the id of its
    /// head.
    fn child_leaf<'l, K: FromTracked<'l>, S: InlineKeyStrategy<K>>(
        pages: &TestPages<K, Tracked<'l>, S>,
        ledger: &'l DropLedger,
    ) -> (Subtree<K, Tracked<'l>, S>, usize, usize) {
        let (mut leaf, name) = named_leaf(pages, ledger);
        let head = ledger.track();
        let head_id = head.id();
        assert!(leaf.borrow_mut().replace_head(head).is_none());
        (leaf.forget_type(), name, head_id)
    }

    /// The names of the leaf children of an internal node, in order.
    fn child_names<'l, K: FromTracked<'l>, V, S: InlineKeyStrategy<K>>(
        node: NodeRef<Immut<'_>, K, V, S, Internal>,
    ) -> Vec<usize> {
        (0..=node.len())
            .map(|index| node.edge_at(index).descend().keys()[0].tracked_id())
            .collect()
    }

    /// The root of a tree that must be internal.
    fn internal_root<K, V, S>(tree: &mut Subtree<K, V, S>) -> NodeMut<'_, K, V, S, Internal> {
        match tree.borrow_mut().force() {
            ForceResult::Internal(root) => root,
            ForceResult::Leaf(_) => panic!("the root is a leaf"),
        }
    }

    /// A root with no keys above one named leaf, the leftmost, which has no
    /// head; and the leaf's name.
    fn root_above_one_leaf<'l, K: FromTracked<'l>, S: InlineKeyStrategy<K>>(
        pages: &TestPages<K, Tracked<'l>, S>,
        ledger: &'l DropLedger,
    ) -> (Subtree<K, Tracked<'l>, S>, usize) {
        let (leaf, name) = named_leaf(pages, ledger);
        let mut tree = leaf.forget_type();
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
        let (mut tree, first_name) = root_above_one_leaf(&pages, ledger);
        let mut separators: Vec<(usize, usize)> = Vec::new();
        let mut children = std::vec![first_name];

        for _ in 0..steps {
            let len = separators.len();
            match rng.below(10) {
                0..=3 => {
                    let index = rng.below(len + 1);
                    let edge = internal_root(&mut tree).edge_at(index);
                    if let EdgeRoom::Fits(edge) = edge.check_room() {
                        let (child, name, head_id) = child_leaf(&pages, ledger);
                        let key = K::from_tracked(ledger.track());
                        separators.insert(index, (key.tracked_id(), head_id));
                        children.insert(index + 1, name);
                        assert_eq!(edge.insert_fit(InlinedKey::new(key), child).index(), index);
                    }
                }
                4 | 5 if len > 0 => {
                    let index = rng.below(len);
                    let (key, child) = internal_root(&mut tree)
                        .kv_at(index)
                        .remove_with_right_child();
                    let head_id = child.borrow().leftmost_head_id();
                    assert_eq!(
                        (key.tracked_id(), head_id.expect("the child has a head")),
                        separators.remove(index)
                    );
                    assert_eq!(child.height(), 0);
                    let name = child.borrow().keys()[0].tracked_id();
                    assert_eq!(name, children.remove(index + 1));
                    drop(key);
                    child.destroy(|page| pages.release(page));
                }
                6 if len > 0 => {
                    let index = rng.below(len);
                    let key = K::from_tracked(ledger.track());
                    let new_id = key.tracked_id();
                    let old_key = internal_root(&mut tree)
                        .kv_at(index)
                        .replace_key(InlinedKey::new(key));
                    assert_eq!(old_key.tracked_id(), separators[index].0);
                    separators[index].0 = new_id;
                }
                7 if len > 0 => {
                    let index = rng.below(len);
                    let value = ledger.track();
                    let new_id = value.id();
                    let head = internal_root(&mut tree).kv_at(index).into_value_head();
                    let old_value = core::mem::replace(head.into_value_mut(), value);
                    assert_eq!(old_value.id(), separators[index].1);
                    separators[index].1 = new_id;
                }
                8 => {
                    let root = tree.borrow();
                    let ForceResult::Internal(root) = root.force() else {
                        panic!("the root is internal");
                    };
                    let index = rng.below(len + 1);
                    match root.edge_at(index).left_kv() {
                        Ok(kv) => {
                            let child = kv.right_edge().descend();
                            assert_eq!(child.keys()[0].tracked_id(), children[index]);
                        }
                        Err(edge) => assert_eq!(edge.index(), 0),
                    }
                    match root.edge_at(index).right_kv() {
                        Ok(kv) => {
                            let child = kv.left_edge().descend();
                            assert_eq!(child.keys()[0].tracked_id(), children[index]);
                        }
                        Err(edge) => assert_eq!(edge.index(), len),
                    }
                }
                9 => {
                    let index = rng.below(len + 1);
                    let child = internal_root(&mut tree).edge_at(index).descend();
                    assert_eq!(child.height(), 0);
                    let ForceResult::Leaf(child) = child.force() else {
                        panic!("the children are leaves");
                    };
                    let mut kv = child.first_kv().expect("each child holds its name");
                    assert_eq!(kv.key().tracked_id(), children[index]);
                    drop(core::mem::replace(kv.value_mut(), ledger.track()));
                }
                _ => {}
            }
            let root = tree.borrow();
            let ForceResult::Internal(root) = root.force() else {
                panic!("the root is internal");
            };
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
        let (mut tree, first_name) = root_above_one_leaf(&pages, ledger);
        let mut separators = Vec::new();
        let mut children = std::vec![first_name];
        while let EdgeRoom::Fits(edge) = internal_root(&mut tree).last_edge().check_room() {
            let (child, name, head_id) = child_leaf(&pages, ledger);
            let key = K::from_tracked(ledger.track());
            separators.push((key.tracked_id(), head_id));
            children.push(name);
            edge.insert_fit(InlinedKey::new(key), child);
        }
        let (child, name, head_id) = child_leaf(&pages, ledger);
        let key = K::from_tracked(ledger.track());
        separators.insert(position, (key.tracked_id(), head_id));
        children.insert(position + 1, name);

        let edge = internal_root(&mut tree).edge_at(position);
        let Insertion::Split(Split {
            left,
            separator,
            right,
        }) = edge.insert(InlinedKey::new(key), child, || pages.fresh())
        else {
            panic!("a full node splits");
        };
        let separator_value = right.borrow().first_edge().descend().leftmost_head_id();
        let mut halves = left.reborrow().tracked_ids();
        halves.push((
            separator.key().tracked_id(),
            separator_value.expect("the right half's leftmost leaf has a head"),
        ));
        halves.extend(right.borrow().tracked_ids());
        assert_eq!(halves, separators);
        let mut halves_children = child_names(left.reborrow());
        halves_children.extend(child_names(right.borrow()));
        assert_eq!(halves_children, children);
        assert!(left.len().abs_diff(right.borrow().len()) <= 2);
        assert_eq!((left.height(), right.height()), (1, 1));
        left.reborrow().assert_inline_keys_match();
        right.borrow().assert_inline_keys_match();
        drop(separator);
        right.destroy(|page| pages.release(page));
        tree.destroy(|page| pages.release(page));
        pages.assert_all_released();
    }

    #[test]
    fn split_and_insert_at_every_position() {
        fn every_position<'l, K: FromTracked<'l>, S: InlineKeyStrategy<K>>(ledger: &'l DropLedger) {
            for position in 0..=NodePtr::<K, Tracked<'l>, S>::capacity(false) {
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
    #[cfg_attr(
        miri,
        ignore = "the rejected child is leaked on purpose, which Miri reports"
    )]
    fn insert_rejects_a_child_of_the_wrong_height() {
        let ledger = DropLedger::new();
        let pages = TestPages::<Tracked<'_>, Tracked<'_>>::new();
        let (mut tree, first_name) = root_above_one_leaf(&pages, &ledger);
        let (mut too_tall, _, _) = child_leaf(&pages, &ledger);
        too_tall.push_internal_level(pages.fresh());

        let EdgeRoom::Fits(edge) = internal_root(&mut tree).first_edge().check_room() else {
            panic!("a root without keys has room");
        };
        let key = InlinedKey::new(ledger.track());
        let rejected = catch_unwind(AssertUnwindSafe(|| edge.insert_fit(key, too_tall)));
        assert!(rejected.is_err());
        let root = tree.borrow();
        let ForceResult::Internal(root) = root.force() else {
            panic!("the root is internal");
        };
        assert_eq!(root.len(), 0);
        assert_eq!(child_names(root), [first_name]);
        tree.destroy(|page| pages.release(page));
        assert_eq!(pages.in_use(), 2, "only the rejected child is leaked");
    }

    /// A separator over a leaf without a head is a broken tree: balancing it
    /// panics instead of moving an empty head slot.
    #[test]
    fn balancing_refuses_a_headless_right_leaf() {
        let ledger = DropLedger::new();
        let pages = TestPages::<Tracked<'_>, Tracked<'_>>::new();
        let (mut tree, _) = root_above_one_leaf(&pages, &ledger);
        let (headless, _) = named_leaf(&pages, &ledger);
        let EdgeRoom::Fits(edge) = internal_root(&mut tree).last_edge().check_room() else {
            panic!("a root without keys has room");
        };
        edge.insert_fit(InlinedKey::new(ledger.track()), headless.forget_type());
        let separator = internal_root(&mut tree).kv_at(0);
        let context = catch_unwind(AssertUnwindSafe(|| separator.into_balancing_context()));
        assert!(context.is_err());
        tree.destroy(|page| pages.release(page));
        pages.assert_all_released();
        ledger.assert_all_dropped_once();
    }

    #[test]
    fn push_and_pop_levels() {
        let ledger = DropLedger::new();
        let pages = TestPages::<Tracked<'_>, Tracked<'_>>::new();
        let mut tree = Subtree::new_leaf(pages.fresh()).forget_type();
        for _ in 0..3 {
            let ForceResult::Leaf(leaf) = tree.borrow_mut().force() else {
                panic!("the root is a leaf");
            };
            let EdgeRoom::Fits(edge) = leaf.last_edge().check_room() else {
                panic!("the leaf has room");
            };
            edge.insert_fit(InlinedKey::new(ledger.track()), ledger.track());
        }
        let ids = tree.borrow().subtree_tracked_ids();

        for height in 1..=3 {
            let root = tree.push_internal_level(pages.fresh());
            assert_eq!((root.len(), root.height()), (0, height));
            assert_eq!(tree.height(), height);
        }
        let mut node = tree.borrow();
        while let ForceResult::Internal(internal) = node.force() {
            assert_eq!(internal.len(), 0);
            node = internal.first_edge().descend();
        }
        assert_eq!(node.height(), 0);
        assert_eq!(node.subtree_tracked_ids(), ids);

        for height in (0..3).rev() {
            pages.release(tree.pop_internal_level());
            assert_eq!(tree.height(), height);
        }
        let popped_leaf = catch_unwind(AssertUnwindSafe(|| tree.pop_internal_level()));
        assert!(popped_leaf.is_err(), "a leaf root has no level to pop");
        assert_eq!(tree.borrow().subtree_tracked_ids(), ids);
        tree.destroy(|page| pages.release(page));
        pages.assert_all_released();
        ledger.assert_all_dropped_once();
    }

    /// Bulky keys: 12 entries in either kind of node.
    type BulkyTree<'p, 'l> = TestTree<'p, Bulky<'l>, Tracked<'l>>;

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

    /// The child of the root at `index`, which must be a leaf.
    fn leaf_child<'t, K, V, S: InlineKeyStrategy<K>>(
        tree: &'t mut TestTree<'_, K, V, S>,
        index: usize,
    ) -> NodeMut<'t, K, V, S, Leaf> {
        let ForceResult::Leaf(leaf) = internal_root(tree.root_mut())
            .edge_at(index)
            .descend()
            .force()
        else {
            panic!("the root's children are leaves");
        };
        leaf
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

        let context = internal_root(tree.root_mut())
            .kv_at(0)
            .into_balancing_context();
        assert_eq!(
            (context.left_child_len(), context.right_child_len()),
            (6, 6)
        );
        let Err(mut context) = context.can_merge() else {
            panic!("6 + 1 + 6 entries don't fit in one leaf");
        };
        assert_eq!(context.steal_left(), Ok(()));
        assert_eq!(
            (context.left_child_len(), context.right_child_len()),
            (5, 7)
        );
        assert_eq!(context.steal_right(), Ok(()));
        assert_eq!(context.steal_right(), Ok(()));
        assert_eq!(
            (context.left_child_len(), context.right_child_len()),
            (7, 5)
        );
        assert_eq!(context.into_parent().key().tracked_id(), model[7].0);
        assert_eq!(entry_ids(&tree), model);
        assert_eq!(leaf_child(&mut tree, 0).reborrow().head_id(), None);
        assert_eq!(
            leaf_child(&mut tree, 1).reborrow().head_id(),
            Some(model[7].1)
        );

        let ((key, value), _) = leaf_child(&mut tree, 0).kv_at(3).remove();
        assert_eq!((key.tracked_id(), value.id()), model.remove(3));
        drop((key, value));
        let context = internal_root(tree.root_mut())
            .kv_at(0)
            .into_balancing_context();
        let Ok(mergeable) = context.can_merge() else {
            panic!("6 + 1 + 5 entries fit in one leaf");
        };
        let (merged, page) = mergeable.merge();
        pages.release(page);
        assert_eq!(merged.index(), 0);
        let root = merged.into_node();
        assert_eq!(root.len(), 0);
        let ForceResult::Leaf(merged_leaf) = root.first_edge().descend().force() else {
            panic!("the child is a leaf");
        };
        assert_eq!(merged_leaf.len(), 12);
        assert!(
            merged_leaf.head().is_none(),
            "the leftmost leaf has no head"
        );
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
        let mut tree = ascending_tree(&pages, &ledger, 19);
        assert_eq!(tree.height(), 1);
        let model = entry_ids(&tree);

        let mut context = internal_root(tree.root_mut())
            .kv_at(0)
            .into_balancing_context();
        assert_eq!(
            (context.left_child_len(), context.right_child_len()),
            (6, 12)
        );
        assert_eq!(context.steal_left(), Err(StealError::ReceiverFull));
        assert_eq!(
            (context.left_child_len(), context.right_child_len()),
            (6, 12)
        );
        let left = context.into_left_child();
        let ForceResult::Leaf(left) = left.force() else {
            panic!("the children are leaves");
        };
        let mut left = left;
        while let Some(kv) = left.reborrow_mut().first_kv() {
            kv.remove();
        }

        let mut context = internal_root(tree.root_mut())
            .kv_at(0)
            .into_balancing_context();
        assert_eq!(context.steal_left(), Err(StealError::DonorEmpty));
        assert_eq!(context.steal_right(), Ok(()));
        assert_eq!(
            (context.left_child_len(), context.right_child_len()),
            (1, 11)
        );
        let right = context.into_right_child();
        let ForceResult::Leaf(mut right) = right.force() else {
            panic!("the children are leaves");
        };
        while let Some(kv) = right.reborrow_mut().last_kv() {
            kv.remove();
        }
        let mut context = internal_root(tree.root_mut())
            .kv_at(0)
            .into_balancing_context();
        assert_eq!(context.steal_right(), Err(StealError::DonorEmpty));
        assert_eq!(entry_ids(&tree), [model[6], model[7]]);

        tree.destroy();
        pages.assert_all_released();
        ledger.assert_all_dropped_once();
    }

    #[test]
    fn steals_and_merges_of_internal_nodes() {
        let ledger = DropLedger::new();
        let pages = TestPages::new();
        let mut tree = ascending_tree(&pages, &ledger, 100);
        assert_eq!(tree.height(), 2);
        let mut model = entry_ids(&tree);

        let mut context = internal_root(tree.root_mut())
            .kv_at(0)
            .into_balancing_context();
        let (left_len, right_len) = (context.left_child_len(), context.right_child_len());
        assert_eq!(context.steal_left(), Ok(()));
        assert_eq!(context.steal_right(), Ok(()));
        assert_eq!(context.steal_right(), Ok(()));
        assert_eq!(
            (context.left_child_len(), context.right_child_len()),
            (left_len + 1, right_len - 1)
        );
        assert_eq!(context.into_left_child().height(), 1);
        assert_eq!(entry_ids(&tree), model);

        // Shrink the left child until both children fit in one node.
        loop {
            let context = internal_root(tree.root_mut())
                .kv_at(0)
                .into_balancing_context();
            let capacity = NodePtr::<Bulky<'_>, Tracked<'_>, Auto>::capacity(false);
            if context.left_child_len() + 1 + context.right_child_len() <= capacity {
                break;
            }
            let ForceResult::Internal(left) = context.into_left_child().force() else {
                panic!("the children are internal");
            };
            let (key, grandchild) = left.kv_at(0).remove_with_right_child();
            let value_id = grandchild.borrow().leftmost_head_id();
            let mut removed = std::vec![(key.tracked_id(), value_id.expect("a head"))];
            removed.extend(grandchild.borrow().subtree_tracked_ids());
            model.retain(|entry| !removed.contains(entry));
            drop(key);
            grandchild.destroy(|page| pages.release(page));
        }
        let context = internal_root(tree.root_mut())
            .kv_at(0)
            .into_balancing_context();
        let Ok(mergeable) = context.can_merge() else {
            panic!("the children fit in one node");
        };
        let (merged, page) = mergeable.merge();
        pages.release(page);
        assert_eq!(merged.descend().height(), 1);
        tree.pop_empty_roots();
        assert_eq!(entry_ids(&tree), model);

        tree.destroy();
        pages.assert_all_released();
        ledger.assert_all_dropped_once();
    }

    /// A random key/value of a random internal node, if the walk down meets
    /// one with keys.
    fn random_internal_kv<'t, K, V, S: InlineKeyStrategy<K>>(
        mut node: NodeMut<'t, K, V, S, LeafOrInternal>,
        rng: &TestRng,
    ) -> Option<Kv<NodeMut<'t, K, V, S, Internal>>> {
        loop {
            let ForceResult::Internal(internal) = node.force() else {
                return None;
            };
            let len = internal.len();
            if len > 0 && (internal.height() == 1 || rng.below(2) == 0) {
                return Some(internal.kv_at(rng.below(len)));
            }
            node = internal.edge_at(rng.below(len + 1)).descend();
        }
    }

    /// Removes a random entry of a random leaf, if the leaf has one.
    fn remove_random_leaf_entry<K, V, S: InlineKeyStrategy<K>>(
        mut node: NodeMut<'_, K, V, S, LeafOrInternal>,
        rng: &TestRng,
    ) -> Option<(K, V)> {
        loop {
            match node.force() {
                ForceResult::Leaf(leaf) => {
                    let len = leaf.len();
                    return (len > 0).then(|| leaf.kv_at(rng.below(len)).remove().0);
                }
                ForceResult::Internal(internal) => {
                    let len = internal.len();
                    node = internal.edge_at(rng.below(len + 1)).descend();
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
                if let Some((key, value)) =
                    remove_random_leaf_entry(tree.root_mut().borrow_mut(), &rng)
                {
                    model.retain(|&entry| entry != (key.tracked_id(), value.id()));
                }
            } else if let Some(kv) = random_internal_kv(tree.root_mut().borrow_mut(), &rng) {
                let children_are_leaves = kv.reborrow().into_node().height() == 1;
                let children_capacity = NodePtr::<K, Tracked<'l>, S>::capacity(children_are_leaves);
                let mut context = match kv.into_balancing_context().can_merge() {
                    Ok(mergeable) if rng.below(2) == 0 => {
                        let (_, page) = mergeable.merge();
                        pages.release(page);
                        merges += 1;
                        continue;
                    }
                    Ok(mergeable) => mergeable.into_context(),
                    Err(context) => context,
                };
                let (left_len, right_len) = (context.left_child_len(), context.right_child_len());
                let (stolen, donor_len, receiver_len) = if rng.below(2) == 0 {
                    (context.steal_left(), left_len, right_len)
                } else {
                    (context.steal_right(), right_len, left_len)
                };
                match stolen {
                    Ok(()) => steals += 1,
                    Err(StealError::DonorEmpty) => {
                        assert_eq!(donor_len, 0);
                        refusals += 1;
                    }
                    Err(StealError::ReceiverFull) => {
                        assert_eq!(receiver_len, children_capacity);
                        refusals += 1;
                    }
                }
            }
            tree.pop_empty_roots();
            assert_eq!(entry_ids(&tree), model);
            tree.root().borrow().assert_subtree_inline_keys_match();
        }
        assert!(
            merges > 0 && steals > 0,
            "{merges} merges, {steals} steals, {refusals} refusals"
        );

        tree.destroy();
        pages.assert_all_released();
    }

    #[test]
    fn random_balancing_keeps_every_entry_in_order() {
        let ledger = DropLedger::new();
        random_balancing::<Bulky<'_>, Auto>(&ledger, 5);
        random_balancing::<Tracked<'_>, WithInlineKey>(&ledger, 6);
        ledger.assert_all_dropped_once();
    }
}
