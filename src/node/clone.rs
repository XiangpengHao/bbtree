//! Copying an owned tree: [`Subtree::clone_tree`], node by node, so that the
//! copy has the same shape, as std's `BTreeMap::clone` does. It calls no
//! `Ord` and no `InlineKey`: every inline key is copied (D18), and every
//! node's prefix length (05-fence-key.md).

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
                let mut copy_leaf = copy.tree_mut().borrow_mut();
                if let Some(head) = leaf.head() {
                    copy_leaf.replace_head(head.into_value().clone());
                }
                copy_leaf.set_empty_prefix_len(leaf.prefix_len());
                let mut edge = leaf.first_edge();
                while let Ok(kv) = edge.right_kv() {
                    let window = kv.window();
                    let (key, value) = kv.into_key_value();
                    let key = InlinedKey::from_parts(key.clone(), window);
                    let value = value.clone();
                    let EdgeRoom::Fits(copy_edge) =
                        copy_leaf.reborrow_mut().last_edge().check_room()
                    else {
                        unreachable!("a leaf's entries fit in a leaf of its kind");
                    };
                    copy_edge.insert_fit(key, value);
                    edge = kv.right_edge();
                }
                copy.finish().forget_type()
            }
            ForceResult::Internal(internal) => {
                let first_child = Self::clone_subtree(internal.first_edge().descend());
                let mut copy = PartialCopy::new(first_child);
                let mut copy_root = copy.tree_mut().push_internal_level(FreshPage::allocate());
                copy_root.set_empty_prefix_len(internal.prefix_len());
                let mut edge = internal.first_edge();
                while let Ok(separator) = edge.right_kv() {
                    let window = separator.window();
                    let key = InlinedKey::from_parts(separator.into_key().clone(), window);
                    let child = Self::clone_subtree(separator.right_edge().descend());
                    let EdgeRoom::Fits(copy_edge) =
                        copy_root.reborrow_mut().last_edge().check_room()
                    else {
                        unreachable!("a node's separators fit in a node of its kind");
                    };
                    copy_edge.insert_fit(key, child);
                    edge = separator.right_edge();
                }
                copy.finish()
            }
        }
    }
}

/// A copy being made: taken apart if a `clone` panics, so that nothing
/// cloned so far leaks.
struct PartialCopy<K, V, S: InlineKeyStrategy<K>, Kind: NodeKind> {
    /// `None` once finished.
    tree: Option<Subtree<K, V, S, Kind>>,
}

impl<K, V, S: InlineKeyStrategy<K>, Kind: NodeKind> PartialCopy<K, V, S, Kind> {
    fn new(tree: Subtree<K, V, S, Kind>) -> Self {
        Self { tree: Some(tree) }
    }

    fn tree_mut(&mut self) -> &mut Subtree<K, V, S, Kind> {
        self.tree
            .as_mut()
            .expect("a copy is unfinished until `finish`")
    }

    fn finish(mut self) -> Subtree<K, V, S, Kind> {
        self.tree
            .take()
            .expect("a copy is unfinished until `finish`")
    }
}

impl<K, V, S: InlineKeyStrategy<K>, Kind: NodeKind> Drop for PartialCopy<K, V, S, Kind> {
    fn drop(&mut self) {
        if let Some(tree) = self.tree.take() {
            tree.destroy(PageByKind::deallocate);
        }
    }
}
