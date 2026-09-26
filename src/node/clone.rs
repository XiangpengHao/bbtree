//! Copying an owned tree: [`Subtree::clone_tree`], node by node, so that the
//! copy has the same shape, as std's `BTreeMap::clone` does. It calls no
//! `Ord` and no `InlineKey`: every inline key is copied (D18).

#![forbid(unsafe_code)]

use super::handle::{EdgeRoom, ForceResult, Immut, LeafOrInternal, NodeKind, NodeRef, Subtree};
use super::inline_keys::InlinedKey;
use super::page::{FreshPage, PageByKind};
use crate::inline_key::InlineKeyStrategy;

impl<K: Clone, V: Clone, S: InlineKeyStrategy<K>> Subtree<K, V, S> {
    /// A copy of the tree, with every key and value cloned in key order,
    /// each separator's value right after its key (D17), on pages from the
    /// global allocator.
    ///
    /// If a `clone` panics, what was copied so far is dropped, and its pages
    /// released, before the panic goes on: nothing leaks, and the tree is
    /// unchanged.
    pub(crate) fn clone_tree(&self) -> Self {
        Self::clone_subtree(self.borrow())
    }

    fn clone_subtree(node: NodeRef<Immut<'_>, K, V, S, LeafOrInternal>) -> Self {
        match node.force() {
            ForceResult::Leaf(leaf) => {
                let mut copy = PartialCopy::new(Subtree::new_leaf(FreshPage::allocate()));
                let ForceResult::Leaf(mut copy_leaf) = copy.tree_mut().borrow_mut().force() else {
                    unreachable!("a new leaf is a leaf");
                };
                if let Some(head) = leaf.head() {
                    copy_leaf.replace_head(head.into_value().clone());
                }
                let inline_keys = leaf.inline_keys();
                for index in 0..leaf.len() {
                    let (key, value) = leaf.kv_at(index).into_key_value();
                    let inline_key = inline_keys.map_or(0, |inline_keys| inline_keys[index]);
                    let key = InlinedKey::from_parts(key.clone(), inline_key);
                    let value = value.clone();
                    let EdgeRoom::Fits(edge) = copy_leaf.reborrow_mut().last_edge().check_room()
                    else {
                        unreachable!("a leaf's entries fit in a leaf of its kind");
                    };
                    edge.insert_fit(key, value);
                }
                copy.finish()
            }
            ForceResult::Internal(internal) => {
                let first_child = Self::clone_subtree(internal.first_edge().descend());
                let mut copy = PartialCopy::new(first_child);
                copy.tree_mut().push_internal_level(FreshPage::allocate());
                let inline_keys = internal.inline_keys();
                for index in 0..internal.len() {
                    let separator = internal.kv_at(index);
                    let inline_key = inline_keys.map_or(0, |inline_keys| inline_keys[index]);
                    let key = InlinedKey::from_parts(separator.into_key().clone(), inline_key);
                    let child = Self::clone_subtree(separator.right_edge().descend());
                    let ForceResult::Internal(copy_root) = copy.tree_mut().borrow_mut().force()
                    else {
                        unreachable!("a pushed level is internal");
                    };
                    let EdgeRoom::Fits(edge) = copy_root.last_edge().check_room() else {
                        unreachable!("a node's separators fit in a node of its kind");
                    };
                    edge.insert_fit(key, child);
                }
                copy.finish()
            }
        }
    }
}

/// A copy being made: taken apart if a `clone` panics, so that nothing
/// cloned so far leaks.
struct PartialCopy<K, V, S: InlineKeyStrategy<K>> {
    /// `None` once finished.
    tree: Option<Subtree<K, V, S>>,
}

impl<K, V, S: InlineKeyStrategy<K>> PartialCopy<K, V, S> {
    fn new<Kind: NodeKind>(tree: Subtree<K, V, S, Kind>) -> Self {
        Self {
            tree: Some(tree.forget_type()),
        }
    }

    fn tree_mut(&mut self) -> &mut Subtree<K, V, S> {
        self.tree
            .as_mut()
            .expect("a copy is unfinished until `finish`")
    }

    fn finish(mut self) -> Subtree<K, V, S> {
        self.tree
            .take()
            .expect("a copy is unfinished until `finish`")
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Drop for PartialCopy<K, V, S> {
    fn drop(&mut self) {
        if let Some(tree) = self.tree.take() {
            tree.destroy(PageByKind::deallocate);
        }
    }
}
