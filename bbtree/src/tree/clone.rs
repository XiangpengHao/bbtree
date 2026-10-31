//! Copying an owned tree: [`Tree::clone_in`], node by node, so that the
//! copy has the same shape, as std's `BTreeMap::clone` does. It calls no
//! `Ord` and no `InlineKey`: it copies every inline key (D18), every node's
//! prefix length (`prefix.rs`), and what every key shares (D50).

#![forbid(unsafe_code)]

use core::mem;

use super::node::Node;
use super::node::inline_keys::{Separator, WindowedKey};
use super::node::internal::Internal;
use super::node::leaf::Leaf;
use super::node::page::{FreshPage, NodePtr};
use super::root::Tree;
use crate::allocator::Allocator;
use crate::inline_key::InlineKeyStrategy;

impl<K: Clone, V: Clone, S: InlineKeyStrategy<K>> Tree<K, V, S> {
    /// A copy of the tree on pages from `alloc`, the copy's allocator. Every
    /// key and value is cloned in key order: each separator's value right
    /// after its key (D17), which with inline keys is its head's key (D49).
    pub(crate) fn clone_in(&self, alloc: &impl Allocator) -> Self {
        let Some(root_page) = self.root_page() else {
            return Self::EMPTY;
        };
        Self::from_node(
            root_page.node().clone_nodes(alloc),
            *self.shared_prefix(),
            alloc,
        )
    }
}

impl<K: Clone, V: Clone, S: InlineKeyStrategy<K>> Node<K, V, S> {
    /// A copy of this node and every node under it, on pages from `alloc`.
    /// Nothing owns it until the caller puts it under an edge or makes it a
    /// tree's root.
    fn clone_nodes(self, alloc: &impl Allocator) -> Self {
        match self {
            Node::Leaf(leaf) => {
                let copy = Leaf::new(FreshPage::allocate(alloc), leaf.prefix_len());
                let guard = PartialCopy {
                    top: Node::Leaf(copy),
                    alloc,
                };
                if leaf.has_head() {
                    let key =
                        NodePtr::<K, V, S>::has_inline_keys().then(|| leaf.head_key().clone());
                    copy.put_head(key, leaf.head_value().clone());
                }
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
                let first_child = internal.child(0).clone_nodes(alloc);
                let guard = PartialCopy {
                    top: first_child,
                    alloc,
                };
                let page = FreshPage::allocate(alloc);
                let copy = Internal::new(page, first_child, internal.prefix_len());
                let guard = guard.with_top(Node::Internal(copy));
                for index in 0..internal.len() {
                    let window = internal.window(index);
                    // With inline keys, the separator keys are in the
                    // heads, which the leaves copy (D49).
                    let separator = if NodePtr::<K, V, S>::has_inline_keys() {
                        Separator::without_key(window)
                    } else {
                        let key = internal.separator_key(index).clone();
                        Separator::with_key(WindowedKey::from_parts(key, window))
                    };
                    let child = internal.child(index + 1).clone_nodes(alloc);
                    // A node's separators fit in a node of its kind.
                    copy.insert_fit(index, separator, child);
                }
                guard.finish()
            }
        }
    }
}

/// A copy being made, from its top down, on pages from `alloc`: taken apart
/// if a `clone` panics, so that nothing cloned so far leaks.
struct PartialCopy<'a, K, V, S: InlineKeyStrategy<K>, A: Allocator> {
    top: Node<K, V, S>,
    alloc: &'a A,
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator> PartialCopy<'_, K, V, S, A> {
    /// The copy, with `top` above what it held, which `top` owns now.
    fn with_top(self, top: Node<K, V, S>) -> Self {
        let alloc = self.alloc;
        mem::forget(self);
        Self { top, alloc }
    }

    fn finish(self) -> Node<K, V, S> {
        let top = self.top;
        mem::forget(self);
        top
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator> Drop for PartialCopy<'_, K, V, S, A> {
    fn drop(&mut self) {
        self.top.destroy(self.alloc);
    }
}
