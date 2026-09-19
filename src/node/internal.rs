//! Internal nodes as the tree uses them: insert a child's separator and
//! right half, splitting a full node first. Also the tests of the
//! internal-node operations, levels and balancing in `handle.rs`.

#![forbid(unsafe_code)]

use super::handle::{Edge, EdgeRoom, Insertion, Internal, NodeMut, Subtree};
use super::page::FreshPage;

impl<'a, K, V> Edge<NodeMut<'a, K, V, Internal>> {
    /// Inserts `key`/`value` at this edge with `right_child` as the edge to
    /// its right, splitting the node first if it is full, with the right half
    /// in a page from `new_page`. This is how a child's split lands in its
    /// parent.
    ///
    /// # Panics
    ///
    /// If `right_child` is not exactly one level below this node; the node is
    /// unchanged then.
    pub(crate) fn insert(
        self,
        key: K,
        value: V,
        right_child: Subtree<K, V>,
        new_page: impl FnOnce() -> FreshPage<K, V>,
    ) -> Insertion<'a, K, V, Internal> {
        match self.check_room() {
            EdgeRoom::Fits(edge) => Insertion::Fit(edge.insert_fit(key, value, right_child)),
            EdgeRoom::Full(edge) => {
                Insertion::Split(edge.split_and_insert(new_page(), key, value, right_child))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use core::panic::AssertUnwindSafe;
    use std::panic::catch_unwind;
    use std::vec::Vec;

    use crate::node::handle::{
        EdgeRoom, ForceResult, Immut, Insertion, Internal, Kv, Leaf, LeafOrInternal, NodeMut,
        NodeRef, Split, StealError, Subtree,
    };
    use crate::node::page::TestPages;
    use crate::node::test_support::{Bulky, DropLedger, TestRng, TestTree, Tracked, TrackedValue};

    /// A leaf holding one tracked entry, named by the id of its key.
    fn child_leaf<'l, V: TrackedValue<'l>>(
        pages: &TestPages<Tracked<'l>, V>,
        ledger: &'l DropLedger,
    ) -> (Subtree<Tracked<'l>, V>, usize) {
        let mut leaf = Subtree::new_leaf(pages.fresh());
        let key = ledger.track();
        let id = key.id();
        let EdgeRoom::Fits(edge) = leaf.borrow_mut().first_edge().check_room() else {
            panic!("an empty leaf has room");
        };
        edge.insert_fit(key, V::from_tracked(ledger.track()));
        (leaf.forget_type(), id)
    }

    /// The names of the leaf children of an internal node, in order.
    fn child_ids<'l, V>(node: NodeRef<Immut<'_>, Tracked<'l>, V, Internal>) -> Vec<usize> {
        (0..=node.len())
            .map(|index| node.edge_at(index).descend().keys()[0].id())
            .collect()
    }

    /// The root of a tree that must be internal.
    fn internal_root<K, V>(tree: &mut Subtree<K, V>) -> NodeMut<'_, K, V, Internal> {
        match tree.borrow_mut().force() {
            ForceResult::Internal(root) => root,
            ForceResult::Leaf(_) => panic!("the root is a leaf"),
        }
    }

    /// A root with no keys above one named leaf, and the leaf's name.
    fn root_above_one_leaf<'l, V: TrackedValue<'l>>(
        pages: &TestPages<Tracked<'l>, V>,
        ledger: &'l DropLedger,
    ) -> (Subtree<Tracked<'l>, V>, usize) {
        let (mut tree, id) = child_leaf(pages, ledger);
        tree.push_internal_level(pages.fresh());
        (tree, id)
    }

    /// Random operations on one internal node with leaf children, checked
    /// against a model of its entries and of its children's names.
    fn random_operations<'l, V: TrackedValue<'l>>(ledger: &'l DropLedger, steps: usize) {
        let pages = TestPages::<Tracked<'l>, V>::new();
        let rng = TestRng::new(2);
        let (mut tree, first_id) = root_above_one_leaf(&pages, ledger);
        let mut entries: Vec<(usize, usize)> = Vec::new();
        let mut children = std::vec![first_id];

        for _ in 0..steps {
            let len = entries.len();
            match rng.below(10) {
                0..=3 => {
                    let index = rng.below(len + 1);
                    let edge = internal_root(&mut tree).edge_at(index);
                    if let EdgeRoom::Fits(edge) = edge.check_room() {
                        let (child, child_id) = child_leaf(&pages, ledger);
                        let (key, value) = (ledger.track(), V::from_tracked(ledger.track()));
                        entries.insert(index, (key.id(), value.tracked_id()));
                        children.insert(index + 1, child_id);
                        assert_eq!(edge.insert_fit(key, value, child).index(), index);
                    }
                }
                4 | 5 if len > 0 => {
                    let index = rng.below(len);
                    let ((key, value), child) = internal_root(&mut tree)
                        .kv_at(index)
                        .remove_with_right_child();
                    assert_eq!((key.id(), value.tracked_id()), entries.remove(index));
                    assert_eq!(child.height(), 0);
                    assert_eq!(child.borrow().keys()[0].id(), children.remove(index + 1));
                    child.destroy(|page| pages.release(page));
                }
                6 if len > 0 => {
                    let index = rng.below(len);
                    let (key, value) = (ledger.track(), V::from_tracked(ledger.track()));
                    let new_ids = (key.id(), value.tracked_id());
                    let (old_key, old_value) = internal_root(&mut tree)
                        .kv_at(index)
                        .replace_key_value(key, value);
                    assert_eq!((old_key.id(), old_value.tracked_id()), entries[index]);
                    entries[index] = new_ids;
                }
                7 => {
                    let index = rng.below(len + 1);
                    let child = internal_root(&mut tree).edge_at(index).descend();
                    assert_eq!(child.height(), 0);
                    let ForceResult::Leaf(child) = child.force() else {
                        panic!("the children are leaves");
                    };
                    let mut kv = child.first_kv().expect("each child holds its name");
                    assert_eq!(kv.key().id(), children[index]);
                    let value = V::from_tracked(ledger.track());
                    drop(core::mem::replace(kv.value_mut(), value));
                }
                8 => {
                    let root = tree.borrow();
                    let ForceResult::Internal(root) = root.force() else {
                        panic!("the root is internal");
                    };
                    let index = rng.below(len + 1);
                    match root.edge_at(index).left_kv() {
                        Ok(kv) => {
                            assert_eq!(kv.right_edge().descend().keys()[0].id(), children[index])
                        }
                        Err(edge) => assert_eq!(edge.index(), 0),
                    }
                    match root.edge_at(index).right_kv() {
                        Ok(kv) => {
                            assert_eq!(kv.left_edge().descend().keys()[0].id(), children[index])
                        }
                        Err(edge) => assert_eq!(edge.index(), len),
                    }
                }
                _ => {}
            }
            let root = tree.borrow();
            let ForceResult::Internal(root) = root.force() else {
                panic!("the root is internal");
            };
            assert_eq!(root.tracked_ids(), entries);
            assert_eq!(child_ids(root), children);
        }
        tree.destroy(|page| pages.release(page));
        pages.assert_all_released();
    }

    #[test]
    fn random_operations_match_a_vec_model() {
        let ledger = DropLedger::new();
        let steps = if cfg!(miri) { 200 } else { 10_000 };
        random_operations::<Bulky<'_>>(&ledger, steps);
        random_operations::<Tracked<'_>>(&ledger, steps);
        ledger.assert_all_dropped_once();
    }

    /// Builds a full internal node with named leaf children, inserts one more
    /// entry and child at `position`, and checks the split against the model.
    fn split_at<'l, V: TrackedValue<'l>>(ledger: &'l DropLedger, position: usize) {
        let pages = TestPages::<Tracked<'l>, V>::new();
        let (mut tree, first_id) = root_above_one_leaf(&pages, ledger);
        let mut entries = Vec::new();
        let mut children = std::vec![first_id];
        while let EdgeRoom::Fits(edge) = internal_root(&mut tree).last_edge().check_room() {
            let (child, child_id) = child_leaf(&pages, ledger);
            let (key, value) = (ledger.track(), V::from_tracked(ledger.track()));
            entries.push((key.id(), value.tracked_id()));
            children.push(child_id);
            edge.insert_fit(key, value, child);
        }
        let (child, child_id) = child_leaf(&pages, ledger);
        let (key, value) = (ledger.track(), V::from_tracked(ledger.track()));
        entries.insert(position, (key.id(), value.tracked_id()));
        children.insert(position + 1, child_id);

        let edge = internal_root(&mut tree).edge_at(position);
        let Insertion::Split(Split {
            left,
            separator,
            right,
        }) = edge.insert(key, value, child, || pages.fresh())
        else {
            panic!("a full node splits");
        };
        let mut halves = left.reborrow().tracked_ids();
        halves.push((separator.0.id(), separator.1.tracked_id()));
        halves.extend(right.borrow().tracked_ids());
        assert_eq!(halves, entries);
        let mut halves_children = child_ids(left.reborrow());
        halves_children.extend(child_ids(right.borrow()));
        assert_eq!(halves_children, children);
        assert!(left.len().abs_diff(right.borrow().len()) <= 2);
        assert_eq!((left.height(), right.height()), (1, 1));
        drop(separator);
        right.destroy(|page| pages.release(page));
        tree.destroy(|page| pages.release(page));
        pages.assert_all_released();
    }

    #[test]
    fn split_and_insert_at_every_position() {
        let ledger = DropLedger::new();
        for position in 0..=12 {
            split_at::<Bulky<'_>>(&ledger, position);
        }
        if !cfg!(miri) {
            for position in 0..=72 {
                split_at::<Tracked<'_>>(&ledger, position);
            }
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
        let (mut tree, first_id) = root_above_one_leaf(&pages, &ledger);
        let (mut too_tall, _) = child_leaf(&pages, &ledger);
        too_tall.push_internal_level(pages.fresh());

        let EdgeRoom::Fits(edge) = internal_root(&mut tree).first_edge().check_room() else {
            panic!("a root without keys has room");
        };
        let (key, value) = (ledger.track(), ledger.track());
        let rejected = catch_unwind(AssertUnwindSafe(|| edge.insert_fit(key, value, too_tall)));
        assert!(rejected.is_err());
        let root = tree.borrow();
        let ForceResult::Internal(root) = root.force() else {
            panic!("the root is internal");
        };
        assert_eq!(root.len(), 0);
        assert_eq!(child_ids(root), [first_id]);
        tree.destroy(|page| pages.release(page));
        assert_eq!(pages.in_use(), 2, "only the rejected child is leaked");
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
            edge.insert_fit(ledger.track(), ledger.track());
        }
        let ids = tree.borrow().tracked_ids();

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
        assert_eq!(node.tracked_ids(), ids);

        for height in (0..3).rev() {
            let ForceResult::Internal(root) = tree.force() else {
                panic!("the root is internal");
            };
            let (only_child, page) = root.pop_internal_level();
            pages.release(page);
            tree = only_child;
            assert_eq!(tree.height(), height);
        }
        assert_eq!(tree.borrow().tracked_ids(), ids);
        tree.destroy(|page| pages.release(page));
        pages.assert_all_released();
        ledger.assert_all_dropped_once();
    }

    type BulkyTree<'p, 'l> = TestTree<'p, Tracked<'l>, Bulky<'l>>;

    /// A tree of `count` entries with ascending keys.
    fn ascending_tree<'p, 'l>(
        pages: &'p TestPages<Tracked<'l>, Bulky<'l>>,
        ledger: &'l DropLedger,
        count: usize,
    ) -> BulkyTree<'p, 'l> {
        let mut tree = TestTree::new(pages);
        for key in ledger.track_many(count) {
            tree.insert(key, Bulky::from_tracked(ledger.track()));
        }
        tree
    }

    fn entry_ids(tree: &BulkyTree<'_, '_>) -> Vec<(usize, usize)> {
        tree.entries(|key, value| (key.id(), value.tracked_id()))
    }

    /// The child of the root at `index`, which must be a leaf.
    fn leaf_child<'t, K, V>(
        tree: &'t mut TestTree<'_, K, V>,
        index: usize,
    ) -> NodeMut<'t, K, V, Leaf> {
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
        // 13 entries in 12-entry leaves: one split, two leaves of 6.
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
        assert_eq!(context.into_parent().key().id(), model[7].0);
        assert_eq!(entry_ids(&tree), model);

        let ((key, value), _) = leaf_child(&mut tree, 0).kv_at(3).remove();
        assert_eq!((key.id(), value.tracked_id()), model.remove(3));
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
            if context.left_child_len() + 1 + context.right_child_len() <= 12 {
                break;
            }
            let ForceResult::Internal(left) = context.into_left_child().force() else {
                panic!("the children are internal");
            };
            let ((key, value), grandchild) = left.kv_at(0).remove_with_right_child();
            let mut removed = std::vec![(key.id(), value.tracked_id())];
            removed.extend(grandchild.borrow().tracked_ids());
            model.retain(|entry| !removed.contains(entry));
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
    fn random_internal_kv<'t, K, V>(
        mut node: NodeMut<'t, K, V, LeafOrInternal>,
        rng: &TestRng,
    ) -> Option<Kv<NodeMut<'t, K, V, Internal>>> {
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
    fn remove_random_leaf_entry<K, V>(
        mut node: NodeMut<'_, K, V, LeafOrInternal>,
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

    #[test]
    fn random_balancing_keeps_every_entry_in_order() {
        let ledger = DropLedger::new();
        let pages = TestPages::new();
        let rng = TestRng::new(5);
        let count = if cfg!(miri) { 80 } else { 800 };
        let mut keys = ledger.track_many(count);
        rng.shuffle(&mut keys);
        let mut tree = TestTree::new(&pages);
        for key in keys {
            tree.insert(key, Bulky::from_tracked(ledger.track()));
        }
        let mut model = entry_ids(&tree);

        let steps = if cfg!(miri) { 100 } else { 4000 };
        let (mut merges, mut steals, mut refusals) = (0, 0, 0);
        for _ in 0..steps {
            if rng.below(3) == 0 {
                if let Some((key, value)) =
                    remove_random_leaf_entry(tree.root_mut().borrow_mut(), &rng)
                {
                    model.retain(|&entry| entry != (key.id(), value.tracked_id()));
                }
            } else if let Some(kv) = random_internal_kv(tree.root_mut().borrow_mut(), &rng) {
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
                        assert_eq!(receiver_len, 12);
                        refusals += 1;
                    }
                }
            }
            tree.pop_empty_roots();
            assert_eq!(entry_ids(&tree), model);
        }
        assert!(
            merges > 0 && steals > 0,
            "{merges} merges, {steals} steals, {refusals} refusals"
        );

        tree.destroy();
        pages.assert_all_released();
        ledger.assert_all_dropped_once();
    }
}
