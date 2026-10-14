//! The root: the one node of a tree without a parent, and what the tree
//! keeps for it in place of one.
//!
//! Every other node gets its fences from the separators above it, and skips
//! the prefix they share (05-fence-key.md). The root has nothing above it,
//! so the tree keeps its fences beside it, as the bytes every key shares
//! (D50, `shared_prefix.rs`): a search checks a query against them once,
//! before the root, and a node with no separator above it on a side, along
//! the tree's left or right border, takes the root's fence there. They live
//! in the root's page, after the root node (`node/root_page.rs`). The tree
//! also keeps the last leaf an append went to, to append there again without
//! a search (D45).
//!
//! The root node is a leaf, in a tree of one node, or an internal node; the
//! tree grows and shrinks a level only at its root, in its page, which the
//! tree keeps for life. Nothing records how tall the tree is: a node stack
//! down counts its own steps (`node_stack.rs`). A map without entries may have
//! no tree at all, as std's has no root then (D1): making one allocates
//! nothing. Every operation on a whole tree starts here, on [`Tree`], each in
//! its feature's module (`tree/mod.rs`).

use core::cmp::Ordering;
use core::mem;

use super::node::Node;
use super::node::inline_keys::SearchKey;
use super::node::leaf::Leaf;
use super::node::page::{FreshPage, NodePtr, PageByType};
use super::node::root_page::RootPage;
use super::prefix::PrefixedKey;
use super::shared_prefix::SharedPrefix;
use crate::allocator::Allocator;
use crate::inline_key::InlineKeyStrategy;

/// A tree, by its root page, if it has one, which the tree owns with every
/// node under it; and the last leaf an append went to.
///
/// It has no `Drop`: dropping a `Tree` leaks the tree, which is safe;
/// `Tree::destroy` takes it apart (`teardown.rs`).
///
/// Invariant: `root_page` holds a well-formed tree, which nothing but this
/// `Tree` reaches. Every key of the tree shares its shared prefix, and every
/// node along the tree's left and right borders, the root too, has its
/// length as its prefix length; searches trust it to route, never to stay
/// in bounds.
///
/// If present, `rightmost_leaf` points to the tree's physically last leaf.
/// Every exclusive use of the tree but the owner's fitting append clears it
/// first ([`Self::root_mut`]). It holds no key reference, cached length, or
/// assertion about the behavior of `Ord`.
pub(crate) struct Tree<K, V, S> {
    root_page: Option<RootPage<K, V, S>>,
    rightmost_leaf: Option<NodePtr<K, V, S>>,
}

// SAFETY: a `Tree` owns its pages, keys and values, and nothing else reaches
// them (type invariant), as a `Box` owns its contents: sending the `Tree` to
// another thread sends the keys and values along. The pages come from the
// map's allocator, which goes with the map: a map is `Send` only if its
// allocator is. The remembered leaf is one of the tree's own pages. The
// strategy is only a marker.
unsafe impl<K: Send, V: Send, S> Send for Tree<K, V, S> {}

// SAFETY: a `&Tree` gives only shared access to its keys and values, as a
// `&Box` does; every change needs `&mut Tree` or the `Tree` itself.
unsafe impl<K: Sync, V: Sync, S> Sync for Tree<K, V, S> {}

impl<K, V, S: InlineKeyStrategy<K>> Tree<K, V, S> {
    /// No tree: what a new map has, allocating nothing.
    pub(crate) const EMPTY: Self = Self {
        root_page: None,
        rightmost_leaf: None,
    };

    /// The tree's root page, to read the tree from; `None` without a tree.
    #[inline(always)]
    pub(crate) fn root_page(&self) -> Option<RootPage<K, V, S>> {
        self.root_page
    }

    /// The tree's root page, to change the tree from: it ends the owner's
    /// permission to append to the remembered leaf, which a change could
    /// move or free.
    #[inline(always)]
    pub(crate) fn root_page_mut(&mut self) -> Option<RootPage<K, V, S>> {
        self.forget_rightmost_leaf();
        self.root_page
    }

    /// Whether there is a tree, with or without entries.
    pub(crate) fn has_root(&self) -> bool {
        self.root_page.is_some()
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
    fn existing_root_page(&self) -> RootPage<K, V, S> {
        self.root_page.expect("an operation on a tree there is not")
    }

    /// The root node, to read the tree from.
    ///
    /// # Panics
    ///
    /// Without a tree.
    #[inline(always)]
    pub(crate) fn root(&self) -> Node<K, V, S> {
        self.existing_root_page().node()
    }

    /// The root node, to change the tree from: it ends the owner's
    /// permission to append to the remembered leaf ([`Self::root_page_mut`]).
    ///
    /// # Panics
    ///
    /// Without a tree.
    #[inline(always)]
    pub(crate) fn root_mut(&mut self) -> Node<K, V, S> {
        self.forget_rightmost_leaf();
        self.root()
    }

    /// How many edges lead down from the root to each leaf: where two trees
    /// line up, to join them (`tree/join.rs`), or which of two is smaller.
    /// Counted down the last edges: every leaf lies as deep, and that is the
    /// border a join of this tree with one right of it walks anyway.
    ///
    /// # Panics
    ///
    /// Without a tree.
    pub(crate) fn leaf_depth(&self) -> usize {
        let (mut node, mut depth) = (self.root(), 0);
        while let Node::Internal(internal) = node {
            node = internal.child(internal.len());
            depth += 1;
        }
        depth
    }

    /// What every key of the tree shares (D50).
    ///
    /// # Panics
    ///
    /// Without a tree.
    pub(in crate::tree) fn shared_prefix(&self) -> &SharedPrefix {
        self.existing_root_page().shared_prefix()
    }

    /// What every key of the tree shares, to change: keeping every node
    /// along the tree's borders at its length is the caller's business
    /// (`tree/prefix.rs`).
    ///
    /// # Panics
    ///
    /// Without a tree.
    pub(in crate::tree) fn shared_prefix_mut(&mut self) -> &mut SharedPrefix {
        self.existing_root_page().shared_prefix_mut()
    }

    /// Ends the owner's permission to append directly before another
    /// exclusive user can change or release the remembered leaf.
    pub(in crate::tree) fn forget_rightmost_leaf(&mut self) {
        if self.rightmost_leaf.is_some() {
            self.rightmost_leaf = None;
        }
    }

    #[cfg(test)]
    pub(crate) fn remembers_rightmost_leaf(&self) -> bool {
        self.rightmost_leaf.is_some()
    }

    /// A new tree, on a root page from `alloc`: an empty root leaf, without
    /// a head, and nothing shared yet.
    pub(crate) fn new(alloc: &impl Allocator) -> Self {
        Self {
            root_page: Some(RootPage::new(FreshPage::allocate(alloc))),
            rightmost_leaf: None,
        }
    }

    /// The tree's root page, as [`Self::root_page`] reads it; without a
    /// tree, that of a new one from `alloc`, as [`Self::new`] makes it:
    /// where an insertion starts (`tree/prefix.rs`).
    #[inline]
    pub(in crate::tree) fn root_page_or_new(
        &mut self,
        alloc: &impl Allocator,
    ) -> RootPage<K, V, S> {
        *self
            .root_page
            .get_or_insert_with(|| RootPage::new(FreshPage::allocate(alloc)))
    }

    /// The tree of `top` and the nodes under it, on plain pages, whose keys
    /// all share `shared_prefix`, which every node along its borders must
    /// have as its prefix length (D50): `top` moves into a root page, from
    /// and back to `alloc`. The tree owns them from now on.
    pub(in crate::tree) fn from_node(
        top: Node<K, V, S>,
        shared_prefix: SharedPrefix,
        alloc: &impl Allocator,
    ) -> Self {
        let (page, emptied) = RootPage::from_node(FreshPage::allocate(alloc), top, shared_prefix);
        emptied.deallocate(alloc);
        Self {
            root_page: Some(page),
            rightmost_leaf: None,
        }
    }

    /// The root node on a plain page of its kind, giving up the tree, for
    /// another tree to hang under an edge (`tree/join.rs`): its root page
    /// goes back to `alloc`, and the new one comes from it.
    ///
    /// # Panics
    ///
    /// Without a tree.
    pub(in crate::tree) fn into_node(self, alloc: &impl Allocator) -> Node<K, V, S> {
        let (top, emptied) = self.existing_root_page().into_node(alloc);
        emptied.deallocate(alloc);
        top
    }

    /// Makes the tree a level taller ([`RootPage::raise`]): the root node
    /// moves to a page of its kind from `alloc`, under the root, which is
    /// internal now.
    ///
    /// # Panics
    ///
    /// Without a tree.
    pub(in crate::tree) fn raise(&mut self, alloc: &impl Allocator) {
        self.forget_rightmost_leaf();
        let root_page = self.existing_root_page().raise(alloc);
        self.root_page = Some(root_page);
    }

    /// Makes the tree a level shorter ([`RootPage::lower`]): the root's only
    /// child moves up into the root page, whose node must be internal and
    /// hold no keys. Returns the child's emptied page.
    ///
    /// # Panics
    ///
    /// Without a tree, or if the root is a leaf; the tree is unchanged then.
    pub(in crate::tree) fn lower(&mut self) -> PageByType<K, V, S> {
        self.forget_rightmost_leaf();
        let (page, emptied) = self.existing_root_page().lower();
        self.root_page = Some(page);
        emptied
    }

    /// Remembers `leaf`, the tree's last leaf, for the next insert to append
    /// to (D45).
    ///
    /// # Safety
    ///
    /// `leaf` is the physically last leaf owned by this tree. No outstanding
    /// handle may change it after this call; every subsequent structural
    /// mutation must first forget it.
    pub(in crate::tree) unsafe fn remember_rightmost_leaf(&mut self, leaf: Leaf<K, V, S>) {
        self.rightmost_leaf = Some(leaf.node_ptr());
    }

    /// An append of `key` to the remembered last leaf, if it has room, and
    /// `key` is greater than its last key and has what every key shares
    /// (D50); or else `key` back, for the ordinary search. Forgets the leaf
    /// before comparing.
    #[inline]
    pub(crate) fn rightmost_append(&mut self, key: K) -> Result<RightmostAppend<'_, K, V, S>, K>
    where
        K: Ord,
    {
        let Some(leaf) = self.rightmost_leaf else {
            return Err(key);
        };
        self.rightmost_leaf = None;
        // The field's invariant identifies a live leaf of this tree; the
        // exclusive owner borrow is its only mutation access.
        let leaf = Leaf::at(leaf);
        let len = leaf.len();
        if len >= Leaf::<K, V, S>::capacity() {
            return Err(key);
        }
        let Some(last) = len.checked_sub(1) else {
            return Err(key);
        };
        // SAFETY: `last < len`.
        let last_key = unsafe { leaf.key_unchecked(last) };
        if SearchKey::<K, S, K>::new(&key).cmp_full(last_key) != Ordering::Greater {
            return Err(key);
        }
        // A key that shares less goes the general way, which shrinks the
        // shared prefix for it.
        let key = PrefixedKey::checked::<S>(self.shared_prefix(), key)?;
        Ok(RightmostAppend {
            leaf,
            key,
            rightmost_leaf: &mut self.rightmost_leaf,
        })
    }
}

/// An append of a key to the physically last leaf, which has room for it,
/// lent together with its owner's rightmost-leaf field, forgotten
/// meanwhile. Dropping this token drops the key, changes no entries and
/// leaves the leaf forgotten; consuming it appends and remembers the leaf
/// again.
pub(crate) struct RightmostAppend<'a, K, V, S> {
    leaf: Leaf<K, V, S>,
    /// Greater than every key of the tree, and sharing what they share.
    key: PrefixedKey<K>,
    rightmost_leaf: &'a mut Option<NodePtr<K, V, S>>,
}

impl<K, V, S: InlineKeyStrategy<K>> RightmostAppend<'_, K, V, S> {
    /// Appends the key, with `value`.
    #[inline]
    pub(in crate::tree) fn insert(self, value: V) {
        let Self {
            leaf,
            key,
            rightmost_leaf,
        } = self;
        let index = leaf.len();
        // Compute the key's window before the leaf changes. A panic or an
        // unused token leaves the leaf forgotten, without changing the tree.
        leaf.insert_fit(index, key.window_at(leaf.prefix_len()), value);
        // Fitting appends preserve this leaf and all ancestors. There are
        // no callbacks after insertion; the caller counts the entry next.
        *rightmost_leaf = Some(leaf.node_ptr());
    }
}
