//! Copying an owned tree: [`Tree`]'s `Clone`, node by node, so that the
//! copy has the same shape, as std's `BTreeMap::clone` does. It calls no
//! `Ord` and no `InlineKey`: every inline key is copied (D18), every node's
//! prefix length (05-fence-key.md), and what every key shares (D50).

#![forbid(unsafe_code)]

use super::node::Node;
use super::node::inline_keys::{Separator, WindowedKey};
use super::node::internal::Internal;
use super::node::leaf::Leaf;
use super::node::page::{FreshPage, NodePtr, PageByType};
use super::root::Tree;
use crate::inline_key::InlineKeyStrategy;

impl<K: Clone, V: Clone, S: InlineKeyStrategy<K>> Clone for Tree<K, V, S> {
    /// A copy of the tree, with every key and value cloned in key order,
    /// each separator's value right after its key (D17), its head's own
    /// where separator keys are heads (D49), on pages from the global
    /// allocator.
    ///
    /// If a `clone` panics, what was copied so far is dropped, and its pages
    /// released, before the panic goes on: nothing leaks, and the tree is
    /// unchanged.
    fn clone(&self) -> Self {
        let Some(root_page) = self.root_page() else {
            return Self::EMPTY;
        };
        Self::from_node(
            root_page.node().clone_nodes(),
            root_page.shared_prefix().clone(),
        )
    }
}

impl<K: Clone, V: Clone, S: InlineKeyStrategy<K>> Node<K, V, S> {
    /// A copy of this node and every node under it, which nothing owns until
    /// the caller hangs it under an edge or makes it a tree's root.
    fn clone_nodes(self) -> Self {
        match self {
            Node::Leaf(leaf) => {
                let copy = Leaf::new(FreshPage::allocate());
                let guard = PartialCopy::new(Node::Leaf(copy));
                if leaf.has_head() {
                    let key = NodePtr::<K, V, S>::has_separator_keys_in_heads()
                        .then(|| leaf.head_key().clone());
                    copy.put_head(key, leaf.head_value().clone());
                }
                copy.set_empty_prefix_len(leaf.prefix_len());
                for index in 0..leaf.len() {
                    let window = leaf.window(index);
                    let (key, value) = leaf.key_value(index);
                    let key = WindowedKey::from_parts(key.clone(), window);
                    let value = value.clone();
                    // A leaf's entries fit in a leaf of its kind.
                    copy.insert_fit(index, key, value);
                }
                guard.finish()
            }
            Node::Internal(internal) => {
                let first_child = internal.child(0).clone_nodes();
                let guard = PartialCopy::new(first_child);
                let copy = Internal::new(FreshPage::allocate(), first_child);
                let guard = guard.with_top(Node::Internal(copy));
                copy.set_empty_prefix_len(internal.prefix_len());
                for index in 0..internal.len() {
                    let window = internal.window(index);
                    // Where separator keys are heads (D49), the leaf copies
                    // them, as it copies its head.
                    let separator = if NodePtr::<K, V, S>::has_separator_keys_in_heads() {
                        Separator::without_key(window)
                    } else {
                        let key = internal.separator_key(index).clone();
                        Separator::with_key(WindowedKey::from_parts(key, window))
                    };
                    let child = internal.child(index + 1).clone_nodes();
                    // A node's separators fit in a node of its kind.
                    copy.insert_fit(index, separator, child);
                }
                guard.finish()
            }
        }
    }
}

/// A copy being made: taken apart if a `clone` panics, so that nothing
/// cloned so far leaks.
struct PartialCopy<K, V, S: InlineKeyStrategy<K>> {
    /// The top of what was copied so far; `None` once finished.
    top: Option<Node<K, V, S>>,
}

impl<K, V, S: InlineKeyStrategy<K>> PartialCopy<K, V, S> {
    fn new(top: Node<K, V, S>) -> Self {
        Self { top: Some(top) }
    }

    /// The copy, with `top` above what it held, which `top` owns now.
    fn with_top(mut self, top: Node<K, V, S>) -> Self {
        self.top = Some(top);
        self
    }

    fn finish(mut self) -> Node<K, V, S> {
        self.top
            .take()
            .expect("a copy is unfinished until `finish`")
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Drop for PartialCopy<K, V, S> {
    fn drop(&mut self) {
        if let Some(top) = self.top.take() {
            top.destroy(PageByType::deallocate);
        }
    }
}
