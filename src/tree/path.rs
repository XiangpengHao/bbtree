//! Paths down a tree: the node at each height, from the root down to a
//! position in one of them, and the index taken at each (D11, D24). Every
//! range, entry and removal keeps one, as do inserts that split. Iterators
//! move two of them through the tree (`walk.rs`), an entry keeps the one its
//! search found (`map/entry.rs`), and inserting and removing go back up the
//! one that led to the key, splitting or fixing the nodes on it
//! (`insert.rs`, `remove.rs`).
//!
//! A path stands for a borrow of the whole tree, which its borrow type
//! says: shared ([`Immut`]), value-mutating ([`ValMut`]), owning, for a
//! walk that takes the tree apart ([`Dying`]), or exclusive ([`Mut`], held
//! by a [`PathMut`]). The borrow type is what the walks hand entries out by
//! (`walk.rs`), and what makes a path `Send` and `Sync` as the borrow it
//! stands for; the nodes it hands out are plain handles, with the access of
//! the path's borrow (`tree/node/mod.rs`).
//!
//! A path has a slot for every level of the tallest tree there can be
//! ([`MAX_HEIGHT`]), about 250 bytes. It is made detached and started where
//! it is, writing only the levels in use, and is not moved once written: the
//! CPU can't forward the narrow stores that wrote it to the wide loads of a
//! copy, which then wait for those stores to retire, and with them every
//! older cache miss (D23).
//!
//! # Safety argument
//!
//! Every node a path hands out relies on the [`Path`] invariant, which holds
//! because:
//!
//! - a path starts at a root, and every node below it on the path was read
//!   from its parent's edge when the path moved down to it;
//! - only the bottom node changes while a path lives: a copyable borrow
//!   changes no structure, and an exclusive path changes only its bottom
//!   node, from which nothing leads back up;
//! - the root changes only through an exclusive path, which updates itself
//!   ([`PathMut::push_root`]).

use core::borrow::Borrow;
use core::cmp::Ordering;
use core::marker::PhantomData;
use core::mem::MaybeUninit;

use super::node::inline_keys::{InlinedKey, SearchKey};
use super::node::internal::Internal;
use super::node::leaf::Leaf;
use super::node::page::{FreshPage, NodePtr, PageByKind};
use super::node::{KeyList, Node};
use super::root::Root;
use super::shared_prefix::SharedPrefix;
use crate::inline_key::InlineKeyStrategy;

/// The most levels above the leaves that a tree can have. A non-root
/// internal node has at least 5 children (D13, D22), the root at least 2,
/// and a leaf takes at least 1 KB (D15), so a tree one level taller would
/// need at least 2·5^23 KB of leaves, more than 2^64 bytes.
pub(crate) const MAX_HEIGHT: usize = 23;

/// Borrow type of a [`Path`]: shared access for `'a` to the tree, like
/// `&'a`.
pub(crate) struct Immut<'a>(PhantomData<&'a ()>);

/// Borrow type of a [`Path`]: exclusive access for `'a` to the tree, like
/// `&'a mut`.
///
/// Like `&mut`, it is invariant in `K` and `V`: a path that could claim
/// shorter-lived keys or values could store them into a tree that outlives
/// them.
pub(crate) struct Mut<'a, K, V>(PhantomData<&'a mut (K, V)>);

/// Borrow type of a [`Path`]: access for `'a` to the tree by a walk that
/// hands out each value as `&'a mut V` at most once, while keys and
/// structure stay shared, as `IterMut` does.
///
/// Its paths may be copied, so that both ends of a walk hold some: they only
/// read the structure and keys. Handing out a value is unsafe, and the walk
/// vouches that it does so once per value.
///
/// Invariant in `K` and `V`, like `&'a mut`.
pub(crate) struct ValMut<'a, K, V>(PhantomData<&'a mut (K, V)>);

/// Borrow type of a [`Path`]: ownership of a tree that a walk is taking
/// apart, moving entries out from either end and freeing each node once past
/// it (`tree/walk.rs`), as std's `IntoIter` does.
///
/// Its paths may be copied, so that both ends of the walk hold some. Past
/// the walk's ends, entries have been moved out and nodes freed, while the
/// nodes' lengths still count them: only the walk knows where the gaps are.
/// It reads only the structure between its ends. Moving an entry out and
/// freeing a node are unsafe, and the walk vouches that it does each once.
///
/// It owns `K` and `V`, and is covariant in them, as an owned tree is.
pub(crate) struct Dying<K, V>(PhantomData<(K, V)>);

/// A borrow type whose paths may be copied, because nothing they do
/// changes the tree's structure: [`Immut`], [`ValMut`] and [`Dying`].
pub(crate) trait CopyableBorrow {}

impl CopyableBorrow for Immut<'_> {}

impl<K, V> CopyableBorrow for ValMut<'_, K, V> {}

impl<K, V> CopyableBorrow for Dying<K, V> {}

/// An end of a node, where a path goes down to the first of its entries,
/// or to the last: [`FirstEdge`] or [`LastEdge`]. A type, so that code for
/// one end, which a caller always knows, holds no choice between them.
pub(crate) trait NodeEnd {
    /// Whether this is the last edge.
    const IS_LAST: bool;

    /// This end's edge in `node`: 0, or its length, the only read of it.
    fn edge<K, V, S>(node: Node<K, V, S>) -> usize {
        if Self::IS_LAST { node.len() } else { 0 }
    }
}

/// A node's first edge.
pub(crate) enum FirstEdge {}

impl NodeEnd for FirstEdge {
    const IS_LAST: bool = false;
}

/// A node's last edge.
pub(crate) enum LastEdge {}

impl NodeEnd for LastEdge {
    const IS_LAST: bool = true;
}

/// A path from a tree's root down to a position in one of its nodes, with
/// the access to the whole tree that borrow type `B` says; or a detached
/// path, which reaches no node until it is attached to a root.
///
/// Invariant: `bottom <= root_height + 1` and `root_height <= MAX_HEIGHT`;
/// the path is detached exactly when `bottom > root_height`. For every
/// height `h` from `bottom` to `root_height`, `nodes[h]` and `indexes[h]` are
/// initialized, and `nodes[h]` holds a node at height `h` of one tree, the
/// node at `root_height` its root. For every `h` above `bottom`, `indexes[h]`
/// is an edge of `nodes[h]`, and `nodes[h - 1]` is the child at that edge.
/// `indexes[bottom]` is the position the path ends at in the bottom node, an
/// edge or a key, which every use checks against the node's length. A node
/// holds at most 1023 keys (`tree/node/layout.rs`), so every index fits in a
/// `u16`.
pub(crate) struct Path<B, K, V, S> {
    nodes: [MaybeUninit<NodePtr<K, V, S>>; MAX_HEIGHT + 1],
    indexes: [MaybeUninit<u16>; MAX_HEIGHT + 1],
    bottom: u8,
    root_height: u8,
    _borrow: PhantomData<B>,
}

impl<B: CopyableBorrow, K, V, S> Clone for Path<B, K, V, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<B: CopyableBorrow, K, V, S> Copy for Path<B, K, V, S> {}

// SAFETY: a shared path gives only shared access to the keys and values of
// the tree for `'a`, as a `&'a (K, V)` would, and nothing else: sending it
// to another thread shares them with that thread, which `K: Sync` and
// `V: Sync` allow. The strategy is only a marker.
unsafe impl<K: Sync, V: Sync, S> Send for Path<Immut<'_>, K, V, S> {}
// SAFETY: as for `Send`: sharing a shared path across threads shares the
// keys and values, nothing more.
unsafe impl<K: Sync, V: Sync, S> Sync for Path<Immut<'_>, K, V, S> {}
// SAFETY: a value-mutating path stands for an exclusive borrow of the whole
// tree, as a `&'a mut` would, which it hands out as shared keys and
// exclusive values: sending it to another thread moves that exclusive access
// there, which `K: Send` and `V: Send` allow, as for `&mut (K, V)`; the keys
// it hands out are shared only within the thread that has it. These are
// std's bounds for its `IterMut`.
unsafe impl<K: Send, V: Send, S> Send for Path<ValMut<'_, K, V>, K, V, S> {}
// SAFETY: through `&` a value-mutating path only reads the structure, keys
// and values, which `K: Sync` and `V: Sync` allow sharing.
unsafe impl<K: Sync, V: Sync, S> Sync for Path<ValMut<'_, K, V>, K, V, S> {}
// SAFETY: an exclusive path stands for an exclusive borrow of the tree, as a
// `&mut` would: sending it moves that access to another thread, which
// `K: Send` and `V: Send` allow; through `&` it only reads.
unsafe impl<K: Send, V: Send, S> Send for Path<Mut<'_, K, V>, K, V, S> {}
// SAFETY: as for `Send`.
unsafe impl<K: Sync, V: Sync, S> Sync for Path<Mut<'_, K, V>, K, V, S> {}
// SAFETY: a dying path stands for ownership of the tree's keys and values,
// as a `Box<(K, V)>` would: sending it to another thread sends them there,
// which `K: Send` and `V: Send` allow. The pages come from the global
// allocator, which any thread may free.
unsafe impl<K: Send, V: Send, S> Send for Path<Dying<K, V>, K, V, S> {}
// SAFETY: as for `Send`; through `&` it only reads.
unsafe impl<K: Sync, V: Sync, S> Sync for Path<Dying<K, V>, K, V, S> {}

impl<B, K, V, S> Path<B, K, V, S> {
    /// A path that reaches no node. Making it writes nothing but its heights,
    /// where it is made: a struct literal, not a constant, which would be
    /// copied in whole.
    #[inline(always)]
    pub(crate) fn detached() -> Self {
        Self {
            nodes: [const { MaybeUninit::uninit() }; MAX_HEIGHT + 1],
            indexes: [const { MaybeUninit::uninit() }; MAX_HEIGHT + 1],
            bottom: 1,
            root_height: 0,
            _borrow: PhantomData,
        }
    }

    /// Makes the path just `root`, ending at its first edge.
    ///
    /// `root` is the root of a well-formed tree, to which the path has the
    /// access `B` says for as long as it lives.
    pub(crate) fn attach(&mut self, root: Node<K, V, S>) {
        self.write_level(root.height(), root.node_ptr(), 0);
        self.set_heights(root.height(), root.height());
    }

    /// Writes level `height`.
    fn write_level(&mut self, height: usize, node: NodePtr<K, V, S>, index: usize) {
        self.nodes[height].write(node);
        self.write_index(height, index);
    }

    /// Writes the index of level `height`.
    fn write_index(&mut self, height: usize, index: usize) {
        debug_assert!(index <= u16::MAX.into());
        self.indexes[height].write(index as u16);
    }

    /// Sets the bottom's and the root's heights.
    fn set_heights(&mut self, bottom: usize, root_height: usize) {
        debug_assert!(bottom <= root_height + 1 && root_height <= MAX_HEIGHT);
        self.bottom = bottom as u8;
        self.root_height = root_height as u8;
    }

    /// The height of the node the path ends at.
    pub(crate) fn height(&self) -> usize {
        usize::from(self.bottom)
    }

    /// The root's height.
    pub(crate) fn root_height(&self) -> usize {
        usize::from(self.root_height)
    }

    /// Whether the path reaches a node: it was attached to a root.
    pub(crate) fn is_attached(&self) -> bool {
        self.bottom <= self.root_height
    }

    /// Whether `height` is on the path.
    fn is_on_path(&self, height: usize) -> bool {
        self.height() <= height && height <= self.root_height()
    }

    /// The node at `height`, which must be on the path.
    fn node_ptr(&self, height: usize) -> NodePtr<K, V, S> {
        assert!(
            self.is_on_path(height),
            "height {height} is not on the path"
        );
        // SAFETY: the levels on the path are initialized (type invariant).
        unsafe { self.nodes[height].assume_init() }
    }

    /// The node at `height`, which must be on the path.
    pub(crate) fn node(&self, height: usize) -> Node<K, V, S> {
        Node::at(self.node_ptr(height), height)
    }

    /// The node at `height` above the leaves, internal, as the caller
    /// knows, which debug builds check.
    ///
    /// # Panics
    ///
    /// If `height` is not on the path.
    pub(crate) fn internal(&self, height: usize) -> Internal<K, V, S> {
        debug_assert!(height > 0, "the node at height 0 is a leaf");
        Internal::at(self.node_ptr(height), height)
    }

    /// The edge of a leaf the path ends at: where a walk's cursor stands
    /// once started, read with no check on its way.
    ///
    /// # Safety
    ///
    /// The path is attached and ends at an edge of a leaf: at height 0, at
    /// an index no greater than the leaf's length.
    #[inline(always)]
    pub(crate) unsafe fn leaf_edge(&self) -> (Leaf<K, V, S>, usize) {
        debug_assert!(
            self.is_attached() && self.bottom == 0,
            "the path ends at no leaf"
        );
        // SAFETY: the bottom level, at height 0, is on the path and
        // initialized (caller and type invariant), and holds a leaf.
        unsafe {
            (
                Leaf::at(self.nodes[0].assume_init()),
                usize::from(self.indexes[0].assume_init()),
            )
        }
    }

    /// The index the path takes at `height`: above the bottom, the edge that
    /// leads down the path.
    ///
    /// # Panics
    ///
    /// If `height` is not on the path.
    pub(crate) fn index_at(&self, height: usize) -> usize {
        assert!(
            self.is_on_path(height),
            "height {height} is not on the path"
        );
        // SAFETY: the levels on the path are initialized (type invariant).
        usize::from(unsafe { self.indexes[height].assume_init() })
    }

    /// The position the path ends at in its bottom node: an edge or a key.
    ///
    /// # Panics
    ///
    /// If the path is detached.
    pub(crate) fn index(&self) -> usize {
        self.index_at(self.height())
    }

    /// Makes `index` the position the path ends at in its bottom node: an
    /// edge, or a key, which its uses check.
    ///
    /// # Panics
    ///
    /// If the path is detached.
    pub(crate) fn set_index(&mut self, index: usize) {
        let bottom = self.height();
        assert!(self.is_on_path(bottom), "the path is detached");
        self.write_index(bottom, index);
    }

    /// Makes `index` the position the path ends at in its leaf, where a
    /// walk's cursor stands: the cursor's own move, which checks nothing.
    /// A path that ends elsewhere gets a wrong position, but no undefined
    /// behavior.
    pub(crate) fn set_leaf_index(&mut self, index: usize) {
        debug_assert!(
            self.is_attached() && self.bottom == 0,
            "the path ends at no leaf"
        );
        self.write_index(0, index);
    }

    /// Goes up to the bottom node's parent, at the edge that leads to it.
    ///
    /// # Panics
    ///
    /// At the root.
    pub(crate) fn ascend(&mut self) {
        assert!(self.bottom < self.root_height, "ascending from the root");
        self.bottom += 1;
    }

    /// Whether `self` and `other`, paths in the same tree, end at the same
    /// position of the same node. A range checks that both ends have
    /// started before calling this on each step.
    ///
    /// # Safety
    ///
    /// Both paths are attached.
    pub(crate) unsafe fn is_at(&self, other: &Self) -> bool {
        debug_assert!(
            self.is_attached() && other.is_attached(),
            "comparing a detached path"
        );
        let bottom = self.height();
        // SAFETY: both paths are attached (caller), so their bottom levels
        // are on the path and initialized (type invariant); `bottom` is
        // `other`'s too once the first comparison holds.
        bottom == other.height()
            && unsafe {
                self.indexes[bottom].assume_init() == other.indexes[bottom].assume_init()
                    && self.nodes[bottom]
                        .assume_init()
                        .is(other.nodes[bottom].assume_init())
            }
    }

    /// How the position `self` ends at compares with `other`'s, in the same
    /// tree, both at the leaves: in key order, by the indexes from the root
    /// down.
    pub(crate) fn cmp_position(&self, other: &Self) -> Ordering {
        debug_assert!(self.bottom == 0 && other.bottom == 0);
        let own = (0..=self.root_height())
            .rev()
            .map(|height| self.index_at(height));
        let others = (0..=other.root_height())
            .rev()
            .map(|height| other.index_at(height));
        own.cmp(others)
    }

    /// The same path, with the access `B2` to the tree.
    ///
    /// # Safety
    ///
    /// The path has access `B2` to the tree for as long as the result lives.
    unsafe fn with_borrow<B2>(&self) -> Path<B2, K, V, S> {
        Path {
            nodes: self.nodes,
            indexes: self.indexes,
            bottom: self.bottom,
            root_height: self.root_height,
            _borrow: PhantomData,
        }
    }
}

impl<B, K, V, S: InlineKeyStrategy<K>> Path<B, K, V, S> {
    /// Goes down the edge the path ends at, to the child there, at its first
    /// edge.
    ///
    /// # Panics
    ///
    /// If the bottom node is a leaf, or has no such edge.
    pub(crate) fn descend(&mut self) {
        let height = self.height();
        assert!(height > 0, "descending from a leaf");
        let child = self.internal(height).child(self.index()).node_ptr();
        self.write_level(height - 1, child, 0);
        self.bottom -= 1;
    }

    /// Goes down from the edge the path ends at to a leaf, along the `End`
    /// edge of every node below, and ends at that leaf's `End` edge. At a
    /// leaf already, stays.
    ///
    /// The node and its height stay in registers on the way down; the path
    /// only takes stores, which nothing reads until it is done.
    ///
    /// # Panics
    ///
    /// If the bottom node has no such edge.
    pub(crate) fn descend_to_leaf<End: NodeEnd>(&mut self) {
        let mut height = self.height();
        let mut edge = self.index();
        let mut node = self.node(height);
        while let Node::Internal(internal) = node {
            node = internal.child(edge);
            height -= 1;
            edge = End::edge(node);
            self.write_level(height, node.node_ptr(), edge);
        }
        self.bottom = height as u8;
    }

    /// Goes down from the bottom node to the first key under it, at the
    /// start of its leftmost leaf; returns `false`, ending at that leaf's
    /// edge, if the leaf has no key, which only an empty tree's root has.
    pub(crate) fn descend_to_first_key(&mut self) -> bool {
        self.set_index(0);
        self.descend_to_leaf::<FirstEdge>();
        self.node(self.height()).len() > 0
    }

    /// Goes down from the bottom node to the last key under it, at the end
    /// of its rightmost leaf; returns `false`, ending at that leaf's edge, if
    /// the leaf has no key, which only an empty tree's root has.
    pub(crate) fn descend_to_last_key(&mut self) -> bool {
        let last_edge = self.node(self.height()).len();
        self.set_index(last_edge);
        self.descend_to_leaf::<LastEdge>();
        match self.index().checked_sub(1) {
            Some(last_key) => {
                self.set_index(last_key);
                true
            }
            None => false,
        }
    }

    /// Goes down from the bottom node, the root of a tree whose keys all
    /// share `shared` (D50), searching each node on the way for `key`, and
    /// returns whether it is there: the path ends at it then, in whichever
    /// node holds it, or else at the leaf edge where it belongs. A key that
    /// lacks what every key shares belongs at the tree's first or last edge,
    /// which the path goes to without searching.
    ///
    /// The node and its height stay in registers on the way down; the path
    /// only takes stores, which nothing reads until it is done. The root's
    /// kind is decided once, and each internal node's children's, as the
    /// way down reads an edge; the loop runs over internal nodes only, and
    /// the leaf's search follows it.
    pub(crate) fn search<Q>(&mut self, key: &SearchKey<'_, K, S, Q>, shared: &SharedPrefix) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        match shared.place(key) {
            Some(Ordering::Less) => {
                self.set_index(0);
                self.descend_to_leaf::<FirstEdge>();
                return false;
            }
            Some(Ordering::Greater) => {
                let len = self.node(self.height()).len();
                self.set_index(len);
                self.descend_to_leaf::<LastEdge>();
                return false;
            }
            Some(Ordering::Equal) | None => {}
        }
        let mut height = self.height();
        let leaf = match self.node(height) {
            Node::Leaf(leaf) => leaf,
            Node::Internal(mut internal) => loop {
                let edge = match internal.search(key) {
                    Ok(index) => {
                        self.write_index(height, index);
                        self.bottom = height as u8;
                        return true;
                    }
                    Err(edge) => edge,
                };
                self.write_index(height, edge);
                // SAFETY: a search returns an edge `<= len`.
                let child = unsafe { internal.child_unchecked(edge) };
                height -= 1;
                self.nodes[height].write(child.node_ptr());
                match child {
                    Node::Leaf(leaf) => break leaf,
                    Node::Internal(child) => internal = child,
                }
            },
        };
        let (found, index) = match leaf.search(key) {
            Ok(index) => (true, index),
            Err(index) => (false, index),
        };
        self.write_index(height, index);
        self.bottom = height as u8;
        found
    }

    /// Goes down from the bottom node to the leaf edge just before or just
    /// after `key`, or, if no entry has that key, where it would be. A
    /// separator stands between the end of the leaf left of it and the start
    /// of the leaf right of it (D17).
    pub(crate) fn search_to_edge<Q>(
        &mut self,
        key: &SearchKey<'_, K, S, Q>,
        side: KeySide,
        shared: &SharedPrefix,
    ) where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if !self.search(key, shared) {
            return;
        }
        let index = self.index();
        match side {
            KeySide::Before => {
                self.set_index(index);
                self.descend_to_leaf::<LastEdge>();
            }
            KeySide::After => {
                self.set_index(index + 1);
                self.descend_to_leaf::<FirstEdge>();
            }
        }
    }
}

/// Where a path stands relative to a key it looked up.
#[derive(Clone, Copy)]
pub(crate) enum KeySide {
    /// Just before the key, or where it would be.
    Before,
    /// Just after the key, or where it would be.
    After,
}

impl<'a, K: 'a, V: 'a, S: InlineKeyStrategy<K>> Path<Immut<'a>, K, V, S> {
    /// The key the path ends at and its value: beside it in a leaf, or for
    /// a separator, the head of the leftmost leaf right of it (D17).
    ///
    /// # Panics
    ///
    /// If the path ends at no key.
    pub(crate) fn key_value(&self) -> (&'a K, &'a V) {
        let index = self.index();
        match self.node(self.height()) {
            Node::Leaf(leaf) => leaf.key_value(index),
            Node::Internal(internal) => {
                (internal.key(index), internal.head_leaf(index).head_value())
            }
        }
    }
}

impl<B: CopyableBorrow, K, V, S> Path<B, K, V, S> {
    /// The same path, with shared access for as long as `self` is borrowed.
    pub(crate) fn reborrow(&self) -> Path<Immut<'_>, K, V, S> {
        // SAFETY: a copyable borrow gives at least shared access to the
        // structure and keys, and `&self` keeps the tree unchanged meanwhile.
        // Reading a value through it is sound only for values not handed out
        // as `&mut`, which the walks see to.
        unsafe { self.with_borrow() }
    }
}

/// An exclusive path, from `root` down: what an entry keeps, and what
/// inserts and removals go back up. It changes the nodes below the root
/// through the bottom node, and the root only through [`Self::push_root`]
/// and [`Self::detach_popping_an_empty_root`].
///
/// It is made detached, and starts where it is: every way of starting it
/// first attaches it to the root.
pub(crate) struct PathMut<'a, K, V, S> {
    root: &'a mut Root<K, V, S>,
    path: Path<Mut<'a, K, V>, K, V, S>,
}

impl<'a, K, V, S: InlineKeyStrategy<K>> PathMut<'a, K, V, S> {
    /// A detached path down the tree of `root`, for one of the methods that
    /// start it. It ends the owner's permission to append to its remembered
    /// leaf, which the path may change.
    #[inline(always)]
    pub(crate) fn new(root: &'a mut Root<K, V, S>) -> Self {
        root.clear_insertion_hint();
        Self {
            root,
            path: Path::detached(),
        }
    }

    /// The root the path goes down from, shared.
    pub(crate) fn root(&self) -> &Root<K, V, S> {
        self.root
    }

    /// Makes what every key of the tree shares cover `key`, before the path
    /// inserts it; see [`Root::cover_key`]. The path still leads where it
    /// did: only the prefixes along the tree's borders change.
    pub(crate) fn cover_key(&mut self, key: &K) {
        self.root.cover_key(key);
    }

    /// Finishes a plain insertion, remembering its leaf only when the path
    /// proves the new entry is physically last. This consumes the path:
    /// entry handles that keep their path may still remove or merge nodes,
    /// so they must never publish the hint while retaining mutation access.
    /// No ordinary fitting search needs to track ancestors for this proof.
    pub(crate) fn finish_insertion(self) {
        if self.path.height() != 0 {
            return;
        }
        let leaf = self.leaf();
        if leaf.len() == 0 || self.path.index() != leaf.len() - 1 {
            return;
        }
        for height in 1..=self.path.root_height() {
            let node = self.path.internal(height);
            if self.path.index_at(height) != node.len() {
                return;
            }
        }
        // SAFETY: every ancestor takes its final edge, so this is the
        // physically last leaf. `self` is consumed and lends no handles;
        // future exclusive access goes through the owner's invalidation.
        unsafe { self.root.remember_rightmost_leaf(leaf) };
    }

    /// Starts the path at the root, at its first edge.
    pub(crate) fn start_at_root(&mut self) {
        self.path.attach(self.root.top());
    }

    /// Starts the path at the root and searches down; see [`Path::search`].
    pub(crate) fn search<Q>(&mut self, key: &SearchKey<'_, K, S, Q>) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.start_at_root();
        self.path.search(key, self.root.shared_prefix())
    }

    /// Starts the path at the root and goes down to the leaf edge beside
    /// `key`; see [`Path::search_to_edge`].
    pub(crate) fn search_to_edge<Q>(&mut self, key: &SearchKey<'_, K, S, Q>, side: KeySide)
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.start_at_root();
        self.path
            .search_to_edge(key, side, self.root.shared_prefix());
    }

    /// Starts the path at the root and goes down to the tree's first key;
    /// see [`Path::descend_to_first_key`].
    pub(crate) fn descend_to_first_key(&mut self) -> bool {
        self.start_at_root();
        self.path.descend_to_first_key()
    }

    /// Starts the path at the root and goes down to the tree's last key; see
    /// [`Path::descend_to_last_key`].
    pub(crate) fn descend_to_last_key(&mut self) -> bool {
        self.start_at_root();
        self.path.descend_to_last_key()
    }

    /// See [`Path::height`].
    pub(crate) fn height(&self) -> usize {
        self.path.height()
    }

    /// See [`Path::root_height`].
    pub(crate) fn root_height(&self) -> usize {
        self.path.root_height()
    }

    /// See [`Path::index`].
    pub(crate) fn index(&self) -> usize {
        self.path.index()
    }

    /// See [`Path::set_index`].
    pub(crate) fn set_index(&mut self, index: usize) {
        self.path.set_index(index);
    }

    /// See [`Path::ascend`].
    pub(crate) fn ascend(&mut self) {
        self.path.ascend();
    }

    /// See [`Path::descend`].
    pub(crate) fn descend(&mut self) {
        self.path.descend();
    }

    /// See [`Path::descend_to_leaf`].
    pub(crate) fn descend_to_leaf<End: NodeEnd>(&mut self) {
        self.path.descend_to_leaf::<End>();
    }

    /// The bottom node's bounds, low and high: the keys just left and just
    /// right of the path in the nodes above it, nearest first; `None` past
    /// the tree's first or last key.
    pub(super) fn bounds(&self) -> (Option<&K>, Option<&K>) {
        let (mut low, mut high) = (None, None);
        for height in self.path.height() + 1..=self.path.root_height() {
            let keys = self.path.internal(height).keys();
            let index = self.path.index_at(height);
            low = low.or_else(|| index.checked_sub(1).and_then(|left| keys.get(left)));
            high = high.or_else(|| keys.get(index));
            if low.is_some() && high.is_some() {
                break;
            }
        }
        (low, high)
    }

    /// The bottom node. Changing it leaves the path leading to it.
    pub(crate) fn node(&self) -> Node<K, V, S> {
        self.path.node(self.path.height())
    }

    /// The bottom node, a leaf, as where a search or a descent to a leaf
    /// ended.
    ///
    /// # Panics
    ///
    /// If the bottom node is internal.
    pub(crate) fn leaf(&self) -> Leaf<K, V, S> {
        let height = self.path.height();
        assert_eq!(height, 0, "the bottom node is internal");
        Leaf::at(self.path.node_ptr(height))
    }

    /// The bottom node, internal, as where the path went up from a node
    /// below.
    ///
    /// # Panics
    ///
    /// If the bottom node is a leaf.
    pub(crate) fn internal(&self) -> Internal<K, V, S> {
        let height = self.path.height();
        assert!(height > 0, "the bottom node is a leaf");
        self.path.internal(height)
    }

    /// Detaches the path, which a removal is done with, and gives way, if
    /// merges left the root internal and without keys, to the root's only
    /// child, handing the old root's page to `release`.
    pub(crate) fn detach_popping_an_empty_root(
        &mut self,
        release: &mut impl FnMut(PageByKind<K, V, S>),
    ) {
        self.path = Path::detached();
        if self.root.height() > 0 && self.root.top().len() == 0 {
            release(self.root.pop_internal_level().into());
        }
    }

    /// Puts a new root with no keys above the root, which the path must end
    /// at, and ends the path at the new root's only edge, which leads to the
    /// old one.
    ///
    /// # Panics
    ///
    /// If the path does not end at the root.
    pub(crate) fn push_root(&mut self, page: FreshPage<Internal<K, V, S>>) {
        assert_eq!(
            self.path.height(),
            self.path.root_height(),
            "a new root goes above a path that ends at the root"
        );
        let root = self.root.push_internal_level(page).node_ptr();
        // The old root is the new root's child at edge 0.
        let height = self.path.root_height() + 1;
        self.path.write_level(height, root, 0);
        self.path.set_heights(height, height);
    }

    /// The key the path ends at.
    ///
    /// # Panics
    ///
    /// If the path ends at no key.
    pub(crate) fn key(&self) -> &K {
        let index = self.index();
        match self.node() {
            Node::Leaf(leaf) => leaf.key(index),
            Node::Internal(internal) => internal.key(index),
        }
    }

    /// Puts `key` in place of the key the path ends at, with its inline key
    /// at the node's prefix length, and returns the old key: its value, and
    /// the path, stay. Keeping the keys in order is the caller's business.
    ///
    /// # Panics
    ///
    /// If the path ends at no key.
    pub(crate) fn replace_key(&mut self, key: K) -> K {
        let index = self.index();
        let node = self.node();
        let key = InlinedKey::new(key, node.prefix_len());
        match node {
            Node::Leaf(leaf) => leaf.replace_key(index, key),
            Node::Internal(internal) => internal.replace_key(index, key),
        }
    }

    /// The value of the key the path ends at: beside it in a leaf, or for a
    /// separator, a head below (D17).
    ///
    /// # Panics
    ///
    /// If the path ends at no key.
    pub(crate) fn value(&self) -> &V {
        let index = self.index();
        match self.node() {
            Node::Leaf(leaf) => leaf.key_value(index).1,
            Node::Internal(internal) => internal.head_leaf(index).head_value(),
        }
    }

    /// The value of the key the path ends at, exclusive; see
    /// [`Self::value`].
    pub(crate) fn value_mut(&mut self) -> &mut V {
        let index = self.index();
        match self.node() {
            Node::Leaf(leaf) => leaf.value_mut(index),
            Node::Internal(internal) => internal.head_leaf(index).head_value_mut(),
        }
    }

    /// The value of the key the path ends at, exclusive for the rest of
    /// `'a`, giving up the path; see [`Self::value`].
    pub(crate) fn into_value_mut(self) -> &'a mut V {
        let index = self.index();
        match self.node() {
            Node::Leaf(leaf) => leaf.value_mut(index),
            Node::Internal(internal) => internal.head_leaf(index).head_value_mut(),
        }
    }
}
