//! Typed handles to nodes, positions in them, owned subtrees, and every
//! operation that changes a node: the trusted core of the node module (D14).
//!
//! # Safety argument
//!
//! This file, `page.rs` and `slots.rs` hold all of `node/`'s `unsafe` code;
//! the other node modules are `#![forbid(unsafe_code)]` and build on the safe
//! API here. Each `// SAFETY:` comment below relies on these invariants:
//!
//! 1. **Handles.** A [`NodeRef`] points to a well-formed node of its kind and
//!    height, with the access its borrow type promises (see [`NodeRef`]).
//! 2. **Edges own subtrees.** Every initialized edge of a node at height `h`
//!    is the only pointer to a distinct subtree at height `h - 1`. An edge is
//!    written only from a [`Subtree`] of that height (checked off the hot
//!    path), or moved between siblings, and taking one out gives the
//!    `Subtree` back.
//! 3. **Positions are proofs.** An [`Edge`] has `index <= len` and a [`Kv`]
//!    has `index < len`. Each owns its node handle, so `len` can't change
//!    under it.
//! 4. **Tokens are proofs.** A [`FittingEdge`] is in a node with
//!    `len < capacity`, a [`FullEdge`] in a node with `len == capacity`, and a
//!    [`Mergeable`] context fits both children and their separator in one node.
//!
//! Every fact is established once, by a check the B-tree makes anyway (the
//! search, the insert-or-split decision, the merge decision) or by a checked
//! index constructor off the hot path. The operations that need it take the
//! proof and check nothing.

use core::borrow::Borrow;
use core::cmp::Ordering;
use core::hint;
use core::marker::PhantomData;
use core::mem;

use super::page::{FreshPage, NodePtr};
use super::slots::{Down, Slots, Up};

/// Node kind: a leaf, at height 0.
pub(crate) enum Leaf {}

/// Node kind: an internal node, above height 0.
pub(crate) enum Internal {}

/// Node kind: either, until [`NodeRef::force`] looks at the height.
pub(crate) enum LeafOrInternal {}

/// What a node kind says about the node at some height.
///
/// # Safety
///
/// `is_leaf(height)` is `height == 0` for every height a node of this kind
/// can have: a constant for `Leaf` and `Internal`, so that their code knows
/// it statically.
pub(crate) unsafe trait NodeKind {
    fn is_leaf(height: usize) -> bool;
}

// SAFETY: leaves are exactly the nodes at height 0.
unsafe impl NodeKind for Leaf {
    fn is_leaf(_height: usize) -> bool {
        true
    }
}

// SAFETY: internal nodes are exactly the nodes above height 0.
unsafe impl NodeKind for Internal {
    fn is_leaf(_height: usize) -> bool {
        false
    }
}

// SAFETY: the height decides.
unsafe impl NodeKind for LeafOrInternal {
    fn is_leaf(height: usize) -> bool {
        height == 0
    }
}

/// Borrow type of a [`NodeRef`]: shared access for `'a` to a node and its
/// subtree, like `&'a`.
pub(crate) struct Immut<'a>(PhantomData<&'a ()>);

/// Borrow type of a [`NodeRef`]: exclusive access for `'a` to a node and its
/// subtree, like `&'a mut`.
///
/// Like `&mut`, it is invariant in `K` and `V`: a handle that could claim
/// shorter-lived keys or values could store them into a tree that outlives
/// them.
pub(crate) struct Mut<'a, K, V>(PhantomData<&'a mut (K, V)>);

/// A handle to one node of kind `Kind` at `height`, with the access of
/// `BorrowType` ([`Immut`] or [`Mut`]).
///
/// Invariant:
/// - `node` holds a well-formed node at `height`, of kind `Kind` (a leaf at
///   height 0, an internal node above): `len` is at most its capacity, keys
///   and values `0..len` are initialized, and if it is internal, edges
///   `0..=len` are too, each the root of a distinct well-formed subtree at
///   `height - 1` that the node owns.
/// - With `Immut<'a>`, the subtree stays allocated and unchanged for `'a`.
/// - With `Mut<'a, K, V>`, the subtree stays allocated for `'a` and nothing
///   else accesses it during `'a`.
///
/// Handles never lead back up to a parent, so a handle derived from another
/// one (by `reborrow_mut` or `descend`) is the only way into its subtree while
/// it lives.
pub(crate) struct NodeRef<BorrowType, K, V, Kind> {
    node: NodePtr<K, V>,
    height: usize,
    _marker: PhantomData<(BorrowType, Kind)>,
}

/// Exclusive access to a node: a [`NodeRef`] with [`Mut`].
pub(crate) type NodeMut<'a, K, V, Kind> = NodeRef<Mut<'a, K, V>, K, V, Kind>;

/// An [`Edge`] of a node with exclusive access.
pub(crate) type EdgeMut<'a, K, V, Kind> = Edge<NodeMut<'a, K, V, Kind>>;

impl<K, V, Kind> Clone for NodeRef<Immut<'_>, K, V, Kind> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K, V, Kind> Copy for NodeRef<Immut<'_>, K, V, Kind> {}

/// How a node searches its keys. Fixed per key type at compile time, so the
/// choice costs nothing at run time.
enum KeySearch {
    /// For keys that compare in a register: branchless binary search.
    Branchless,
    /// For keys whose comparisons are expensive: branchy binary search, then
    /// a linear scan of the last few keys.
    NarrowThenScan,
}

impl KeySearch {
    /// Keys left in the window when the narrowing search hands over to the
    /// linear scan.
    const SCAN_WINDOW: usize = 8;

    /// Branchless for keys with no drop glue and at most 16 bytes: integers,
    /// `char`, small tuples of them. Their comparisons are a few instructions
    /// on data already loaded, so a chain of conditional moves is cheapest.
    ///
    /// Everything else (`String`, `Vec`, `Box`, large arrays) narrows then
    /// scans. Without specialization this is a heuristic: `&str` and `&[u8]`
    /// keys also have no drop glue and fit in 16 bytes, so they take the
    /// branchless path although their comparisons follow a pointer.
    const fn for_key<K>() -> Self {
        if !mem::needs_drop::<K>() && mem::size_of::<K>() <= 16 {
            Self::Branchless
        } else {
            Self::NarrowThenScan
        }
    }
}

/// A node, position or subtree sorted by kind; see [`NodeRef::force`].
pub(crate) enum ForceResult<LeafThing, InternalThing> {
    Leaf(LeafThing),
    Internal(InternalThing),
}

/// A node handle sorted by kind.
pub(crate) type NodeByKind<BorrowType, K, V> =
    ForceResult<NodeRef<BorrowType, K, V, Leaf>, NodeRef<BorrowType, K, V, Internal>>;

/// An edge sorted by its node's kind.
pub(crate) type EdgeByKind<BorrowType, K, V> =
    ForceResult<Edge<NodeRef<BorrowType, K, V, Leaf>>, Edge<NodeRef<BorrowType, K, V, Internal>>>;

impl<BorrowType, K, V, Kind: NodeKind> NodeRef<BorrowType, K, V, Kind> {
    /// # Safety
    ///
    /// The type invariant holds for `node` at `height`, with this borrow type
    /// and kind.
    unsafe fn from_raw_parts(node: NodePtr<K, V>, height: usize) -> Self {
        Self {
            node,
            height,
            _marker: PhantomData,
        }
    }

    /// The number of keys.
    pub(crate) fn len(&self) -> usize {
        // SAFETY: the node is allocated and initialized, and only this handle
        // writes its header (invariant 1).
        unsafe { self.node.len() }
    }

    pub(crate) fn height(&self) -> usize {
        self.height
    }

    /// Temporary shared access.
    pub(crate) fn reborrow(&self) -> NodeRef<Immut<'_>, K, V, Kind> {
        // SAFETY: `&self` keeps this handle, the only one that could change
        // the subtree, unused while the result lives.
        unsafe { NodeRef::from_raw_parts(self.node, self.height) }
    }

    pub(crate) fn first_edge(self) -> Edge<Self> {
        // SAFETY: `0 <= len`.
        unsafe { Edge::new_unchecked(self, 0) }
    }

    pub(crate) fn last_edge(self) -> Edge<Self> {
        let len = self.len();
        // SAFETY: `len <= len`.
        unsafe { Edge::new_unchecked(self, len) }
    }

    /// `None` if the node has no keys.
    pub(crate) fn first_kv(self) -> Option<Kv<Self>> {
        // SAFETY: `0 < len`.
        (self.len() > 0).then(|| unsafe { Kv::new_unchecked(self, 0) })
    }

    /// `None` if the node has no keys.
    pub(crate) fn last_kv(self) -> Option<Kv<Self>> {
        let len = self.len();
        // SAFETY: `len - 1 < len`.
        (len > 0).then(|| unsafe { Kv::new_unchecked(self, len - 1) })
    }

    /// The edge at `index`. Off the hot path: it checks the index.
    ///
    /// # Panics
    ///
    /// If `index > len`.
    pub(crate) fn edge_at(self, index: usize) -> Edge<Self> {
        assert!(index <= self.len(), "edge index out of bounds");
        // SAFETY: checked just above.
        unsafe { Edge::new_unchecked(self, index) }
    }

    /// The key/value at `index`. Off the hot path: it checks the index.
    ///
    /// # Panics
    ///
    /// If `index >= len`.
    pub(crate) fn kv_at(self, index: usize) -> Kv<Self> {
        assert!(index < self.len(), "key/value index out of bounds");
        // SAFETY: checked just above.
        unsafe { Kv::new_unchecked(self, index) }
    }

    /// Binary search for `key` among the node's keys: `Found` at a key equal
    /// to it, otherwise `GoDown` at the edge where it belongs.
    ///
    /// The position is in bounds for any `Ord`, consistent or not; see
    /// `search_index`.
    pub(crate) fn search<Q>(self, key: &Q) -> SearchResult<Self>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        match self.search_index(key) {
            // SAFETY: `search_index` returns `Ok(index)` with `index < len`.
            Ok(index) => SearchResult::Found(unsafe { Kv::new_unchecked(self, index) }),
            // SAFETY: `search_index` returns `Err(index)` with `index <= len`.
            Err(index) => SearchResult::GoDown(unsafe { Edge::new_unchecked(self, index) }),
        }
    }

    /// [`Self::search`] with an append fast path for inserts: a key greater
    /// than the node's last key goes straight to the last edge.
    ///
    /// Branchless search pays a chain of dependent loads even when keys
    /// arrive in order, which a branchy search would predict perfectly. The
    /// one comparison against the last key is a well-predicted branch for
    /// ascending inserts, and an independent load (no extra dependency) for
    /// random ones.
    pub(crate) fn search_expecting_append<Q>(self, key: &Q) -> SearchResult<Self>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let len = self.len();
        if len > 0 {
            // SAFETY: `len - 1 < len`.
            let last_key = unsafe { self.reborrow().keys().get_unchecked(len - 1) };
            if last_key.borrow() < key {
                // SAFETY: `len <= len`.
                return SearchResult::GoDown(unsafe { Edge::new_unchecked(self, len) });
            }
        }
        self.search(key)
    }

    /// How this node's keys are searched, fixed per key type at compile time.
    const KEY_SEARCH: KeySearch = KeySearch::for_key::<K>();

    /// `Ok(i)` if key `i` equals `key`, otherwise `Err(i)` with `i` the edge
    /// where `key` belongs. In bounds for any `Ord`, consistent or not.
    fn search_index<Q>(&self, key: &Q) -> Result<usize, usize>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        match Self::KEY_SEARCH {
            KeySearch::Branchless => self.branchless_search_index(key),
            KeySearch::NarrowThenScan => self.narrow_then_scan_index(key),
        }
    }

    /// Branchless: every search runs the full `log2(len)` steps with no early
    /// exit, and each step only picks the next `base` with
    /// `select_unpredictable`, which compiles to a conditional move instead
    /// of a mispredicted branch. The loop narrows to `base`, the last key not
    /// greater than `key` (or 0), and one final comparison classifies it.
    ///
    /// In bounds by construction, whatever `Ord` answers: the loop keeps
    /// `base + size <= len` and probes only `mid = base + size / 2 < base + size`,
    /// so `base < len` at the end. `Ok(base)` is then `< len`, and
    /// `Err(base)`/`Err(base + 1)` are `<= len`.
    fn branchless_search_index<Q>(&self, key: &Q) -> Result<usize, usize>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let keys = self.reborrow().keys();
        let mut size = keys.len();
        if size == 0 {
            return Err(0);
        }
        let mut base = 0;
        while size > 1 {
            let half = size / 2;
            let mid = base + half;
            // SAFETY: `mid < base + size <= keys.len()`.
            let probe = unsafe { keys.get_unchecked(mid) };
            let probe_is_greater = probe.borrow().cmp(key) == Ordering::Greater;
            base = hint::select_unpredictable(probe_is_greater, base, mid);
            size -= half;
        }
        // SAFETY: `base < base + size <= keys.len()`, with `size == 1` here.
        let last_not_greater = unsafe { keys.get_unchecked(base) };
        match last_not_greater.borrow().cmp(key) {
            Ordering::Equal => Ok(base),
            Ordering::Less => Err(base + 1),
            Ordering::Greater => Err(base),
        }
    }

    /// A branchy binary search down to at most `KeySearch::SCAN_WINDOW` keys,
    /// then a linear scan that stops at the first key not less than `key`.
    ///
    /// For keys whose comparisons are expensive (a pointer to follow, a
    /// `memcmp`), predictable branches let the CPU start the next
    /// comparisons speculatively, so their cache misses overlap instead of
    /// waiting on each other as in the branchless chain.
    ///
    /// In bounds by construction, whatever `Ord` answers: the loop keeps
    /// `low <= high <= len` and probes only `mid` with `low <= mid < high`;
    /// the scan visits `low..high` and returns an index in `low..=high`.
    fn narrow_then_scan_index<Q>(&self, key: &Q) -> Result<usize, usize>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let keys = self.reborrow().keys();
        let (mut low, mut high) = (0, keys.len());
        while high - low > KeySearch::SCAN_WINDOW {
            let mid = low + (high - low) / 2;
            // SAFETY: `mid < high <= keys.len()`.
            let probe = unsafe { keys.get_unchecked(mid) };
            match key.cmp(probe.borrow()) {
                Ordering::Less => high = mid,
                Ordering::Equal => return Ok(mid),
                Ordering::Greater => low = mid + 1,
            }
        }
        // SAFETY: `low <= high <= keys.len()`.
        let window = unsafe { keys.get_unchecked(low..high) };
        for (offset, probe) in window.iter().enumerate() {
            match key.cmp(probe.borrow()) {
                Ordering::Greater => {}
                Ordering::Equal => return Ok(low + offset),
                Ordering::Less => return Err(low + offset),
            }
        }
        Err(high)
    }

    fn is_leaf(&self) -> bool {
        Kind::is_leaf(self.height)
    }

    /// The maximum number of keys.
    fn capacity(&self) -> usize {
        NodePtr::<K, V>::capacity(self.is_leaf())
    }

    /// The fewest keys either half of a split of a node of this kind holds:
    /// the smaller side of the full node around its separator, before the
    /// new entry joins a side.
    #[cfg(test)]
    pub(crate) fn min_len_after_split(&self) -> usize {
        let capacity = self.capacity();
        let separator_index = FullEdge::<NodeMut<'_, K, V, Kind>>::separator_index(capacity);
        separator_index.min(capacity - separator_index - 1)
    }

    fn key_slots(&self) -> Slots<K, Up> {
        // SAFETY: the node is allocated (invariant 1).
        unsafe { self.node.key_slots(self.is_leaf()) }
    }

    fn value_slots(&self) -> Slots<V, Down> {
        // SAFETY: the node is allocated (invariant 1).
        unsafe { self.node.value_slots(self.is_leaf()) }
    }
}

impl<BorrowType, K, V> NodeRef<BorrowType, K, V, Internal> {
    fn edge_slots(&self) -> Slots<NodePtr<K, V>, Up> {
        // SAFETY: the node is allocated (invariant 1).
        unsafe { self.node.edge_slots() }
    }
}

impl<BorrowType, K, V> NodeRef<BorrowType, K, V, LeafOrInternal> {
    /// Finds out the node's kind, from its height.
    pub(crate) fn force(self) -> NodeByKind<BorrowType, K, V> {
        // SAFETY: the height decides the kind.
        unsafe {
            if self.height == 0 {
                ForceResult::Leaf(NodeRef::from_raw_parts(self.node, self.height))
            } else {
                ForceResult::Internal(NodeRef::from_raw_parts(self.node, self.height))
            }
        }
    }
}

impl<'a, K, V, Kind: NodeKind> NodeRef<Immut<'a>, K, V, Kind> {
    /// The keys `0..len`, in order.
    pub(crate) fn keys(self) -> &'a [K] {
        // SAFETY: keys `0..len` are initialized and unchanged for `'a`
        // (invariant 1); they grow up, so memory order is index order.
        unsafe { self.key_slots().run(0, self.len()).as_ref() }
    }
}

impl<K, V, Kind: NodeKind> NodeMut<'_, K, V, Kind> {
    /// Temporary exclusive access.
    pub(crate) fn reborrow_mut(&mut self) -> NodeMut<'_, K, V, Kind> {
        // SAFETY: `&mut self` keeps this handle unused while the result lives,
        // and handles never lead back up, so the result is the only way into
        // the subtree meanwhile.
        unsafe { NodeRef::from_raw_parts(self.node, self.height) }
    }

    /// # Safety
    ///
    /// `len` is at most the capacity, and keys and values `0..len` (and in an
    /// internal node, edges `0..=len`) are initialized by the time the node is
    /// used again.
    unsafe fn set_len(&mut self, len: usize) {
        debug_assert!(len <= self.capacity());
        // SAFETY: the node is allocated and this handle is exclusive
        // (invariant 1); `len <= capacity` is forwarded.
        unsafe { self.node.set_len(len) }
    }
}

impl<K, V> NodeMut<'_, K, V, Internal> {
    /// # Panics
    ///
    /// If `child` is not exactly one level below this node.
    fn assert_child_level(&self, child: &Subtree<K, V>) {
        assert_eq!(
            child.height + 1,
            self.height,
            "an edge must lead exactly one level down"
        );
    }
}

/// A position between two adjacent keys of a node, or before the first, or
/// after the last: index `0..=len`. Where a new key goes, and in an internal
/// node, the edge to the child holding the keys in between.
///
/// Invariant: `index <= node.len()`. Holding the node handle keeps `len` from
/// changing, so the invariant holds as long as the `Edge` exists.
#[derive(Clone, Copy)]
pub(crate) struct Edge<Node> {
    node: Node,
    index: usize,
}

/// A key/value pair of a node: index `0..len`.
///
/// Invariant: `index < node.len()`; see [`Edge`].
#[derive(Clone, Copy)]
pub(crate) struct Kv<Node> {
    node: Node,
    index: usize,
}

/// The result of [`NodeRef::search`].
pub(crate) enum SearchResult<Node> {
    /// The key/value whose key equals the searched key.
    Found(Kv<Node>),
    /// The edge where the searched key belongs.
    GoDown(Edge<Node>),
}

impl<Node> Edge<Node> {
    pub(crate) fn index(&self) -> usize {
        self.index
    }

    pub(crate) fn into_node(self) -> Node {
        self.node
    }
}

impl<Node> Kv<Node> {
    pub(crate) fn index(&self) -> usize {
        self.index
    }

    pub(crate) fn into_node(self) -> Node {
        self.node
    }
}

impl<BorrowType, K, V, Kind: NodeKind> Edge<NodeRef<BorrowType, K, V, Kind>> {
    /// # Safety
    ///
    /// `index <= node.len()`.
    unsafe fn new_unchecked(node: NodeRef<BorrowType, K, V, Kind>, index: usize) -> Self {
        debug_assert!(index <= node.len());
        Self { node, index }
    }

    /// The key/value to the left, or `Err(self)` at the first edge.
    pub(crate) fn left_kv(self) -> Result<Kv<NodeRef<BorrowType, K, V, Kind>>, Self> {
        if self.index > 0 {
            // SAFETY: `index - 1 < index <= len`.
            Ok(unsafe { Kv::new_unchecked(self.node, self.index - 1) })
        } else {
            Err(self)
        }
    }

    /// The key/value to the right, or `Err(self)` at the last edge.
    pub(crate) fn right_kv(self) -> Result<Kv<NodeRef<BorrowType, K, V, Kind>>, Self> {
        if self.index < self.node.len() {
            // SAFETY: checked just above.
            Ok(unsafe { Kv::new_unchecked(self.node, self.index) })
        } else {
            Err(self)
        }
    }

    pub(crate) fn reborrow(&self) -> Edge<NodeRef<Immut<'_>, K, V, Kind>> {
        // SAFETY: the same index in the same node.
        unsafe { Edge::new_unchecked(self.node.reborrow(), self.index) }
    }
}

impl<BorrowType, K, V> Edge<NodeRef<BorrowType, K, V, LeafOrInternal>> {
    /// Finds out the node's kind; see [`NodeRef::force`].
    pub(crate) fn force(self) -> EdgeByKind<BorrowType, K, V> {
        // SAFETY: the same index in the same node.
        unsafe {
            match self.node.force() {
                ForceResult::Leaf(node) => ForceResult::Leaf(Edge::new_unchecked(node, self.index)),
                ForceResult::Internal(node) => {
                    ForceResult::Internal(Edge::new_unchecked(node, self.index))
                }
            }
        }
    }
}

impl<BorrowType, K, V> Edge<NodeRef<BorrowType, K, V, Internal>> {
    /// The child this edge leads to, one level down, with the access the edge
    /// had.
    pub(crate) fn descend(self) -> NodeRef<BorrowType, K, V, LeafOrInternal> {
        // SAFETY: `index <= len` (invariant 3), so edge `index` is initialized:
        // the root of a well-formed subtree at `height - 1` that the node owns
        // (invariants 1 and 2). The child handle takes over the access that
        // `self` gives up: copied if shared, moved if exclusive.
        unsafe {
            let child = self.node.edge_slots().slot(self.index).read();
            NodeRef::from_raw_parts(child, self.node.height - 1)
        }
    }
}

impl<'a, K, V, Kind: NodeKind> Edge<NodeMut<'a, K, V, Kind>> {
    pub(crate) fn reborrow_mut(&mut self) -> Edge<NodeMut<'_, K, V, Kind>> {
        // SAFETY: the same index in the same node.
        unsafe { Edge::new_unchecked(self.node.reborrow_mut(), self.index) }
    }

    /// The insert-or-split decision: whether the node has room for one more
    /// entry.
    pub(crate) fn check_room(self) -> EdgeRoom<NodeMut<'a, K, V, Kind>> {
        if self.node.len() < self.node.capacity() {
            EdgeRoom::Fits(FittingEdge(self))
        } else {
            EdgeRoom::Full(FullEdge(self))
        }
    }
}

impl<BorrowType, K, V, Kind: NodeKind> Kv<NodeRef<BorrowType, K, V, Kind>> {
    /// # Safety
    ///
    /// `index < node.len()`.
    unsafe fn new_unchecked(node: NodeRef<BorrowType, K, V, Kind>, index: usize) -> Self {
        debug_assert!(index < node.len());
        Self { node, index }
    }

    pub(crate) fn left_edge(self) -> Edge<NodeRef<BorrowType, K, V, Kind>> {
        // SAFETY: `index < len`.
        unsafe { Edge::new_unchecked(self.node, self.index) }
    }

    pub(crate) fn right_edge(self) -> Edge<NodeRef<BorrowType, K, V, Kind>> {
        // SAFETY: `index + 1 <= len`.
        unsafe { Edge::new_unchecked(self.node, self.index + 1) }
    }

    pub(crate) fn reborrow(&self) -> Kv<NodeRef<Immut<'_>, K, V, Kind>> {
        // SAFETY: the same index in the same node.
        unsafe { Kv::new_unchecked(self.node.reborrow(), self.index) }
    }

    pub(crate) fn key(&self) -> &K {
        self.reborrow().into_key_value().0
    }

    pub(crate) fn value(&self) -> &V {
        self.reborrow().into_key_value().1
    }
}

impl<'a, K, V, Kind: NodeKind> Kv<NodeRef<Immut<'a>, K, V, Kind>> {
    pub(crate) fn into_key_value(self) -> (&'a K, &'a V) {
        // SAFETY: `index < len`, so key and value `index` are initialized, and
        // they stay unchanged for `'a` (invariants 1 and 3).
        unsafe {
            (
                self.node.key_slots().slot(self.index).as_ref(),
                self.node.value_slots().slot(self.index).as_ref(),
            )
        }
    }
}

impl<'a, K, V, Kind: NodeKind> Kv<NodeMut<'a, K, V, Kind>> {
    pub(crate) fn reborrow_mut(&mut self) -> Kv<NodeMut<'_, K, V, Kind>> {
        // SAFETY: the same index in the same node.
        unsafe { Kv::new_unchecked(self.node.reborrow_mut(), self.index) }
    }

    pub(crate) fn value_mut(&mut self) -> &mut V {
        self.reborrow_mut().into_value_mut()
    }

    pub(crate) fn into_value_mut(self) -> &'a mut V {
        // SAFETY: `index < len`, so value `index` is initialized; the exclusive
        // access for `'a` that `self` gives up goes to the reference.
        unsafe { self.node.value_slots().slot(self.index).as_mut() }
    }

    /// Replaces the key and value, returning the old ones. Keeping the keys
    /// in order is the caller's business.
    pub(crate) fn replace_key_value(&mut self, key: K, value: V) -> (K, V) {
        // SAFETY: `index < len`, so key and value `index` are initialized, and
        // this handle is exclusive.
        unsafe {
            (
                self.node.key_slots().slot(self.index).replace(key),
                self.node.value_slots().slot(self.index).replace(value),
            )
        }
    }
}

impl<'a, K, V> Kv<NodeMut<'a, K, V, Leaf>> {
    /// Removes this key/value, shifting later ones left, and returns it with
    /// the edge where it was.
    pub(crate) fn remove(self) -> ((K, V), EdgeMut<'a, K, V, Leaf>) {
        let Self {
            node: mut leaf,
            index,
        } = self;
        let len = leaf.len();
        // SAFETY: `index < len`, and keys and values `0..len` are initialized;
        // afterwards `0..len - 1` are. The edge has `index <= len - 1`.
        unsafe {
            let key = leaf.key_slots().remove(index, len);
            let value = leaf.value_slots().remove(index, len);
            leaf.set_len(len - 1);
            ((key, value), Edge::new_unchecked(leaf, index))
        }
    }
}

impl<'a, K, V> Kv<NodeMut<'a, K, V, Internal>> {
    /// Removes this key/value and the edge to its right, shifting later ones
    /// left, and returns both: the edge as the subtree it led to.
    pub(crate) fn remove_with_right_child(self) -> ((K, V), Subtree<K, V>) {
        let Self {
            node: mut parent,
            index,
        } = self;
        let len = parent.len();
        // SAFETY: `index < len`, so key/value `index` and edge `index + 1` are
        // initialized; afterwards keys and values `0..len - 1` and edges
        // `0..len` are. The edge was the root of a well-formed subtree one
        // level down that the node owned, and the returned `Subtree` owns now
        // (invariant 2).
        unsafe {
            let key = parent.key_slots().remove(index, len);
            let value = parent.value_slots().remove(index, len);
            let child = parent.edge_slots().remove(index + 1, len + 1);
            parent.set_len(len - 1);
            (
                (key, value),
                Subtree::from_raw_parts(child, parent.height - 1),
            )
        }
    }

    /// The balancing context of this key/value and the two children around it.
    pub(crate) fn into_balancing_context(self) -> BalancingContext<'a, K, V> {
        let child_height = self.node.height - 1;
        // SAFETY: `index < len`, so edges `index` and `index + 1` are
        // initialized: roots of two distinct, disjoint subtrees one level down
        // that the parent owns (invariant 2), and neither includes the
        // parent's page. So exclusive handles to both children can live next to
        // the parent's, as long as the context doesn't change those two edges
        // while the children are in use: only `merge` changes one, and it
        // consumes the context.
        unsafe {
            let edges = self.node.edge_slots();
            let left_child = edges.slot(self.index).read();
            let right_child = edges.slot(self.index + 1).read();
            BalancingContext {
                left_child: NodeRef::from_raw_parts(left_child, child_height),
                right_child: NodeRef::from_raw_parts(right_child, child_height),
                parent: self,
            }
        }
    }
}

/// An [`Edge`] in a node with room for one more entry: `len < capacity`.
///
/// Made only by [`Edge::check_room`], and by a split, which leaves room on
/// both sides.
pub(crate) struct FittingEdge<Node>(Edge<Node>);

/// An [`Edge`] in a full node: `len == capacity`. Made only by
/// [`Edge::check_room`].
pub(crate) struct FullEdge<Node>(Edge<Node>);

/// The result of [`Edge::check_room`].
pub(crate) enum EdgeRoom<Node> {
    Fits(FittingEdge<Node>),
    Full(FullEdge<Node>),
}

impl<'a, K, V, Kind: NodeKind> FittingEdge<NodeMut<'a, K, V, Kind>> {
    /// # Safety
    ///
    /// The node has room for one more entry: `len < capacity`.
    unsafe fn new_unchecked(edge: Edge<NodeMut<'a, K, V, Kind>>) -> Self {
        debug_assert!(edge.node.len() < edge.node.capacity());
        Self(edge)
    }
}

impl<'a, K, V> FittingEdge<NodeMut<'a, K, V, Leaf>> {
    /// Inserts `key`/`value` at this edge, shifting later entries right, and
    /// returns its position.
    pub(crate) fn insert_fit(self, key: K, value: V) -> Kv<NodeMut<'a, K, V, Leaf>> {
        let Edge {
            node: mut leaf,
            index,
        } = self.0;
        let len = leaf.len();
        // SAFETY: `index <= len < capacity` (invariants 3 and 4); keys and
        // values `0..len` are initialized, and `0..len + 1` are afterwards.
        unsafe {
            leaf.key_slots().insert(index, len, key);
            leaf.value_slots().insert(index, len, value);
            leaf.set_len(len + 1);
            Kv::new_unchecked(leaf, index)
        }
    }
}

impl<'a, K, V> FittingEdge<NodeMut<'a, K, V, Internal>> {
    /// Inserts `key`/`value` at this edge with `right_child` as the edge to
    /// its right, shifting later entries and edges right, and returns its
    /// position.
    ///
    /// # Panics
    ///
    /// If `right_child` is not exactly one level below this node; the node is
    /// unchanged then.
    pub(crate) fn insert_fit(
        self,
        key: K,
        value: V,
        right_child: Subtree<K, V>,
    ) -> Kv<NodeMut<'a, K, V, Internal>> {
        self.0.node.assert_child_level(&right_child);
        // SAFETY: checked just above.
        unsafe { self.insert_fit_unchecked(key, value, right_child) }
    }

    /// # Safety
    ///
    /// `right_child` is exactly one level below this node.
    unsafe fn insert_fit_unchecked(
        self,
        key: K,
        value: V,
        right_child: Subtree<K, V>,
    ) -> Kv<NodeMut<'a, K, V, Internal>> {
        let Edge {
            node: mut parent,
            index,
        } = self.0;
        let len = parent.len();
        // SAFETY: `index <= len < capacity` (invariants 3 and 4), so
        // `index + 1 <= len + 1 < capacity + 1`; keys and values `0..len` and
        // edges `0..=len` are initialized, and one more of each afterwards. The
        // new edge is the root of a well-formed subtree one level down (the
        // caller), owned by the `Subtree` given up here (invariant 2).
        unsafe {
            parent.key_slots().insert(index, len, key);
            parent.value_slots().insert(index, len, value);
            parent
                .edge_slots()
                .insert(index + 1, len + 1, right_child.node);
            parent.set_len(len + 1);
            Kv::new_unchecked(parent, index)
        }
    }
}

/// A full node split in two: the node keeps its lower entries as `left`,
/// `separator` is the entry for the parent, and `right` is a new node at the
/// same height with the upper entries.
pub(crate) struct Split<'a, K, V, Kind> {
    pub(crate) left: NodeMut<'a, K, V, Kind>,
    pub(crate) separator: (K, V),
    pub(crate) right: Subtree<K, V, Kind>,
}

/// What inserting into one node did.
pub(crate) enum Insertion<'a, K, V, Kind> {
    /// The node had room; the new entry's position.
    Fit(Kv<NodeMut<'a, K, V, Kind>>),
    /// The node was full and split; the new entry is in one of the halves,
    /// and the separator and the right half are for the parent.
    Split(Split<'a, K, V, Kind>),
}

/// Where a split leaves the position for the new entry.
enum InsertionSide {
    Left(usize),
    Right(usize),
}

impl<'a, K, V> FullEdge<NodeMut<'a, K, V, Leaf>> {
    /// Splits the full leaf and inserts `key`/`value` on the side where this
    /// edge ended up.
    pub(crate) fn split_and_insert(
        self,
        right_page: FreshPage<K, V>,
        key: K,
        value: V,
    ) -> Split<'a, K, V, Leaf> {
        let (mut split, side) = self.split_in_middle(right_page);
        // SAFETY: `side` comes from the split that made `split`.
        unsafe { split.fitting_edge(side) }.insert_fit(key, value);
        split
    }
}

impl<'a, K, V> FullEdge<NodeMut<'a, K, V, Internal>> {
    /// Splits the full internal node and inserts `key`/`value`, with
    /// `right_child` as the edge to its right, on the side where this edge
    /// ended up.
    ///
    /// # Panics
    ///
    /// If `right_child` is not exactly one level below this node; the node is
    /// unchanged then.
    pub(crate) fn split_and_insert(
        self,
        right_page: FreshPage<K, V>,
        key: K,
        value: V,
        right_child: Subtree<K, V>,
    ) -> Split<'a, K, V, Internal> {
        self.0.node.assert_child_level(&right_child);
        let (mut split, side) = self.split_in_middle(right_page);
        // SAFETY: `side` comes from the split that made `split`, and both
        // halves are at the original height, checked against `right_child`
        // above.
        unsafe {
            split
                .fitting_edge(side)
                .insert_fit_unchecked(key, value, right_child);
        }
        split
    }
}

impl<'a, K, V, Kind: NodeKind> FullEdge<NodeMut<'a, K, V, Kind>> {
    /// The split policy: the separator is the middle entry of the full node.
    /// Whichever side the new entry joins, the halves differ by at most two.
    ///
    /// Must return an index below `capacity`.
    fn separator_index(capacity: usize) -> usize {
        capacity / 2
    }

    /// Splits the full node around the entry `separator_index` picks, and
    /// tells where the new entry goes: left if this edge is left of the
    /// separator, right otherwise. Both sides have room.
    fn split_in_middle(
        self,
        right_page: FreshPage<K, V>,
    ) -> (Split<'a, K, V, Kind>, InsertionSide) {
        let Edge {
            node: mut left,
            index,
        } = self.0;
        let len = left.len();
        let is_leaf = left.is_leaf();
        let separator_index = Self::separator_index(len);
        debug_assert!(separator_index < len);
        let right_len = len - separator_index - 1;
        let right = NodePtr::from_fresh_page(right_page);
        // SAFETY: `separator_index < len == capacity` (invariant 4 and the
        // policy). Keys and values `separator_index + 1..len` (and in an
        // internal node, edges `separator_index + 1..=len`) are initialized
        // and move to the start of `right`, a fresh page of the same kind.
        // Key/value `separator_index` is then the last initialized one on the
        // left and is read out, leaving `separator_index` entries and one more
        // edge there.
        let separator = unsafe {
            left.key_slots().move_run_to(
                separator_index + 1,
                right_len,
                right.key_slots(is_leaf),
                0,
            );
            left.value_slots().move_run_to(
                separator_index + 1,
                right_len,
                right.value_slots(is_leaf),
                0,
            );
            if !is_leaf {
                left.node.edge_slots().move_run_to(
                    separator_index + 1,
                    right_len + 1,
                    right.edge_slots(),
                    0,
                );
            }
            right.set_len(right_len);
            let separator = (
                left.key_slots().slot(separator_index).read(),
                left.value_slots().slot(separator_index).read(),
            );
            left.set_len(separator_index);
            separator
        };
        // SAFETY: `right` now holds a well-formed node of the same kind and
        // height as `left`: its entries, and in an internal node its edges,
        // came from `left`.
        let right = unsafe { Subtree::from_raw_parts(right, left.height) };
        let side = if index <= separator_index {
            InsertionSide::Left(index)
        } else {
            InsertionSide::Right(index - separator_index - 1)
        };
        (
            Split {
                left,
                separator,
                right,
            },
            side,
        )
    }
}

impl<K, V, Kind: NodeKind> Split<'_, K, V, Kind> {
    /// The position for the new entry, in whichever half it belongs.
    ///
    /// # Safety
    ///
    /// `side` comes from the `split_in_middle` that made `self`, and neither
    /// half changed since.
    unsafe fn fitting_edge(&mut self, side: InsertionSide) -> FittingEdge<NodeMut<'_, K, V, Kind>> {
        // SAFETY: a full node of `capacity` entries split around one leaves
        // `separator_index < capacity` on the left and
        // `capacity - separator_index - 1 < capacity` on the right, so both
        // have room. The original edge `index <= capacity` became
        // `index <= separator_index` on the left, or
        // `index - separator_index - 1 <= capacity - separator_index - 1` on
        // the right.
        unsafe {
            let edge = match side {
                InsertionSide::Left(index) => Edge::new_unchecked(self.left.reborrow_mut(), index),
                InsertionSide::Right(index) => Edge::new_unchecked(self.right.borrow_mut(), index),
            };
            FittingEdge::new_unchecked(edge)
        }
    }
}

/// An owned tree: a root node at `height` and everything below it.
///
/// It owns its pages, keys and values, but has no `Drop`: dropping a
/// `Subtree` leaks the tree, which is safe; `Subtree::destroy` takes it apart.
/// Edges move only as `Subtree`s: inserting an edge takes one, and removing an
/// edge gives one back.
///
/// Invariant: `node` holds a well-formed node of kind `Kind` at `height` (see
/// [`NodeRef`]), and nothing but this `Subtree` reaches any node of the tree.
pub(crate) struct Subtree<K, V, Kind = LeafOrInternal> {
    node: NodePtr<K, V>,
    height: usize,
    _marker: PhantomData<(K, V, Kind)>,
}

// SAFETY: a `Subtree` owns its pages, keys and values, and nothing else
// reaches them (type invariant), as a `Box` owns its contents: sending the
// `Subtree` to another thread sends the keys and values along. The pages come
// from the global allocator, which any thread may free.
unsafe impl<K: Send, V: Send, Kind> Send for Subtree<K, V, Kind> {}

// SAFETY: a `&Subtree` gives only shared access to its keys and values
// (`borrow`, then `Immut` handles), as a `&Box` does; every change needs
// `&mut Subtree` or the `Subtree` itself.
unsafe impl<K: Sync, V: Sync, Kind> Sync for Subtree<K, V, Kind> {}

impl<K, V, Kind: NodeKind> Subtree<K, V, Kind> {
    /// # Safety
    ///
    /// The type invariant holds for `node` at `height`, with this kind.
    unsafe fn from_raw_parts(node: NodePtr<K, V>, height: usize) -> Self {
        Self {
            node,
            height,
            _marker: PhantomData,
        }
    }

    pub(crate) fn height(&self) -> usize {
        self.height
    }

    pub(crate) fn forget_type(self) -> Subtree<K, V> {
        // SAFETY: the same tree; `LeafOrInternal` fits any kind.
        unsafe { Subtree::from_raw_parts(self.node, self.height) }
    }

    pub(crate) fn borrow(&self) -> NodeRef<Immut<'_>, K, V, Kind> {
        // SAFETY: the tree is well-formed (type invariant), and `&self` keeps
        // it unchanged while the handle lives.
        unsafe { NodeRef::from_raw_parts(self.node, self.height) }
    }

    pub(crate) fn borrow_mut(&mut self) -> NodeMut<'_, K, V, Kind> {
        // SAFETY: the tree is well-formed (type invariant), and `&mut self`
        // gives the handle exclusive access.
        unsafe { NodeRef::from_raw_parts(self.node, self.height) }
    }
}

impl<K, V> Subtree<K, V, Leaf> {
    /// A new, empty leaf.
    pub(crate) fn new_leaf(page: FreshPage<K, V>) -> Self {
        // SAFETY: an empty leaf is complete, and the page is owned by the
        // subtree from now on.
        unsafe { Self::from_raw_parts(NodePtr::from_fresh_page(page), 0) }
    }

    /// Releases the page of an empty leaf. Were the leaf to hold entries,
    /// they would be leaked.
    pub(crate) fn into_page(self) -> FreshPage<K, V> {
        debug_assert_eq!(self.borrow().len(), 0, "releasing a leaf with entries");
        // SAFETY: the subtree owned the page, and consuming it leaves nothing
        // that reaches the node (type invariant).
        unsafe { self.node.into_fresh_page() }
    }
}

impl<K, V> Subtree<K, V, Internal> {
    /// Removes the root, which must hold no keys, and returns its only child
    /// as the new tree along with the root's page. Were the root to hold keys,
    /// they, their values and the other children would be leaked.
    pub(crate) fn pop_internal_level(self) -> (Subtree<K, V>, FreshPage<K, V>) {
        debug_assert_eq!(self.borrow().len(), 0, "popping a root with keys");
        // SAFETY: the root is internal, so edge 0 is initialized: the root of a
        // well-formed subtree one level down that the root owned, and the
        // returned `Subtree` owns now (invariant 2). Consuming `self` leaves
        // nothing that reaches the root's page.
        unsafe {
            let child = self.node.edge_slots().slot(0).read();
            (
                Subtree::from_raw_parts(child, self.height - 1),
                self.node.into_fresh_page(),
            )
        }
    }
}

impl<K, V> Subtree<K, V> {
    /// Finds out the root's kind, from the height.
    pub(crate) fn force(self) -> ForceResult<Subtree<K, V, Leaf>, Subtree<K, V, Internal>> {
        // SAFETY: the height decides the kind.
        unsafe {
            if self.height == 0 {
                ForceResult::Leaf(Subtree::from_raw_parts(self.node, self.height))
            } else {
                ForceResult::Internal(Subtree::from_raw_parts(self.node, self.height))
            }
        }
    }

    /// Puts a new root with no keys above the current one, which becomes its
    /// only child, and returns the new root.
    pub(crate) fn push_internal_level(
        &mut self,
        page: FreshPage<K, V>,
    ) -> NodeMut<'_, K, V, Internal> {
        let root = NodePtr::from_fresh_page(page);
        // SAFETY: the new root's page is allocated. Its edge 0 becomes the old
        // root, a well-formed subtree one level down, which the new root owns
        // from now on (invariant 2); with no keys, that is its only edge.
        unsafe { root.edge_slots().slot(0).write(self.node) };
        self.node = root;
        self.height += 1;
        // SAFETY: the tree is well-formed again, and `&mut self` gives the
        // handle exclusive access.
        unsafe { NodeRef::from_raw_parts(self.node, self.height) }
    }
}

/// A key/value of an internal node and the two children around it, ready to
/// be merged or to move an entry from one child to the other.
///
/// Invariant: `left_child` and `right_child` are the children at the edges
/// left and right of `parent`.
pub(crate) struct BalancingContext<'a, K, V> {
    parent: Kv<NodeMut<'a, K, V, Internal>>,
    left_child: NodeMut<'a, K, V, LeafOrInternal>,
    right_child: NodeMut<'a, K, V, LeafOrInternal>,
}

/// A [`BalancingContext`] whose children fit in one node together with the
/// parent's key/value. Made only by [`BalancingContext::can_merge`].
pub(crate) struct Mergeable<'a, K, V>(BalancingContext<'a, K, V>);

/// Why a steal moved nothing.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StealError {
    /// The child to take an entry from has none.
    DonorEmpty,
    /// The child to put an entry into is full.
    ReceiverFull,
}

impl<'a, K, V> BalancingContext<'a, K, V> {
    pub(crate) fn left_child_len(&self) -> usize {
        self.left_child.len()
    }

    pub(crate) fn right_child_len(&self) -> usize {
        self.right_child.len()
    }

    pub(crate) fn into_parent(self) -> Kv<NodeMut<'a, K, V, Internal>> {
        self.parent
    }

    pub(crate) fn into_left_child(self) -> NodeMut<'a, K, V, LeafOrInternal> {
        self.left_child
    }

    pub(crate) fn into_right_child(self) -> NodeMut<'a, K, V, LeafOrInternal> {
        self.right_child
    }

    /// The merge decision: whether both children and the parent's key/value
    /// fit in one node; `Err(self)` if not.
    pub(crate) fn can_merge(self) -> Result<Mergeable<'a, K, V>, Self> {
        let merged_len = self.left_child.len() + 1 + self.right_child.len();
        if merged_len <= self.left_child.capacity() {
            Ok(Mergeable(self))
        } else {
            Err(self)
        }
    }

    /// Rotates one entry rightwards: the left child's last key/value goes up
    /// into the parent and the parent's goes down to the front of the right
    /// child. For internal children, the left child's last edge becomes the
    /// right child's first.
    pub(crate) fn steal_left(&mut self) -> Result<(), StealError> {
        let left_len = self.left_child.len();
        let right_len = self.right_child.len();
        if left_len == 0 {
            return Err(StealError::DonorEmpty);
        }
        if right_len == self.right_child.capacity() {
            return Err(StealError::ReceiverFull);
        }
        let is_leaf = self.left_child.is_leaf();
        let parent = &mut self.parent;
        // SAFETY: checked just above: the left child has an entry
        // `left_len - 1` (and an edge `left_len`) to give, and the right child
        // has room for one more entry (and edge). The parent's key/value is
        // initialized (invariant 3). Each child keeps entries `0..len` and
        // edges `0..=len` initialized, and the moved edge stays one level
        // below the children, which are at the same height (invariant 2).
        unsafe {
            let key = self.left_child.key_slots().remove(left_len - 1, left_len);
            let value = self.left_child.value_slots().remove(left_len - 1, left_len);
            let edge = (!is_leaf).then(|| {
                self.left_child
                    .node
                    .edge_slots()
                    .remove(left_len, left_len + 1)
            });
            self.left_child.set_len(left_len - 1);

            let key = parent.node.key_slots().slot(parent.index).replace(key);
            let value = parent.node.value_slots().slot(parent.index).replace(value);

            self.right_child.key_slots().insert(0, right_len, key);
            self.right_child.value_slots().insert(0, right_len, value);
            if let Some(edge) = edge {
                self.right_child
                    .node
                    .edge_slots()
                    .insert(0, right_len + 1, edge);
            }
            self.right_child.set_len(right_len + 1);
        }
        Ok(())
    }

    /// Rotates one entry leftwards: the right child's first key/value goes up
    /// into the parent and the parent's goes down to the end of the left
    /// child. For internal children, the right child's first edge becomes the
    /// left child's last.
    pub(crate) fn steal_right(&mut self) -> Result<(), StealError> {
        let left_len = self.left_child.len();
        let right_len = self.right_child.len();
        if right_len == 0 {
            return Err(StealError::DonorEmpty);
        }
        if left_len == self.left_child.capacity() {
            return Err(StealError::ReceiverFull);
        }
        let is_leaf = self.left_child.is_leaf();
        let parent = &mut self.parent;
        // SAFETY: as in `steal_left`, mirrored: the right child gives entry
        // and edge 0, and the left child has room at entry `left_len` and
        // edge `left_len + 1`.
        unsafe {
            let key = self.right_child.key_slots().remove(0, right_len);
            let value = self.right_child.value_slots().remove(0, right_len);
            let edge =
                (!is_leaf).then(|| self.right_child.node.edge_slots().remove(0, right_len + 1));
            self.right_child.set_len(right_len - 1);

            let key = parent.node.key_slots().slot(parent.index).replace(key);
            let value = parent.node.value_slots().slot(parent.index).replace(value);

            self.left_child.key_slots().insert(left_len, left_len, key);
            self.left_child
                .value_slots()
                .insert(left_len, left_len, value);
            if let Some(edge) = edge {
                self.left_child
                    .node
                    .edge_slots()
                    .insert(left_len + 1, left_len + 1, edge);
            }
            self.left_child.set_len(left_len + 1);
        }
        Ok(())
    }
}

impl<'a, K, V> Mergeable<'a, K, V> {
    /// Gives up the token.
    pub(crate) fn into_context(self) -> BalancingContext<'a, K, V> {
        self.0
    }

    /// Moves the parent's key/value and all of the right child into the left
    /// child, and removes that key/value and the edge to the right child from
    /// the parent. Returns the parent's edge to the merged child, and the
    /// right child's emptied page.
    pub(crate) fn merge(self) -> (EdgeMut<'a, K, V, Internal>, FreshPage<K, V>) {
        let BalancingContext {
            parent: Kv {
                node: mut parent,
                index,
            },
            mut left_child,
            right_child,
        } = self.0;
        let parent_len = parent.len();
        let left_len = left_child.len();
        let right_len = right_child.len();
        let merged_len = left_len + 1 + right_len;
        let is_leaf = left_child.is_leaf();
        // SAFETY:
        // - Parent: `index < parent_len` (invariant 3). Key/value `index` and
        //   edge `index + 1`, the right child, come out; the parent keeps keys
        //   and values `0..parent_len - 1` and edges `0..parent_len`.
        // - Left child: `merged_len <= capacity` (invariant 4). The parent's
        //   key/value goes into slot `left_len`, the right child's entries
        //   `0..right_len` into `left_len + 1..merged_len`, and for internal
        //   children, its edges `0..=right_len` into
        //   `left_len + 1..=merged_len`, all from a distinct page and one level
        //   below, like the left child's own edges.
        // - Right child: everything moved out, and its edge is gone from the
        //   parent, so nothing reaches its page anymore (invariant 2).
        let freed_page = unsafe {
            let key = parent.key_slots().remove(index, parent_len);
            let value = parent.value_slots().remove(index, parent_len);
            parent.edge_slots().remove(index + 1, parent_len + 1);
            parent.set_len(parent_len - 1);

            left_child.key_slots().slot(left_len).write(key);
            left_child.value_slots().slot(left_len).write(value);
            right_child
                .key_slots()
                .move_run_to(0, right_len, left_child.key_slots(), left_len + 1);
            right_child.value_slots().move_run_to(
                0,
                right_len,
                left_child.value_slots(),
                left_len + 1,
            );
            if !is_leaf {
                right_child.node.edge_slots().move_run_to(
                    0,
                    right_len + 1,
                    left_child.node.edge_slots(),
                    left_len + 1,
                );
            }
            left_child.set_len(merged_len);
            right_child.node.into_fresh_page()
        };
        // SAFETY: `index <= parent_len - 1`, the parent's new length.
        let merged_edge = unsafe { Edge::new_unchecked(parent, index) };
        (merged_edge, freed_page)
    }
}

#[cfg(test)]
mod tests {
    use std::string::String;

    use super::KeySearch;

    #[test]
    fn key_types_pick_their_search() {
        assert!(matches!(KeySearch::for_key::<u64>(), KeySearch::Branchless));
        assert!(matches!(KeySearch::for_key::<u32>(), KeySearch::Branchless));
        assert!(matches!(
            KeySearch::for_key::<(u64, u64)>(),
            KeySearch::Branchless
        ));
        assert!(matches!(
            KeySearch::for_key::<String>(),
            KeySearch::NarrowThenScan
        ));
        assert!(matches!(
            KeySearch::for_key::<[u8; 32]>(),
            KeySearch::NarrowThenScan
        ));
        // The documented misfire: a borrowed string looks like plain data.
        assert!(matches!(
            KeySearch::for_key::<&str>(),
            KeySearch::Branchless
        ));
    }
}
