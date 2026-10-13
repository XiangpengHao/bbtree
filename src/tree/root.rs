//! The root: the one node of a tree without a parent, and what the tree
//! keeps for it in place of one.
//!
//! Every other node gets its fences from the separators above it, and skips
//! the prefix they share (05-fence-key.md). The root has nothing above it,
//! so the tree keeps its fences beside it, as the bytes every key shares
//! (D50, `shared_prefix.rs`): a search checks a query against them once,
//! before the root, and a node with no separator above it on a side, along
//! the tree's left or right border, takes the root's fence there. The tree
//! also keeps the last leaf an append went to, to append there again
//! without a search (D45).
//!
//! The root's node is a leaf, in a tree of one node, or an internal node;
//! only the root grows or shrinks the tree's height. Every operation on a
//! whole tree starts here, on [`Root`], each in its feature's module
//! (`tree/mod.rs`).

use core::cmp::Ordering;

use super::node::Node;
use super::node::inline_keys::{InlinedKey, SearchKey};
use super::node::internal::Internal;
use super::node::leaf::Leaf;
use super::node::page::{FreshPage, NodePtr};
use super::shared_prefix::SharedPrefix;
use crate::inline_key::InlineKeyStrategy;

/// A tree, by its root: the root node at `height`, which the tree owns with
/// every node under it, its fences, as what every key shares (D50), and the
/// last leaf an append went to.
///
/// It has no `Drop`: dropping a `Root` leaks the tree, which is safe;
/// `Root::destroy` takes it apart (`teardown.rs`).
///
/// Invariant: `node` holds a well-formed tree of nodes at `height` and
/// below, which nothing but this `Root` reaches. Every key of the tree
/// shares `shared_prefix`, and every node along the tree's left and right
/// borders, the root too, has its length as its prefix length; searches
/// trust it to route, never to stay in bounds.
///
/// If present, `rightmost_leaf` points to the tree's physically last leaf.
/// Every exclusive use of the tree but the owner's fitting append clears it
/// first ([`Self::top_mut`]). It holds no key reference, cached length, or
/// assertion about the behavior of `Ord`.
pub(crate) struct Root<K, V, S> {
    node: NodePtr<K, V, S>,
    height: usize,
    shared_prefix: SharedPrefix,
    rightmost_leaf: Option<NodePtr<K, V, S>>,
}

// SAFETY: a `Root` owns its pages, keys and values, and nothing else reaches
// them (type invariant), as a `Box` owns its contents: sending the `Root` to
// another thread sends the keys and values along. The pages come from the
// global allocator, which any thread may free. The remembered leaf is one of
// the tree's own pages. The strategy is only a marker.
unsafe impl<K: Send, V: Send, S> Send for Root<K, V, S> {}

// SAFETY: a `&Root` gives only shared access to its keys and values, as a
// `&Box` does; every change needs `&mut Root` or the `Root` itself.
unsafe impl<K: Sync, V: Sync, S> Sync for Root<K, V, S> {}

impl<K, V, S> Root<K, V, S> {
    /// A tree of `top` and the nodes under it, whose keys all share
    /// `shared_prefix`, which every node along its borders must have as its
    /// prefix length (D50). The tree owns them from now on.
    pub(in crate::tree) fn new(top: Node<K, V, S>, shared_prefix: SharedPrefix) -> Self {
        Self {
            node: top.node_ptr(),
            height: top.height(),
            shared_prefix,
            rightmost_leaf: None,
        }
    }

    /// The root node, to read the tree from.
    #[inline(always)]
    pub(crate) fn top(&self) -> Node<K, V, S> {
        Node::at(self.node, self.height)
    }

    /// The root node, to change the tree from: it ends the owner's
    /// permission to append to the remembered leaf, which a change could
    /// move or free.
    #[inline(always)]
    pub(crate) fn top_mut(&mut self) -> Node<K, V, S> {
        self.clear_insertion_hint();
        self.top()
    }

    /// The root node, giving up the tree: for a walk to take it apart, or for
    /// another tree to hang it under an edge, without what only a root has.
    pub(crate) fn into_top(self) -> Node<K, V, S> {
        Node::at(self.node, self.height)
    }

    pub(crate) fn height(&self) -> usize {
        self.height
    }

    /// What every key of the tree shares (D50).
    pub(crate) fn shared_prefix(&self) -> &SharedPrefix {
        &self.shared_prefix
    }

    /// What every key of the tree shares, to change: keeping every node
    /// along the tree's borders at its length is the caller's business
    /// (`tree/prefix.rs`).
    pub(in crate::tree) fn shared_prefix_mut(&mut self) -> &mut SharedPrefix {
        &mut self.shared_prefix
    }

    /// Ends the owner's permission to append directly before another
    /// exclusive user can change or release the remembered leaf.
    pub(in crate::tree) fn clear_insertion_hint(&mut self) {
        if self.rightmost_leaf.is_some() {
            self.rightmost_leaf = None;
        }
    }

    #[cfg(test)]
    pub(crate) fn has_insertion_hint(&self) -> bool {
        self.rightmost_leaf.is_some()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Root<K, V, S> {
    /// An empty tree: a root leaf, without a head, and nothing shared yet.
    pub(crate) fn new_leaf(page: FreshPage<Leaf<K, V, S>>) -> Self {
        Self::new(Node::Leaf(Leaf::new(page)), SharedPrefix::none())
    }

    /// Puts a new root with no keys above the current one, which becomes its
    /// only child, and returns the new root. On both borders, it takes the
    /// tree's shared prefix (D50), as the old root has.
    pub(crate) fn push_internal_level(
        &mut self,
        page: FreshPage<Internal<K, V, S>>,
    ) -> Internal<K, V, S> {
        let old = self.top_mut();
        let root = Internal::new(page, old);
        root.set_empty_prefix_len(self.shared_prefix.len());
        self.node = root.node_ptr();
        self.height = root.height();
        root
    }

    /// Removes the root, which must be internal and hold no keys, making its
    /// only child the root, and returns the old root's page: the inverse of
    /// [`Self::push_internal_level`]. Were the root to hold keys, they and
    /// the other children would be leaked.
    ///
    /// # Panics
    ///
    /// If the root is a leaf; the tree is unchanged then.
    pub(crate) fn pop_internal_level(&mut self) -> FreshPage<Internal<K, V, S>> {
        let Node::Internal(root) = self.top_mut() else {
            panic!("popping a leaf root");
        };
        let (child, page) = root.take_only_child();
        self.node = child.node_ptr();
        self.height = child.height();
        page
    }

    /// Publishes a path's proof after it relinquishes its mutation access.
    ///
    /// # Safety
    ///
    /// `leaf` is the physically last leaf owned by this tree. No outstanding
    /// handle may change it after this call; every subsequent structural
    /// mutation must first clear the hint.
    pub(in crate::tree) unsafe fn remember_rightmost_leaf(&mut self, leaf: Leaf<K, V, S>) {
        self.rightmost_leaf = Some(leaf.node_ptr());
    }

    /// Validates a fitting append to the remembered last leaf when `key`
    /// is greater than its current last key and has what every key shares
    /// (D50). Clears an active hint before comparison, and retains no borrow
    /// of `key`: the returned token can consume it on insertion. Failure
    /// leaves ordinary search to the caller.
    #[inline]
    pub(crate) fn rightmost_append(&mut self, key: &K) -> Option<RightmostAppend<'_, K, V, S>>
    where
        K: Ord,
    {
        let leaf = Leaf::at(self.rightmost_leaf?);
        self.rightmost_leaf = None;
        // The hint's invariant identifies a live leaf of this tree; the
        // exclusive owner borrow is its only mutation access. No ordering
        // result is used to justify the pointer or its bounds.
        let len = leaf.len();
        if len >= Leaf::<K, V, S>::capacity() {
            return None;
        }
        let last = len.checked_sub(1)?;
        // SAFETY: `last < len`.
        let last_key = unsafe { leaf.key_unchecked(last) };
        if SearchKey::<K, S, K>::new(key).cmp_full(last_key) != Ordering::Greater {
            return None;
        }
        // A key that shares less goes the general way, which covers it.
        let shared = &self.shared_prefix;
        if shared.len_shared_with::<K, S>(key) < shared.len() {
            return None;
        }
        Some(RightmostAppend {
            leaf,
            hint: &mut self.rightmost_leaf,
        })
    }
}

/// The physically last leaf, with room, lent together with its owner's
/// cleared insertion hint. Dropping this token changes no entries and leaves
/// the hint empty; consuming it appends and restores the hint.
pub(crate) struct RightmostAppend<'a, K, V, S> {
    leaf: Leaf<K, V, S>,
    hint: &'a mut Option<NodePtr<K, V, S>>,
}

impl<K, V, S: InlineKeyStrategy<K>> RightmostAppend<'_, K, V, S> {
    /// Appends the same key that was validated by
    /// [`Root::rightmost_append`], with its value. Key identity is a logical
    /// ordering requirement, not a memory-safety premise: the leaf was
    /// checked for room.
    #[inline]
    pub(crate) fn insert(self, key: K, value: V) {
        let Self { leaf, hint } = self;
        let index = leaf.len();
        let prefix_len = leaf.prefix_len();
        // Compute the summary before mutation. A panic or an unused token
        // leaves the hint cleared, without changing the tree.
        leaf.insert_fit(index, InlinedKey::new(key, prefix_len), value);
        // Fitting appends preserve this leaf and all ancestors. There are
        // no callbacks after insertion; the caller counts the entry next.
        *hint = Some(leaf.node_ptr());
    }
}
