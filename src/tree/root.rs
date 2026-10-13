//! The root: the one node of a tree without a parent, and what the tree
//! keeps for it in place of one.
//!
//! Every other node gets its fences from the separators above it, and skips
//! the prefix they share (05-fence-key.md). The root has nothing above it,
//! so the tree keeps its fences beside it, as the bytes every key shares
//! (D50, `shared_prefix.rs`): a search checks a query against them once,
//! before the root, and a node with no separator above it on a side, along
//! the tree's left or right border, takes the root's fence there. Both live
//! in the root's page, after the root node, with the tree's height
//! (`node/root_page.rs`). The tree also keeps the last leaf an append went
//! to, to append there again without a search (D45).
//!
//! The root node is a leaf, in a tree of one node, or an internal node;
//! only the root grows or shrinks the tree's height, in its page, which the
//! tree keeps for life. A map without entries may have no tree at all, as
//! std's has no root then (D1): making one allocates nothing. Every
//! operation on a whole tree starts here, on [`Root`], each in its
//! feature's module (`tree/mod.rs`).

use core::cmp::Ordering;
use core::mem;

use super::node::Node;
use super::node::inline_keys::{InlinedKey, SearchKey};
use super::node::internal::Internal;
use super::node::leaf::Leaf;
use super::node::page::{FreshPage, NodePtr, PageByKind};
use super::node::root_page::RootPage;
use super::shared_prefix::SharedPrefix;
use crate::inline_key::InlineKeyStrategy;

/// A tree, by its root page, if it has one, which the tree owns with every
/// node under it; and the last leaf an append went to.
///
/// It has no `Drop`: dropping a `Root` leaks the tree, which is safe;
/// `Root::destroy` takes it apart (`teardown.rs`).
///
/// Invariant: `page` holds a well-formed tree, which nothing but this
/// `Root` reaches. Every key of the tree shares its shared prefix, and every
/// node along the tree's left and right borders, the root too, has its
/// length as its prefix length; searches trust it to route, never to stay
/// in bounds.
///
/// If present, `rightmost_leaf` points to the tree's physically last leaf.
/// Every exclusive use of the tree but the owner's fitting append clears it
/// first ([`Self::top_mut`]). It holds no key reference, cached length, or
/// assertion about the behavior of `Ord`.
pub(crate) struct Root<K, V, S> {
    page: Option<RootPage<K, V, S>>,
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

impl<K, V, S: InlineKeyStrategy<K>> Root<K, V, S> {
    /// No tree: what a new map has, allocating nothing.
    pub(crate) const EMPTY: Self = Self {
        page: None,
        rightmost_leaf: None,
    };

    /// The tree's root page, to read the tree from; `None` without a tree.
    #[inline(always)]
    pub(crate) fn page(&self) -> Option<RootPage<K, V, S>> {
        self.page
    }

    /// The tree's root page, to change the tree from: it ends the owner's
    /// permission to append to the remembered leaf, which a change could
    /// move or free.
    #[inline(always)]
    pub(crate) fn page_mut(&mut self) -> Option<RootPage<K, V, S>> {
        self.clear_insertion_hint();
        self.page
    }

    /// Whether there is a tree, with or without entries.
    pub(crate) fn has_tree(&self) -> bool {
        self.page.is_some()
    }

    /// The tree, leaving none here.
    pub(crate) fn take(&mut self) -> Self {
        mem::replace(self, Self::EMPTY)
    }

    /// The root page of a tree there must be.
    ///
    /// # Panics
    ///
    /// Without a tree.
    #[inline(always)]
    fn tree(&self) -> RootPage<K, V, S> {
        self.page.expect("an operation on a tree there is not")
    }

    /// The root node, to read the tree from.
    ///
    /// # Panics
    ///
    /// Without a tree.
    #[inline(always)]
    pub(crate) fn top(&self) -> Node<K, V, S> {
        self.tree().top()
    }

    /// The root node, to change the tree from: it ends the owner's
    /// permission to append to the remembered leaf ([`Self::page_mut`]).
    ///
    /// # Panics
    ///
    /// Without a tree.
    #[inline(always)]
    pub(crate) fn top_mut(&mut self) -> Node<K, V, S> {
        self.clear_insertion_hint();
        self.top()
    }

    /// The root node's height.
    ///
    /// # Panics
    ///
    /// Without a tree.
    pub(crate) fn height(&self) -> usize {
        self.tree().height()
    }

    /// What every key of the tree shares (D50).
    ///
    /// # Panics
    ///
    /// Without a tree.
    pub(crate) fn shared_prefix(&self) -> &SharedPrefix {
        self.tree().shared_prefix()
    }

    /// What every key of the tree shares, to change: keeping every node
    /// along the tree's borders at its length is the caller's business
    /// (`tree/prefix.rs`).
    ///
    /// # Panics
    ///
    /// Without a tree.
    pub(in crate::tree) fn shared_prefix_mut(&mut self) -> &mut SharedPrefix {
        self.tree().shared_prefix_mut()
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
    /// A new tree, in `page`: an empty root leaf, without a head, and
    /// nothing shared yet.
    pub(crate) fn new(page: FreshPage<RootPage<K, V, S>>) -> Self {
        Self {
            page: Some(RootPage::new(page)),
            rightmost_leaf: None,
        }
    }

    /// The tree's root page, after making a new tree, from the global
    /// allocator, if there is none: what an insertion starts from.
    pub(crate) fn page_or_new(&mut self) -> RootPage<K, V, S> {
        *self
            .page
            .get_or_insert_with(|| RootPage::new(FreshPage::allocate()))
    }

    /// The tree of `top`, `height` levels above the leaves, and the nodes
    /// under it, on plain pages, whose keys all share `shared_prefix`, which
    /// every node along its borders must have as its prefix length (D50):
    /// `top` moves into a root page, from and back to the global allocator.
    /// The tree owns them from now on.
    pub(in crate::tree) fn from_node(
        top: Node<K, V, S>,
        height: usize,
        shared_prefix: SharedPrefix,
    ) -> Self {
        let (page, emptied) =
            RootPage::from_node(FreshPage::allocate(), top, height, shared_prefix);
        emptied.deallocate();
        Self {
            page: Some(page),
            rightmost_leaf: None,
        }
    }

    /// The root node on a plain page of its kind, and its height, giving up
    /// the tree, for another tree to hang under an edge (`tree/join.rs`):
    /// its root page goes back to the global allocator, and the new one comes
    /// from it.
    ///
    /// # Panics
    ///
    /// Without a tree.
    pub(in crate::tree) fn into_node(self) -> (Node<K, V, S>, usize) {
        let page = self.tree();
        let height = page.height();
        let (top, emptied) = match page.top() {
            Node::Leaf(_) => page.into_node::<Leaf<K, V, S>>(FreshPage::allocate()),
            Node::Internal(_) => page.into_node::<Internal<K, V, S>>(FreshPage::allocate()),
        };
        emptied.deallocate();
        (top, height)
    }

    /// Makes the tree a level taller ([`RootPage::raise`]): the root node
    /// moves to a page from `new_leaf_page` or `new_internal_page`, by its
    /// kind, under the root, which is internal now. Returns the moved node.
    ///
    /// # Panics
    ///
    /// Without a tree.
    pub(crate) fn raise(
        &mut self,
        new_leaf_page: impl FnOnce() -> FreshPage<Leaf<K, V, S>>,
        new_internal_page: impl FnOnce() -> FreshPage<Internal<K, V, S>>,
    ) -> Node<K, V, S> {
        let page = self.tree();
        match self.top_mut() {
            Node::Leaf(_) => page.raise(new_leaf_page()),
            Node::Internal(_) => page.raise(new_internal_page()),
        }
    }

    /// Makes the tree a level shorter ([`RootPage::lower`]): the root's only
    /// child moves up into the root page, whose node must be internal and
    /// hold no keys. Returns the child's emptied page.
    ///
    /// # Panics
    ///
    /// Without a tree, or if the root is a leaf; the tree is unchanged then.
    pub(crate) fn lower(&mut self) -> PageByKind<K, V, S> {
        self.clear_insertion_hint();
        self.tree().lower()
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
        let shared = self.shared_prefix();
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
