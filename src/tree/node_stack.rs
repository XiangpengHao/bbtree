//! Node stacks: the nodes from a tree's root down to a position in one of
//! them, and the index taken at each (D11, D24). Every range, entry and
//! removal keeps one, as do inserts that split. Iterators
//! move two of them through the tree (`walk.rs`), an entry keeps the one its
//! search found (`map/entry.rs`), and inserting and removing go back up the
//! one that led to the key, splitting or fixing the nodes on it
//! (`insert.rs`, `remove.rs`).
//!
//! A node stack stands for a borrow of the whole tree, which its borrow type
//! says: shared ([`Immut`]), value-mutating ([`ValMut`]), owning, for a
//! walk that takes the tree apart ([`Dying`]), or exclusive ([`Mut`], held
//! by a [`NodeStackMut`]). The borrow type is what the walks hand entries
//! out by (`walk.rs`), and what makes a node stack `Send` and `Sync` as the
//! borrow it stands for; the nodes it hands out are plain handles, with the
//! access of the node stack's borrow (`tree/node/mod.rs`).
//!
//! A node stack has a slot for every level of the tallest tree there can be
//! ([`MAX_DEPTH`]), about 250 bytes, the root's first. Nothing records how
//! tall a tree is: a node stack counts the levels it goes down, and every leaf
//! is as deep as any other. It is made detached and started where it is,
//! writing only the levels in use, and is not moved once written: the
//! CPU can't forward the narrow stores that wrote it to the wide loads of a
//! copy, which then wait for those stores to retire, and with them every
//! older cache miss (D23).
//!
//! # Safety argument
//!
//! Every node a node stack hands out relies on the [`NodeStack`] invariant,
//! which holds because:
//!
//! - a node stack starts at a root, and every node below it on the node stack
//!   was read from its parent's edge when the node stack moved down to it;
//! - only the bottom node changes while a node stack lives: a copyable borrow
//!   changes no structure, and an exclusive node stack changes only its bottom
//!   node, from which nothing leads back up;
//! - the root changes only through an exclusive node stack, which updates
//!   itself ([`NodeStackMut::ascend_raising`]).
//!
//! A node stack keeps each node as its edge does, tagged with its kind
//! (`node/page.rs`), so that it decides each node's kind by the pointer it
//! holds; debug builds check that only its bottom node may be a leaf.

use core::borrow::Borrow;
use core::cmp::Ordering;
use core::marker::PhantomData;
use core::mem::MaybeUninit;
use core::ops::Bound;

use super::node::Node;
use super::node::inline_keys::{SearchKey, WindowedKey};
use super::node::internal::Internal;
use super::node::leaf::Leaf;
use super::node::page::NodePtr;
use super::prefix::PrefixedKey;
use super::root::Tree;
use super::search::Found;
use super::shared_prefix::SharedPrefix;
use crate::allocator::Allocator;
use crate::inline_key::InlineKeyStrategy;

/// The deepest a leaf can be below the root. A non-root internal node has
/// at least 5 children (D13, D22), the root at least 2, and a leaf takes at
/// least 1 KB (D15), so leaves one level deeper would take at least 2·5^23
/// KB, more than 2^64 bytes.
pub(in crate::tree) const MAX_DEPTH: usize = 23;

/// Borrow type of a [`NodeStack`]: shared access for `'a` to the tree, like
/// `&'a`.
pub(crate) struct Immut<'a>(PhantomData<&'a ()>);

/// Borrow type of a [`NodeStack`]: exclusive access for `'a` to the tree, like
/// `&'a mut`.
///
/// Like `&mut`, it is invariant in `K` and `V`: a node stack that could claim
/// shorter-lived keys or values could store them into a tree that outlives
/// them.
pub(in crate::tree) struct Mut<'a, K, V>(PhantomData<&'a mut (K, V)>);

/// Borrow type of a [`NodeStack`]: access for `'a` to the tree by a walk that
/// hands out each value as `&'a mut V` at most once, while keys and
/// structure stay shared, as `IterMut` does.
///
/// Its node stacks may be copied, so that both ends of a walk hold some: they
/// only read the structure and keys. Handing out a value is unsafe, and the
/// walk vouches that it does so once per value.
///
/// Invariant in `K` and `V`, like `&'a mut`.
pub(crate) struct ValMut<'a, K, V>(PhantomData<&'a mut (K, V)>);

/// Borrow type of a [`NodeStack`]: ownership of a tree that a walk is taking
/// apart, moving entries out from either end and freeing each node once past
/// it (`tree/walk.rs`), as std's `IntoIter` does.
///
/// Its node stacks may be copied, so that both ends of the walk hold some.
/// Past the walk's ends, entries have been moved out and nodes freed, while
/// the nodes' lengths still count them: only the walk knows where the gaps
/// are. It reads only the structure between its ends. Moving an entry out and
/// freeing a node are unsafe, and the walk vouches that it does each once.
///
/// It owns `K` and `V`, and is covariant in them, as an owned tree is.
pub(in crate::tree) struct Dying<K, V>(PhantomData<(K, V)>);

/// A borrow type whose node stacks may be copied, because nothing they do
/// changes the tree's structure: [`Immut`], [`ValMut`] and [`Dying`].
pub(crate) trait CopyableBorrow {}

impl CopyableBorrow for Immut<'_> {}

impl<K, V> CopyableBorrow for ValMut<'_, K, V> {}

impl<K, V> CopyableBorrow for Dying<K, V> {}

/// An end of a node, where a node stack goes down to the first of its entries,
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

impl<K, V, S: InlineKeyStrategy<K>> Node<K, V, S> {
    /// The leaf at this node's `End` border: down its first or last edges.
    pub(in crate::tree) fn border_leaf<End: NodeEnd>(self) -> Leaf<K, V, S> {
        let mut node = self;
        loop {
            match node {
                Node::Leaf(leaf) => return leaf,
                Node::Internal(internal) => node = internal.child(End::edge(node)),
            }
        }
    }
}

/// A node stack from a tree's root down to a position in one of its nodes,
/// with the access to the whole tree that borrow type `B` says; or a detached
/// node stack, which reaches no node until it is attached to a root.
///
/// Invariant: `levels <= MAX_DEPTH + 1`, the number of nodes on the node
/// stack, 0 for a detached node stack. For every depth `d` below `levels`,
/// `nodes[d]` and `indexes[d]` are initialized, and `nodes[d]` holds a node of
/// one tree, tagged with its kind, `nodes[0]` its root. Above the bottom node,
/// at depth `levels - 1`, `nodes[d]` is internal, `indexes[d]` is an edge of
/// it, and `nodes[d + 1]` is the child at that edge. The bottom's index is
/// the position the node stack ends at in the bottom node, an edge or a key,
/// which every use checks against the node's length. A node holds at most
/// 1023 keys (`tree/node/layout.rs`), so every index fits in a `u16`.
pub(in crate::tree) struct NodeStack<B, K, V, S> {
    nodes: [MaybeUninit<NodePtr<K, V, S>>; MAX_DEPTH + 1],
    indexes: [MaybeUninit<u16>; MAX_DEPTH + 1],
    levels: u8,
    _borrow: PhantomData<B>,
}

impl<B: CopyableBorrow, K, V, S> Clone for NodeStack<B, K, V, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<B: CopyableBorrow, K, V, S> Copy for NodeStack<B, K, V, S> {}

// SAFETY: a shared node stack gives only shared access to the keys and values
// of the tree for `'a`, as a `&'a (K, V)` would, and nothing else: sending it
// to another thread shares them with that thread, which `K: Sync` and
// `V: Sync` allow. The strategy is only a marker.
unsafe impl<K: Sync, V: Sync, S> Send for NodeStack<Immut<'_>, K, V, S> {}
// SAFETY: as for `Send`: sharing a shared node stack across threads shares the
// keys and values, nothing more.
unsafe impl<K: Sync, V: Sync, S> Sync for NodeStack<Immut<'_>, K, V, S> {}
// SAFETY: a value-mutating node stack stands for an exclusive borrow of the
// whole tree, as a `&'a mut` would, which it hands out as shared keys and
// exclusive values: sending it to another thread moves that exclusive access
// there, which `K: Send` and `V: Send` allow, as for `&mut (K, V)`; the keys
// it hands out are shared only within the thread that has it. These are
// std's bounds for its `IterMut`.
unsafe impl<K: Send, V: Send, S> Send for NodeStack<ValMut<'_, K, V>, K, V, S> {}
// SAFETY: through `&` a value-mutating node stack only reads the structure,
// keys and values, which `K: Sync` and `V: Sync` allow sharing.
unsafe impl<K: Sync, V: Sync, S> Sync for NodeStack<ValMut<'_, K, V>, K, V, S> {}
// SAFETY: an exclusive node stack stands for an exclusive borrow of the tree,
// as a `&mut` would: sending it moves that access to another thread, which
// `K: Send` and `V: Send` allow; through `&` it only reads.
unsafe impl<K: Send, V: Send, S> Send for NodeStack<Mut<'_, K, V>, K, V, S> {}
// SAFETY: as for `Send`.
unsafe impl<K: Sync, V: Sync, S> Sync for NodeStack<Mut<'_, K, V>, K, V, S> {}
// SAFETY: a dying node stack stands for ownership of the tree's keys and
// values, as a `Box<(K, V)>` would: sending it to another thread sends them
// there, which `K: Send` and `V: Send` allow. The pages go back to the
// allocator the walk holds, which goes with it: the walk is `Send` only if its
// allocator is.
unsafe impl<K: Send, V: Send, S> Send for NodeStack<Dying<K, V>, K, V, S> {}
// SAFETY: as for `Send`; through `&` it only reads.
unsafe impl<K: Sync, V: Sync, S> Sync for NodeStack<Dying<K, V>, K, V, S> {}

impl<B, K, V, S> NodeStack<B, K, V, S> {
    /// A node stack that reaches no node. Making it writes nothing but its
    /// count of levels, where it is made: a struct literal, not a constant,
    /// which would be copied in whole.
    #[inline(always)]
    pub(in crate::tree) fn detached() -> Self {
        Self {
            nodes: [const { MaybeUninit::uninit() }; MAX_DEPTH + 1],
            indexes: [const { MaybeUninit::uninit() }; MAX_DEPTH + 1],
            levels: 0,
            _borrow: PhantomData,
        }
    }

    /// Makes the node stack just `root`, ending at its first edge.
    ///
    /// `root` is the root of a well-formed tree, to which the node stack has
    /// the access `B` says for as long as it lives.
    pub(in crate::tree) fn attach(&mut self, root: Node<K, V, S>) {
        self.write_level(0, root.node_ptr(), 0);
        self.levels = 1;
    }

    /// Writes the level at `depth`.
    fn write_level(&mut self, depth: usize, node: NodePtr<K, V, S>, index: usize) {
        self.nodes[depth].write(node);
        self.write_index(depth, index);
    }

    /// Writes the index of the level at `depth`.
    fn write_index(&mut self, depth: usize, index: usize) {
        debug_assert!(index <= u16::MAX.into());
        self.indexes[depth].write(index as u16);
    }

    /// How far below the root the node stack's bottom node is: 0 at the
    /// root. A detached node stack ends at no node: its depth is past every
    /// slot, which each use of it checks, as it checks any depth.
    pub(in crate::tree) fn depth(&self) -> usize {
        usize::from(self.levels).wrapping_sub(1)
    }

    /// Whether the node stack ends at the root.
    pub(in crate::tree) fn is_at_root(&self) -> bool {
        self.levels == 1
    }

    /// Whether the node stack reaches a node: it was attached to a root.
    pub(in crate::tree) fn is_attached(&self) -> bool {
        self.levels > 0
    }

    /// Whether `depth` is on the node stack.
    fn is_on_stack(&self, depth: usize) -> bool {
        depth < usize::from(self.levels)
    }

    /// The node at `depth`, which must be on the node stack, as the node stack
    /// keeps it.
    fn node_ptr(&self, depth: usize) -> NodePtr<K, V, S> {
        assert!(
            self.is_on_stack(depth),
            "depth {depth} is not on the node stack"
        );
        // SAFETY: the levels on the node stack are in the array and
        // initialized (type invariant).
        unsafe { self.nodes.get_unchecked(depth).assume_init() }
    }

    /// The node at `depth`, which must be on the node stack: internal above
    /// the bottom, which debug builds check.
    pub(in crate::tree) fn node_at(&self, depth: usize) -> Node<K, V, S> {
        let node = Node::at(self.node_ptr(depth));
        debug_assert!(
            depth == self.depth() || matches!(node, Node::Internal(_)),
            "a node stack's nodes above its bottom are internal"
        );
        node
    }

    /// The node at `depth`, internal, as the caller knows, which debug
    /// builds check.
    ///
    /// # Panics
    ///
    /// If `depth` is not on the node stack.
    pub(in crate::tree) fn internal_at(&self, depth: usize) -> Internal<K, V, S> {
        Internal::at(self.node_ptr(depth))
    }

    /// Whether the node stack's bottom node is a leaf.
    ///
    /// # Panics
    ///
    /// If the node stack is detached.
    pub(in crate::tree) fn ends_at_leaf(&self) -> bool {
        self.node_ptr(self.depth()).is_leaf()
    }

    /// The edge of a leaf the node stack ends at: where a walk's cursor stands
    /// once started, read with no check on its way.
    ///
    /// # Safety
    ///
    /// The node stack is attached and ends at an edge of a leaf: at an index
    /// no greater than the leaf's length.
    #[inline(always)]
    pub(in crate::tree) unsafe fn leaf_edge(&self) -> (Leaf<K, V, S>, usize) {
        debug_assert!(self.ends_at_leaf(), "the node stack ends at no leaf");
        let bottom = usize::from(self.levels) - 1;
        // SAFETY: the bottom level is on the node stack and initialized
        // (caller and type invariant), and holds a leaf.
        unsafe {
            (
                Leaf::at(self.nodes.get_unchecked(bottom).assume_init()),
                usize::from(self.indexes.get_unchecked(bottom).assume_init()),
            )
        }
    }

    /// The index the node stack takes at `depth`: above the bottom, the edge
    /// that leads down the node stack.
    ///
    /// # Panics
    ///
    /// If `depth` is not on the node stack.
    pub(in crate::tree) fn index_at(&self, depth: usize) -> usize {
        assert!(
            self.is_on_stack(depth),
            "depth {depth} is not on the node stack"
        );
        // SAFETY: the levels on the node stack are in the array and
        // initialized (type invariant).
        usize::from(unsafe { self.indexes.get_unchecked(depth).assume_init() })
    }

    /// The position the node stack ends at in its bottom node: an edge or a
    /// key.
    ///
    /// # Panics
    ///
    /// If the node stack is detached.
    pub(in crate::tree) fn index(&self) -> usize {
        self.index_at(self.depth())
    }

    /// Makes `index` the position the node stack ends at in its bottom node:
    /// an edge, or a key, which its uses check.
    ///
    /// # Panics
    ///
    /// If the node stack is detached.
    pub(in crate::tree) fn set_index(&mut self, index: usize) {
        self.write_index(self.depth(), index);
    }

    /// Makes `index` the position the node stack ends at in its leaf, where a
    /// walk's cursor stands: the cursor's own move, which checks nothing.
    ///
    /// # Safety
    ///
    /// The node stack is attached and ends at a leaf.
    #[inline(always)]
    pub(in crate::tree) unsafe fn set_leaf_index(&mut self, index: usize) {
        debug_assert!(self.ends_at_leaf(), "the node stack ends at no leaf");
        debug_assert!(index <= u16::MAX.into());
        let bottom = usize::from(self.levels) - 1;
        // SAFETY: an attached node stack's bottom level is in the array
        // (caller and type invariant).
        unsafe { self.indexes.get_unchecked_mut(bottom) }.write(index as u16);
    }

    /// Goes up to the bottom node's parent, at the edge that leads to it,
    /// and returns it: internal, as the node with that edge.
    ///
    /// # Panics
    ///
    /// At the root.
    pub(in crate::tree) fn ascend(&mut self) -> Internal<K, V, S> {
        assert!(self.levels > 1, "ascending from the root");
        self.levels -= 1;
        self.internal_at(usize::from(self.levels) - 1)
    }

    /// Whether `self` and `other`, node stacks in the same tree, are attached
    /// and end at the same position of the same node.
    pub(in crate::tree) fn is_at(&self, other: &Self) -> bool {
        self.is_attached()
            && self.levels == other.levels
            && self.index() == other.index()
            && self
                .node_ptr(self.depth())
                .is(other.node_ptr(other.depth()))
    }

    /// How the position `self` ends at compares with `other`'s, in the same
    /// tree, both at the leaves: in key order, by the indexes from the root
    /// down.
    pub(in crate::tree) fn cmp_position(&self, other: &Self) -> Ordering {
        debug_assert!(self.ends_at_leaf() && other.ends_at_leaf());
        let own = (0..=self.depth()).map(|depth| self.index_at(depth));
        let others = (0..=other.depth()).map(|depth| other.index_at(depth));
        own.cmp(others)
    }
}

impl<B, K, V, S: InlineKeyStrategy<K>> NodeStack<B, K, V, S> {
    /// Goes down the edge the node stack ends at, to the child there, at its
    /// first edge.
    ///
    /// # Panics
    ///
    /// If the bottom node is a leaf, or has no such edge.
    pub(in crate::tree) fn descend(&mut self) {
        let depth = self.depth();
        let Node::Internal(internal) = self.node_at(depth) else {
            panic!("descending from a leaf");
        };
        let child = internal.child(self.index()).node_ptr();
        self.write_level(depth + 1, child, 0);
        self.levels += 1;
    }

    /// Goes down from the edge the node stack ends at to a leaf, along the
    /// `End` edge of every node below, and ends at that leaf's `End` edge. At
    /// a leaf already, stays.
    ///
    /// The node and its depth stay in registers on the way down; the node
    /// stack only takes stores, which nothing reads until it is done.
    ///
    /// # Panics
    ///
    /// If the bottom node has no such edge.
    pub(in crate::tree) fn descend_to_leaf<End: NodeEnd>(&mut self) {
        let mut depth = self.depth();
        let mut edge = self.index();
        let mut node = self.node_at(depth);
        while let Node::Internal(internal) = node {
            node = internal.child(edge);
            depth += 1;
            edge = End::edge(node);
            self.write_level(depth, node.node_ptr(), edge);
        }
        self.levels = depth as u8 + 1;
    }

    /// Goes down from the bottom node to its `End` end: its first or last
    /// edge, then the `End` edges of the nodes below it, to a leaf.
    pub(in crate::tree) fn descend_to_end<End: NodeEnd>(&mut self) {
        self.set_index(End::edge(self.node_at(self.depth())));
        self.descend_to_leaf::<End>();
    }

    /// Goes down from the bottom node to the first key under it, at the
    /// start of its leftmost leaf; returns `false`, ending at that leaf's
    /// edge, if the leaf has no key, which only an empty tree's root has.
    pub(in crate::tree) fn descend_to_first_key(&mut self) -> bool {
        self.descend_to_end::<FirstEdge>();
        self.node_at(self.depth()).len() > 0
    }

    /// Goes down from the bottom node to the last key under it, at the end
    /// of its rightmost leaf; returns `false`, ending at that leaf's edge, if
    /// the leaf has no key, which only an empty tree's root has.
    pub(in crate::tree) fn descend_to_last_key(&mut self) -> bool {
        self.descend_to_end::<LastEdge>();
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
    /// returns whether it is there: the node stack ends at it then, in
    /// whichever node holds it, or else at the leaf edge where it belongs. A
    /// key that lacks what every key shares belongs at the tree's first or
    /// last edge, which the node stack goes to without searching.
    ///
    /// The node and its depth stay in registers on the way down; the node
    /// stack only takes stores, which nothing reads until it is done. The
    /// root's kind is decided once, and each internal node's children's, as
    /// the way down reads an edge; the loop runs over internal nodes only, and
    /// the leaf's search follows it.
    pub(in crate::tree) fn search<Q>(
        &mut self,
        key: &SearchKey<'_, K, S, Q>,
        shared: &SharedPrefix,
    ) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        match shared.place_outside(key) {
            Some(Ordering::Less) => {
                self.descend_to_end::<FirstEdge>();
                return false;
            }
            Some(Ordering::Greater) => {
                self.descend_to_end::<LastEdge>();
                return false;
            }
            Some(Ordering::Equal) | None => {}
        }
        let mut depth = self.depth();
        let leaf = match self.node_at(depth) {
            Node::Leaf(leaf) => leaf,
            Node::Internal(mut internal) => loop {
                let edge = match internal.search(key) {
                    Ok(index) => {
                        self.write_index(depth, index);
                        self.levels = depth as u8 + 1;
                        return true;
                    }
                    Err(edge) => edge,
                };
                self.write_index(depth, edge);
                // SAFETY: a search returns an edge `<= len`.
                let child = unsafe { internal.edge_unchecked(edge) };
                depth += 1;
                self.nodes[depth].write(child);
                match Node::at(child) {
                    Node::Leaf(leaf) => break leaf,
                    Node::Internal(child) => internal = child,
                }
            },
        };
        let (found, index) = match leaf.search(key) {
            Ok(index) => (true, index),
            Err(index) => (false, index),
        };
        self.write_index(depth, index);
        self.levels = depth as u8 + 1;
        found
    }

    /// Goes down from the bottom node to the leaf edge just before or just
    /// after `key`, or, if no entry has that key, where it would be. A
    /// separator stands between the end of the leaf left of it and the start
    /// of the leaf right of it (D17).
    pub(in crate::tree) fn search_to_edge<Q>(
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
        match side {
            KeySide::Before => self.descend_to_leaf::<LastEdge>(),
            KeySide::After => {
                self.set_index(self.index() + 1);
                self.descend_to_leaf::<FirstEdge>();
            }
        }
    }
}

/// Where a node stack stands relative to a key it looked up.
#[derive(Clone, Copy)]
pub(crate) enum KeySide {
    /// Just before the key, or where it would be.
    Before,
    /// Just after the key, or where it would be.
    After,
}

impl KeySide {
    /// Where a range starting at `start` starts: before an included key,
    /// after an excluded one; `None` if it starts at the tree's start.
    pub(crate) fn of_start<Q: ?Sized>(start: Bound<&Q>) -> Option<(&Q, Self)> {
        match start {
            Bound::Included(key) => Some((key, Self::Before)),
            Bound::Excluded(key) => Some((key, Self::After)),
            Bound::Unbounded => None,
        }
    }

    /// Where a range ending at `end` ends: after an included key, before an
    /// excluded one; `None` if it ends at the tree's end.
    pub(crate) fn of_end<Q: ?Sized>(end: Bound<&Q>) -> Option<(&Q, Self)> {
        match end {
            Bound::Included(key) => Some((key, Self::After)),
            Bound::Excluded(key) => Some((key, Self::Before)),
            Bound::Unbounded => None,
        }
    }
}

impl<B: CopyableBorrow, K, V, S> NodeStack<B, K, V, S> {
    /// The same node stack, with shared access for as long as `self` is
    /// borrowed.
    pub(in crate::tree) fn reborrow(&self) -> NodeStack<Immut<'_>, K, V, S> {
        // A copyable borrow gives at least shared access to the structure
        // and keys, and `&self` keeps the tree unchanged meanwhile. Reading
        // a value through it is sound only for values not handed out as
        // `&mut`, which the walks see to.
        NodeStack {
            nodes: self.nodes,
            indexes: self.indexes,
            levels: self.levels,
            _borrow: PhantomData,
        }
    }
}

/// An exclusive node stack, from the root down: what an entry keeps, and what
/// inserts and removals go back up. It changes the nodes below the root
/// through the bottom node, and the root only through
/// [`Self::ascend_raising`] and [`Self::detach_lowering_an_empty_root`].
///
/// It is made detached, and starts where it is: every way of starting it
/// first attaches it to the root.
pub(crate) struct NodeStackMut<'a, K, V, S> {
    tree: &'a mut Tree<K, V, S>,
    stack: NodeStack<Mut<'a, K, V>, K, V, S>,
}

impl<'a, K, V, S: InlineKeyStrategy<K>> NodeStackMut<'a, K, V, S> {
    /// A detached node stack down `tree`, for one of the methods that
    /// start it. It ends the owner's permission to append to its remembered
    /// leaf, which the node stack may change.
    #[inline(always)]
    pub(crate) fn new(tree: &'a mut Tree<K, V, S>) -> Self {
        tree.forget_rightmost_leaf();
        Self {
            tree,
            stack: NodeStack::detached(),
        }
    }

    /// The tree the node stack goes down, shared.
    pub(in crate::tree) fn tree(&self) -> &Tree<K, V, S> {
        self.tree
    }

    /// Shrinks what every key of the tree shares to what `key` shares too,
    /// and returns `key`, which shares it now
    /// ([`Tree::shrink_shared_prefix_for`]), for [`Self::insert`]. The node
    /// stack still leads where it did: only the prefixes along the tree's
    /// borders change.
    pub(in crate::tree) fn shrink_shared_prefix_for(
        &mut self,
        key: K,
        alloc: &impl Allocator,
    ) -> PrefixedKey<K> {
        self.tree.shrink_shared_prefix_for(key, alloc)
    }

    /// Finishes a plain insertion, remembering its leaf only when the node
    /// stack proves the new entry is physically last. This consumes the node
    /// stack: entry handles that keep their node stack may still remove or
    /// merge nodes, so they must never remember the leaf while retaining
    /// mutation access.
    pub(in crate::tree) fn finish_insertion(self) {
        let leaf = self.leaf();
        let last_edges = (0..self.stack.depth())
            .all(|depth| self.stack.index_at(depth) == self.stack.internal_at(depth).len());
        if self.stack.index() + 1 != leaf.len() || !last_edges {
            return;
        }
        // SAFETY: every ancestor takes its final edge, so this is the
        // physically last leaf. `self` is consumed and lends no handles;
        // future exclusive access goes through the owner's invalidation.
        unsafe { self.tree.remember_rightmost_leaf(leaf) };
    }

    /// Starts the node stack at the root, at its first edge.
    ///
    /// # Panics
    ///
    /// Without a tree.
    pub(crate) fn start_at_root(&mut self) {
        self.stack.attach(self.tree.root());
    }

    /// Starts the node stack at the root and searches down; see
    /// [`NodeStack::search`].
    pub(crate) fn search<Q>(&mut self, key: &SearchKey<'_, K, S, Q>) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.start_at_root();
        self.stack.search(key, self.tree.shared_prefix())
    }

    /// Starts the node stack at the root and goes down to the leaf edge beside
    /// `key`; see [`NodeStack::search_to_edge`].
    pub(in crate::tree) fn search_to_edge<Q>(&mut self, key: &SearchKey<'_, K, S, Q>, side: KeySide)
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.start_at_root();
        self.stack
            .search_to_edge(key, side, self.tree.shared_prefix());
    }

    /// Starts the node stack at the root and goes down to the tree's first
    /// key; see [`NodeStack::descend_to_first_key`].
    pub(crate) fn descend_to_first_key(&mut self) -> bool {
        self.start_at_root();
        self.stack.descend_to_first_key()
    }

    /// Starts the node stack at the root and goes down to the tree's last key;
    /// see [`NodeStack::descend_to_last_key`].
    pub(crate) fn descend_to_last_key(&mut self) -> bool {
        self.start_at_root();
        self.stack.descend_to_last_key()
    }

    /// See [`NodeStack::depth`].
    pub(in crate::tree) fn depth(&self) -> usize {
        self.stack.depth()
    }

    /// See [`NodeStack::is_at_root`].
    pub(in crate::tree) fn is_at_root(&self) -> bool {
        self.stack.is_at_root()
    }

    /// See [`NodeStack::ends_at_leaf`].
    pub(in crate::tree) fn ends_at_leaf(&self) -> bool {
        self.stack.ends_at_leaf()
    }

    /// See [`NodeStack::index`].
    pub(in crate::tree) fn index(&self) -> usize {
        self.stack.index()
    }

    /// See [`NodeStack::set_index`].
    pub(in crate::tree) fn set_index(&mut self, index: usize) {
        self.stack.set_index(index);
    }

    /// See [`NodeStack::ascend`].
    pub(in crate::tree) fn ascend(&mut self) -> Internal<K, V, S> {
        self.stack.ascend()
    }

    /// Goes up to the bottom node's parent and returns it, as
    /// [`Self::ascend`] does; at the root, raises the tree a level first
    /// ([`Tree::raise`]): the root node moves to a page of its kind from
    /// `alloc`, under the root, now internal with no keys, which the node
    /// stack then ends at, at its only edge.
    pub(in crate::tree) fn ascend_raising(&mut self, alloc: &impl Allocator) -> Internal<K, V, S> {
        if !self.stack.is_at_root() {
            return self.stack.ascend();
        }
        self.tree.raise(alloc);
        self.stack.attach(self.tree.root());
        self.stack.internal_at(0)
    }

    /// See [`NodeStack::descend`].
    pub(in crate::tree) fn descend(&mut self) {
        self.stack.descend();
    }

    /// See [`NodeStack::descend_to_leaf`].
    pub(in crate::tree) fn descend_to_leaf<End: NodeEnd>(&mut self) {
        self.stack.descend_to_leaf::<End>();
    }

    /// See [`NodeStack::descend_to_end`].
    pub(in crate::tree) fn descend_to_end<End: NodeEnd>(&mut self) {
        self.stack.descend_to_end::<End>();
    }

    /// The bottom node's bounds, low and high: the keys just left and just
    /// right of the node stack in the nodes above it, nearest first; `None`
    /// past the tree's first or last key.
    pub(super) fn fences(&self) -> (Option<&K>, Option<&K>) {
        let (mut low, mut high) = (None, None);
        for depth in (0..self.stack.depth()).rev() {
            let node = self.stack.internal_at(depth);
            let index = self.stack.index_at(depth);
            low = low.or_else(|| index.checked_sub(1).map(|left| node.separator_key(left)));
            high = high.or_else(|| (index < node.len()).then(|| node.separator_key(index)));
            if low.is_some() && high.is_some() {
                break;
            }
        }
        (low, high)
    }

    /// The bottom node. Changing it leaves the node stack leading to it.
    pub(in crate::tree) fn node(&self) -> Node<K, V, S> {
        self.stack.node_at(self.stack.depth())
    }

    /// The node at `depth` on the node stack, at or above its bottom: changing
    /// its prefix or its keys' windows leaves the node stack leading where it
    /// did, while its edges are the bottom node's business to change.
    ///
    /// # Panics
    ///
    /// If `depth` is not on the node stack.
    pub(super) fn node_at(&self, depth: usize) -> Node<K, V, S> {
        self.stack.node_at(depth)
    }

    /// See [`NodeStack::index_at`].
    pub(super) fn index_at(&self, depth: usize) -> usize {
        self.stack.index_at(depth)
    }

    /// See [`NodeStack::internal_at`].
    pub(super) fn internal_at(&self, depth: usize) -> Internal<K, V, S> {
        self.stack.internal_at(depth)
    }

    /// The bottom node, a leaf, as where a search or a descent to a leaf
    /// ended.
    ///
    /// # Panics
    ///
    /// If the bottom node is internal.
    pub(in crate::tree) fn leaf(&self) -> Leaf<K, V, S> {
        match self.node() {
            Node::Leaf(leaf) => leaf,
            Node::Internal(_) => panic!("the bottom node is internal"),
        }
    }

    /// The bottom node, internal, as where the node stack went up from a node
    /// below.
    ///
    /// # Panics
    ///
    /// If the bottom node is a leaf.
    pub(in crate::tree) fn internal(&self) -> Internal<K, V, S> {
        match self.node() {
            Node::Internal(internal) => internal,
            Node::Leaf(_) => panic!("the bottom node is a leaf"),
        }
    }

    /// Detaches the node stack, which a removal is done with, and lowers the
    /// tree, if merges left the root internal and without keys, moving the
    /// root's only child up into the root ([`Tree::lower`]), and giving the
    /// child's page back to `alloc`.
    pub(in crate::tree) fn detach_lowering_an_empty_root(&mut self, alloc: &impl Allocator) {
        self.stack = NodeStack::detached();
        if matches!(self.tree.root(), Node::Internal(root) if root.len() == 0) {
            self.tree.lower().deallocate(alloc);
        }
    }

    /// The key the node stack ends at.
    ///
    /// # Panics
    ///
    /// If the node stack ends at no key.
    pub(crate) fn key(&self) -> &K {
        let index = self.index();
        match self.node() {
            Node::Leaf(leaf) => leaf.key(index),
            Node::Internal(internal) => internal.separator_key(index),
        }
    }

    /// Puts `key` in place of the key the node stack ends at, with its inline
    /// key at the node's prefix length, and returns the old key: its value,
    /// and the node stack, stay. Keeping the keys in order is the caller's
    /// business.
    ///
    /// # Panics
    ///
    /// If the node stack ends at no key.
    pub(crate) fn replace_key(&mut self, key: K) -> K {
        let index = self.index();
        match self.node() {
            Node::Leaf(leaf) => leaf.replace_key(index, WindowedKey::new(key, leaf.prefix_len())),
            Node::Internal(internal) => {
                internal.replace_key(index, WindowedKey::new(key, internal.prefix_len()))
            }
        }
    }

    /// The key the node stack ends at, as a search finds it.
    ///
    /// # Panics
    ///
    /// If the node stack ends at no key.
    fn found(&self) -> Found<K, V, S> {
        let index = self.index();
        let node = self.node();
        assert!(index < node.len(), "the node stack ends at no key");
        match node {
            Node::Leaf(leaf) => Found::InLeaf(leaf, index),
            Node::Internal(internal) => Found::Separator(internal, index),
        }
    }

    /// The value of the key the node stack ends at: beside it in a leaf, or
    /// for a separator, a head below (D17).
    ///
    /// # Panics
    ///
    /// If the node stack ends at no key.
    pub(crate) fn value(&self) -> &V {
        self.found().key_value().1
    }

    /// The value of the key the node stack ends at, exclusive; see
    /// [`Self::value`].
    pub(crate) fn value_mut(&mut self) -> &mut V {
        self.found().value_mut()
    }

    /// The value of the key the node stack ends at, exclusive for the rest of
    /// `'a`, giving up the node stack; see [`Self::value`].
    pub(crate) fn into_value_mut(self) -> &'a mut V {
        self.found().value_mut()
    }
}
