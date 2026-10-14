//! Internal nodes: the nodes that route. An internal node's page holds its
//! children, one more than separators (D12, D32), and no values (D17).
//! Where the keys have inline keys, it holds its separators' inline keys
//! (D18) and nothing else: each separator's key is the head of the leftmost
//! leaf right of it, which a search reads only on a tie (D49), and the node
//! is a 512 B page (D49). Without inline keys, it keeps its separator keys.
//!
//! [`Internal`] is a handle to one, with every operation on one internal
//! node: routing a key to a child, reading and replacing its separators,
//! taking and giving children, splitting it, cutting it, and balancing it
//! with a sibling ([`Child`]). Its children are all of one kind, which each
//! edge's tag tells. The handle's contracts are the node module's
//! (`tree/node/mod.rs`).

use core::alloc::Layout;
use core::borrow::Borrow;
use core::marker::PhantomData;
use core::ops::Range;
use core::ptr::NonNull;

use super::inline_key_search::NodeInlineKeys;
use super::inline_keys::{InlineTarget, SearchKey, Separator, Window, WindowedKey};
use super::leaf::Leaf;
use super::node_plan::NodePlan;
use super::page::{FreshPage, NodeKind, NodePtr, PageByType, PageType};
use super::siblings::Siblings;
use super::slots::{KeyColumns, KeyList, KeySlots, NodeKeys, SeparatorSlots, Slots};
use super::{Child, EntrySlots, Insertion, InsertionSide, Node, Split};
use crate::allocator::Allocator;
use crate::inline_key::{InlineKeyStrategy, InlineWord};

/// An internal node, by its page.
pub(crate) struct Internal<K, V, S> {
    node: NodePtr<K, V, S>,
}

impl<K, V, S> Clone for Internal<K, V, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K, V, S> Copy for Internal<K, V, S> {}

// SAFETY: internal nodes have the internal layout.
unsafe impl<K, V, S: InlineKeyStrategy<K>> PageType for Internal<K, V, S> {
    fn page_layout() -> Layout {
        NodePtr::<K, V, S>::internal_layout().page
    }
}

// SAFETY: as for `PageType`; internal nodes are not leaves.
unsafe impl<K, V, S: InlineKeyStrategy<K>> NodeKind for Internal<K, V, S> {
    const IS_LEAF: bool = false;
}

impl<K, V, S> Internal<K, V, S> {
    /// The internal node at `node`, which must hold one.
    #[inline(always)]
    pub(in crate::tree) fn at(node: NodePtr<K, V, S>) -> Self {
        debug_assert!(!node.is_leaf(), "the node is a leaf");
        Self { node }
    }

    /// The node's page, for a node stack or an edge to keep.
    #[inline(always)]
    pub(in crate::tree) fn node_ptr(self) -> NodePtr<K, V, S> {
        self.node
    }

    /// The number of separators: one fewer than children.
    #[inline(always)]
    pub(crate) fn len(self) -> usize {
        // SAFETY: the node is allocated and initialized (invariant 1), and
        // nothing writes its header meanwhile (the handles' contract).
        unsafe { self.node.len() }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Internal<K, V, S> {
    /// A new internal node in `page` with no separators and `first_child`
    /// as its only child, one level above it, which it owns from now on
    /// (invariant 2), at `prefix_len`: what its fences share, or on the
    /// tree's border, what every key of the tree shares (05-fence-key.md,
    /// D50).
    pub(in crate::tree) fn new(
        page: FreshPage<Self>,
        first_child: Node<K, V, S>,
        prefix_len: usize,
    ) -> Self {
        let node = Self::at(NodePtr::from_fresh_page(page));
        node.put_only_child(first_child);
        node.set_empty_prefix_len(prefix_len);
        node
    }

    /// Makes `child`, a node one level down, edge 0 of this node, which
    /// holds no separators and no edge, and its only child, which it owns
    /// from now on (invariant 2): a new node, or the root's page turned
    /// internal (`root_page.rs`).
    pub(super) fn put_only_child(self, child: Node<K, V, S>) {
        debug_assert_eq!(self.len(), 0, "a node with separators has children");
        // SAFETY: the node is an allocated internal node, whose edge 0 holds
        // nothing to drop or free (the caller); with no separators, that is its
        // only edge, and it has no inline keys either.
        unsafe { self.edge_slots().slot(0).write(child.node_ptr()) };
    }

    /// Gives the node's page back.
    ///
    /// # Safety
    ///
    /// The node is on a page allocated as an internal node's: not a tree's
    /// root, which is in its root page ([`RootPage::free`]). Every separator
    /// the node held has been moved out, dropped or deliberately leaked,
    /// every child it led to freed or owned elsewhere, and nothing uses the
    /// node, through this handle or a copy, again.
    ///
    /// [`RootPage::free`]: super::root_page::RootPage::free
    pub(in crate::tree) unsafe fn free(self) -> FreshPage<Self> {
        // SAFETY: the page was allocated as an internal node's (caller), and
        // the caller hands over everything in it and every use of it.
        unsafe { self.node.into_fresh_page() }
    }

    /// The plan of the map's internal nodes (`node_plan.rs`).
    #[inline(always)]
    pub(in crate::tree) fn plan() -> NodePlan<K> {
        NodePlan::of::<S, Self>()
    }

    /// The most separators an internal node holds.
    #[inline(always)]
    pub(in crate::tree) fn capacity() -> usize {
        NodePtr::<K, V, S>::capacity::<Self>()
    }

    /// Whether the node holds fewer separators than [`Child::min_len`].
    #[cfg(test)]
    pub(in crate::tree) fn is_underfull(self) -> bool {
        self.len() < <Self as Child<K, V, S>>::min_len()
    }

    /// How many leading bytes the node's fences share, and so every key
    /// under it, which its inline keys skip (05-fence-key.md); 0 for a node
    /// without inline keys.
    #[inline(always)]
    pub(in crate::tree) fn prefix_len(self) -> usize {
        // SAFETY: the node is allocated and initialized, and nothing writes
        // its header meanwhile (the handles' contract).
        unsafe { self.node.prefix_len() }
    }

    /// The node's separators: keys, or windows where separator keys are
    /// heads (D49).
    #[inline(always)]
    fn separator_slots(self) -> SeparatorSlots<K, V, S> {
        // SAFETY: the node is an allocated internal node (invariant 1).
        unsafe { self.node.separator_slots() }
    }

    /// The node's separator keys, for a node that keeps them, in a map
    /// without inline keys (D49): such a node has no columns beside them.
    fn key_slots(self) -> KeySlots<K, S, Self> {
        // SAFETY: the node is an allocated internal node (invariant 1).
        unsafe { self.node.key_slots::<Self>() }
    }

    /// The arrays beside the separators: the inline keys.
    #[inline(always)]
    fn columns(self) -> KeyColumns {
        // SAFETY: the node is an allocated internal node (invariant 1).
        unsafe { self.node.key_columns::<Self>() }
    }

    #[inline(always)]
    fn edge_slots(self) -> Slots<NodePtr<K, V, S>> {
        // SAFETY: the node is an allocated internal node (invariant 1).
        unsafe { self.node.edge_slots() }
    }
}

/// An internal node's children.
impl<K, V, S: InlineKeyStrategy<K>> Internal<K, V, S> {
    /// Whether the node's children are leaves, as its first edge's tag
    /// tells for all of them: the one decision about their kind, after which
    /// code for that kind takes over.
    #[inline(always)]
    pub(in crate::tree) fn has_leaf_children(self) -> bool {
        // SAFETY: edge 0 is initialized (invariant 1).
        unsafe { self.edge_unchecked(0).is_leaf() }
    }

    /// Edge `index`, the child's tagged pointer, with `index <= len`,
    /// unchecked.
    ///
    /// # Safety
    ///
    /// `index <= len`.
    #[inline(always)]
    pub(in crate::tree) unsafe fn edge_unchecked(self, index: usize) -> NodePtr<K, V, S> {
        debug_assert!(index <= self.len());
        // SAFETY: `index <= len` (caller), so edge `index` is initialized
        // (invariant 1).
        unsafe { self.edge_slots().slot(index).read() }
    }

    /// The child at edge `index`, one level down.
    ///
    /// # Panics
    ///
    /// If `index > len`.
    #[inline(always)]
    pub(crate) fn child(self, index: usize) -> Node<K, V, S> {
        assert!(index <= self.len(), "edge index out of bounds");
        // SAFETY: checked just above.
        Node::at(unsafe { self.edge_unchecked(index) })
    }

    /// The child at edge `index`, with `index <= len`, unchecked.
    ///
    /// # Safety
    ///
    /// `index <= len`.
    #[inline(always)]
    pub(in crate::tree) unsafe fn child_unchecked(self, index: usize) -> Node<K, V, S> {
        // SAFETY: forwarded.
        Node::at(unsafe { self.edge_unchecked(index) })
    }

    /// The child at edge `index`, a leaf, as the caller decided once for
    /// this node's children ([`Self::has_leaf_children`]), which debug builds
    /// check.
    ///
    /// # Panics
    ///
    /// If `index > len`.
    pub(in crate::tree) fn leaf_child(self, index: usize) -> Leaf<K, V, S> {
        debug_assert!(
            self.has_leaf_children(),
            "descending to a child of the other kind"
        );
        assert!(index <= self.len(), "edge index out of bounds");
        // SAFETY: checked just above.
        Leaf::at(unsafe { self.edge_unchecked(index) })
    }

    /// The child at edge `index`, an internal node, as the caller decided
    /// once for this node's children ([`Self::has_leaf_children`]), which
    /// debug builds check.
    ///
    /// # Panics
    ///
    /// If `index > len`.
    pub(in crate::tree) fn internal_child(self, index: usize) -> Self {
        debug_assert!(
            !self.has_leaf_children(),
            "descending to a child of the other kind"
        );
        assert!(index <= self.len(), "edge index out of bounds");
        // SAFETY: checked just above.
        Self::at(unsafe { self.edge_unchecked(index) })
    }

    /// # Panics
    ///
    /// If `child` is not of the kind of this node's children. That they all
    /// lie at one depth, which the kinds cannot tell apart above the leaves,
    /// is the caller's business.
    fn assert_child_kind(self, child: Node<K, V, S>) {
        assert_eq!(
            matches!(child, Node::Leaf(_)),
            self.has_leaf_children(),
            "an edge must lead to a node of its siblings' kind"
        );
    }

    /// The leftmost leaf right of separator `index`, which holds its value
    /// as its head (D17), and where separator keys are heads, its key (D49):
    /// down edge `index + 1`, then first edges.
    ///
    /// # Panics
    ///
    /// If `index >= len`.
    pub(in crate::tree) fn head_leaf(self, index: usize) -> Leaf<K, V, S> {
        let mut node = self.child(index + 1);
        loop {
            node = match node {
                Node::Leaf(leaf) => return leaf,
                Node::Internal(internal) => internal.child(0),
            };
        }
    }

    /// Puts `child`, another tree's root, one level below this node, at edge
    /// `index` in place of the node there, and returns that one, as two
    /// trees join (`tree/join.rs`). Keeping keys in order, heads where
    /// separators expect them (D17), and the new child's prefix at least
    /// this node's, is the caller's business.
    ///
    /// # Panics
    ///
    /// If `child` is not of the kind of this node's children
    /// ([`Self::assert_child_kind`]), or `index > len`; the node is
    /// unchanged then.
    pub(in crate::tree) fn replace_child(
        self,
        index: usize,
        child: Node<K, V, S>,
    ) -> Node<K, V, S> {
        self.assert_child_kind(child);
        assert!(index <= self.len(), "edge index out of bounds");
        // SAFETY: `index <= len` (checked), so edge `index` is initialized:
        // the only pointer to a node one level down (invariant 2), which
        // goes to the caller. `child`, one level down (the caller), takes its
        // place, owned by this node from now on.
        unsafe {
            let old = self.edge_slots().slot(index).replace(child.node_ptr());
            Node::at(old)
        }
    }

    /// Separator `index` and the two children around it, of kind `C`, as
    /// the caller decided once for this node's children
    /// ([`Self::has_leaf_children`]), ready to be merged or to move an entry
    /// from one child to the other.
    ///
    /// # Panics
    ///
    /// If `index >= len` (the children's kind debug builds check); or if they
    /// are leaves and the right one has no head to hold the separator's
    /// value, which never happens in a tree built by the node operations
    /// (D17).
    pub(in crate::tree) fn siblings<C: Child<K, V, S>>(self, index: usize) -> Siblings<K, V, S, C> {
        assert!(index < self.len(), "separator index out of bounds");
        let (left, right) = (C::child_of(self, index), C::child_of(self, index + 1));
        assert!(
            right.has_first_trailing_slot(),
            "the leaf right of a separator has no head"
        );
        Siblings::new(self, index, left, right)
    }
}

/// An internal node's separators.
impl<K, V, S: InlineKeyStrategy<K>> Internal<K, V, S> {
    /// The node's keys `0..len`, in order, in a map without inline keys,
    /// whose internal nodes keep their separators' keys (D17). In a map with
    /// inline keys an internal node keeps no keys: see [`Self::head_keys`].
    ///
    /// # Panics
    ///
    /// In a map whose separator keys are heads (D49).
    #[inline(always)]
    fn keys<'a>(self) -> NodeKeys<'a, K> {
        assert!(
            !NodePtr::<K, V, S>::has_separator_keys_in_heads(),
            "an internal node keeps no keys where separator keys are heads"
        );
        // SAFETY: keys `0..len` are initialized and unchanged while the
        // caller borrows them (invariant 1 and the handles' contract), and
        // the last key's stride ends within the page (`layout.rs`).
        unsafe { NodeKeys::new(self.key_slots().keys(), self.len()) }
    }

    /// The keys of the node's separators `0..len`, in order, in a map with
    /// inline keys, where each is the head key of the leftmost leaf right
    /// of its separator (D49): the node keeps none of them, and getting one
    /// walks down to its leaf ([`Self::head_leaf`]).
    ///
    /// # Panics
    ///
    /// In a map whose internal nodes keep their keys (D17).
    #[inline(always)]
    fn head_keys<'a>(self) -> HeadKeys<'a, K, V, S> {
        assert!(
            NodePtr::<K, V, S>::has_separator_keys_in_heads(),
            "separator keys are in their internal node, not in heads"
        );
        HeadKeys {
            node: self,
            start: 0,
            len: self.len(),
            _borrow: PhantomData,
        }
    }

    /// Separator `index`'s key, wherever the map keeps it: in this node
    /// (D17), or as the head key of the leftmost leaf right of it (D49),
    /// which this walks down to.
    ///
    /// # Panics
    ///
    /// If `index >= len`.
    pub(in crate::tree) fn separator_key<'a>(self, index: usize) -> &'a K {
        if NodePtr::<K, V, S>::has_separator_keys_in_heads() {
            self.head_leaf(index).head_key()
        } else {
            self.keys()
                .get(index)
                .expect("separator index out of bounds")
        }
    }

    /// Separator `index`'s key and value: its value is the head of the
    /// leftmost leaf right of it (D17), and where separator keys are heads,
    /// so is its key (D49), both found in one walk down.
    ///
    /// # Panics
    ///
    /// If `index >= len`.
    pub(in crate::tree) fn separator_key_value<'a>(self, index: usize) -> (&'a K, &'a V) {
        let head_leaf = self.head_leaf(index);
        let key = if NodePtr::<K, V, S>::has_separator_keys_in_heads() {
            head_leaf.head_key()
        } else {
            self.keys()
                .get(index)
                .expect("separator index out of bounds")
        };
        (key, head_leaf.head_value())
    }

    /// Where separator `index`'s key is in a node that keeps its keys, for a
    /// walk to hand it out as its borrow allows (`tree/walk.rs`).
    ///
    /// # Panics
    ///
    /// If separator keys are heads in this map, or `index >= len`.
    pub(in crate::tree) fn key_slot(self, index: usize) -> NonNull<K> {
        assert!(
            !NodePtr::<K, V, S>::has_separator_keys_in_heads(),
            "separator keys are heads"
        );
        assert!(index < self.len(), "separator index out of bounds");
        // SAFETY: `index < len <= capacity`, and the node keeps its keys;
        // nothing is dereferenced.
        unsafe { self.key_slots().keys().slot(index) }
    }

    /// Where the entry of separator `index` is, for a walk to hand it out
    /// as its borrow allows (`tree/walk.rs`): its key, in this node or,
    /// where separator keys are heads, in the head of `head_leaf` (D49), and
    /// its value, the head of `head_leaf`, which must be the leftmost leaf
    /// right of it (D17).
    ///
    /// # Panics
    ///
    /// If `head_leaf` has no head, or this node keeps keys and
    /// `index >= len`.
    pub(in crate::tree) fn separator_entry_slots(
        self,
        index: usize,
        head_leaf: Leaf<K, V, S>,
    ) -> EntrySlots<K, V> {
        let value = head_leaf.head_value_slot();
        let key = if NodePtr::<K, V, S>::has_separator_keys_in_heads() {
            head_leaf.head_key_slot()
        } else {
            self.key_slot(index)
        };
        EntrySlots { key, value }
    }

    /// The node's inline keys, if it has them (D18): inline key `i` is
    /// separator `i`'s, for `i < len`, and a search may read the slots past
    /// them.
    #[inline(always)]
    pub(in crate::tree) fn inline_keys<'a>(self) -> Option<NodeInlineKeys<'a>> {
        let slots = self.columns().inline_keys()?;
        // SAFETY: the inline-key slots number the capacity, all are
        // initialized (invariant 4), and none changes while the caller
        // borrows them (the handles' contract); they grow up, so memory
        // order is index order.
        Some(unsafe { NodeInlineKeys::new(slots, Self::capacity(), self.len()) })
    }

    /// Separator `index`'s window: its inline key at the node's prefix
    /// length, or [`Window::NONE`] in a node without inline keys. A node
    /// whose plan keeps fewer bytes of it than a leaf reads it from the key
    /// again (D43), from its head (D49).
    ///
    /// # Panics
    ///
    /// If `index >= len`.
    pub(in crate::tree) fn window(self, index: usize) -> Window {
        assert!(index < self.len(), "separator index out of bounds");
        let plan = Self::plan();
        let Some(inline_keys) = self.inline_keys() else {
            return Window::NONE;
        };
        let prefix_len = self.prefix_len();
        let inline_key = match plan.inliner() {
            Some(inliner) if plan.rehydrates_windows() => {
                inliner.inline_key_at(self.separator_key(index), prefix_len)
            }
            _ => inline_keys.inline_key(index),
        };
        Window::new(prefix_len, inline_key)
    }

    /// The window of a separator beside edge `index`, the one left of it if
    /// there is one: a bound of the child there, which shares the child's
    /// prefix, at this node's prefix length. `None` if the node has no
    /// separators.
    ///
    /// # Panics
    ///
    /// If `index > len`.
    pub(in crate::tree) fn fence_window(self, index: usize) -> Option<Window> {
        let len = self.len();
        assert!(index <= len, "edge index out of bounds");
        let beside = index.checked_sub(1).or((index < len).then_some(index))?;
        Some(self.window(beside))
    }

    /// `separator`, from the child at edge `index` (from its split), with
    /// its window moved back to this node's prefix length, filled from
    /// [`Self::fence_window`]. A node without separators is a new root,
    /// whose only child, the old root, has the tree's shared prefix, as it
    /// does (D50).
    fn separator_from_child(self, index: usize, separator: Separator<K, S>) -> Separator<K, S> {
        match self.fence_window(index) {
            Some(fill) => separator.moved_back_to(fill),
            None => {
                debug_assert_eq!(separator.window().prefix_len(), self.prefix_len());
                separator
            }
        }
    }

    /// Puts `separator`, whose window is at this node's prefix length, in
    /// place of separator `index`, and returns the old one, as kept: the key
    /// where the node keeps keys, or its window. Keeping the separators in
    /// order, and heads where they expect them, is the caller's business.
    ///
    /// # Panics
    ///
    /// If `index >= len`.
    pub(in crate::tree) fn replace_separator(
        self,
        index: usize,
        separator: Separator<K, S>,
    ) -> Separator<K, S> {
        assert!(index < self.len(), "separator index out of bounds");
        // SAFETY: `index < len`, so separator `index` is initialized, and
        // the access is exclusive.
        unsafe { self.separator_slots().replace(index, separator) }
    }

    /// Replaces separator `index` with `separator`, returning the old one, a
    /// key where internal nodes keep keys, a window where they are heads
    /// (D49). Keeping the keys in order, and heads where separators expect
    /// them, is the caller's business.
    ///
    /// The new separator's window may be at a longer prefix length, if it
    /// comes from a node below it whose bounds it is, such as the leaf right
    /// of it: it moves back to this node's, filled from the window the node
    /// hands out for the separator there now.
    pub(in crate::tree) fn replace_separator_from_below(
        self,
        index: usize,
        separator: Separator<K, S>,
    ) -> Separator<K, S> {
        let separator = separator.moved_back_to(self.window(index));
        self.replace_separator(index, separator)
    }

    /// Replaces separator `index`'s key and its inline key, returning the
    /// old key: in this node, or where separator keys are heads, in the head
    /// of the leftmost leaf right of it (D49). Keeping the keys in order is
    /// the caller's business.
    ///
    /// # Panics
    ///
    /// If `index >= len`.
    pub(in crate::tree) fn replace_key(self, index: usize, key: WindowedKey<K, S>) -> K {
        if !NodePtr::<K, V, S>::has_separator_keys_in_heads() {
            let old = self.replace_separator_from_below(index, Separator::with_key(key));
            return (old.into_windowed_key())
                .expect("a separator of a node that keeps keys is a key")
                .into_key();
        }
        let (key, window) = key.into_parts();
        let old = self.head_leaf(index).replace_head_key(key);
        self.replace_separator_from_below(index, Separator::without_key(window));
        old
    }
}

/// Routing a key through an internal node.
impl<K, V, S: InlineKeyStrategy<K>> Internal<K, V, S> {
    /// `key`'s inline key at this node's prefix length, if the node has
    /// inline keys and `key` one to compare with them (D18); see
    /// [`Leaf`]'s.
    #[inline(always)]
    pub(in crate::tree) fn inline_target<Q: ?Sized>(
        self,
        key: &SearchKey<'_, K, S, Q>,
    ) -> Option<InlineTarget>
    where
        K: Borrow<Q>,
    {
        key.inline_target(self.prefix_len())
    }

    /// Binary search for `key` among the node's separators: `Ok(i)` if
    /// separator `i` equals it, otherwise `Err(i)` with `i` the edge to the
    /// child it belongs under. With inline keys (D18), over those first; a
    /// separator's key is read, where separator keys are heads, only on a
    /// tie (D49), or on every probe for a lookup through a form the nodes
    /// can't encode.
    ///
    /// The position is in bounds for any `Ord` and `InlineKey`, consistent
    /// or not.
    // Keep this adapter with its caller, which already holds the node.
    // Outlining it returns the result through the stack at each visit. The
    // node search itself is inlined where the plan says.
    #[inline(always)]
    pub(in crate::tree) fn search<Q>(self, key: &SearchKey<'_, K, S, Q>) -> Result<usize, usize>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if Self::plan().inlines_node_search() {
            self.search_always_inlined(key)
        } else {
            self.search_maybe_inlined(key)
        }
    }

    /// [`Self::search_always_inlined`], inlined or not as LLVM decides at each
    /// caller (`NodePlan::inlines_node_search`).
    fn search_maybe_inlined<Q>(self, key: &SearchKey<'_, K, S, Q>) -> Result<usize, usize>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.search_always_inlined(key)
    }

    /// If the node has inline keys and `key` has one to compare (D18), taken
    /// at the node's prefix length, the search runs over them; otherwise
    /// over the separator keys, as the node's plan says (D19). The key
    /// shares the node's prefix: it lies between the node's fences, or
    /// within what every key of the tree shares, which a search checks at
    /// the top (D50).
    #[inline(always)]
    fn search_always_inlined<Q>(self, key: &SearchKey<'_, K, S, Q>) -> Result<usize, usize>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if let Some(target) = self.inline_target(key)
            && let Some(candidates) = self.candidates(target)
        {
            // The candidates' keys are heads (D49), which a search reads only
            // on a tie, and most searches have none: they go no further than
            // the inline keys, nor into a call.
            if candidates.is_empty() {
                return Err(candidates.start);
            }
            return self.search_candidates(candidates, key, target);
        }
        // No inline key to narrow with: a map without inline keys, whose
        // node keeps its keys, or a lookup through a form the nodes can't
        // encode, in a map whose separator keys are heads.
        let search = Self::plan().all_keys_search();
        let cmp_key = |probe: &K| key.key().cmp(probe.borrow());
        if NodePtr::<K, V, S>::has_separator_keys_in_heads() {
            search.search(self.head_keys(), key.key(), cmp_key)
        } else {
            search.search(self.keys(), key.key(), cmp_key)
        }
    }

    /// The candidates for `target`: the positions of the separators whose
    /// inline keys equal `target`'s in the bytes the node keeps (D18), or
    /// `None` if the node has no inline keys; see [`Leaf`]'s. The count runs
    /// over the internal capacity, a constant the compiler unrolls.
    #[inline(always)]
    fn candidates(self, target: InlineTarget) -> Option<Range<usize>> {
        Some(self.inline_keys()?.equal_range(target.inline_key()))
    }

    /// The search among `candidates`, the separators whose inline keys
    /// equal `key`'s (D18), as the node's plan says; see [`Leaf`]'s. A node
    /// with inline keys keeps no keys: each probe walks down to its leaf's
    /// head (D49).
    fn search_candidates<Q>(
        self,
        candidates: Range<usize>,
        key: &SearchKey<'_, K, S, Q>,
        target: InlineTarget,
    ) -> Result<usize, usize>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let found = Self::plan().candidate_search().search(
            self.head_keys().slice(candidates.clone()),
            key.key(),
            |stored: &K| key.cmp_tied::<Self>(target, stored),
        );
        found
            .map(|index| candidates.start + index)
            .map_err(|index| candidates.start + index)
    }
}

/// Changing an internal node's separators and children.
impl<K, V, S: InlineKeyStrategy<K>> Internal<K, V, S> {
    /// Raises the length to `len`, for separators and edges just written
    /// past it; see [`Leaf`]'s.
    ///
    /// # Safety
    ///
    /// `self.len() <= len <= capacity`, and separators `0..len` and edges
    /// `0..=len` are initialized.
    unsafe fn grow_len(self, len: usize) {
        debug_assert!(self.len() <= len && len <= Self::capacity());
        // SAFETY: the node is allocated and the access exclusive; the length
        // and initialization requirements are forwarded.
        unsafe { self.node.set_len(len) }
    }

    /// Sets the length; the inline-key slots a shorter length leaves get the
    /// greatest inline key (invariant 4).
    ///
    /// # Safety
    ///
    /// `len` is at most the capacity, and separators `0..len` and edges
    /// `0..=len` are initialized and valid at this call.
    unsafe fn set_len(self, len: usize) {
        debug_assert!(len <= Self::capacity());
        let old_len = self.len();
        // SAFETY: `old_len <= capacity`, and the access is exclusive.
        unsafe { self.columns().mark_vacant(len..old_len) };
        // SAFETY: the node is allocated and the access exclusive; `len <=
        // capacity` is forwarded.
        unsafe { self.node.set_len(len) }
    }

    /// Inserts `separator` at edge `index` with `right_child` as the edge to
    /// its right, shifting later separators and edges right. The separator's
    /// value is the head of `right_child`'s leftmost leaf (D17), and where
    /// separator keys are heads, so is its key (D49).
    ///
    /// # Panics
    ///
    /// If the node is full, `index > len`, or `right_child` is not of the
    /// kind of this node's children ([`Self::assert_child_kind`]); the node
    /// is unchanged then.
    pub(in crate::tree) fn insert_fit(
        self,
        index: usize,
        separator: Separator<K, S>,
        right_child: Node<K, V, S>,
    ) {
        let len = self.len();
        assert!(
            index <= len && len < Self::capacity(),
            "inserting at edge {index} of an internal node of {len} separators"
        );
        self.assert_child_kind(right_child);
        // SAFETY: the node has room and `index <= len`, and `right_child` is
        // of its siblings' kind, all checked just above, and at their depth
        // (the caller).
        unsafe { self.insert_fit_unchecked(index, separator, right_child) }
    }

    /// [`Self::insert_fit`], unchecked.
    ///
    /// # Safety
    ///
    /// `index <= len < capacity`, and `right_child` is exactly one level
    /// below this node.
    unsafe fn insert_fit_unchecked(
        self,
        index: usize,
        separator: Separator<K, S>,
        right_child: Node<K, V, S>,
    ) {
        let len = self.len();
        debug_assert!(index <= len && len < Self::capacity());
        let separator = self.separator_from_child(index, separator);
        // SAFETY: `index <= len < capacity` (the caller), so `index + 1 <=
        // len + 1 < capacity + 1`; separators `0..len` and edges `0..=len`
        // are initialized, and one more of each afterwards. The new edge
        // leads to a node one level down (the caller), which this node owns
        // from now on (invariant 2).
        unsafe {
            self.separator_slots().insert(index, len, separator);
            self.edge_slots()
                .insert(index + 1, len + 1, right_child.node_ptr());
            self.grow_len(len + 1);
        }
    }

    /// Inserts `separator` at edge `index` with `right_child` as the edge to
    /// its right, splitting the node first if it is full, with the right
    /// half in a page from `alloc`. This is how a child's split lands in
    /// its parent; the separator's value is already in `right_child`, as the
    /// head of its leftmost leaf (D17), and so is its key where separator
    /// keys are heads (D49).
    ///
    /// # Panics
    ///
    /// If `right_child` is not of the kind of this node's children
    /// ([`Self::assert_child_kind`]); the node is unchanged then.
    pub(in crate::tree) fn insert(
        self,
        index: usize,
        separator: Separator<K, S>,
        right_child: Node<K, V, S>,
        alloc: &impl Allocator,
    ) -> Insertion<K, S, Self> {
        if self.len() < Self::capacity() {
            self.insert_fit(index, separator, right_child);
            Insertion::Fit(index)
        } else {
            let right_page = FreshPage::allocate(alloc);
            Insertion::Split(self.split_insert(index, separator, right_child, right_page))
        }
    }

    /// Splits the full node and inserts `separator`, with `right_child` as
    /// the edge to its right, at edge `index`, on the side where that edge
    /// ended up (`InsertionSide::of_split`). The middle separator goes up;
    /// separators and edges move, and no leaf changes. The right half goes
    /// into `right_page`.
    ///
    /// # Panics
    ///
    /// If `right_child` is not of the kind of this node's children
    /// ([`Self::assert_child_kind`]); the node is unchanged then.
    pub(in crate::tree) fn split_insert(
        self,
        index: usize,
        separator: Separator<K, S>,
        right_child: Node<K, V, S>,
        right_page: FreshPage<Self>,
    ) -> Split<K, S, Self> {
        self.assert_child_kind(right_child);
        debug_assert_eq!(self.len(), Self::capacity());
        let (separator_index, inserted) =
            InsertionSide::of_split(Self::capacity(), index, Self::min_len());
        let right = Self::at(NodePtr::from_fresh_page(right_page));
        // SAFETY: `right` is a fresh internal node of the same map, reached
        // by nothing else, at this node's prefix length once set here.
        unsafe { right.node.set_prefix_len(self.prefix_len()) };
        let middle = self.split_around(separator_index, right);
        // SAFETY: both halves have room (`InsertionSide::of_split`), and
        // `right_child` is of their children's kind, checked above, and at
        // their depth (the caller).
        unsafe {
            match inserted {
                InsertionSide::Left(index) => {
                    self.insert_fit_unchecked(index, separator, right_child)
                }
                InsertionSide::Right(index) => {
                    right.insert_fit_unchecked(index, separator, right_child);
                }
            }
        }
        Split {
            separator: middle,
            right,
            inserted,
        }
    }

    /// Moves the separators right of `separator_index`, and the edges right
    /// of it, into `right`, a fresh internal node at this node's prefix
    /// length, and takes out separator `separator_index`, for the
    /// parent; returns it.
    fn split_around(self, separator_index: usize, right: Self) -> Separator<K, S> {
        let len = self.len();
        debug_assert!(separator_index < len);
        let right_len = len - separator_index - 1;
        // SAFETY: `separator_index < len`. Separators `separator_index +
        // 1..len` and edges `separator_index + 1..=len` are initialized and
        // move to the start of `right`, a fresh internal node of the same map
        // at the same prefix length, which owns the nodes they lead to from
        // now on (invariant 2). Separator `separator_index` is then the last
        // initialized one on the left and is read out. Where separator keys
        // are heads (D49), each stays in the leaf right of it, below the edge
        // that moved with it.
        unsafe {
            self.separator_slots().move_run_to(
                separator_index + 1,
                right_len,
                right.separator_slots(),
                0,
            );
            self.edge_slots().move_run_to(
                separator_index + 1,
                right_len + 1,
                right.edge_slots(),
                0,
            );
            right.set_len(right_len);
            let separator = self.separator_slots().read(separator_index);
            self.set_len(separator_index);
            separator
        }
    }

    /// Removes separator `index` and the edge to its right, shifting later
    /// separators and edges left, and returns both: the edge as the node it
    /// led to, whose leftmost leaf holds the separator's value as its head
    /// (D17), and where separator keys are heads, its key (D49).
    ///
    /// # Panics
    ///
    /// If `index >= len`.
    pub(in crate::tree) fn remove_with_right_child(
        self,
        index: usize,
    ) -> (Separator<K, S>, Node<K, V, S>) {
        let len = self.len();
        assert!(index < len, "separator index out of bounds");
        // SAFETY: `index < len`, so separator `index` and edge `index + 1`
        // are initialized; afterwards separators `0..len - 1` and edges
        // `0..len` are. The edge led to a node one level down that this node
        // owned, which goes to the caller.
        unsafe {
            let separator = self.separator_slots().remove(index, len);
            let child = self.edge_slots().remove(index + 1, len + 1);
            self.set_len(len - 1);
            (separator, Node::at(child))
        }
    }

    /// Cuts the node at edge `index`: moves the separators from `index` on,
    /// with their inline keys, and the edges right of them, into a new node
    /// in `page`, after `first_child`, the part of the child at `index` that
    /// went right, and returns it. This node keeps the separators before
    /// `index` and the edges up to it. Either may be left with fewer
    /// separators than its minimum, down to none.
    ///
    /// # Panics
    ///
    /// If `index > len`, or if `first_child` is not of the kind of this
    /// node's children ([`Self::assert_child_kind`]); the node is unchanged
    /// then.
    pub(in crate::tree) fn cut_off_at(
        self,
        index: usize,
        first_child: Node<K, V, S>,
        page: FreshPage<Self>,
    ) -> Self {
        let len = self.len();
        assert!(index <= len, "cutting a node past its last edge");
        self.assert_child_kind(first_child);
        let count = len - index;
        let right = Self::new(page, first_child, self.prefix_len());
        // SAFETY: `index <= len`, so separators `index..len` and edges
        // `index + 1..=len` are initialized; they move to separators
        // `0..count` and edges `1..=count` of `right`, a fresh internal node
        // of the same map, after `first_child` as its edge 0, at the other
        // edges' depth (the caller),
        // which `right` owns from now on (invariant 2). The access is
        // exclusive, and `right` is reached by nothing else yet. Afterwards
        // this node holds separators `0..index` and edges `0..=index`. The
        // separators keep their windows, at this node's prefix length, which
        // `right` takes. Where separator keys are heads (D49), each stays in
        // the leaf right of it, below the edge that moves with it.
        unsafe {
            self.separator_slots()
                .move_run_to(index, count, right.separator_slots(), 0);
            self.edge_slots()
                .move_run_to(index + 1, count, right.edge_slots(), 1);
            right.set_len(count);
            self.set_len(index);
        }
        right
    }
}

/// An internal node's prefix (05-fence-key.md).
impl<K, V, S: InlineKeyStrategy<K>> Internal<K, V, S> {
    /// Moves the node's prefix length back to `fill`'s, and every inline key
    /// with it; see [`Leaf::move_prefix_back_to`].
    pub(in crate::tree) fn move_prefix_back_to(self, fill: Window) {
        let prefix_len = self.prefix_len();
        if prefix_len <= fill.prefix_len() {
            return;
        }
        let len = self.len();
        let Some(inline_keys) = self.columns().inline_keys() else {
            return;
        };
        // SAFETY: inline keys `0..len` are initialized (invariants 1 and 4),
        // each is rewritten in place with the window of the same key, and
        // the header with them; the access is exclusive.
        unsafe {
            for index in 0..len {
                let window = Window::new(prefix_len, inline_keys.read(index)).moved_back_to(fill);
                inline_keys.write(index, window.inline_key());
            }
            self.node.set_prefix_len(fill.prefix_len());
        }
    }

    /// Makes `prefix_len` the node's prefix length, and `inline_keys` its
    /// separators' inline keys; see [`Leaf::set_windows`].
    ///
    /// # Panics
    ///
    /// If `inline_keys` does not say it has one inline key per separator;
    /// the node is unchanged then.
    pub(in crate::tree) fn set_windows(
        self,
        prefix_len: usize,
        inline_keys: impl ExactSizeIterator<Item = InlineWord>,
    ) {
        let len = self.len();
        assert_eq!(inline_keys.len(), len, "one inline key per separator");
        let Some(slots) = self.columns().inline_keys() else {
            return;
        };
        // SAFETY: each slot written has an index below `len <= capacity`,
        // however many inline keys the iterator gives; inline keys are plain
        // data, and the access is exclusive.
        unsafe {
            for (index, inline_key) in (0..len).zip(inline_keys) {
                slots.write(index, inline_key);
            }
            self.node.set_prefix_len(prefix_len);
        }
    }

    /// Gives a node with no separators `prefix_len`; see
    /// [`Leaf::set_empty_prefix_len`].
    ///
    /// # Panics
    ///
    /// If the node has separators.
    pub(in crate::tree) fn set_empty_prefix_len(self, prefix_len: usize) {
        assert_eq!(
            self.len(),
            0,
            "a node with separators keeps their prefix length"
        );
        // SAFETY: the node is allocated and the access exclusive.
        unsafe { self.node.set_prefix_len(prefix_len) }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Child<K, V, S> for Internal<K, V, S> {
    fn child_of(parent: Internal<K, V, S>, index: usize) -> Self {
        parent.internal_child(index)
    }

    fn len(self) -> usize {
        Internal::len(self)
    }

    #[inline(always)]
    fn capacity() -> usize {
        Internal::<K, V, S>::capacity()
    }

    #[inline(always)]
    fn plan() -> NodePlan<K> {
        Internal::<K, V, S>::plan()
    }

    fn prefix_len(self) -> usize {
        Internal::prefix_len(self)
    }

    fn has_first_trailing_slot(self) -> bool {
        true
    }

    fn move_prefix_back_to(self, fill: Window) {
        Internal::move_prefix_back_to(self, fill);
    }

    unsafe fn free_page(self) -> PageByType<K, V, S> {
        // SAFETY: forwarded.
        unsafe { self.free() }.into()
    }

    unsafe fn steal_left_unchecked(siblings: &Siblings<K, V, S, Self>, fill: Window) {
        let (parent, index) = (siblings.parent(), siblings.index());
        let (left, right) = (siblings.left(), siblings.right());
        let (left_len, right_len) = (left.len(), right.len());
        // SAFETY: the left child has separator `left_len - 1` and edge
        // `left_len` to give, and the right child room for one more
        // (caller), all three of the same map. The parent's separator is
        // initialized and comes down to the right child's front, at its
        // prefix length, `fill`'s; the left child's last edge becomes the
        // right child's first, and stays one level below both (invariant 2).
        // Where separator keys are heads (D49), each stays in the leaf right
        // of it: the one going up has the moved edge's node right of it, and
        // the one coming down the right child's old first.
        unsafe {
            let separator = left.separator_slots().remove(left_len - 1, left_len);
            let old = parent.replace_separator(index, separator.moved_back_to(fill));
            right.separator_slots().insert(0, right_len, old);
            let edge = left.edge_slots().remove(left_len, left_len + 1);
            right.edge_slots().insert(0, right_len + 1, edge);
            left.set_len(left_len - 1);
            right.set_len(right_len + 1);
        }
    }

    unsafe fn steal_right_unchecked(siblings: &Siblings<K, V, S, Self>, fill: Window) {
        let (parent, index) = (siblings.parent(), siblings.index());
        let (left, right) = (siblings.left(), siblings.right());
        let (left_len, right_len) = (left.len(), right.len());
        // SAFETY: as in `steal_left_unchecked`, mirrored: the right child gives
        // separator 0 and edge 0, shifting the rest down, and the left child
        // has room at separator `left_len` and edge `left_len + 1`.
        unsafe {
            let separator = right.separator_slots().remove(0, right_len);
            let old = parent.replace_separator(index, separator.moved_back_to(fill));
            left.separator_slots().insert(left_len, left_len, old);
            let edge = right.edge_slots().remove(0, right_len + 1);
            left.edge_slots().insert(left_len + 1, left_len + 1, edge);
            left.set_len(left_len + 1);
            right.set_len(right_len - 1);
        }
    }

    unsafe fn append(left: Self, _fill: Window, separator: Separator<K, S>, right: Self) {
        let (left_len, right_len) = (left.len(), right.len());
        // SAFETY: `left_len + 1 + right_len <= capacity` (caller). The
        // separator goes into slot `left_len`, at the nodes' prefix length
        // (caller), and `right`'s separators `0..right_len` and edges
        // `0..=right_len`, all initialized, into `left_len + 1..` and
        // `left_len + 1..=left_len + 1 + right_len`: edges from a distinct
        // page and one level below, like this node's own (invariant 2).
        unsafe {
            left.separator_slots().write(left_len, separator);
            right
                .separator_slots()
                .move_run_to(0, right_len, left.separator_slots(), left_len + 1);
            right
                .edge_slots()
                .move_run_to(0, right_len + 1, left.edge_slots(), left_len + 1);
            left.set_len(left_len + 1 + right_len);
        }
    }

    fn key<'a>(self, index: usize) -> &'a K {
        self.separator_key(index)
    }

    fn inline_keys<'a>(self) -> Option<NodeInlineKeys<'a>> {
        Internal::inline_keys(self)
    }

    #[cfg(any(test, fuzzing))]
    fn length_tags<'a>(self) -> Option<&'a [u16]> {
        None
    }

    /// No further than its children's prefixes: a prefix that fits its
    /// fences never passes them, but a panic may have left one of the
    /// children's short of their fences.
    fn allowed_prefix_len(self, prefix_len: usize) -> usize {
        if prefix_len <= self.prefix_len() {
            return prefix_len;
        }
        (0..=self.len())
            .map(|index| self.child(index).prefix_len())
            .fold(prefix_len, usize::min)
    }

    fn set_windows(
        self,
        prefix_len: usize,
        inline_keys: impl ExactSizeIterator<Item = InlineWord>,
    ) {
        Internal::set_windows(self, prefix_len, inline_keys);
    }
}

/// The keys of an internal node's separators `start..start + len`, in a map
/// whose separator keys are heads (D49), lent for `'a`: each the head key of
/// the leftmost leaf right of its separator. The internal node keeps none of
/// them; getting one walks down to its leaf ([`Internal::head_leaf`]).
struct HeadKeys<'a, K, V, S> {
    node: Internal<K, V, S>,
    start: usize,
    len: usize,
    _borrow: PhantomData<&'a K>,
}

impl<K, V, S> Clone for HeadKeys<'_, K, V, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K, V, S> Copy for HeadKeys<'_, K, V, S> {}

impl<'a, K, V, S: InlineKeyStrategy<K>> KeyList<'a, K> for HeadKeys<'a, K, V, S> {
    #[inline(always)]
    fn len(self) -> usize {
        self.len
    }

    #[inline(always)]
    unsafe fn get_unchecked(self, index: usize) -> &'a K {
        self.node.head_leaf(self.start + index).head_key()
    }

    fn slice(self, range: Range<usize>) -> Self {
        assert!(
            range.start <= range.end && range.end <= self.len,
            "keys out of bounds"
        );
        Self {
            start: self.start + range.start,
            len: range.end - range.start,
            ..self
        }
    }
}
