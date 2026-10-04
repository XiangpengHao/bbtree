//! Paths down a tree: the node at each height, from the root down to a
//! position in one of them, and the index taken at each (D11, D24). Every
//! walk, entry, insert and removal goes by one. The iterators' walks move two
//! of them through the tree (`walk.rs`), an entry keeps the one its search
//! found (`map/entry.rs`), and inserting and removing go back up the one
//! that led to the key, splitting or fixing the nodes on it
//! (`map/insert.rs`, `map/remove.rs`).
//!
//! A path stands for a borrow of the whole tree, as a handle to its root
//! would: shared ([`Immut`]), value-mutating ([`ValMut`]) or exclusive
//! ([`Mut`]). It keeps its nodes as pointers, not handles, because exclusive
//! handles to a node and to its parent can't both be live, and makes handles
//! from them as they are needed: copies, for the borrow types whose handles
//! copy, and for an exclusive path ([`PathMut`]), one handle to its bottom
//! node at a time.
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
//! This is the tree's path module (D14), the one module besides `page.rs`,
//! `slots.rs` and `handle.rs` with `unsafe` code, besides the walks'
//! handing out of values (`walk.rs`). Every handle it makes from a pointer
//! relies on the [`Path`] invariant, which holds because:
//!
//! - a path starts at a root, and every node below it on the path was read
//!   from its parent's edge when the path moved down to it;
//! - only the bottom node can change while a path lives: a copyable borrow
//!   changes no structure, and an exclusive path lends a handle only to its
//!   bottom node, from which nothing leads back up;
//! - the root changes only through an exclusive path, which updates itself
//!   ([`PathMut::push_root`]).

use core::borrow::Borrow;
use core::cmp::Ordering;
use core::marker::PhantomData;
use core::mem::MaybeUninit;

use super::handle::{
    CopyableBorrow, Dying, Edge, ForceResult, Immut, Internal, Kv, Leaf, LeafOrInternal, Mut,
    NodeKind, NodeMut, NodeRef, SearchResult, Subtree, ValMut,
};
use super::inline_keys::SearchKey;
use super::page::{FreshPage, NodePtr, PageByKind, PageKind};
use crate::inline_key::InlineKeyStrategy;

/// The most levels above the leaves that a tree can have. A non-root
/// internal node has at least 5 children (D13, D22), the root at least 2,
/// and a leaf takes at least 1 KB (D15), so a tree one level taller would
/// need at least 2·5^23 KB of leaves, more than 2^64 bytes.
pub(crate) const MAX_HEIGHT: usize = 23;

/// An end of a node, where a path goes down to the first of its entries,
/// or to the last: [`FirstEdge`] or [`LastEdge`]. A type, so that code for
/// one end, which a caller always knows, holds no choice between them.
pub(crate) trait NodeEnd {
    /// Whether this is the last edge.
    const IS_LAST: bool;

    /// This end's edge in `node`: 0, or its length, the only read of it.
    fn edge<B, K, V, S, Kind: NodeKind>(node: &NodeRef<B, K, V, S, Kind>) -> usize {
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
/// the access to the whole tree that borrow type `B` gives a handle to its
/// root (see [`NodeRef`]); or a detached path, which reaches no node until
/// it is attached to a root.
///
/// Invariant: `bottom <= root_height + 1` and `root_height <= MAX_HEIGHT`;
/// the path is detached exactly when `bottom > root_height`. For every
/// height `h` from `bottom` to `root_height`, `nodes[h]` and `indexes[h]` are
/// initialized, and `nodes[h]` holds a node at height `h` of one tree, the
/// node at `root_height` its root. For every `h` above `bottom`, `indexes[h]`
/// is an edge of `nodes[h]`, and `nodes[h - 1]` is the child at that edge.
/// `indexes[bottom]` is the position the path ends at in the bottom node, an
/// edge or a key, which every use checks against the node's length. A node
/// holds at most 1023 keys (`layout.rs`), so every index fits in a `u16`.
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

// SAFETY: a path has the access its borrow type gives a handle to the root,
// and nothing more, so it may cross threads as such a handle may: see the
// `Send` and `Sync` impls of `NodeRef`, whose bounds these are.
unsafe impl<K: Sync, V: Sync, S> Send for Path<Immut<'_>, K, V, S> {}
// SAFETY: as for `Send`.
unsafe impl<K: Sync, V: Sync, S> Sync for Path<Immut<'_>, K, V, S> {}
// SAFETY: as for `Send` of `Path<Immut>`.
unsafe impl<K: Send, V: Send, S> Send for Path<ValMut<'_, K, V>, K, V, S> {}
// SAFETY: as for `Send` of `Path<Immut>`.
unsafe impl<K: Sync, V: Sync, S> Sync for Path<ValMut<'_, K, V>, K, V, S> {}
// SAFETY: an exclusive path stands for an exclusive borrow of the tree, as a
// `&mut` would: sending it moves that access to another thread, which
// `K: Send` and `V: Send` allow; through `&` it only reads.
unsafe impl<K: Send, V: Send, S> Send for Path<Mut<'_, K, V>, K, V, S> {}
// SAFETY: as for `Send`.
unsafe impl<K: Sync, V: Sync, S> Sync for Path<Mut<'_, K, V>, K, V, S> {}
// SAFETY: a dying path stands for ownership of the tree's keys and values,
// as a `Box<(K, V)>` would (see `NodeRef<Dying>`).
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

    /// Makes the path just `root`, at `height`, ending at its first edge.
    ///
    /// # Safety
    ///
    /// `root` holds the root of a well-formed tree at `height`, to which the
    /// path has the access `B` gives a handle to it, for as long as the path
    /// lives.
    unsafe fn attach_raw(&mut self, root: NodePtr<K, V, S>, height: usize) {
        self.write_level(height, root, 0);
        self.set_heights(height, height);
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
    /// position of the same node. Called on every step of a range, whose
    /// ends are attached from the start, so it checks nothing.
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
    /// Shared access to the node at `height`, which must be on the path, for
    /// a lifetime the caller picks: to find the way, which reads the
    /// structure and keys but no value.
    ///
    /// # Safety
    ///
    /// The tree keeps its structure and keys for `'v`: nothing lends an
    /// exclusive handle into it meanwhile.
    unsafe fn view<'v>(&self, height: usize) -> NodeRef<Immut<'v>, K, V, S, LeafOrInternal> {
        let node = self.node_ptr(height);
        // SAFETY: the node is on the path (type invariant), every borrow type
        // gives at least shared access to the structure and keys, and they
        // stay unchanged for `'v` (caller).
        unsafe { NodeRef::from_raw_parts(node, height) }
    }

    /// [`Self::view`] of a node above height 0, which is internal.
    ///
    /// # Safety
    ///
    /// As for `view`, and `height > 0`.
    unsafe fn internal_view<'v>(&self, height: usize) -> NodeRef<Immut<'v>, K, V, S, Internal> {
        debug_assert!(height > 0, "the node at height 0 is a leaf");
        let node = self.node_ptr(height);
        // SAFETY: as for `view`; the node is above height 0 (caller), so
        // internal (type invariant).
        unsafe { NodeRef::from_raw_parts(node, height) }
    }

    /// Goes down the edge the path ends at, to the child there, at its first
    /// edge.
    ///
    /// # Panics
    ///
    /// If the bottom node is a leaf, or has no such edge.
    pub(crate) fn descend(&mut self) {
        let height = self.height();
        assert!(height > 0, "descending from a leaf");
        // SAFETY: the view is gone before the path changes, and the bottom
        // node is above height 0.
        let node = unsafe { self.internal_view(height) };
        let child = node.edge_at(self.index()).descend().node_ptr();
        self.write_level(height - 1, child, 0);
        self.bottom -= 1;
    }

    /// Goes down from the edge the path ends at to a leaf, along the `end`
    /// edge of every node below, and ends at that leaf's `end` edge. At a
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
        // SAFETY: the path only records where it goes while the view and
        // the handles below it live; the tree is unchanged.
        let mut node = unsafe { self.view(height) };
        while let ForceResult::Internal(internal) = node.force() {
            node = internal.edge_at(edge).descend();
            height -= 1;
            edge = End::edge(&node);
            self.write_level(height, node.node_ptr(), edge);
        }
        self.bottom = height as u8;
    }

    /// Goes down from the bottom node to its subtree's first key, at the
    /// start of its leftmost leaf; returns `false`, ending at that leaf's
    /// edge, if the leaf has no key, which only an empty tree's root has.
    pub(crate) fn descend_to_first_key(&mut self) -> bool {
        self.set_index(0);
        self.descend_to_leaf::<FirstEdge>();
        // SAFETY: the view is gone before the path changes.
        unsafe { self.view(self.height()) }.len() > 0
    }

    /// Goes down from the bottom node to its subtree's last key, at the end
    /// of its rightmost leaf; returns `false`, ending at that leaf's edge, if
    /// the leaf has no key, which only an empty tree's root has.
    pub(crate) fn descend_to_last_key(&mut self) -> bool {
        // SAFETY: the view is gone before the path changes.
        let last_edge = unsafe { self.view(self.height()) }.len();
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

    /// Goes down from the bottom node, searching each node on the way for
    /// `key`, and returns whether it is there: the path ends at it then, in
    /// whichever node holds it, or else at the leaf edge where it belongs.
    pub(crate) fn search<Q>(&mut self, key: &SearchKey<'_, K, S, Q>) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.search_with::<Q, PlainSearch>(key)
    }

    /// [`Self::search`] with each node searched expecting the key to go
    /// after its last, as inserts do (`NodeRef::search_expecting_append`).
    pub(crate) fn search_expecting_append<Q>(&mut self, key: &SearchKey<'_, K, S, Q>) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.search_with::<Q, AppendingSearch>(key)
    }

    /// Goes down from the bottom node to the leaf edge just before or just
    /// after `key`, or, if no entry has that key, where it would be. A
    /// separator stands between the end of the leaf left of it and the start
    /// of the leaf right of it (D17).
    pub(crate) fn search_to_edge<Q>(&mut self, key: &SearchKey<'_, K, S, Q>, side: KeySide)
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if !self.search(key) {
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

    /// The search, with the node and its height in registers on the way
    /// down; the path only takes stores, which nothing reads until it is
    /// done. Each level decides its node's kind once, first, and searches it
    /// as a node of that kind, with `Search`, which the whole search uses.
    fn search_with<Q, Search: NodeSearch>(&mut self, key: &SearchKey<'_, K, S, Q>) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let mut height = self.height();
        // SAFETY: the path only records where it goes while the view and the
        // handles below it live; the tree is unchanged.
        let mut node = unsafe { self.view(height) };
        loop {
            let internal = match node.force() {
                ForceResult::Leaf(leaf) => {
                    let (found, index) = match Search::search(leaf, key) {
                        SearchResult::Found(kv) => (true, kv.index()),
                        SearchResult::GoDown(edge) => (false, edge.index()),
                    };
                    self.write_index(height, index);
                    self.bottom = height as u8;
                    return found;
                }
                ForceResult::Internal(internal) => internal,
            };
            let edge = match Search::search(internal, key) {
                SearchResult::Found(kv) => {
                    self.write_index(height, kv.index());
                    self.bottom = height as u8;
                    return true;
                }
                SearchResult::GoDown(edge) => edge,
            };
            self.write_index(height, edge.index());
            node = edge.descend();
            height -= 1;
            self.nodes[height].write(node.node_ptr());
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

/// How a path's search searches each node on the way down: chosen once per
/// search, as a type, so that the loop down holds no choice.
trait NodeSearch {
    fn search<'n, K, V, S: InlineKeyStrategy<K>, Kind: PageKind, Q>(
        node: NodeRef<Immut<'n>, K, V, S, Kind>,
        key: &SearchKey<'_, K, S, Q>,
    ) -> SearchResult<NodeRef<Immut<'n>, K, V, S, Kind>>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized;
}

/// Searches each node with [`NodeRef::search`].
enum PlainSearch {}

impl NodeSearch for PlainSearch {
    fn search<'n, K, V, S: InlineKeyStrategy<K>, Kind: PageKind, Q>(
        node: NodeRef<Immut<'n>, K, V, S, Kind>,
        key: &SearchKey<'_, K, S, Q>,
    ) -> SearchResult<NodeRef<Immut<'n>, K, V, S, Kind>>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        node.search(key)
    }
}

/// Searches each node with [`NodeRef::search_expecting_append`], as
/// inserts do.
enum AppendingSearch {}

impl NodeSearch for AppendingSearch {
    fn search<'n, K, V, S: InlineKeyStrategy<K>, Kind: PageKind, Q>(
        node: NodeRef<Immut<'n>, K, V, S, Kind>,
        key: &SearchKey<'_, K, S, Q>,
    ) -> SearchResult<NodeRef<Immut<'n>, K, V, S, Kind>>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        node.search_expecting_append(key)
    }
}

impl<B: CopyableBorrow, K, V, S: InlineKeyStrategy<K>> Path<B, K, V, S> {
    /// Makes the path just the root, ending at its first edge, with the
    /// access `root` had.
    pub(crate) fn attach(&mut self, root: NodeRef<B, K, V, S, LeafOrInternal>) {
        // SAFETY: a handle to the root has access `B` to the tree, which its
        // copies share, and a copy is what the path keeps.
        unsafe { self.attach_raw(root.node_ptr(), root.height()) }
    }

    /// The node at `height`, with the path's access.
    ///
    /// # Panics
    ///
    /// If `height` is not on the path.
    pub(crate) fn node(&self, height: usize) -> NodeRef<B, K, V, S, LeafOrInternal> {
        let node = self.node_ptr(height);
        // SAFETY: the node is on the path (type invariant), with the access
        // `B` gives, which a copy of a handle shares.
        unsafe { NodeRef::from_raw_parts(node, height) }
    }

    /// The node at `height` above the leaves, internal, with the path's
    /// access.
    ///
    /// # Panics
    ///
    /// If `height` is 0 or not on the path.
    pub(crate) fn internal(&self, height: usize) -> NodeRef<B, K, V, S, Internal> {
        assert!(height > 0, "the node at height 0 is a leaf");
        let node = self.node_ptr(height);
        // SAFETY: the node is on the path (type invariant), above height 0,
        // so internal, with the access `B` gives, which a copy of a handle
        // shares.
        unsafe { NodeRef::from_raw_parts(node, height) }
    }

    /// The edge of a leaf the path ends at, with the path's access: where a
    /// walk's cursor stands once started, read with no check on its way.
    ///
    /// # Safety
    ///
    /// The path is attached and ends at an edge of a leaf: at height 0, at
    /// an index no greater than the leaf's length.
    pub(crate) unsafe fn leaf_edge(&self) -> Edge<NodeRef<B, K, V, S, Leaf>> {
        debug_assert!(
            self.is_attached() && self.bottom == 0,
            "the path ends at no leaf"
        );
        // SAFETY: the bottom level, at height 0, is on the path and
        // initialized (caller and type invariant), and holds a leaf, with the
        // access `B` gives, which a copy of a handle shares; its index is an
        // edge of it (caller).
        unsafe {
            let leaf: NodeRef<B, K, V, S, Leaf> =
                NodeRef::from_raw_parts(self.nodes[0].assume_init(), 0);
            leaf.edge_at_unchecked(usize::from(self.indexes[0].assume_init()))
        }
    }

    /// The key the path ends at, with the path's access.
    ///
    /// # Panics
    ///
    /// If the path ends at no key.
    pub(crate) fn kv(&self) -> Kv<NodeRef<B, K, V, S, LeafOrInternal>> {
        self.node(self.height()).kv_at(self.index())
    }

    /// The same path, with shared access for as long as `self` is borrowed.
    pub(crate) fn reborrow(&self) -> Path<Immut<'_>, K, V, S> {
        // SAFETY: a copyable borrow gives at least shared access to the
        // structure and keys, and `&self` keeps the tree unchanged meanwhile.
        // Like `NodeRef::reborrow`, reading a value through it is sound only
        // for values not handed out as `&mut`, which the walks see to.
        unsafe { self.with_borrow() }
    }
}

/// An exclusive path, from the root of `tree` down: what an entry keeps, and
/// what inserts and removals go back up. It lends one handle at a time, to
/// its bottom node, and changes the root only through [`Self::push_root`].
///
/// It is made detached, and starts where it is: every way of starting it
/// first attaches it to the root.
pub(crate) struct PathMut<'a, K, V, S> {
    tree: &'a mut Subtree<K, V, S>,
    path: Path<Mut<'a, K, V>, K, V, S>,
}

impl<'a, K, V, S: InlineKeyStrategy<K>> PathMut<'a, K, V, S> {
    /// A detached path in `tree`, for one of the methods that start it.
    #[inline(always)]
    pub(crate) fn new(tree: &'a mut Subtree<K, V, S>) -> Self {
        Self {
            tree,
            path: Path::detached(),
        }
    }

    /// Starts the path at the root, at its first edge.
    pub(crate) fn start_at_root(&mut self) {
        let root = Subtree::borrow(self.tree).node_ptr();
        let height = self.tree.height();
        // SAFETY: `root` is the tree's root, at `height`, and the path keeps
        // the exclusive borrow of the tree for `'a`.
        unsafe { self.path.attach_raw(root, height) };
    }

    /// Starts the path at the root and searches down; see [`Path::search`].
    pub(crate) fn search<Q>(&mut self, key: &SearchKey<'_, K, S, Q>) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.start_at_root();
        self.path.search(key)
    }

    /// Starts the path at the root and searches down; see
    /// [`Path::search_expecting_append`].
    pub(crate) fn search_expecting_append<Q>(&mut self, key: &SearchKey<'_, K, S, Q>) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.start_at_root();
        self.path.search_expecting_append(key)
    }

    /// Starts the path at the root and goes down to the leaf edge beside
    /// `key`; see [`Path::search_to_edge`].
    pub(crate) fn search_to_edge<Q>(&mut self, key: &SearchKey<'_, K, S, Q>, side: KeySide)
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.start_at_root();
        self.path.search_to_edge(key, side);
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
            // SAFETY: `&self` keeps the path from lending an exclusive
            // handle while the keys are borrowed, and `height` is above the
            // bottom, so above 0.
            let keys = unsafe { self.path.internal_view(height) }.keys();
            let index = self.path.index_at(height);
            low = low.or_else(|| index.checked_sub(1).and_then(|left| keys.get(left)));
            high = high.or_else(|| keys.get(index));
            if low.is_some() && high.is_some() {
                break;
            }
        }
        (low, high)
    }

    /// [`Self::view`] of a bottom node that is internal, as where the path
    /// went up from a node below.
    ///
    /// # Panics
    ///
    /// If the bottom node is a leaf.
    pub(crate) fn internal(&self) -> NodeRef<Immut<'_>, K, V, S, Internal> {
        let height = self.path.height();
        assert!(height > 0, "the bottom node is a leaf");
        // SAFETY: `&self` keeps the path from lending an exclusive handle
        // while the view lives, and the bottom node is above height 0.
        unsafe { self.path.internal_view(height) }
    }

    /// Shared access to the bottom node, for as long as `self` is borrowed.
    pub(crate) fn view(&self) -> NodeRef<Immut<'_>, K, V, S, LeafOrInternal> {
        // SAFETY: `&self` keeps the path from lending an exclusive handle
        // while the view lives.
        unsafe { self.path.view(self.path.height()) }
    }

    /// Exclusive access to the bottom node and its subtree, for as long as
    /// `self` is borrowed. Nothing it does reaches the nodes above, so the
    /// path still leads to the bottom node afterward, whatever changed in
    /// it.
    pub(crate) fn node_mut(&mut self) -> NodeMut<'_, K, V, S, LeafOrInternal> {
        let height = self.path.height();
        let node = self.path.node_ptr(height);
        // SAFETY: the bottom node is on the path (type invariant), which has
        // the exclusive borrow of the tree. `&mut self` keeps this handle the
        // only one while it lives, and handles never lead back up, so the
        // nodes above stay unchanged.
        unsafe { NodeRef::from_raw_parts(node, height) }
    }

    /// [`Self::node_mut`] of a bottom node that is a leaf, as where a search
    /// or a descent to a leaf ended.
    ///
    /// # Panics
    ///
    /// If the bottom node is internal.
    pub(crate) fn leaf_mut(&mut self) -> NodeMut<'_, K, V, S, Leaf> {
        let height = self.path.height();
        assert_eq!(height, 0, "the bottom node is internal");
        let node = self.path.node_ptr(height);
        // SAFETY: as for `node_mut`; the node is at height 0, so a leaf (type
        // invariant).
        unsafe { NodeRef::from_raw_parts(node, height) }
    }

    /// [`Self::node_mut`] of a bottom node that is internal, as where the
    /// path went up from a node below.
    ///
    /// # Panics
    ///
    /// If the bottom node is a leaf.
    pub(crate) fn internal_mut(&mut self) -> NodeMut<'_, K, V, S, Internal> {
        let height = self.path.height();
        assert!(height > 0, "the bottom node is a leaf");
        let node = self.path.node_ptr(height);
        // SAFETY: as for `node_mut`; the node is above height 0, so internal
        // (type invariant).
        unsafe { NodeRef::from_raw_parts(node, height) }
    }

    /// Exclusive access to the bottom node and its subtree for the rest of
    /// `'a`, giving up the path.
    pub(crate) fn into_node_mut(self) -> NodeMut<'a, K, V, S, LeafOrInternal> {
        let height = self.path.height();
        let node = self.path.node_ptr(height);
        // SAFETY: as for `node_mut`, with the path's borrow of the tree, for
        // `'a`, going to the handle.
        unsafe { NodeRef::from_raw_parts(node, height) }
    }

    /// Detaches the path, which a removal is done with, and gives way, if
    /// merges left the root internal and without keys, to the root's only
    /// child, handing the old root's page to `release`.
    pub(crate) fn detach_popping_an_empty_root(
        &mut self,
        release: &mut impl FnMut(PageByKind<K, V, S>),
    ) {
        self.path = Path::detached();
        if self.tree.height() > 0 && Subtree::borrow(self.tree).len() == 0 {
            release(self.tree.pop_internal_level().into());
        }
    }

    /// Puts a new root with no keys above the root, which the path must end
    /// at, and ends the path at the new root's only edge, which leads to the
    /// old one.
    ///
    /// # Panics
    ///
    /// If the path does not end at the root.
    pub(crate) fn push_root(&mut self, page: FreshPage<K, V, S, Internal>) {
        assert_eq!(
            self.path.height(),
            self.path.root_height(),
            "a new root goes above a path that ends at the root"
        );
        let root = self.tree.push_internal_level(page).node_ptr();
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
        match self.view().force() {
            ForceResult::Leaf(leaf) => leaf.kv_at(index).into_key(),
            ForceResult::Internal(internal) => internal.kv_at(index).into_key(),
        }
    }

    /// The value of the key the path ends at: beside it in a leaf, or for a
    /// separator, a head below (D17).
    ///
    /// # Panics
    ///
    /// If the path ends at no key.
    pub(crate) fn value(&self) -> &V {
        self.view().kv_at(self.index()).into_key_value().1
    }

    /// The value of the key the path ends at, exclusive; see
    /// [`Self::value`].
    pub(crate) fn value_mut(&mut self) -> &mut V {
        let index = self.index();
        self.node_mut().kv_at(index).into_value_mut()
    }

    /// The value of the key the path ends at, exclusive for the rest of
    /// `'a`, giving up the path; see [`Self::value`].
    pub(crate) fn into_value_mut(self) -> &'a mut V {
        let index = self.index();
        self.into_node_mut().kv_at(index).into_value_mut()
    }
}
