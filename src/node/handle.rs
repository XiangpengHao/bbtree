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
//! 3. **Positions are proofs.** An [`Edge`] has `index <= len`, a [`Kv`]
//!    has `index < len`, and a [`Head`] is in a leaf that has a head. Each
//!    owns its node handle, so neither `len` nor the head can change under
//!    it.
//! 4. **Tokens are proofs.** A [`FittingEdge`] is in a node with
//!    `len < capacity`, a [`FullEdge`] in a node with `len == capacity`, and a
//!    [`Mergeable`] context fits both children and their separator in one node.
//! 5. **Heads are flagged.** A leaf's head slot is initialized exactly when its
//!    header says the leaf has a head; every operation that fills or empties
//!    the slot updates the header with it (D17).
//! 6. **Inline keys follow keys.** In a node with inline keys (D18), inline
//!    key `i` is key `i`'s for every `i < len`: every operation moves keys
//!    through [`KeySlots`], which moves their inline keys with them. All
//!    `capacity` inline-key slots are initialized, from the node's creation
//!    on (`NodePtr::from_fresh_page`), and only ever overwritten by inline
//!    keys or copies of them, so a search may read past `len`: slots `len..`
//!    hold stale inline keys, which it masks out.
//!
//! Every fact is established once, by a check the B-tree makes anyway (the
//! search, the insert-or-split decision, the merge decision) or by a checked
//! constructor off the hot path. The operations that need it take the proof
//! and check nothing.
//!
//! Inline keys steer searches only: whatever they hold, every position a
//! search returns is in bounds, so a wrong `InlineKey` gives wrong answers,
//! never undefined behavior.

use core::borrow::Borrow;
use core::cmp::Ordering;
use core::hint;
use core::marker::PhantomData;
use core::mem;
use core::ops::Range;

use super::inline_keys::{InlineTarget, InlinedKey, SearchKey};
use super::page::{FreshPage, NodePtr, PageByKind, PageKind};
use super::slots::{Down, KeySlots, Slots, Up};
use crate::inline_key::{InlineKeyStrategy, InlineWord};

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

/// Borrow type of a [`NodeRef`]: access for `'a` to a node and its subtree
/// by a walk that hands out each value as `&'a mut V` at most once, while
/// keys and structure stay shared, as `IterMut` does.
///
/// Its handles may be copied, so that both ends of a walk hold some: what
/// they offer safely only reads the structure and keys. Handing out a value
/// is unsafe, and the walk vouches that it does so once per value.
///
/// Invariant in `K` and `V`, like `&'a mut`.
pub(crate) struct ValMut<'a, K, V>(PhantomData<&'a mut (K, V)>);

/// A borrow type whose handles may be copied, because nothing they offer
/// safely changes the tree: [`Immut`] and [`ValMut`].
pub(crate) trait CopyableBorrow {}

impl CopyableBorrow for Immut<'_> {}

impl<K, V> CopyableBorrow for ValMut<'_, K, V> {}

/// A handle to one node of kind `Kind` at `height`, with the access of
/// `BorrowType` ([`Immut`], [`Mut`] or [`ValMut`]), in a map with inline-key
/// strategy `S` (D18).
///
/// Invariant:
/// - `node` holds a well-formed node at `height`, of kind `Kind` (a leaf at
///   height 0, an internal node above): `len` is at most its capacity and
///   keys `0..len` are initialized, with their inline keys if the node has
///   them. A leaf's values `0..len` are initialized too, and so is its head
///   exactly when its header says it has one. An internal node holds no
///   values (D17); its edges `0..=len` are initialized, each the root of a
///   distinct well-formed subtree at `height - 1` that the node owns.
/// - With `Immut<'a>`, the subtree stays allocated and unchanged for `'a`.
/// - With `Mut<'a, K, V>`, the subtree stays allocated for `'a` and nothing
///   else accesses it during `'a`.
/// - With `ValMut<'a, K, V>`, the subtree stays allocated, and its structure
///   and keys unchanged, for `'a`; nothing else accesses it during `'a`
///   except through copies of the handle, which only read the structure and
///   keys, and through the values they hand out, each at most once, as
///   `&'a mut V`.
///
/// Handles never lead back up to a parent, so a handle derived from another
/// one (by `reborrow_mut` or `descend`) is the only way into its subtree while
/// it lives.
pub(crate) struct NodeRef<BorrowType, K, V, S, Kind> {
    node: NodePtr<K, V, S>,
    height: usize,
    _marker: PhantomData<(BorrowType, Kind)>,
}

/// Exclusive access to a node: a [`NodeRef`] with [`Mut`].
pub(crate) type NodeMut<'a, K, V, S, Kind> = NodeRef<Mut<'a, K, V>, K, V, S, Kind>;

/// An [`Edge`] of a node with exclusive access.
pub(crate) type EdgeMut<'a, K, V, S, Kind> = Edge<NodeMut<'a, K, V, S, Kind>>;

impl<BorrowType: CopyableBorrow, K, V, S, Kind> Clone for NodeRef<BorrowType, K, V, S, Kind> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<BorrowType: CopyableBorrow, K, V, S, Kind> Copy for NodeRef<BorrowType, K, V, S, Kind> {}

// SAFETY: a shared handle gives only shared access to the keys and values of
// its subtree for `'a`, as a `&'a (K, V)` would, and nothing else: sending it
// to another thread shares them with that thread, which `K: Sync` and
// `V: Sync` allow. The strategy is only a marker.
unsafe impl<K: Sync, V: Sync, S, Kind> Send for NodeRef<Immut<'_>, K, V, S, Kind> {}

// SAFETY: as for `Send`: sharing a shared handle across threads shares the
// keys and values, nothing more.
unsafe impl<K: Sync, V: Sync, S, Kind> Sync for NodeRef<Immut<'_>, K, V, S, Kind> {}

// SAFETY: a value-mutating handle stands for an exclusive borrow of the
// whole tree, as a `&'a mut` would, which it hands out as shared keys and
// exclusive values: sending it to another thread moves that exclusive access
// there, which `K: Send` and `V: Send` allow, as for `&mut (K, V)`; the keys
// it hands out are shared only within the thread that has it. These are
// std's bounds for its `IterMut`.
unsafe impl<K: Send, V: Send, S, Kind> Send for NodeRef<ValMut<'_, K, V>, K, V, S, Kind> {}

// SAFETY: through `&` a value-mutating handle only reads the structure, keys
// and values, which `K: Sync` and `V: Sync` allow sharing.
unsafe impl<K: Sync, V: Sync, S, Kind> Sync for NodeRef<ValMut<'_, K, V>, K, V, S, Kind> {}

/// How a node searches its keys when inline keys don't decide (D18). Fixed
/// per key type at compile time, so the choice costs nothing at run time.
enum KeySearch {
    /// For keys that compare in a register: branchless binary search.
    Branchless,
    /// For keys whose comparisons are expensive: plain binary search.
    Binary,
}

impl KeySearch {
    /// Branchless for keys with no drop glue and at most 16 bytes: integers,
    /// `char`, small tuples of them. Their comparisons are a few instructions
    /// on data already loaded, so a chain of conditional moves is cheapest.
    ///
    /// Everything else (`String`, `Vec`, `Box`, large arrays) uses plain
    /// binary search. Without specialization this is a heuristic: `&str` and `&[u8]`
    /// keys also have no drop glue and fit in 16 bytes, so they take the
    /// branchless path although their comparisons follow a pointer, unless
    /// inline keys decide (D18).
    const fn for_key<K>() -> Self {
        if !mem::needs_drop::<K>() && mem::size_of::<K>() <= 16 {
            Self::Branchless
        } else {
            Self::Binary
        }
    }
}

/// A node, position or subtree sorted by kind; see [`NodeRef::force`].
pub(crate) enum ForceResult<LeafThing, InternalThing> {
    Leaf(LeafThing),
    Internal(InternalThing),
}

/// A node handle sorted by kind.
pub(crate) type NodeByKind<BorrowType, K, V, S> =
    ForceResult<NodeRef<BorrowType, K, V, S, Leaf>, NodeRef<BorrowType, K, V, S, Internal>>;

/// An edge sorted by its node's kind.
pub(crate) type EdgeByKind<BorrowType, K, V, S> = ForceResult<
    Edge<NodeRef<BorrowType, K, V, S, Leaf>>,
    Edge<NodeRef<BorrowType, K, V, S, Internal>>,
>;

/// A key sorted by its node's kind: beside its value in a leaf, or a
/// separator in an internal node.
pub(crate) type KvByKind<BorrowType, K, V, S> =
    ForceResult<Kv<NodeRef<BorrowType, K, V, S, Leaf>>, Kv<NodeRef<BorrowType, K, V, S, Internal>>>;

impl<BorrowType, K, V, S, Kind: NodeKind> NodeRef<BorrowType, K, V, S, Kind> {
    /// # Safety
    ///
    /// The type invariant holds for `node` at `height`, with this borrow type
    /// and kind.
    pub(super) unsafe fn from_raw_parts(node: NodePtr<K, V, S>, height: usize) -> Self {
        Self {
            node,
            height,
            _marker: PhantomData,
        }
    }

    /// The node's page, for a path to keep (`path.rs`): making a handle from
    /// it again is what needs care.
    pub(super) fn node_ptr(&self) -> NodePtr<K, V, S> {
        self.node
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
    pub(crate) fn reborrow(&self) -> NodeRef<Immut<'_>, K, V, S, Kind> {
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

    fn is_leaf(&self) -> bool {
        Kind::is_leaf(self.height)
    }
}

impl<BorrowType, K, V, S: InlineKeyStrategy<K>, Kind: NodeKind> NodeRef<BorrowType, K, V, S, Kind> {
    /// Binary search for `key` among the node's keys: `Found` at a key equal
    /// to it, otherwise `GoDown` at the edge where it belongs. With inline
    /// keys (D18), over those first.
    ///
    /// The position is in bounds for any `Ord` and `InlineKey`, consistent
    /// or not; see `search_index`.
    pub(crate) fn search<Q>(self, key: &SearchKey<'_, K, S, Q>) -> SearchResult<Self>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.prefetch_search_area();
        self.search_prefetched(key)
    }

    /// [`Self::search`] once [`Self::prefetch_search_area`] has run.
    fn search_prefetched<Q>(self, key: &SearchKey<'_, K, S, Q>) -> SearchResult<Self>
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
    /// random ones. With inline keys, it compares the last inline key, and
    /// the last key only if they tie.
    pub(crate) fn search_expecting_append<Q>(
        self,
        key: &SearchKey<'_, K, S, Q>,
    ) -> SearchResult<Self>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.prefetch_search_area();
        let len = self.len();
        if len > 0 && self.is_after_last_key(key, len) {
            // SAFETY: `len <= len`.
            return SearchResult::GoDown(unsafe { Edge::new_unchecked(self, len) });
        }
        self.search_prefetched(key)
    }

    /// Whether `key` is greater than the last of the node's `len > 0` keys.
    /// Inline keys that differ tell without reading the key (D18).
    fn is_after_last_key<Q>(&self, key: &SearchKey<'_, K, S, Q>, len: usize) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if let Some(target) = key.inline_target()
            && let Some(inline_keys) = self.reborrow().inline_keys()
        {
            // SAFETY: `len - 1 < len`, the number of inline keys.
            let last_inline_key = unsafe { *inline_keys.get_unchecked(len - 1) };
            if last_inline_key != target.inline_key() {
                return last_inline_key < target.inline_key();
            }
        }
        // SAFETY: `len - 1 < len`.
        let last_key = unsafe { self.reborrow().keys().get_unchecked(len - 1) };
        last_key.borrow() < key.key()
    }

    /// Starts loading every cache line a search of the node may read, up to
    /// the capacity: its keys (D20), and before them its inline keys if it
    /// has them (D18). One software prefetch per 64-byte line. Every search
    /// calls it before reading anything of the node.
    ///
    /// A search of a large node probes keys in an order that the data
    /// decides, so without it each probe waits for the previous probe's cache
    /// miss; with it, the misses overlap. With inline keys, the keys matter
    /// too: every full-key comparison, on a tie or to confirm a hit, starts
    /// by reading the key in the node. It covers the capacity, which the node
    /// kind fixes, rather than `len`: reading `len` first would make the
    /// prefetches wait for the header's miss.
    fn prefetch_search_area(&self) {
        #[cfg(target_arch = "x86_64")]
        {
            use core::arch::x86_64::{_MM_HINT_T0, _mm_prefetch};

            const CACHE_LINE_SIZE: usize = 64;
            let area = NodePtr::<K, V, S>::key_area(self.is_leaf());
            let start = area.inline_keys_offset.unwrap_or(area.keys_offset);
            let end = area.keys_offset + area.capacity * mem::size_of::<K>();
            let first_line = start - start % CACHE_LINE_SIZE;
            let page = self.node.page_start();
            for line in (first_line..end).step_by(CACHE_LINE_SIZE) {
                // SAFETY: the intrinsic needs the `sse` target feature, which
                // every x86_64 target has. A prefetch is only a hint: it never
                // faults and, to the language, accesses no memory (Miri runs
                // it as a no-op), so any address would do; this one is inside
                // the node's page anyway.
                unsafe { _mm_prefetch::<_MM_HINT_T0>(page.wrapping_add(line).cast()) };
            }
        }
    }

    /// How this node's keys are searched when inline keys don't decide,
    /// fixed per key type at compile time.
    const KEY_SEARCH: KeySearch = KeySearch::for_key::<K>();

    /// `Ok(i)` if key `i` equals `key`, otherwise `Err(i)` with `i` the edge
    /// where `key` belongs. In bounds for any `Ord` and `InlineKey`,
    /// consistent or not.
    ///
    /// If the node has inline keys and `key` has one to compare (D18), the
    /// search runs over them; otherwise over the keys, as the key type picks
    /// (D19). After monomorphization, a map without inline keys has only the
    /// latter.
    fn search_index<Q>(&self, key: &SearchKey<'_, K, S, Q>) -> Result<usize, usize>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if let Some(target) = key.inline_target()
            && let Some(inline_keys) = self.reborrow().inline_key_slots()
        {
            return self.inline_search_index(inline_keys, key, target);
        }
        let keys = self.reborrow().keys();
        match Self::KEY_SEARCH {
            KeySearch::Branchless => Self::branchless_search_index(keys, key.key()),
            KeySearch::Binary => {
                Self::binary_search_index(keys, |probe| key.key().cmp(probe.borrow()))
            }
        }
    }

    /// The search over inline keys (D18). One count over them finds the run
    /// of keys whose inline keys equal the target's: only those can equal
    /// `key`. A plain binary search over their full keys (D19's `Binary`)
    /// decides within the run, comparing them as [`SearchKey::cmp_tied`]
    /// does: by length where that tells, and byte arrays word by word. Keys
    /// outside
    /// the run differ from `key` in their inline keys, which order them (the
    /// `InlineKey` contract), so if `key` is in no run, its edge is where the
    /// run would start.
    ///
    /// In bounds for any `Ord` and `InlineKey`: the run lies within `0..len`
    /// whatever the inline keys hold ([`NodeInlineKeys::equal_run`]), taking
    /// it out of the keys checks that, and the binary search stays within
    /// it.
    fn inline_search_index<Q>(
        &self,
        inline_keys: NodeInlineKeys<'_>,
        key: &SearchKey<'_, K, S, Q>,
        target: InlineTarget,
    ) -> Result<usize, usize>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let run = inline_keys.equal_run(target.inline_key());
        let candidates = &self.reborrow().keys()[run.clone()];
        let found = Self::binary_search_index(candidates, |stored| key.cmp_tied(stored));
        found
            .map(|index| run.start + index)
            .map_err(|index| run.start + index)
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
    fn branchless_search_index<Q>(keys: &[K], key: &Q) -> Result<usize, usize>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
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

    /// Plain binary search that stops as soon as it finds the key searched
    /// for: `cmp_key(probe)` orders that key against key `probe`.
    ///
    /// In bounds by construction, whatever `cmp_key` answers: the loop keeps
    /// `low <= high <= len` and probes only `mid` with `low <= mid < high`,
    /// so `Ok(mid)` is `< len` and `Err(low)` is `<= len`.
    fn binary_search_index(keys: &[K], cmp_key: impl Fn(&K) -> Ordering) -> Result<usize, usize> {
        let (mut low, mut high) = (0, keys.len());
        while low < high {
            let mid = low + (high - low) / 2;
            // SAFETY: `mid < high <= keys.len()`.
            let probe = unsafe { keys.get_unchecked(mid) };
            match cmp_key(probe) {
                Ordering::Less => high = mid,
                Ordering::Equal => return Ok(mid),
                Ordering::Greater => low = mid + 1,
            }
        }
        Err(low)
    }

    /// The maximum number of keys.
    fn capacity(&self) -> usize {
        NodePtr::<K, V, S>::capacity(self.is_leaf())
    }

    /// The fewest keys a node below the root holds between removals: an
    /// eighth of its capacity, and at least 4 (D22), so that an internal node
    /// below the root has at least 5 children, which bounds the height
    /// (`path.rs`). A removal that leaves fewer makes the node underfull, for
    /// its parent to fix. A split leaves about half the capacity, at least 6
    /// of 12 (D13), in each half, so inserts never do.
    pub(crate) fn min_len(&self) -> usize {
        (self.capacity() / 8).max(4)
    }

    /// Whether the node holds fewer keys than [`Self::min_len`].
    pub(crate) fn is_underfull(&self) -> bool {
        self.len() < self.min_len()
    }

    /// The prefix length of a node with inline keys, reserved for
    /// 05-fence-key.md, or `None` for a node without.
    pub(super) fn prefix_len(&self) -> Option<usize> {
        // SAFETY: the node is allocated and initialized, and only this handle
        // writes its header (invariant 1).
        unsafe { self.node.prefix_len() }
    }

    fn key_slots(&self) -> KeySlots<K, S> {
        // SAFETY: the node is allocated, and is a leaf exactly if `is_leaf`
        // says so (invariant 1).
        unsafe { self.node.key_slots(self.is_leaf()) }
    }
}

/// A node's inline-key slots up to its capacity, as a search reads them
/// (D18): slots `0..len` hold the inline keys, the rest stale ones.
#[derive(Clone, Copy)]
struct NodeInlineKeys<'a> {
    slots: &'a [InlineWord],
    len: usize,
}

impl NodeInlineKeys<'_> {
    /// The positions of the inline keys equal to `inline_key`: exactly those
    /// for sorted inline keys, which the `InlineKey` contract gives, and a
    /// range within `0..len` for any. If none equals it, the empty range at
    /// the first inline key greater than it.
    ///
    /// Found by a count of every slot ([`Self::equal_run_by_count`]) where
    /// the target has AVX-512, which compares unsigned 64-bit integers in
    /// vectors, and by a binary search otherwise
    /// ([`Self::equal_run_by_search`]): without such compares, the count is
    /// an emulated sequence that costs more than the mispredicts it avoids.
    /// Measured on 1M-entry maps (04, Q6).
    ///
    /// Not generic, so it needs `#[inline]` to inline into the searches,
    /// which other crates instantiate.
    #[inline]
    fn equal_run(self, inline_key: InlineWord) -> Range<usize> {
        if cfg!(target_feature = "avx512f") {
            self.equal_run_by_count(inline_key)
        } else {
            self.equal_run_by_search(inline_key)
        }
    }

    /// One pass over every slot up to the capacity counts the inline keys
    /// below `inline_key` and those equal to it, masking out the slots from
    /// `len` on. It has no branch and no dependent load, and its trip count
    /// is the capacity, a constant per node kind, so the compiler unrolls it
    /// into a few vector compares. Within `0..len` for any slots: each of
    /// the `len` live slots counts at most once.
    #[inline]
    fn equal_run_by_count(self, inline_key: InlineWord) -> Range<usize> {
        let (mut below, mut equal) = (0, 0);
        for (index, &slot) in self.slots.iter().enumerate() {
            let live = index < self.len;
            below += usize::from(live & (slot < inline_key));
            equal += usize::from(live & (slot == inline_key));
        }
        below..below + equal
    }

    /// A branchless binary search finds where the run starts, and a scan
    /// from there finds where it ends: one compare, which inline keys that
    /// don't tie make predictable, rather than a second search, whose exit
    /// depends on where the first one ended. Within `0..len` for any slots:
    /// the search and the scan stay within the live slots.
    #[inline]
    fn equal_run_by_search(self, inline_key: InlineWord) -> Range<usize> {
        let live = &self.slots[..self.len];
        let start = Self::count_below(live, inline_key);
        let mut end = start;
        while live.get(end) == Some(&inline_key) {
            end += 1;
        }
        start..end
    }

    /// How many of `inline_keys`, sorted, are below `inline_key`, by a
    /// branchless binary search, as in `NodeRef::branchless_search_index`;
    /// in `0..=len` for any inline keys, by construction: the loop keeps
    /// `base + size <= len`, and probes only `mid = base + size / 2 < base +
    /// size`.
    #[inline]
    fn count_below(inline_keys: &[InlineWord], inline_key: InlineWord) -> usize {
        let mut size = inline_keys.len();
        if size == 0 {
            return 0;
        }
        let mut base = 0;
        while size > 1 {
            let half = size / 2;
            let mid = base + half;
            // SAFETY: `mid < base + size <= inline_keys.len()`.
            let probe = unsafe { *inline_keys.get_unchecked(mid) };
            base = hint::select_unpredictable(probe < inline_key, mid, base);
            size -= half;
        }
        // SAFETY: `base < base + size <= inline_keys.len()`, with `size == 1`
        // here.
        let last = unsafe { *inline_keys.get_unchecked(base) };
        base + usize::from(last < inline_key)
    }
}

impl<BorrowType, K, V, S: InlineKeyStrategy<K>> NodeRef<BorrowType, K, V, S, Leaf> {
    /// The leaf's head, or `None` if it has none: in a tree built by the node
    /// operations, only the leftmost leaf has none (D17).
    pub(crate) fn head(self) -> Option<Head<Self>> {
        // SAFETY: the node is allocated and its header written (invariant 1).
        let has_head = unsafe { self.node.has_head() };
        has_head.then_some(Head { leaf: self })
    }

    fn value_slots(&self) -> Slots<V, Down> {
        // SAFETY: the node is an allocated leaf (invariant 1).
        unsafe { self.node.value_slots() }
    }

    /// Slot 0 is the head, slot `i + 1` value `i`.
    fn head_and_value_slots(&self) -> Slots<V, Down> {
        // SAFETY: the node is an allocated leaf (invariant 1).
        unsafe { self.node.head_and_value_slots() }
    }
}

impl<BorrowType, K, V, S: InlineKeyStrategy<K>> NodeRef<BorrowType, K, V, S, Internal> {
    fn edge_slots(&self) -> Slots<NodePtr<K, V, S>, Up> {
        // SAFETY: the node is an allocated internal node (invariant 1).
        unsafe { self.node.edge_slots() }
    }
}

impl<BorrowType, K, V, S> NodeRef<BorrowType, K, V, S, LeafOrInternal> {
    /// Finds out the node's kind, from its height.
    pub(crate) fn force(self) -> NodeByKind<BorrowType, K, V, S> {
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

impl<'a, K, V, S: InlineKeyStrategy<K>, Kind: NodeKind> NodeRef<Immut<'a>, K, V, S, Kind> {
    /// The keys `0..len`, in order.
    pub(crate) fn keys(self) -> &'a [K] {
        // SAFETY: keys `0..len` are initialized and unchanged for `'a`
        // (invariant 1); they grow up, so memory order is index order.
        unsafe { self.key_slots().keys().run(0, self.len()).as_ref() }
    }

    /// The inline keys `0..len`, in order, if the node has them (D18):
    /// inline key `i` is key `i`'s.
    pub(super) fn inline_keys(self) -> Option<&'a [InlineWord]> {
        let inline_keys = self.key_slots().inline_keys()?;
        // SAFETY: inline keys `0..len` are initialized, like keys `0..len`
        // (invariants 1 and 6), and unchanged for `'a`; they grow up, so
        // memory order is index order.
        Some(unsafe { inline_keys.run(0, self.len()).as_ref() })
    }

    /// Every inline-key slot up to the capacity, if the node has inline keys
    /// (D18), for a search that reads past `len` and masks those out.
    fn inline_key_slots(self) -> Option<NodeInlineKeys<'a>> {
        let inline_keys = self.key_slots().inline_keys()?;
        // SAFETY: all `capacity` inline-key slots are initialized (invariant
        // 6), and none changes for `'a` (invariant 1).
        let slots = unsafe { inline_keys.run(0, self.capacity()).as_ref() };
        Some(NodeInlineKeys {
            slots,
            len: self.len(),
        })
    }
}

impl<K, V, S, Kind: NodeKind> NodeMut<'_, K, V, S, Kind> {
    /// Temporary exclusive access.
    pub(crate) fn reborrow_mut(&mut self) -> NodeMut<'_, K, V, S, Kind> {
        // SAFETY: `&mut self` keeps this handle unused while the result lives,
        // and handles never lead back up, so the result is the only way into
        // the subtree meanwhile.
        unsafe { NodeRef::from_raw_parts(self.node, self.height) }
    }
}

impl<K, V, S: InlineKeyStrategy<K>, Kind: NodeKind> NodeMut<'_, K, V, S, Kind> {
    /// # Safety
    ///
    /// `len` is at most the capacity, and keys `0..len` (with their inline
    /// keys, and in a leaf, values `0..len`; in an internal node, edges
    /// `0..=len`) are initialized by the time the node is used again.
    unsafe fn set_len(&mut self, len: usize) {
        debug_assert!(len <= self.capacity());
        // SAFETY: the node is allocated and this handle is exclusive
        // (invariant 1); `len <= capacity` is forwarded.
        unsafe { self.node.set_len(len) }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> NodeMut<'_, K, V, S, Leaf> {
    /// Makes `value` the leaf's head, and returns the old head if it had one.
    /// Keeping heads where separators expect them is the caller's business.
    pub(crate) fn replace_head(&mut self, value: V) -> Option<V> {
        let head = self.head_and_value_slots();
        // SAFETY: this handle is exclusive (invariant 1). Head slot 0 is
        // initialized exactly when the header says so (invariant 5): then it
        // is replaced, otherwise written and flagged.
        unsafe {
            if self.node.has_head() {
                Some(head.slot(0).replace(value))
            } else {
                head.slot(0).write(value);
                self.node.set_has_head(true);
                None
            }
        }
    }

    /// Empties the leaf: drops its head, then every key and value in
    /// ascending order, each key before its value, where they lie. Removing
    /// them one by one from the front would shift the rest of the leaf each
    /// time; here nothing moves.
    ///
    /// If one of their `Drop`s panics, the rest are still dropped, in the
    /// same order, before the panic goes on; a second panic aborts, as any
    /// panic during unwinding does. Either way the leaf is left empty,
    /// without a head.
    pub(crate) fn drop_entries(&mut self) {
        let len = self.len();
        let has_head = self.reborrow().head().is_some();
        // SAFETY: this handle is exclusive (invariant 1). An empty leaf
        // without a head is well-formed, and `entries` takes over the head
        // and keys and values `0..len` before any of them is dropped.
        unsafe {
            self.set_len(0);
            self.node.set_has_head(false);
        }
        let mut entries = LeafEntriesToDrop {
            keys: self.key_slots().keys(),
            head_and_values: self.head_and_value_slots(),
            has_head,
            next: 0,
            len,
        };
        // A panicking `Drop` unwinds through `entries`, whose own `Drop` goes
        // on with the rest.
        entries.drop_rest();
    }
}

/// The head and entries of a leaf that has given them up but still holds
/// them in its slots, being dropped in place by
/// [`NodeMut::drop_entries`]; whatever is left when this goes out of scope,
/// as a panicking `Drop` makes it, is dropped then.
///
/// Invariant: the head slot (slot 0 of `head_and_values`) holds a value
/// exactly if `has_head`, and keys and values `next..len` are initialized;
/// nothing else reads or drops them, and their page stays allocated while
/// this lives.
struct LeafEntriesToDrop<K, V> {
    keys: Slots<K, Up>,
    head_and_values: Slots<V, Down>,
    has_head: bool,
    next: usize,
    len: usize,
}

impl<K, V> LeafEntriesToDrop<K, V> {
    /// Drops the head, then each key before its value, in ascending order.
    /// Each is taken out of `self` before its `Drop` runs, so a panic leaves
    /// `self` holding exactly the ones not yet dropped.
    fn drop_rest(&mut self) {
        if mem::take(&mut self.has_head) {
            // SAFETY: the head slot held a value (type invariant), which
            // `has_head` no longer claims, so it is read out once.
            drop(unsafe { self.head_and_values.slot(0).read() });
        }
        while self.next < self.len {
            let index = self.next;
            self.next += 1;
            // SAFETY: `index < len <= capacity`, and key and value `index`
            // were initialized (type invariant); `next` has moved past them,
            // so they are read out once. Value `index` is head-and-value
            // slot `index + 1`. Dropping the pair drops the key, then the
            // value, even if the key's `Drop` panics.
            let key_value = unsafe {
                (
                    self.keys.slot(index).read(),
                    self.head_and_values.slot(index + 1).read(),
                )
            };
            drop(key_value);
        }
    }
}

impl<K, V> Drop for LeafEntriesToDrop<K, V> {
    fn drop(&mut self) {
        self.drop_rest();
    }
}

impl<K, V, S> NodeMut<'_, K, V, S, Internal> {
    /// # Panics
    ///
    /// If `child` is not exactly one level below this node.
    fn assert_child_level(&self, child: &Subtree<K, V, S>) {
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

/// A key of a node, with its value: index `0..len`. In a leaf, the value
/// sits beside the key. In an internal node, the key is a separator, and its
/// value is the head of the leftmost leaf right of it (D17).
///
/// Invariant: `index < node.len()`; see [`Edge`].
#[derive(Clone, Copy)]
pub(crate) struct Kv<Node> {
    node: Node,
    index: usize,
}

/// A leaf's head: the value of the separator key just left of the leaf in
/// key order, which lives in an internal node (D17).
///
/// Invariant: the leaf has a head. Holding the leaf handle keeps it there.
pub(crate) struct Head<Node> {
    leaf: Node,
}

/// The result of [`NodeRef::search`].
pub(crate) enum SearchResult<Node> {
    /// The key equal to the searched key.
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

impl<BorrowType, K, V, S, Kind: NodeKind> Edge<NodeRef<BorrowType, K, V, S, Kind>> {
    /// # Safety
    ///
    /// `index <= node.len()`.
    unsafe fn new_unchecked(node: NodeRef<BorrowType, K, V, S, Kind>, index: usize) -> Self {
        debug_assert!(index <= node.len());
        Self { node, index }
    }

    /// The key/value to the left, or `Err(self)` at the first edge.
    pub(crate) fn left_kv(self) -> Result<Kv<NodeRef<BorrowType, K, V, S, Kind>>, Self> {
        if self.index > 0 {
            // SAFETY: `index - 1 < index <= len`.
            Ok(unsafe { Kv::new_unchecked(self.node, self.index - 1) })
        } else {
            Err(self)
        }
    }

    /// The key/value to the right, or `Err(self)` at the last edge.
    pub(crate) fn right_kv(self) -> Result<Kv<NodeRef<BorrowType, K, V, S, Kind>>, Self> {
        if self.index < self.node.len() {
            // SAFETY: checked just above.
            Ok(unsafe { Kv::new_unchecked(self.node, self.index) })
        } else {
            Err(self)
        }
    }

    pub(crate) fn reborrow(&self) -> Edge<NodeRef<Immut<'_>, K, V, S, Kind>> {
        // SAFETY: the same index in the same node.
        unsafe { Edge::new_unchecked(self.node.reborrow(), self.index) }
    }
}

impl<BorrowType, K, V, S> Edge<NodeRef<BorrowType, K, V, S, LeafOrInternal>> {
    /// Finds out the node's kind; see [`NodeRef::force`].
    pub(crate) fn force(self) -> EdgeByKind<BorrowType, K, V, S> {
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

impl<BorrowType, K, V, S: InlineKeyStrategy<K>> Edge<NodeRef<BorrowType, K, V, S, Internal>> {
    /// The child this edge leads to, one level down, with the access the edge
    /// had.
    pub(crate) fn descend(self) -> NodeRef<BorrowType, K, V, S, LeafOrInternal> {
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

impl<'a, K, V, S: InlineKeyStrategy<K>, Kind: NodeKind> Edge<NodeMut<'a, K, V, S, Kind>> {
    /// The insert-or-split decision: whether the node has room for one more
    /// entry.
    pub(crate) fn check_room(self) -> EdgeRoom<NodeMut<'a, K, V, S, Kind>> {
        if self.node.len() < self.node.capacity() {
            EdgeRoom::Fits(FittingEdge(self))
        } else {
            EdgeRoom::Full(FullEdge(self))
        }
    }
}

impl<BorrowType, K, V, S, Kind: NodeKind> Kv<NodeRef<BorrowType, K, V, S, Kind>> {
    /// # Safety
    ///
    /// `index < node.len()`.
    unsafe fn new_unchecked(node: NodeRef<BorrowType, K, V, S, Kind>, index: usize) -> Self {
        debug_assert!(index < node.len());
        Self { node, index }
    }

    pub(crate) fn left_edge(self) -> Edge<NodeRef<BorrowType, K, V, S, Kind>> {
        // SAFETY: `index < len`.
        unsafe { Edge::new_unchecked(self.node, self.index) }
    }

    pub(crate) fn right_edge(self) -> Edge<NodeRef<BorrowType, K, V, S, Kind>> {
        // SAFETY: `index + 1 <= len`.
        unsafe { Edge::new_unchecked(self.node, self.index + 1) }
    }

    pub(crate) fn reborrow(&self) -> Kv<NodeRef<Immut<'_>, K, V, S, Kind>> {
        // SAFETY: the same index in the same node.
        unsafe { Kv::new_unchecked(self.node.reborrow(), self.index) }
    }
}

impl<BorrowType, K, V, S: InlineKeyStrategy<K>, Kind: NodeKind>
    Kv<NodeRef<BorrowType, K, V, S, Kind>>
{
    pub(crate) fn key(&self) -> &K {
        self.reborrow().into_key()
    }
}

impl<BorrowType, K, V, S> Kv<NodeRef<BorrowType, K, V, S, LeafOrInternal>> {
    /// Finds out the node's kind; see [`NodeRef::force`].
    pub(crate) fn force(self) -> KvByKind<BorrowType, K, V, S> {
        // SAFETY: the same index in the same node.
        unsafe {
            match self.node.force() {
                ForceResult::Leaf(node) => ForceResult::Leaf(Kv::new_unchecked(node, self.index)),
                ForceResult::Internal(node) => {
                    ForceResult::Internal(Kv::new_unchecked(node, self.index))
                }
            }
        }
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>, Kind: NodeKind> Kv<NodeRef<Immut<'a>, K, V, S, Kind>> {
    pub(crate) fn into_key(self) -> &'a K {
        // SAFETY: `index < len`, so key `index` is initialized, and it stays
        // unchanged for `'a` (invariants 1 and 3).
        unsafe { self.node.key_slots().keys().slot(self.index).as_ref() }
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>, Kind: NodeKind>
    Kv<NodeRef<ValMut<'a, K, V>, K, V, S, Kind>>
{
    pub(crate) fn into_key(self) -> &'a K {
        // SAFETY: `index < len`, so key `index` is initialized, and it stays
        // unchanged for `'a`, with no `&mut` to it (invariants 1 and 3).
        unsafe { self.node.key_slots().keys().slot(self.index).as_ref() }
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>> Kv<NodeRef<ValMut<'a, K, V>, K, V, S, Leaf>> {
    /// The key, shared, and the value, exclusive, for `'a`.
    ///
    /// # Safety
    ///
    /// The value is handed out only this once for `'a`: no other reference
    /// to it exists, or is made, while the result lives.
    pub(crate) unsafe fn into_key_value_mut(self) -> (&'a K, &'a mut V) {
        // SAFETY: `index < len`, so key and value `index` are initialized
        // (invariants 1 and 3). The key stays unchanged and shared for `'a`;
        // the value is in a slot apart from every key, and the caller
        // vouches that this is its only reference.
        unsafe {
            (
                self.node.key_slots().keys().slot(self.index).as_ref(),
                self.node.value_slots().slot(self.index).as_mut(),
            )
        }
    }
}

impl<K, V, S, Kind: NodeKind> Kv<NodeMut<'_, K, V, S, Kind>> {
    pub(crate) fn reborrow_mut(&mut self) -> Kv<NodeMut<'_, K, V, S, Kind>> {
        // SAFETY: the same index in the same node.
        unsafe { Kv::new_unchecked(self.node.reborrow_mut(), self.index) }
    }
}

impl<K, V, S: InlineKeyStrategy<K>, Kind: NodeKind> Kv<NodeMut<'_, K, V, S, Kind>> {
    /// Replaces the key and its inline key, returning the old key. Keeping
    /// the keys in order is the caller's business.
    pub(crate) fn replace_key(&mut self, key: InlinedKey<K, S>) -> K {
        // SAFETY: `index < len`, so key `index` is initialized, with its
        // inline key, and this handle is exclusive.
        unsafe { self.node.key_slots().replace(self.index, key).into_key() }
    }
}

impl<BorrowType, K, V, S: InlineKeyStrategy<K>> Kv<NodeRef<BorrowType, K, V, S, Leaf>> {
    pub(crate) fn value(&self) -> &V {
        self.reborrow().into_key_value().1
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>> Kv<NodeRef<Immut<'a>, K, V, S, Leaf>> {
    pub(crate) fn into_key_value(self) -> (&'a K, &'a V) {
        // SAFETY: `index < len`, so key and value `index` are initialized, and
        // they stay unchanged for `'a` (invariants 1 and 3).
        unsafe {
            (
                self.node.key_slots().keys().slot(self.index).as_ref(),
                self.node.value_slots().slot(self.index).as_ref(),
            )
        }
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>> Kv<NodeMut<'a, K, V, S, Leaf>> {
    pub(crate) fn value_mut(&mut self) -> &mut V {
        self.reborrow_mut().into_value_mut()
    }

    pub(crate) fn into_value_mut(self) -> &'a mut V {
        // SAFETY: `index < len`, so value `index` is initialized; the exclusive
        // access for `'a` that `self` gives up goes to the reference.
        unsafe { self.node.value_slots().slot(self.index).as_mut() }
    }

    /// Replaces the key, its inline key, and the value, returning the old
    /// key and value. Keeping the keys in order is the caller's business.
    pub(crate) fn replace_key_value(&mut self, key: InlinedKey<K, S>, value: V) -> (K, V) {
        // SAFETY: `index < len`, so key and value `index` are initialized,
        // with the key's inline key, and this handle is exclusive.
        unsafe {
            (
                self.node.key_slots().replace(self.index, key).into_key(),
                self.node.value_slots().slot(self.index).replace(value),
            )
        }
    }

    /// Removes this key/value, shifting later ones left, and returns it with
    /// the edge where it was.
    pub(crate) fn remove(self) -> ((K, V), EdgeMut<'a, K, V, S, Leaf>) {
        let Self {
            node: mut leaf,
            index,
        } = self;
        let len = leaf.len();
        // SAFETY: `index < len`, and keys (with their inline keys) and values
        // `0..len` are initialized; afterwards `0..len - 1` are. The edge has
        // `index <= len - 1`.
        unsafe {
            let key = leaf.key_slots().remove(index, len).into_key();
            let value = leaf.value_slots().remove(index, len);
            leaf.set_len(len - 1);
            ((key, value), Edge::new_unchecked(leaf, index))
        }
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>> Kv<NodeMut<'a, K, V, S, Internal>> {
    /// Removes this separator and the edge to its right, shifting later keys
    /// and edges left, and returns both: the edge as the subtree it led to,
    /// whose leftmost leaf holds the separator's value as its head (D17).
    pub(crate) fn remove_with_right_child(self) -> (K, Subtree<K, V, S>) {
        let Self {
            node: mut parent,
            index,
        } = self;
        let len = parent.len();
        // SAFETY: `index < len`, so key `index` (with its inline key) and edge
        // `index + 1` are initialized; afterwards keys `0..len - 1` and edges
        // `0..len` are. The edge was the root of a well-formed subtree one
        // level down that the node owned, and the returned `Subtree` owns now
        // (invariant 2).
        unsafe {
            let key = parent.key_slots().remove(index, len).into_key();
            let child = parent.edge_slots().remove(index + 1, len + 1);
            parent.set_len(len - 1);
            (key, Subtree::from_raw_parts(child, parent.height - 1))
        }
    }

    /// The balancing context of this separator and the two children around
    /// it.
    ///
    /// # Panics
    ///
    /// If the children are leaves and the right one has no head to hold the
    /// separator's value, which never happens in a tree built by the node
    /// operations (D17).
    pub(crate) fn into_balancing_context(self) -> BalancingContext<'a, K, V, S> {
        let child_height = self.node.height - 1;
        // SAFETY: `index < len`, so edges `index` and `index + 1` are
        // initialized: roots of two distinct, disjoint subtrees one level down
        // that the parent owns (invariant 2), and neither includes the
        // parent's page. So exclusive handles to both children can live next to
        // the parent's, as long as the context doesn't change those two edges
        // while the children are in use: only `merge` changes one, and it
        // consumes the context.
        let context = unsafe {
            let edges = self.node.edge_slots();
            let left_child = edges.slot(self.index).read();
            let right_child = edges.slot(self.index + 1).read();
            BalancingContext {
                left_child: NodeRef::from_raw_parts(left_child, child_height),
                right_child: NodeRef::from_raw_parts(right_child, child_height),
                parent: self,
            }
        };
        if let ForceResult::Leaf(right_leaf) = context.right_child.reborrow().force() {
            assert!(
                right_leaf.head().is_some(),
                "the leaf right of a separator has no head"
            );
        }
        context
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>> Head<NodeRef<Immut<'a>, K, V, S, Leaf>> {
    pub(crate) fn into_value(self) -> &'a V {
        // SAFETY: the head is initialized (invariants 3 and 5) and stays
        // unchanged for `'a` (invariant 1).
        unsafe { self.leaf.head_and_value_slots().slot(0).as_ref() }
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>> Head<NodeRef<ValMut<'a, K, V>, K, V, S, Leaf>> {
    /// The head, exclusive for `'a`.
    ///
    /// # Safety
    ///
    /// The head is handed out only this once for `'a`: no other reference
    /// to it exists, or is made, while the result lives.
    pub(crate) unsafe fn into_value_mut(self) -> &'a mut V {
        // SAFETY: the head is initialized (invariants 3 and 5), in a slot
        // apart from every key and value, and the caller vouches that this
        // is its only reference.
        unsafe { self.leaf.head_and_value_slots().slot(0).as_mut() }
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>> Head<NodeMut<'a, K, V, S, Leaf>> {
    pub(crate) fn into_value_mut(self) -> &'a mut V {
        // SAFETY: the head is initialized (invariants 3 and 5); the exclusive
        // access for `'a` that `self` gives up goes to the reference.
        unsafe { self.leaf.head_and_value_slots().slot(0).as_mut() }
    }

    /// Takes the head out of the leaf, which then has none, and returns it
    /// with the leaf.
    pub(crate) fn remove(self) -> (V, NodeMut<'a, K, V, S, Leaf>) {
        let Self { leaf } = self;
        // SAFETY: the head is initialized (invariants 3 and 5) and is read
        // out, and the header then says the leaf has none; this handle is
        // exclusive.
        unsafe {
            let value = leaf.head_and_value_slots().slot(0).read();
            leaf.node.set_has_head(false);
            (value, leaf)
        }
    }

    /// Takes out the head and the first key, whose value becomes the new
    /// head, and returns both; `None`, changing nothing, if the leaf has no
    /// keys. This is how the leaf right of a separator being removed gives
    /// up the separator's value, its head, and hands up the next key, with
    /// its inline key, to take the separator's place (D17).
    pub(crate) fn remove_with_first_key(self) -> Option<(V, InlinedKey<K, S>)> {
        let Self { mut leaf } = self;
        let len = leaf.len();
        if len == 0 {
            return None;
        }
        // SAFETY: `0 < len`, so key 0 is initialized, with its inline key,
        // and is read out, shifting keys `1..len` down. The head is
        // initialized (invariants 3 and 5), so head-and-value slots `0..=len`
        // are: slot 0, the head, is read out, and value 0 shifts into it, so
        // the leaf still has a head. Keys and values `0..len - 1` stay
        // initialized; this handle is exclusive.
        unsafe {
            let key = leaf.key_slots().remove(0, len);
            let head = leaf.head_and_value_slots().remove(0, len + 1);
            leaf.set_len(len - 1);
            Some((head, key))
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

impl<'a, K, V, S: InlineKeyStrategy<K>, Kind: NodeKind> FittingEdge<NodeMut<'a, K, V, S, Kind>> {
    /// # Safety
    ///
    /// The node has room for one more entry: `len < capacity`.
    unsafe fn new_unchecked(edge: Edge<NodeMut<'a, K, V, S, Kind>>) -> Self {
        debug_assert!(edge.node.len() < edge.node.capacity());
        Self(edge)
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>> FittingEdge<NodeMut<'a, K, V, S, Leaf>> {
    /// Inserts `key`, with its inline key, and `value` at this edge, shifting
    /// later entries right, and returns its position.
    pub(crate) fn insert_fit(
        self,
        key: InlinedKey<K, S>,
        value: V,
    ) -> Kv<NodeMut<'a, K, V, S, Leaf>> {
        let Edge {
            node: mut leaf,
            index,
        } = self.0;
        let len = leaf.len();
        // SAFETY: `index <= len < capacity` (invariants 3 and 4); keys (with
        // their inline keys) and values `0..len` are initialized, and
        // `0..len + 1` are afterwards.
        unsafe {
            leaf.key_slots().insert(index, len, key);
            leaf.value_slots().insert(index, len, value);
            leaf.set_len(len + 1);
            Kv::new_unchecked(leaf, index)
        }
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>> FittingEdge<NodeMut<'a, K, V, S, Internal>> {
    /// Inserts separator `key`, with its inline key, at this edge with
    /// `right_child` as the edge to its right, shifting later keys and edges
    /// right, and returns its position. The separator's value is the head of
    /// `right_child`'s leftmost leaf (D17).
    ///
    /// # Panics
    ///
    /// If `right_child` is not exactly one level below this node; the node is
    /// unchanged then.
    pub(crate) fn insert_fit(
        self,
        key: InlinedKey<K, S>,
        right_child: Subtree<K, V, S>,
    ) -> Kv<NodeMut<'a, K, V, S, Internal>> {
        self.0.node.assert_child_level(&right_child);
        // SAFETY: checked just above.
        unsafe { self.insert_fit_unchecked(key, right_child) }
    }

    /// # Safety
    ///
    /// `right_child` is exactly one level below this node.
    unsafe fn insert_fit_unchecked(
        self,
        key: InlinedKey<K, S>,
        right_child: Subtree<K, V, S>,
    ) -> Kv<NodeMut<'a, K, V, S, Internal>> {
        let Edge {
            node: mut parent,
            index,
        } = self.0;
        let len = parent.len();
        // SAFETY: `index <= len < capacity` (invariants 3 and 4), so
        // `index + 1 <= len + 1 < capacity + 1`; keys (with their inline keys)
        // `0..len` and edges `0..=len` are initialized, and one more of each
        // afterwards. The new edge is the root of a well-formed subtree one
        // level down (the caller), owned by the `Subtree` given up here
        // (invariant 2).
        unsafe {
            parent.key_slots().insert(index, len, key);
            parent
                .edge_slots()
                .insert(index + 1, len + 1, right_child.node);
            parent.set_len(len + 1);
            Kv::new_unchecked(parent, index)
        }
    }
}

/// A full node split in two: the node keeps its lower entries as `left`,
/// `separator` is the key for the parent, with the inline key it had, and
/// `right` is a new node at the same height with the upper entries. The
/// separator's value is the head of `right`'s leftmost leaf: in a leaf
/// split, of `right` itself (D17). `inserted` tells where the new entry
/// went.
pub(crate) struct Split<'a, K, V, S, Kind> {
    pub(crate) left: NodeMut<'a, K, V, S, Kind>,
    pub(crate) separator: InlinedKey<K, S>,
    pub(crate) right: Subtree<K, V, S, Kind>,
    pub(crate) inserted: InsertionSide,
}

/// What inserting into one node did.
pub(crate) enum Insertion<'a, K, V, S, Kind> {
    /// The node had room; the new entry's position.
    Fit(Kv<NodeMut<'a, K, V, S, Kind>>),
    /// The node was full and split; the new entry is in one of the halves,
    /// and the separator and the right half are for the parent.
    Split(Split<'a, K, V, S, Kind>),
}

/// The half of a split that takes the new entry, and its index there: of the
/// edge where it goes, until it is inserted, and of its key after.
#[derive(Clone, Copy)]
pub(crate) enum InsertionSide {
    Left(usize),
    Right(usize),
}

impl InsertionSide {
    pub(crate) fn index(self) -> usize {
        match self {
            Self::Left(index) | Self::Right(index) => index,
        }
    }

    pub(crate) fn is_right(self) -> bool {
        matches!(self, Self::Right(_))
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>> FullEdge<NodeMut<'a, K, V, S, Leaf>> {
    /// Splits the full leaf and inserts `key`/`value` on the side where this
    /// edge ended up. The separator's value becomes the right leaf's head,
    /// and the left leaf keeps its own head, if any (D17).
    pub(crate) fn split_and_insert(
        self,
        right_page: FreshPage<K, V, S, Leaf>,
        key: InlinedKey<K, S>,
        value: V,
    ) -> Split<'a, K, V, S, Leaf> {
        let mut split = self.split_in_middle(right_page);
        // SAFETY: `split` was just made, and neither half changed since.
        unsafe { split.fitting_edge() }.insert_fit(key, value);
        split
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>> FullEdge<NodeMut<'a, K, V, S, Internal>> {
    /// Splits the full internal node and inserts separator `key`, with
    /// `right_child` as the edge to its right, on the side where this edge
    /// ended up. Keys and edges move; no leaf changes.
    ///
    /// # Panics
    ///
    /// If `right_child` is not exactly one level below this node; the node is
    /// unchanged then.
    pub(crate) fn split_and_insert(
        self,
        right_page: FreshPage<K, V, S, Internal>,
        key: InlinedKey<K, S>,
        right_child: Subtree<K, V, S>,
    ) -> Split<'a, K, V, S, Internal> {
        self.0.node.assert_child_level(&right_child);
        let mut split = self.split_in_middle(right_page);
        // SAFETY: `split` was just made, and neither half changed since; both
        // halves are at the original height, checked against `right_child`
        // above.
        unsafe {
            split.fitting_edge().insert_fit_unchecked(key, right_child);
        }
        split
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>, Kind: NodeKind> FullEdge<NodeMut<'a, K, V, S, Kind>> {
    /// The split policy: the separator is the middle entry of the full node.
    /// Whichever side the new entry joins, the halves differ by at most two.
    ///
    /// Must return an index below `capacity`.
    fn separator_index(capacity: usize) -> usize {
        capacity / 2
    }

    /// Splits the full node around the key `separator_index` picks, and
    /// tells where the new entry goes: left if this edge is left of the
    /// separator, right otherwise. Both sides have room.
    ///
    /// The separator key moves up, with its inline key. In a leaf, its value
    /// stays behind as the head of the right leaf (D17). The right half goes
    /// into `right_page`, a page for the same kind of node as this one.
    fn split_in_middle(self, right_page: FreshPage<K, V, S, Kind>) -> Split<'a, K, V, S, Kind>
    where
        Kind: PageKind,
    {
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
        // policy). Keys `separator_index + 1..len` are initialized and move,
        // with their inline keys, to the start of `right`, a fresh page of the
        // same kind and map (both are `Kind`, `K`, `V`, `S`), so `is_leaf`
        // says the kind of both nodes. In a leaf, values `separator_index..len`
        // move to the head and values `0..right_len` of `right`, which then
        // has a head (invariant 5); in an internal node, edges
        // `separator_index + 1..=len` move to edges `0..=right_len`. Key
        // `separator_index` is then the last initialized one on the left and
        // is read out, with its inline key, leaving `separator_index` keys
        // (and values, or one more edge) there; the left leaf's head is
        // untouched.
        let separator = unsafe {
            left.key_slots().move_run_to(
                separator_index + 1,
                right_len,
                right.key_slots(is_leaf),
                0,
            );
            if is_leaf {
                left.node.value_slots().move_run_to(
                    separator_index,
                    right_len + 1,
                    right.head_and_value_slots(),
                    0,
                );
                right.set_has_head(true);
            } else {
                left.node.edge_slots().move_run_to(
                    separator_index + 1,
                    right_len + 1,
                    right.edge_slots(),
                    0,
                );
            }
            right.set_len(right_len);
            let separator = left.key_slots().read(separator_index);
            left.set_len(separator_index);
            separator
        };
        // SAFETY: `right` now holds a well-formed node of the same kind and
        // height as `left`: its keys, inline keys, values and head, or keys,
        // inline keys and edges, came from `left`.
        let right = unsafe { Subtree::from_raw_parts(right, left.height) };
        let inserted = if index <= separator_index {
            InsertionSide::Left(index)
        } else {
            InsertionSide::Right(index - separator_index - 1)
        };
        Split {
            left,
            separator,
            right,
            inserted,
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>, Kind: NodeKind> Split<'_, K, V, S, Kind> {
    /// The position for the new entry, in whichever half it belongs.
    ///
    /// # Safety
    ///
    /// Neither half changed since the split that made `self`.
    unsafe fn fitting_edge(&mut self) -> FittingEdge<NodeMut<'_, K, V, S, Kind>> {
        let side = self.inserted;
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

/// An owned tree: a root node at `height` and everything below it, of a map
/// with inline-key strategy `S`.
///
/// It owns its pages, keys and values, but has no `Drop`: dropping a
/// `Subtree` leaks the tree, which is safe; `Subtree::destroy` takes it apart.
/// Edges move only as `Subtree`s: inserting an edge takes one, and removing an
/// edge gives one back.
///
/// Invariant: `node` holds a well-formed node of kind `Kind` at `height` (see
/// [`NodeRef`]), and nothing but this `Subtree` reaches any node of the tree.
pub(crate) struct Subtree<K, V, S, Kind = LeafOrInternal> {
    node: NodePtr<K, V, S>,
    height: usize,
    _marker: PhantomData<(K, V, Kind)>,
}

// SAFETY: a `Subtree` owns its pages, keys and values, and nothing else
// reaches them (type invariant), as a `Box` owns its contents: sending the
// `Subtree` to another thread sends the keys and values along. The pages come
// from the global allocator, which any thread may free. The strategy is only
// a marker.
unsafe impl<K: Send, V: Send, S, Kind> Send for Subtree<K, V, S, Kind> {}

// SAFETY: a `&Subtree` gives only shared access to its keys and values
// (`borrow`, then `Immut` handles), as a `&Box` does; every change needs
// `&mut Subtree` or the `Subtree` itself.
unsafe impl<K: Sync, V: Sync, S, Kind> Sync for Subtree<K, V, S, Kind> {}

impl<K, V, S, Kind: NodeKind> Subtree<K, V, S, Kind> {
    /// # Safety
    ///
    /// The type invariant holds for `node` at `height`, with this kind.
    unsafe fn from_raw_parts(node: NodePtr<K, V, S>, height: usize) -> Self {
        Self {
            node,
            height,
            _marker: PhantomData,
        }
    }

    pub(crate) fn height(&self) -> usize {
        self.height
    }

    pub(crate) fn forget_type(self) -> Subtree<K, V, S> {
        // SAFETY: the same tree; `LeafOrInternal` fits any kind.
        unsafe { Subtree::from_raw_parts(self.node, self.height) }
    }

    pub(crate) fn borrow(&self) -> NodeRef<Immut<'_>, K, V, S, Kind> {
        // SAFETY: the tree is well-formed (type invariant), and `&self` keeps
        // it unchanged while the handle lives.
        unsafe { NodeRef::from_raw_parts(self.node, self.height) }
    }

    pub(crate) fn borrow_mut(&mut self) -> NodeMut<'_, K, V, S, Kind> {
        // SAFETY: the tree is well-formed (type invariant), and `&mut self`
        // gives the handle exclusive access.
        unsafe { NodeRef::from_raw_parts(self.node, self.height) }
    }

    /// A handle for a walk that hands out values mutably (see [`ValMut`]).
    pub(crate) fn borrow_valmut(&mut self) -> NodeRef<ValMut<'_, K, V>, K, V, S, Kind> {
        // SAFETY: the tree is well-formed (type invariant), and `&mut self`
        // gives exclusive access, which the handle and its copies only use
        // as `ValMut` allows.
        unsafe { NodeRef::from_raw_parts(self.node, self.height) }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Subtree<K, V, S, Leaf> {
    /// A new, empty leaf, without a head.
    pub(crate) fn new_leaf(page: FreshPage<K, V, S, Leaf>) -> Self {
        // SAFETY: an empty leaf is complete, and the page is owned by the
        // subtree from now on.
        unsafe { Self::from_raw_parts(NodePtr::from_fresh_page(page), 0) }
    }

    /// Releases the page of an empty leaf without a head. Were the leaf to
    /// hold entries or a head, they would be leaked.
    pub(crate) fn into_page(self) -> FreshPage<K, V, S, Leaf> {
        debug_assert_eq!(self.borrow().len(), 0, "releasing a leaf with entries");
        debug_assert!(
            self.borrow().head().is_none(),
            "releasing a leaf with a head"
        );
        // SAFETY: the node is a leaf; the subtree owned the page, and
        // consuming it leaves nothing that reaches the node (type invariant).
        unsafe { self.node.into_fresh_page() }
    }
}

impl<K, V, S> Subtree<K, V, S> {
    /// Finds out the root's kind, from the height.
    pub(crate) fn force(self) -> ForceResult<Subtree<K, V, S, Leaf>, Subtree<K, V, S, Internal>> {
        // SAFETY: the height decides the kind.
        unsafe {
            if self.height == 0 {
                ForceResult::Leaf(Subtree::from_raw_parts(self.node, self.height))
            } else {
                ForceResult::Internal(Subtree::from_raw_parts(self.node, self.height))
            }
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Subtree<K, V, S> {
    /// Puts a new root with no keys above the current one, which becomes its
    /// only child, and returns the new root.
    pub(crate) fn push_internal_level(
        &mut self,
        page: FreshPage<K, V, S, Internal>,
    ) -> NodeMut<'_, K, V, S, Internal> {
        let root = NodePtr::from_fresh_page(page);
        // SAFETY: the new root's page is allocated for an internal node. Its
        // edge 0 becomes the old root, a well-formed subtree one level down,
        // which the new root owns from now on (invariant 2); with no keys,
        // that is its only edge, and it has no inline keys either.
        unsafe { root.edge_slots().slot(0).write(self.node) };
        self.node = root;
        self.height += 1;
        // SAFETY: the tree is well-formed again, and `&mut self` gives the
        // handle exclusive access.
        unsafe { NodeRef::from_raw_parts(self.node, self.height) }
    }

    /// Removes the root, which must be internal and hold no keys, making its
    /// only child the root, and returns the old root's page: the inverse of
    /// [`Self::push_internal_level`]. Were the root to hold keys, they and
    /// the other children would be leaked.
    ///
    /// # Panics
    ///
    /// If the root is a leaf; the tree is unchanged then.
    pub(crate) fn pop_internal_level(&mut self) -> FreshPage<K, V, S, Internal> {
        assert!(self.height > 0, "popping a leaf root");
        debug_assert_eq!(Self::borrow(self).len(), 0, "popping a root with keys");
        let root = self.node;
        // SAFETY: the root is internal, so edge 0 is initialized: the root of
        // a well-formed subtree one level down that the root owned, and this
        // `Subtree` owns now (invariant 2). Nothing reaches the old root's
        // page, an internal node's, anymore.
        unsafe {
            self.node = root.edge_slots().slot(0).read();
            self.height -= 1;
            root.into_fresh_page()
        }
    }
}

/// A separator of an internal node and the two children around it, ready to
/// be merged or to move an entry from one child to the other.
///
/// Invariant: `left_child` and `right_child` are the children at the edges
/// left and right of `parent`. If they are leaves, the right one has a head,
/// which holds the separator's value (D17).
pub(crate) struct BalancingContext<'a, K, V, S> {
    parent: Kv<NodeMut<'a, K, V, S, Internal>>,
    left_child: NodeMut<'a, K, V, S, LeafOrInternal>,
    right_child: NodeMut<'a, K, V, S, LeafOrInternal>,
}

/// A [`BalancingContext`] whose children fit in one node together with the
/// parent's separator. Made only by [`BalancingContext::can_merge`].
pub(crate) struct Mergeable<'a, K, V, S>(BalancingContext<'a, K, V, S>);

/// Why a steal moved nothing.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StealError {
    /// The child to take an entry from has none.
    DonorEmpty,
    /// The child to put an entry into is full.
    ReceiverFull,
}

impl<'a, K, V, S: InlineKeyStrategy<K>> BalancingContext<'a, K, V, S> {
    pub(crate) fn left_child_len(&self) -> usize {
        self.left_child.len()
    }

    pub(crate) fn right_child_len(&self) -> usize {
        self.right_child.len()
    }

    pub(crate) fn into_parent(self) -> Kv<NodeMut<'a, K, V, S, Internal>> {
        self.parent
    }

    pub(crate) fn into_left_child(self) -> NodeMut<'a, K, V, S, LeafOrInternal> {
        self.left_child
    }

    pub(crate) fn into_right_child(self) -> NodeMut<'a, K, V, S, LeafOrInternal> {
        self.right_child
    }

    /// The merge decision: whether both children and the parent's separator
    /// fit in one node; `Err(self)` if not.
    pub(crate) fn can_merge(self) -> Result<Mergeable<'a, K, V, S>, Self> {
        let merged_len = self.left_child.len() + 1 + self.right_child.len();
        if merged_len <= self.left_child.capacity() {
            Ok(Mergeable(self))
        } else {
            Err(self)
        }
    }

    /// Rotates one entry rightwards: the left child's last key goes up into
    /// the parent, and the parent's separator comes down to the front of the
    /// right child, each with its inline key.
    ///
    /// Between leaves, the value rotates through the right child's head: the
    /// left child's last value becomes the head, and the old head joins the
    /// separator that came down (D17). Between internal children, the left
    /// child's last edge becomes the right child's first.
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
        // SAFETY: checked just above: the left child has a key `left_len - 1`
        // (with a value, or an edge `left_len`) to give, and the right child
        // has room for one more key (and value or edge). Both children are at
        // the same height, so `is_leaf` says the kind of both. The parent's
        // separator is initialized (invariant 3). Keys move with their inline
        // keys, all three nodes being of the same map (invariant 6). Between
        // leaves, the right child has a head (the context's invariant), so its
        // head-and-value slots `0..=right_len` are initialized, and inserting
        // at slot 0 shifts the old head into value 0, beside the separator now
        // at key 0; the right child keeps a head. Each child keeps keys
        // `0..len`, and values `0..len` or edges `0..=len`, initialized, and
        // the moved edge stays one level below the children, which are at the
        // same height (invariant 2).
        unsafe {
            let key = self.left_child.key_slots().remove(left_len - 1, left_len);
            let separator = parent.node.key_slots().replace(parent.index, key);
            self.right_child.key_slots().insert(0, right_len, separator);
            if is_leaf {
                let left_values = self.left_child.node.value_slots();
                let right_head_and_values = self.right_child.node.head_and_value_slots();
                let value = left_values.remove(left_len - 1, left_len);
                right_head_and_values.insert(0, right_len + 1, value);
            } else {
                let left_edges = self.left_child.node.edge_slots();
                let right_edges = self.right_child.node.edge_slots();
                let edge = left_edges.remove(left_len, left_len + 1);
                right_edges.insert(0, right_len + 1, edge);
            }
            self.left_child.set_len(left_len - 1);
            self.right_child.set_len(right_len + 1);
        }
        Ok(())
    }

    /// Rotates one entry leftwards: the right child's first key goes up into
    /// the parent, and the parent's separator comes down to the end of the
    /// left child, each with its inline key.
    ///
    /// Between leaves, the value rotates through the right child's head: the
    /// old head joins the separator that came down, and the value of the key
    /// that went up becomes the head (D17). Between internal children, the
    /// right child's first edge becomes the left child's last.
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
        // SAFETY: as in `steal_left`, mirrored: the right child gives key 0
        // (and edge 0, or its head, after which value 0 is its head), and
        // the left child has room at key and value `left_len`, or edge
        // `left_len + 1`. The left child's head is untouched.
        unsafe {
            let key = self.right_child.key_slots().remove(0, right_len);
            let separator = parent.node.key_slots().replace(parent.index, key);
            self.left_child
                .key_slots()
                .insert(left_len, left_len, separator);
            if is_leaf {
                let right_head_and_values = self.right_child.node.head_and_value_slots();
                let left_values = self.left_child.node.value_slots();
                let head = right_head_and_values.remove(0, right_len + 1);
                left_values.insert(left_len, left_len, head);
            } else {
                let right_edges = self.right_child.node.edge_slots();
                let left_edges = self.left_child.node.edge_slots();
                let edge = right_edges.remove(0, right_len + 1);
                left_edges.insert(left_len + 1, left_len + 1, edge);
            }
            self.left_child.set_len(left_len + 1);
            self.right_child.set_len(right_len - 1);
        }
        Ok(())
    }
}

impl<'a, K, V, S> Mergeable<'a, K, V, S> {
    /// Gives up the token.
    pub(crate) fn into_context(self) -> BalancingContext<'a, K, V, S> {
        self.0
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>> Mergeable<'a, K, V, S> {
    /// Moves the parent's separator and all of the right child into the left
    /// child, and removes the separator and the edge to the right child from
    /// the parent. Returns the parent's edge to the merged child, and the
    /// right child's emptied page, sorted by the children's kind.
    ///
    /// Between leaves, the separator rejoins its value, the right child's
    /// head, as an ordinary entry, and the left child keeps its own head, if
    /// any (D17). Keys move with their inline keys.
    pub(crate) fn merge(self) -> (EdgeMut<'a, K, V, S, Internal>, PageByKind<K, V, S>) {
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
        // - Parent: `index < parent_len` (invariant 3). Key `index`, with its
        //   inline key, and edge `index + 1`, the right child, come out; the
        //   parent keeps keys `0..parent_len - 1` and edges `0..parent_len`.
        // - Left child: `merged_len <= capacity` (invariant 4). The separator
        //   goes into key slot `left_len`, and the right child's keys
        //   `0..right_len` into `left_len + 1..merged_len`, all with their
        //   inline keys, the three nodes being of the same map (invariant 6).
        //   Between leaves, the right child's head and values `0..right_len`,
        //   all initialized (the context's invariant), go into values
        //   `left_len..merged_len`, the head beside the separator. Between
        //   internal children, the right child's edges `0..=right_len` go into
        //   `left_len + 1..=merged_len`, all from a distinct page and one
        //   level below, like the left child's own edges.
        // - Both children are at the same height, so `is_leaf` says the kind
        //   of both, and their pages are released as that kind's.
        // - Right child: everything moved out, and its edge is gone from the
        //   parent, so nothing reaches its page anymore (invariant 2).
        let freed_page = unsafe {
            let separator = parent.key_slots().remove(index, parent_len);
            parent.edge_slots().remove(index + 1, parent_len + 1);
            parent.set_len(parent_len - 1);

            left_child.key_slots().write(left_len, separator);
            right_child
                .key_slots()
                .move_run_to(0, right_len, left_child.key_slots(), left_len + 1);
            left_child.set_len(merged_len);
            if is_leaf {
                right_child.node.head_and_value_slots().move_run_to(
                    0,
                    right_len + 1,
                    left_child.node.value_slots(),
                    left_len,
                );
                ForceResult::Leaf(right_child.node.into_fresh_page())
            } else {
                right_child.node.edge_slots().move_run_to(
                    0,
                    right_len + 1,
                    left_child.node.edge_slots(),
                    left_len + 1,
                );
                ForceResult::Internal(right_child.node.into_fresh_page())
            }
        };
        // SAFETY: `index <= parent_len - 1`, the parent's new length.
        let merged_edge = unsafe { Edge::new_unchecked(parent, index) };
        (merged_edge, freed_page)
    }
}

#[cfg(test)]
mod tests {
    use std::string::String;
    use std::vec::Vec;

    use super::{KeySearch, NodeInlineKeys};
    use crate::inline_key::InlineWord;
    use crate::node::test_support::TestRng;

    #[test]
    fn key_types_pick_their_search() {
        assert!(matches!(KeySearch::for_key::<u64>(), KeySearch::Branchless));
        assert!(matches!(KeySearch::for_key::<u32>(), KeySearch::Branchless));
        assert!(matches!(
            KeySearch::for_key::<(u64, u64)>(),
            KeySearch::Branchless
        ));
        assert!(matches!(KeySearch::for_key::<String>(), KeySearch::Binary));
        assert!(matches!(
            KeySearch::for_key::<[u8; 32]>(),
            KeySearch::Binary
        ));
        // The documented misfire: a borrowed string looks like plain data.
        assert!(matches!(
            KeySearch::for_key::<&str>(),
            KeySearch::Branchless
        ));
    }

    /// Slots past `len` hold stale inline keys, which a search must ignore:
    /// `stale_count` random ones after `live`.
    fn slots_with_stale_tail(
        rng: &TestRng,
        live: &[InlineWord],
        stale_count: usize,
    ) -> Vec<InlineWord> {
        let stale = (0..stale_count).map(|_| rng.below(20) as InlineWord);
        live.iter().copied().chain(stale).collect()
    }

    /// On sorted inline keys, the run is exactly the equal ones, wherever
    /// they are and however many, whatever the slots past `len` hold, by
    /// either way of finding it.
    #[test]
    fn equal_runs_of_sorted_inline_keys() {
        let rng = TestRng::new(51);
        for _ in 0..if cfg!(miri) { 50 } else { 2000 } {
            let len = rng.below(40);
            let mut inline_keys: Vec<InlineWord> =
                (0..len).map(|_| rng.below(8) as InlineWord * 2).collect();
            inline_keys.sort_unstable();
            let slots = slots_with_stale_tail(&rng, &inline_keys, rng.below(8));
            let node = NodeInlineKeys { slots: &slots, len };
            for target in 0..18 {
                let start = inline_keys.partition_point(|&probe| probe < target);
                let end = inline_keys.partition_point(|&probe| probe <= target);
                assert_eq!(node.equal_run_by_count(target), start..end);
                assert_eq!(node.equal_run_by_search(target), start..end);
            }
        }
    }

    /// On any inline keys, the run stays within the live ones.
    #[test]
    fn equal_runs_stay_in_bounds_for_unsorted_inline_keys() {
        let rng = TestRng::new(52);
        for _ in 0..if cfg!(miri) { 50 } else { 2000 } {
            let len = rng.below(40);
            let inline_keys: Vec<InlineWord> =
                (0..len).map(|_| rng.below(4) as InlineWord).collect();
            let slots = slots_with_stale_tail(&rng, &inline_keys, rng.below(8));
            let node = NodeInlineKeys { slots: &slots, len };
            for target in 0..5 {
                for run in [
                    node.equal_run_by_count(target),
                    node.equal_run_by_search(target),
                ] {
                    assert!(run.start <= run.end && run.end <= len);
                }
            }
        }
    }
}
