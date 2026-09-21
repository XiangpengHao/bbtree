//! Separators and their values (D17): a separator is a key of an internal
//! node, and its value is the head of the leftmost leaf right of it. Reading
//! and changing the value of a key found anywhere in the tree.

#![forbid(unsafe_code)]

use super::handle::{
    ForceResult, Head, Immut, Internal, Kv, Leaf, LeafOrInternal, NodeMut, NodeRef,
};
use crate::inline_key::InlineKeyStrategy;

impl<BorrowType, K, V, S: InlineKeyStrategy<K>> Kv<NodeRef<BorrowType, K, V, S, Internal>> {
    /// The head that holds this separator's value: that of the leftmost leaf
    /// under the edge right of it, reached through first edges.
    ///
    /// # Panics
    ///
    /// If that leaf has no head, which never happens in a tree built by the
    /// node operations: every leaf but the tree's leftmost has one.
    pub(crate) fn into_value_head(self) -> Head<NodeRef<BorrowType, K, V, S, Leaf>> {
        let mut node = self.right_edge().descend();
        loop {
            match node.force() {
                ForceResult::Leaf(leaf) => {
                    return leaf
                        .head()
                        .expect("the leaf right of a separator has a head");
                }
                ForceResult::Internal(internal) => node = internal.first_edge().descend(),
            }
        }
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>> Kv<NodeRef<Immut<'a>, K, V, S, LeafOrInternal>> {
    /// The key and its value: side by side in a leaf, or for a separator, the
    /// key here and the value in a head below.
    pub(crate) fn into_key_value(self) -> (&'a K, &'a V) {
        match self.force() {
            ForceResult::Leaf(kv) => kv.into_key_value(),
            ForceResult::Internal(kv) => (kv.into_key(), kv.into_value_head().into_value()),
        }
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>> Kv<NodeMut<'a, K, V, S, LeafOrInternal>> {
    /// The value: beside the key in a leaf, or for a separator, in a head
    /// below.
    pub(crate) fn into_value_mut(self) -> &'a mut V {
        match self.force() {
            ForceResult::Leaf(kv) => kv.into_value_mut(),
            ForceResult::Internal(kv) => kv.into_value_head().into_value_mut(),
        }
    }
}

#[cfg(test)]
mod tests {
    use core::panic::AssertUnwindSafe;
    use std::collections::BTreeMap;
    use std::panic::catch_unwind;
    use std::vec::Vec;

    use crate::inline_key::Auto;
    use crate::node::handle::{
        EdgeRoom, ForceResult, Immut, Internal, Kv, LeafOrInternal, NodeRef, SearchResult, Subtree,
    };
    use crate::node::inline_keys::{InlinedKey, SearchKey};
    use crate::node::page::TestPages;
    use crate::node::test_support::{DropLedger, TestRng, TestTree, Tracked};

    /// 64-byte keys: with `Tracked` values, 1 KB nodes hold 12 entries in a
    /// leaf and 14 in an internal node, so trees grow tall from few entries.
    type WideKey = [u64; 8];

    /// Every separator of the subtree under `node`, at any height, and the
    /// number of its leaves.
    fn separators_and_leaves<'a, K, V>(
        node: NodeRef<Immut<'a>, K, V, Auto, LeafOrInternal>,
        separators: &mut Vec<Kv<NodeRef<Immut<'a>, K, V, Auto, Internal>>>,
    ) -> usize {
        match node.force() {
            ForceResult::Leaf(_) => 1,
            ForceResult::Internal(internal) => {
                let mut leaves = 0;
                for index in 0..=internal.len() {
                    leaves += separators_and_leaves(internal.edge_at(index).descend(), separators);
                    if index < internal.len() {
                        separators.push(internal.kv_at(index));
                    }
                }
                leaves
            }
        }
    }

    /// The key/value a search from `node` down finds for `key`, if any.
    fn search_tree<'a, K: Ord, V>(
        mut node: NodeRef<Immut<'a>, K, V, Auto, LeafOrInternal>,
        key: &K,
    ) -> Option<Kv<NodeRef<Immut<'a>, K, V, Auto, LeafOrInternal>>> {
        let key = SearchKey::new(key);
        loop {
            node = match node.search(&key) {
                SearchResult::Found(kv) => return Some(kv),
                SearchResult::GoDown(edge) => match edge.force() {
                    ForceResult::Leaf(_) => return None,
                    ForceResult::Internal(edge) => edge.descend(),
                },
            };
        }
    }

    /// The `(key, value id)` of every separator of `tree`, its value read
    /// through its head, and checks that there is one fewer than leaves.
    fn separator_values(tree: &TestTree<'_, WideKey, Tracked<'_>>) -> Vec<(WideKey, usize)> {
        let mut separators = Vec::new();
        let leaves = separators_and_leaves(tree.root().borrow(), &mut separators);
        assert_eq!(
            separators.len() + 1,
            leaves,
            "L leaves have L - 1 separators"
        );
        (separators.into_iter())
            .map(|kv| (*kv.into_key(), kv.into_value_head().into_value().id()))
            .collect()
    }

    /// Every separator of a tall tree finds its own value through its head;
    /// a search for its key stops at it and finds the same; and inserting
    /// its key again replaces the value in the head.
    #[test]
    fn separators_reach_their_values_through_heads() {
        let ledger = DropLedger::new();
        let pages = TestPages::<WideKey, Tracked<'_>>::new();
        let rng = TestRng::new(31);
        let count = if cfg!(miri) { 200 } else { 2000 };
        let mut numbers: Vec<u64> = (0..count).collect();
        rng.shuffle(&mut numbers);
        let mut tree = TestTree::new(&pages);
        let mut value_ids = BTreeMap::new();
        for number in numbers {
            let value = ledger.track();
            value_ids.insert([number; 8], value.id());
            assert!(tree.insert([number; 8], value).is_none());
        }
        assert!(tree.height() >= 2);

        let separators = separator_values(&tree);
        assert!(!separators.is_empty());
        for &(key, value_id) in &separators {
            assert_eq!(value_id, value_ids[&key]);
            let kv = search_tree(tree.root().borrow(), &key).expect("a separator is found");
            assert!(
                kv.reborrow().into_node().height() > 0,
                "found where it lives"
            );
            let (found_key, value) = kv.into_key_value();
            assert_eq!((*found_key, value.id()), (key, value_id));
        }

        for &(key, old_value_id) in &separators {
            let value = ledger.track();
            value_ids.insert(key, value.id());
            let old_value = tree.insert(key, value).expect("the key is there");
            assert_eq!(old_value.id(), old_value_id);
        }
        for (key, value_id) in separator_values(&tree) {
            assert_eq!(value_id, value_ids[&key]);
        }
        let entries = tree.entries(|key, value| (*key, value.id()));
        assert_eq!(entries, value_ids.into_iter().collect::<Vec<_>>());

        tree.destroy();
        pages.assert_all_released();
        ledger.assert_all_dropped_once();
    }

    /// A separator above a leaf without a head is a broken tree: looking up
    /// its value panics instead of reading an empty slot.
    #[test]
    fn a_separator_over_a_headless_leaf_panics() {
        let ledger = DropLedger::new();
        let pages = TestPages::<Tracked<'_>, Tracked<'_>>::new();
        let mut tree = Subtree::new_leaf(pages.fresh()).forget_type();
        let root = tree.push_internal_level(pages.fresh());
        let EdgeRoom::Fits(edge) = root.first_edge().check_room() else {
            panic!("a new root has room");
        };
        let headless = Subtree::new_leaf(pages.fresh()).forget_type();
        edge.insert_fit(InlinedKey::new(ledger.track()), headless);
        let ForceResult::Internal(root) = tree.borrow().force() else {
            panic!("the root is internal");
        };
        let separator = root.kv_at(0);
        assert!(catch_unwind(AssertUnwindSafe(|| separator.into_value_head())).is_err());
        tree.destroy(|page| pages.release(page));
        pages.assert_all_released();
        ledger.assert_all_dropped_once();
    }
}
