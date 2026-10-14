//! Searches that need no ancestor path: exact-key lookups ([`Node::find`])
//! and an insert's ordered search ([`Node::search_for_insert`]), and the
//! values of the keys they find.
//!
//! A separator's value is the head of the leftmost leaf right of it (D17),
//! and with inline keys so is its key (D49): reading or changing what a
//! search found at a separator goes down its right edge.

use core::borrow::Borrow;

use super::node::Node;
use super::node::inline_keys::SearchKey;
use super::node::internal::Internal;
use super::node::leaf::Leaf;
use super::path::{FirstEdge, LastEdge};
use super::root::Tree;
use crate::inline_key::InlineKeyStrategy;

/// Where a search found a key: in a leaf, beside its value, or a separator
/// in an internal node, whose value is a head below (D17).
///
/// Invariant: the index is that of a key of the node, below its length, as
/// only a search makes one.
pub(crate) enum Found<K, V, S> {
    InLeaf(Leaf<K, V, S>, usize),
    Separator(Internal<K, V, S>, usize),
}

/// What a search for an insertion finds: the existing key, or the leaf and
/// the edge of it where the key belongs.
pub(in crate::tree) type InsertSearch<K, V, S> = Result<Found<K, V, S>, (Leaf<K, V, S>, usize)>;

impl<K, V, S: InlineKeyStrategy<K>> Found<K, V, S> {
    /// The key and its value, for as long as the caller borrows the tree.
    #[inline(always)]
    pub(in crate::tree) fn key_value<'a>(self) -> (&'a K, &'a V) {
        match self {
            // SAFETY: the index is below the leaf's length (type invariant).
            Self::InLeaf(leaf, index) => unsafe { leaf.key_value_unchecked(index) },
            Self::Separator(internal, index) => Self::separator_key_value(internal, index),
        }
    }

    /// The value, exclusive, for as long as the caller borrows the tree
    /// exclusively.
    #[inline(always)]
    pub(in crate::tree) fn value_mut<'a>(self) -> &'a mut V {
        match self {
            // SAFETY: the index is below the leaf's length (type invariant).
            Self::InLeaf(leaf, index) => unsafe { leaf.value_mut_unchecked(index) },
            Self::Separator(internal, index) => Self::separator_value_mut(internal, index),
        }
    }

    /// [`Self::key_value`] of separator `index` of `internal`: its key, and
    /// its value, the head of the leftmost leaf right of it (D17).
    ///
    /// Out of line: a search ends at a separator rarely, and the walk down
    /// to its head would otherwise sit in every descent's loop.
    #[inline(never)]
    fn separator_key_value<'a>(internal: Internal<K, V, S>, index: usize) -> (&'a K, &'a V) {
        internal.separator_key_value(index)
    }

    /// [`Self::value_mut`] of separator `index` of `internal`; out of line
    /// as [`Self::separator_key_value`] is.
    #[inline(never)]
    fn separator_value_mut<'a>(internal: Internal<K, V, S>, index: usize) -> &'a mut V {
        internal.head_leaf(index).head_value_mut()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Node<K, V, S> {
    /// Searches downward from this node for the entry whose key equals `key`,
    /// retaining only the current node. Updates that need ancestors and range
    /// endpoints use `Path::search`; an exact-key lookup only needs the
    /// position where it ends.
    ///
    /// It and [`Self::search_for_insert`] each keep their own descent: one
    /// shared, returning where it stopped as a `ControlFlow`, hid the leaf's
    /// kind from the value read after it, which then cleared the pointer's
    /// tag, an instruction more per hit.
    pub(in crate::tree) fn find<Q>(self, key: &SearchKey<'_, K, S, Q>) -> Option<Found<K, V, S>>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let leaf = match self {
            Node::Leaf(leaf) => leaf,
            Node::Internal(mut internal) => loop {
                match internal.search(key) {
                    Ok(index) => return Some(Found::Separator(internal, index)),
                    // SAFETY: a search returns an edge `<= len`.
                    Err(edge) => match unsafe { internal.child_unchecked(edge) } {
                        Node::Leaf(leaf) => break leaf,
                        Node::Internal(child) => internal = child,
                    },
                }
            },
        };
        leaf.find(key).map(|index| Found::InLeaf(leaf, index))
    }

    /// Searches in key order, returning an existing key or the leaf and edge
    /// where it belongs. Unlike [`Self::find`]'s equality filter, the leaf
    /// search must establish where an absent key belongs. A fitting
    /// insertion needs no ancestors; a full leaf can be searched again with
    /// a path for splitting.
    pub(crate) fn search_for_insert<Q>(self, key: &SearchKey<'_, K, S, Q>) -> InsertSearch<K, V, S>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let leaf = match self {
            Node::Leaf(leaf) => leaf,
            Node::Internal(mut internal) => loop {
                match internal.search(key) {
                    Ok(index) => return Ok(Found::Separator(internal, index)),
                    // SAFETY: a search returns an edge `<= len`.
                    Err(edge) => match unsafe { internal.child_unchecked(edge) } {
                        Node::Leaf(leaf) => break leaf,
                        Node::Internal(child) => internal = child,
                    },
                }
            },
        };
        match leaf.search_always_inlined(key) {
            Ok(index) => Ok(Found::InLeaf(leaf, index)),
            Err(edge) => Err((leaf, edge)),
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Tree<K, V, S> {
    /// The first entry, the first key of the leftmost leaf, if the tree has
    /// one.
    pub(crate) fn first_key_value(&self) -> Option<(&K, &V)> {
        let leaf = self.root_page()?.node().border_leaf::<FirstEdge>();
        (leaf.len() > 0).then(|| leaf.key_value(0))
    }

    /// The last entry, the last key of the rightmost leaf, if the tree has
    /// one.
    pub(crate) fn last_key_value(&self) -> Option<(&K, &V)> {
        let leaf = self.root_page()?.node().border_leaf::<LastEdge>();
        Some(leaf.key_value(leaf.len().checked_sub(1)?))
    }

    /// The entry whose key equals `key`, if the tree holds one. A key that
    /// lacks what every key of the tree shares (D50) is in none of its
    /// nodes, and needs no search.
    pub(crate) fn find<Q>(&self, key: &Q) -> Option<(&K, &V)>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let root_page = self.root_page()?;
        let key = SearchKey::new(key);
        if root_page.shared_prefix().place_outside(&key).is_some() {
            return None;
        }
        root_page.node().find(&key).map(Found::key_value)
    }

    /// The value of the entry whose key equals `key`, exclusive; see
    /// [`Self::find`].
    pub(crate) fn find_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let root_page = self.root_page_mut()?;
        let key = SearchKey::new(key);
        if root_page.shared_prefix().place_outside(&key).is_some() {
            return None;
        }
        root_page.node().find(&key).map(Found::value_mut)
    }
}

#[cfg(test)]
mod tests {
    use core::panic::AssertUnwindSafe;
    use std::collections::BTreeMap;
    use std::panic::catch_unwind;
    use std::vec::Vec;

    use super::Found;
    use crate::tree::node::Node;
    use crate::tree::node::inline_keys::{SearchKey, Separator, WindowedKey};
    use crate::tree::node::internal::Internal;
    use crate::tree::node::leaf::Leaf;
    use crate::tree::node::page::TestPages;
    use crate::tree::root::Tree;
    use crate::tree::test_support::{DropLedger, TestRng, TestTree, Tracked};

    /// 64-byte keys: with `Tracked` values, 1 KB nodes hold 12 entries in a
    /// leaf and 14 in an internal node, so trees grow tall from few entries.
    type WideKey = [u64; 8];

    /// Every separator under and at `node`, at any depth, as its node and
    /// index, and the number of leaves.
    fn separators_and_leaves<K, V>(
        node: Node<K, V, crate::inline_key::Auto>,
        separators: &mut Vec<(Internal<K, V, crate::inline_key::Auto>, usize)>,
    ) -> usize {
        match node {
            Node::Leaf(_) => 1,
            Node::Internal(internal) => {
                let mut leaves = 0;
                for index in 0..=internal.len() {
                    leaves += separators_and_leaves(internal.child(index), separators);
                    if index < internal.len() {
                        separators.push((internal, index));
                    }
                }
                leaves
            }
        }
    }

    /// The `(key, value id)` of every separator of `tree`, its value read
    /// through its head, and checks that there is one fewer than leaves.
    fn separator_values(tree: &TestTree<'_, WideKey, Tracked<'_>>) -> Vec<(WideKey, usize)> {
        let mut separators = Vec::new();
        let leaves = separators_and_leaves(tree.root(), &mut separators);
        assert_eq!(
            separators.len() + 1,
            leaves,
            "L leaves have L - 1 separators"
        );
        (separators.into_iter())
            .map(|(node, index)| {
                (
                    *node.separator_key(index),
                    node.head_leaf(index).head_value().id(),
                )
            })
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
        assert!(tree.leaf_depth() >= 2);

        let separators = separator_values(&tree);
        assert!(!separators.is_empty());
        for &(key, value_id) in &separators {
            assert_eq!(value_id, value_ids[&key]);
            let found = tree.root().find(&SearchKey::new(&key));
            let Some(found @ Found::Separator(..)) = found else {
                panic!("a separator is found where it lives, above the leaves");
            };
            let (found_key, value) = found.key_value();
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
    #[cfg_attr(panic = "abort", ignore = "needs unwinding")]
    fn a_separator_over_a_headless_leaf_panics() {
        let ledger = DropLedger::new();
        let pages = TestPages::<Tracked<'_>, Tracked<'_>>::new();
        let mut tree = Tree::new(pages.fresh());
        let root = tree.raise_in(&pages);
        let headless = Leaf::new(pages.fresh_leaf());
        root.insert_fit(
            0,
            Separator::with_key(WindowedKey::new(ledger.track(), 0)),
            Node::Leaf(headless),
        );
        let found = Found::Separator(root, 0);
        assert!(catch_unwind(AssertUnwindSafe(|| found.key_value().1.id())).is_err());
        // Taking the tree apart needs the head the separator's value is.
        root.leaf_child(1).put_head(None, ledger.track());
        tree.destroy(|page| pages.release(page));
        pages.assert_all_released();
        ledger.assert_all_dropped_once();
    }
}
