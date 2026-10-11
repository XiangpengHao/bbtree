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
//!    key `i` is key `i`'s window at the node's prefix length for every
//!    `i < len` (05-fence-key.md). An internal byte-string node may instead
//!    keep windows at a local search prefix; queries validate the extra
//!    bytes, and every key-changing primitive first restores fence windows
//!    (`local_prefix.rs`). Exported key/bound windows always use the fences.
//!    Every operation moves keys through
//!    [`KeySlots`], which moves their inline keys with them; a key that
//!    moves up into a node, or a node whose prefix length shrinks, gets its
//!    window moved back with [`Window::moved_back_to`], and a node whose
//!    prefix length grows reads its keys (`prefix.rs`). All `capacity` inline-key
//!    slots are initialized, from the node's creation on
//!    (`NodePtr::from_fresh_page`), so a search may read past `len`; slots
//!    `len..` hold the greatest inline key, all ones, so that a search may
//!    count every slot without masking those out: a fresh page starts so,
//!    and [`NodeMut::set_len`] refills the slots a shorter length leaves.
//! 7. **Length tags follow keys.** Leaves whose plan keeps length tags
//!    (`node_plan.rs`) keep each live key's byte length modulo 65536 in the
//!    corresponding tag slot. `KeySlots` maintains these through every
//!    mutation. All capacity slots start initialized; a search masks out
//!    slots outside its live primary run and always confirms each selected
//!    candidate against the full key.
//!
//! Every fact is established once, by a check the B-tree makes anyway (the
//! search, the insert-or-split decision, the merge decision) or by a checked
//! constructor off the hot path. The operations that need it take the proof
//! and check nothing.
//!
//! Inline keys and prefix lengths steer searches only: whatever they hold,
//! every position a search returns is in bounds, so a wrong `InlineKey`, or
//! a wrong prefix length, gives wrong answers, never undefined behavior.

use core::borrow::Borrow;
use core::cmp::Ordering;
use core::hint;
use core::marker::PhantomData;
use core::ops::Range;
use core::ptr::NonNull;

use super::inline_keys::{InlineSlot, InlineTarget, InlinedKey, SearchKey, Window};
use super::length_tags::{LengthMatch, LengthTags};
use super::node_plan::{KeySearch, NodePlan};
use super::page::{FreshPage, NodePtr, PageByKind, PageKind};
pub(crate) use super::slots::NodeKeys;
use super::slots::{InlineKeySlots, KeySlots, Slots};
use crate::inline_key::{InlineKeyStrategy, InlineWord};

/// Node kind: a leaf, at height 0.
pub(crate) enum Leaf {}

/// Node kind: an internal node, above height 0.
pub(crate) enum Internal {}

/// Node kind: either, until [`NodeRef::force`] looks at the height.
pub(crate) enum LeafOrInternal {}

/// A node kind: [`Leaf`], [`Internal`], or [`LeafOrInternal`] until
/// [`NodeRef::force`] decides. What depends on the kind, the node's
/// capacity and where its slots are, is only for the kinds known at compile
/// time, [`PageKind`]s: a handle of either kind can only be told its kind,
/// once, so that no code below decides it again.
pub(crate) trait NodeKind {}

impl NodeKind for Leaf {}

impl NodeKind for Internal {}

impl NodeKind for LeafOrInternal {}

/// The slots that follow a node's keys, one more than it has keys, by its
/// kind (`layout.rs`): a leaf's head and values, the head in slot 0 and value
/// `i` in slot `i + 1` (D17), or an internal node's edges, edge `i` in slot
/// `i` (D12). Either way slot `i + 1` goes with key `i`, the one left of
/// it, so splitting, rebalancing and merging move the trailing slots the
/// same way whatever the kind, and the kinds differ only in what slot 0
/// holds: a head a leaf may lack, or an edge an internal node always has.
///
/// # Safety
///
/// The moves are those of [`Slots`] over the node's `capacity + 1` trailing
/// slots, and `has_first_slot` says whether slot 0 is initialized, as
/// invariants 1 and 5 say for a node of the kind.
pub(crate) unsafe trait TrailingSlots: PageKind {
    /// Moves trailing slots `start..start + count` of `node` into
    /// `dest_start..dest_start + count` of `dest`; see [`Slots::move_run_to`].
    ///
    /// # Safety
    ///
    /// Both are allocated nodes of this kind, in different pages, and the
    /// contract of [`Slots::move_run_to`] holds for their trailing slots.
    unsafe fn move_run<K, V, S: InlineKeyStrategy<K>>(
        node: NodePtr<K, V, S>,
        start: usize,
        count: usize,
        dest: NodePtr<K, V, S>,
        dest_start: usize,
    );

    /// Takes trailing slot `index` out of `node`, whose slots `0..len` are
    /// in use, and inserts it into `dest`, whose slots `0..dest_len` are,
    /// at `dest_index`; see [`Slots::remove`] and [`Slots::insert`].
    ///
    /// # Safety
    ///
    /// Both are allocated nodes of this kind, in different pages, and the
    /// contracts of [`Slots::remove`] and [`Slots::insert`] hold for their
    /// trailing slots.
    unsafe fn move_slot<K, V, S: InlineKeyStrategy<K>>(
        node: NodePtr<K, V, S>,
        index: usize,
        len: usize,
        dest: NodePtr<K, V, S>,
        dest_index: usize,
        dest_len: usize,
    );

    /// Whether slot 0 of `node` is initialized.
    ///
    /// # Safety
    ///
    /// `node` is an allocated node of this kind.
    unsafe fn has_first_slot<K, V, S: InlineKeyStrategy<K>>(node: NodePtr<K, V, S>) -> bool;

    /// Records that slot 0 of `node` is initialized, which a leaf's header
    /// flags (invariant 5).
    ///
    /// # Safety
    ///
    /// `node` is an allocated node of this kind with exclusive access, and
    /// its slot 0 is initialized.
    unsafe fn set_has_first_slot<K, V, S: InlineKeyStrategy<K>>(node: NodePtr<K, V, S>);
}

// SAFETY: a leaf's trailing slots are its head and values, its head
// initialized exactly when its header says so (invariant 5).
unsafe impl TrailingSlots for Leaf {
    unsafe fn move_run<K, V, S: InlineKeyStrategy<K>>(
        node: NodePtr<K, V, S>,
        start: usize,
        count: usize,
        dest: NodePtr<K, V, S>,
        dest_start: usize,
    ) {
        // SAFETY: forwarded.
        unsafe {
            node.head_and_value_slots().move_run_to(
                start,
                count,
                dest.head_and_value_slots(),
                dest_start,
            );
        }
    }

    unsafe fn move_slot<K, V, S: InlineKeyStrategy<K>>(
        node: NodePtr<K, V, S>,
        index: usize,
        len: usize,
        dest: NodePtr<K, V, S>,
        dest_index: usize,
        dest_len: usize,
    ) {
        // SAFETY: forwarded.
        unsafe {
            let value = node.head_and_value_slots().remove(index, len);
            dest.head_and_value_slots()
                .insert(dest_index, dest_len, value);
        }
    }

    unsafe fn has_first_slot<K, V, S: InlineKeyStrategy<K>>(node: NodePtr<K, V, S>) -> bool {
        // SAFETY: forwarded.
        unsafe { node.has_head() }
    }

    unsafe fn set_has_first_slot<K, V, S: InlineKeyStrategy<K>>(node: NodePtr<K, V, S>) {
        // SAFETY: forwarded.
        unsafe { node.set_has_head(true) }
    }
}

// SAFETY: an internal node's trailing slots are its edges, of which edge 0
// is always initialized (invariant 1).
unsafe impl TrailingSlots for Internal {
    unsafe fn move_run<K, V, S: InlineKeyStrategy<K>>(
        node: NodePtr<K, V, S>,
        start: usize,
        count: usize,
        dest: NodePtr<K, V, S>,
        dest_start: usize,
    ) {
        // SAFETY: forwarded.
        unsafe {
            node.edge_slots()
                .move_run_to(start, count, dest.edge_slots(), dest_start);
        }
    }

    unsafe fn move_slot<K, V, S: InlineKeyStrategy<K>>(
        node: NodePtr<K, V, S>,
        index: usize,
        len: usize,
        dest: NodePtr<K, V, S>,
        dest_index: usize,
        dest_len: usize,
    ) {
        // SAFETY: forwarded.
        unsafe {
            let edge = node.edge_slots().remove(index, len);
            dest.edge_slots().insert(dest_index, dest_len, edge);
        }
    }

    unsafe fn has_first_slot<K, V, S: InlineKeyStrategy<K>>(_node: NodePtr<K, V, S>) -> bool {
        true
    }

    unsafe fn set_has_first_slot<K, V, S: InlineKeyStrategy<K>>(_node: NodePtr<K, V, S>) {}
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

/// Borrow type of a [`NodeRef`]: ownership of a tree that a walk is taking
/// apart, moving entries out from either end and freeing each node once past
/// it (`walk.rs`), as std's `IntoIter` does.
///
/// Its handles may be copied, so that both ends of the walk hold some. Past
/// the walk's ends, entries have been moved out and nodes freed, while the
/// nodes' lengths still count them: only the walk knows where the gaps are.
/// What the handles offer safely reads only the structure, and the walk uses
/// them only between its ends. Moving an entry out and freeing a node are
/// unsafe, and the walk vouches that it does each once.
///
/// It owns `K` and `V`, and is covariant in them, as an owned tree is.
pub(crate) struct Dying<K, V>(PhantomData<(K, V)>);

/// A borrow type whose handles may be copied, because nothing they offer
/// safely changes the tree: [`Immut`], [`ValMut`] and [`Dying`].
pub(crate) trait CopyableBorrow {}

impl CopyableBorrow for Immut<'_> {}

impl<K, V> CopyableBorrow for ValMut<'_, K, V> {}

impl<K, V> CopyableBorrow for Dying<K, V> {}

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
/// - With `Dying<K, V>`, the walk that holds the handle and its copies owns
///   the tree. The node stays allocated, its length and edges unchanged,
///   until the walk frees it; its keys and values are initialized except
///   those the walk has moved out, which it never reads again.
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

// SAFETY: a dying handle stands for ownership of the tree's keys and values,
// as a `Box<(K, V)>` would: sending it to another thread sends them there,
// which `K: Send` and `V: Send` allow. The pages come from the global
// allocator, which any thread may free.
unsafe impl<K: Send, V: Send, S, Kind> Send for NodeRef<Dying<K, V>, K, V, S, Kind> {}

// SAFETY: through `&` a dying handle only reads the structure, and the keys
// and values not moved out, which `K: Sync` and `V: Sync` allow sharing.
unsafe impl<K: Sync, V: Sync, S, Kind> Sync for NodeRef<Dying<K, V>, K, V, S, Kind> {}

/// A node, position or subtree sorted by kind; see [`NodeRef::force`].
pub(crate) enum ForceResult<LeafThing, InternalThing> {
    Leaf(LeafThing),
    Internal(InternalThing),
}

/// A node handle sorted by kind.
pub(crate) type NodeByKind<BorrowType, K, V, S> =
    ForceResult<NodeRef<BorrowType, K, V, S, Leaf>, NodeRef<BorrowType, K, V, S, Internal>>;

/// A key sorted by its node's kind: beside its value in a leaf, or a
/// separator in an internal node.
pub(crate) type KvByKind<BorrowType, K, V, S> =
    ForceResult<Kv<NodeRef<BorrowType, K, V, S, Leaf>>, Kv<NodeRef<BorrowType, K, V, S, Internal>>>;

/// An existing key, or the vacant leaf edge where an insertion belongs.
pub(crate) type InsertSearchResult<BorrowType, K, V, S> =
    Result<KvByKind<BorrowType, K, V, S>, Edge<NodeRef<BorrowType, K, V, S, Leaf>>>;

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

    /// [`Self::edge_at`] for the hot path, where the index is known to be in
    /// bounds.
    ///
    /// # Safety
    ///
    /// `index <= len`.
    pub(super) unsafe fn edge_at_unchecked(self, index: usize) -> Edge<Self> {
        // SAFETY: forwarded.
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
}

impl<BorrowType, K, V, S: InlineKeyStrategy<K>, Kind: NodeKind> NodeRef<BorrowType, K, V, S, Kind> {
    /// How many leading bytes the node's fences share, and so every key in
    /// it, which its inline keys skip (05-fence-key.md); 0 for a node
    /// without inline keys. The same in both kinds: it is in the header.
    pub(crate) fn prefix_len(&self) -> usize {
        // SAFETY: the node is allocated and initialized, and only this handle
        // writes its header (invariant 1).
        unsafe { self.node.prefix_len() }.unwrap_or(0)
    }
}

impl<BorrowType, K, V, S: InlineKeyStrategy<K>, Kind: PageKind> NodeRef<BorrowType, K, V, S, Kind> {
    /// Length metadata, when present, is initialized to capacity. Its live
    /// prefix follows the keys through every `KeySlots` mutation.
    pub(super) fn length_tags(&self) -> Option<&[u16]> {
        let tags = self.key_slots().columns().length_tags()?;
        // SAFETY: the live handle proves the node's kind and allocation;
        // every tag was initialized when allocated and can be read under
        // this shared reborrow (invariants 1 and 7).
        unsafe { Some(tags.run(0, self.capacity()).as_ref()) }
    }

    /// Binary search for `key` among the node's keys: `Found` at a key equal
    /// to it, otherwise `GoDown` at the edge where it belongs. With inline
    /// keys (D18), over those first.
    ///
    /// The position is in bounds for any `Ord` and `InlineKey`, consistent
    /// or not; see `search_index`.
    // Keep this typed result adapter with its caller, which already owns
    // the node and height. Outlining it returns both through a stack result
    // at each visit; the actual search_index body remains free to outline.
    #[inline(always)]
    pub(crate) fn search<Q>(self, key: &SearchKey<'_, K, S, Q>) -> SearchResult<Self>
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

    /// An active search offset is separate from the conservative fence
    /// prefix used by structural operations.
    #[inline]
    pub(super) fn local_prefix_len(&self) -> Option<usize> {
        Self::plan().local_prefix_bytes()?;
        // SAFETY: this live node has inline keys, and its borrow prevents
        // concurrent header mutation.
        let prefix = unsafe { self.node.search_prefix_len() };
        (prefix != 0).then_some(prefix)
    }

    /// `key`'s inline key at this node's prefix length, if the node has
    /// inline keys and `key` one to compare with them (D18).
    ///
    /// Each search takes it where it uses it, rather than having it handed
    /// in: handed across a call, it went through the stack, written right
    /// before the search read it back, a store the CPU can't always
    /// forward, which stalled every level. Std byte strings read the
    /// window directly from the search key.
    #[inline]
    fn inline_target<Q: ?Sized>(&self, key: &SearchKey<'_, K, S, Q>) -> Option<InlineTarget>
    where
        K: Borrow<Q>,
    {
        if NodePtr::<K, V, S>::has_inline_keys() {
            key.inline_target(self.local_prefix_len().unwrap_or_else(|| self.prefix_len()))
        } else {
            None
        }
    }

    /// The plan of the map's nodes of this kind (`node_plan.rs`).
    #[inline(always)]
    pub(super) fn plan() -> NodePlan<K> {
        NodePtr::<K, V, S>::plan::<Kind>()
    }

    /// `Ok(i)` if key `i` equals `key`, otherwise `Err(i)` with `i` the edge
    /// where `key` belongs. In bounds for any `Ord` and `InlineKey`,
    /// consistent or not.
    ///
    /// If the node has inline keys and `key` has one to compare (D18), taken
    /// at the node's prefix length, the search runs over them; otherwise over
    /// the keys, as the node's plan says (D19).
    /// After monomorphization, a map without inline keys has only the
    /// latter.
    fn search_index<Q>(&self, key: &SearchKey<'_, K, S, Q>) -> Result<usize, usize>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if let Some(prefix) = self.local_prefix_len()
            && let Some(first) = self.reborrow().keys().first()
            && let Some(order) = key.cmp_prefix(first, self.prefix_len(), prefix)
        {
            match order {
                Ordering::Less => return Err(0),
                Ordering::Greater => return Err(self.len()),
                Ordering::Equal => {}
            }
        }
        if let Some(target) = self.inline_target(key)
            && let Some(run) = self.inline_equal_run(target)
        {
            return self.search_run(run, key, target);
        }
        let keys = self.reborrow().keys();
        match Self::plan().full_search() {
            KeySearch::Branchless => Self::branchless_search_index(keys, key.key()),
            KeySearch::Binary => {
                Self::binary_search_index(keys, |probe| key.key().cmp(probe.borrow()))
            }
        }
    }

    /// The positions of the node's keys whose inline keys equal `target`'s
    /// in the bytes the node keeps (D18), or `None` if the node has no inline
    /// keys: only those can equal the key searched for. The count over the
    /// inline keys runs over the kind's capacity, a constant, which the
    /// compiler unrolls ([`LiveSlots::equal_run_by_count`]); over a capacity
    /// that depends on the height, it compiled to a loop of several times the
    /// instructions, and over two capacities, a leaf's and an internal
    /// node's, to one that summed the compares' bits one at a time.
    /// Keep this small typed view in its callers: outlining shared uses
    /// also made LLVM expand 64-bit summary counts into scalar lane-bit sums.
    #[inline(always)]
    fn inline_equal_run(&self, target: InlineTarget) -> Option<Range<usize>> {
        Some(
            self.reborrow()
                .inline_keys()?
                .equal_run(target.inline_key()),
        )
    }

    /// The search within `run`, the keys whose inline keys equal `key`'s
    /// (D18). Keys outside the run differ from `key` in their inline keys,
    /// which order them (the `InlineKey` contract), so if `key` is in no
    /// run, its edge is where the run would start.
    ///
    /// It runs as the node's plan says. Keys with their own `InlineKey` impl
    /// compare by `Ord`, and search as their type picks (D19): branchless
    /// for plain data such as integers, whose run is the whole node where
    /// their kept bytes all tie, as at a node of prefix 0, the root's. Std
    /// byte strings search it by binary search, comparing as
    /// [`SearchKey::cmp_tied`] does, a word at a time from past the bytes
    /// the run shares.
    ///
    /// In bounds for any `Ord` and `InlineKey`: the run lies within `0..len`
    /// whatever the inline keys hold ([`NodeInlineKeys::equal_run`]), taking
    /// it out of the keys checks that, and either search stays within it.
    fn search_run<Q>(
        &self,
        run: Range<usize>,
        key: &SearchKey<'_, K, S, Q>,
        target: InlineTarget,
    ) -> Result<usize, usize>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let candidates = self.reborrow().keys().slice(run.clone());
        let found = match Self::plan().tied_search() {
            KeySearch::Branchless => Self::branchless_search_index(candidates, key.key()),
            KeySearch::Binary => {
                Self::binary_search_index(candidates, |stored| key.cmp_tied::<Kind>(target, stored))
            }
        };
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
    fn branchless_search_index<Q>(keys: NodeKeys<'_, K>, key: &Q) -> Result<usize, usize>
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
    fn binary_search_index(
        keys: NodeKeys<'_, K>,
        cmp_key: impl Fn(&K) -> Ordering,
    ) -> Result<usize, usize> {
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
        NodePtr::<K, V, S>::capacity::<Kind>()
    }

    /// The fewest keys a node below the root holds between removals: an
    /// eighth of its capacity, and at least 4 (D22), so that an internal node
    /// below the root has at least 5 children, which bounds the height
    /// (`path.rs`). A removal that leaves fewer makes the node underfull, for
    /// its parent to fix. A split leaves at least this many entries on each
    /// side after insertion, so inserts never do.
    pub(crate) fn min_len(&self) -> usize {
        (self.capacity() / 8).max(4)
    }

    /// Whether the node holds fewer keys than [`Self::min_len`].
    pub(crate) fn is_underfull(&self) -> bool {
        self.len() < self.min_len()
    }

    /// Whether the node holds as many keys as it can: an insert into it
    /// splits it.
    pub(super) fn is_full(&self) -> bool {
        self.len() == self.capacity()
    }

    fn key_slots(&self) -> KeySlots<K, S, Kind> {
        // SAFETY: the node is allocated, and of kind `Kind` (invariant 1).
        unsafe { self.node.key_slots::<Kind>() }
    }
}

/// A node's inline keys, read in place (D18): every slot up to the
/// capacity, at this node kind's width, of which slots `0..len` hold the inline keys
/// of keys `0..len`, and the rest stale ones, which a search reads and masks
/// out. Its methods speak in windows: they narrow a window to the bytes the
/// node keeps, and give an inline key back as those bytes, zeros after.
///
/// Each typed node kind of a map has one width, so after monomorphization
/// each method is that of one width. They are not generic, so they need
/// `#[inline]` to inline into the searches, which other crates instantiate,
/// and `always`: before the width folds, each looks like a match of three
/// arms with bounds checks, which the compiler kept out of line, a call and
/// a match on every level of every insert.
#[derive(Clone, Copy)]
pub(super) enum NodeInlineKeys<'a> {
    Two(LiveSlots<'a, u16>),
    Four(LiveSlots<'a, u32>),
    Eight(LiveSlots<'a, u64>),
}

/// `$body`, with `$live` bound to the slots of `$inline_keys`, whichever
/// their width.
macro_rules! with_live_slots {
    ($inline_keys:expr, |$live:ident| $body:expr) => {
        match $inline_keys {
            NodeInlineKeys::Two($live) => $body,
            NodeInlineKeys::Four($live) => $body,
            NodeInlineKeys::Eight($live) => $body,
        }
    };
}

impl<'a> NodeInlineKeys<'a> {
    /// The first `capacity` of `slots`, of which the first `len` are live.
    ///
    /// # Safety
    ///
    /// `capacity` is the slots' own, all of them are initialized, and none
    /// changes for `'a`.
    #[inline(always)]
    unsafe fn new(slots: InlineKeySlots, capacity: usize, len: usize) -> Self {
        // SAFETY: forwarded, at the slots' width.
        unsafe {
            match slots {
                InlineKeySlots::Two(slots) => {
                    Self::Two(LiveSlots::new(slots.run(0, capacity).as_ref(), len))
                }
                InlineKeySlots::Four(slots) => {
                    Self::Four(LiveSlots::new(slots.run(0, capacity).as_ref(), len))
                }
                InlineKeySlots::Eight(slots) => {
                    Self::Eight(LiveSlots::new(slots.run(0, capacity).as_ref(), len))
                }
            }
        }
    }

    /// Inline key `index`: the bytes the node keeps of key `index`'s
    /// window, zeros after them.
    ///
    /// # Panics
    ///
    /// If `index >= len`.
    #[inline(always)]
    pub(super) fn inline_key(self, index: usize) -> InlineWord {
        with_live_slots!(self, |live| live.live()[index].window())
    }

    /// How inline key `index` orders against the bytes the node keeps of
    /// `window`. Equal says nothing of the keys.
    ///
    /// # Panics
    ///
    /// If `index >= len`.
    #[inline(always)]
    fn cmp_at(self, index: usize, window: InlineWord) -> Ordering {
        with_live_slots!(self, |live| live
            .cmp_at(index, InlineSlot::of_window(window)))
    }

    /// The positions of the inline keys equal to the bytes the node keeps
    /// of `window`; see [`LiveSlots::equal_run`].
    #[inline(always)]
    fn equal_run(self, window: InlineWord) -> Range<usize> {
        with_live_slots!(self, |live| live.equal_run(InlineSlot::of_window(window)))
    }

    /// An equality mask only for widths and targets that benefit from it.
    /// Unsupported shapes keep the original ordered range and empty check.
    #[inline(always)]
    fn point_mask(self, window: InlineWord) -> Option<super::vector_count::U64EqualityMask> {
        with_live_slots!(self, |live| live.point_mask(InlineSlot::of_window(window)))
    }

    /// Whether every slot past `len` holds the greatest inline key
    /// (invariant 6).
    #[cfg(any(test, fuzzing))]
    pub(super) fn pads_with_greatest(self) -> bool {
        with_live_slots!(self, |live| live.pads_with_greatest())
    }
}

/// A node's inline keys at one width, as [`NodeInlineKeys`] reads them:
/// `slots` up to the capacity, of which the first `len` are live.
#[derive(Clone, Copy)]
pub(super) struct LiveSlots<'a, W> {
    slots: &'a [W],
    len: usize,
}

impl<'a, W: InlineSlot> LiveSlots<'a, W> {
    fn new(slots: &'a [W], len: usize) -> Self {
        debug_assert!(len <= slots.len());
        Self { slots, len }
    }

    /// The live slots.
    fn live(self) -> &'a [W] {
        &self.slots[..self.len]
    }

    #[cfg(any(test, fuzzing))]
    fn pads_with_greatest(self) -> bool {
        let greatest = W::of_window(InlineWord::MAX);
        self.slots[self.len..].iter().all(|&slot| slot == greatest)
    }

    /// How live slot `index` orders against `target`.
    ///
    /// # Panics
    ///
    /// If `index >= len`.
    fn cmp_at(self, index: usize, target: W) -> Ordering {
        self.live()[index].cmp(&target)
    }

    /// The positions of the inline keys equal to `target`: exactly those
    /// for sorted inline keys, which the `InlineKey` contract gives, and a
    /// range within `0..len` for any. If none equals it, the empty range at
    /// the first inline key greater than it.
    ///
    /// Found by a count of every slot: in explicit vectors where the target
    /// has them for slots of this width
    /// ([`VectorCount`](super::vector_count::VectorCount)), otherwise as the
    /// compiler vectorizes it ([`Self::equal_run_by_count`]) where it
    /// compares slots of this width in vectors
    /// ([`InlineSlot::COUNTS_IN_VECTORS`]); and by a binary search
    /// otherwise ([`Self::equal_run_by_search`]): without such compares,
    /// the count is an emulated sequence that costs more than the
    /// mispredicts it avoids. Measured on 1M-entry maps of 8-byte inline
    /// keys (04, Q6).
    /// Wide four-byte arrays on baseline x86-64 also use binary search:
    /// beyond the one-word packed reduction, full-capacity counts cost
    /// more than dependent probes in warm routing nodes.
    #[inline(always)]
    fn equal_run(self, target: W) -> Range<usize> {
        if cfg!(all(
            target_arch = "x86_64",
            not(target_feature = "avx512bw"),
            not(target_feature = "popcnt")
        )) && size_of::<W>() == 4
            && self.slots.len() > 32
        {
            return self.equal_run_by_search(target);
        }
        if let Some(run) = W::equal_run_in_vectors(self.slots, self.len, target) {
            return run;
        }
        if W::COUNTS_IN_VECTORS {
            self.equal_run_by_count(target)
        } else {
            self.equal_run_by_search(target)
        }
    }

    /// Equality-only masks are optional; ordered fallback is chosen by
    /// the leaf without wrapping its existing range in another result.
    #[inline(always)]
    fn point_mask(self, target: W) -> Option<super::vector_count::U64EqualityMask> {
        W::point_mask_in_vectors(self.slots, target)
    }

    /// One pass over every slot up to the capacity counts the inline keys
    /// below `target` and those equal to it. The slots from `len` on hold
    /// the greatest inline key (invariant 6), so they never count below it,
    /// and count equal only to a greatest `target`, whose run the cut at
    /// `len` ends where the live slots do. With no branch, no dependent load
    /// and no mask per slot, and a trip count that is the capacity, one of
    /// two constants per map, the compiler unrolls it into a few vector
    /// compares whose masks it counts. Within `0..len` for any slots, by the
    /// cuts.
    fn equal_run_by_count(self, target: W) -> Range<usize> {
        let below = self.slots.iter().filter(|&&slot| slot < target).count();
        let equal = self.slots.iter().filter(|&&slot| slot == target).count();
        let below = below.min(self.len);
        below..(below + equal).min(self.len)
    }

    /// A branchless binary search finds where the run starts, and a scan
    /// from there finds where it ends: one compare, which inline keys that
    /// don't tie make predictable, rather than a second search, whose exit
    /// depends on where the first one ended. Within `0..len` for any slots:
    /// the search and the scan stay within the live slots.
    fn equal_run_by_search(self, target: W) -> Range<usize> {
        let live = self.live();
        let start = Self::count_below(live, target);
        let mut end = start;
        while live.get(end) == Some(&target) {
            end += 1;
        }
        start..end
    }

    /// How many of `inline_keys`, sorted, are below `target`, by a
    /// branchless binary search, as in `NodeRef::branchless_search_index`;
    /// in `0..=len` for any inline keys, by construction: the loop keeps
    /// `base + size <= len`, and probes only `mid = base + size / 2 < base +
    /// size`.
    fn count_below(inline_keys: &[W], target: W) -> usize {
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
            base = hint::select_unpredictable(probe < target, mid, base);
            size -= half;
        }
        // SAFETY: `base < base + size <= inline_keys.len()`, with `size == 1`
        // here.
        let last = unsafe { *inline_keys.get_unchecked(base) };
        base + usize::from(last < target)
    }
}

impl<BorrowType, K, V, S: InlineKeyStrategy<K>> NodeRef<BorrowType, K, V, S, Leaf> {
    /// A point lookup may reject unequal lengths inside a primary tied run.
    /// Ordered searches still use `search`: a length tag gives no edge.
    pub(super) fn search_point<Q>(self, key: &SearchKey<'_, K, S, Q>) -> Option<Kv<Self>>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let found = if let Some(target) = self.inline_target(key)
            && let Some(inline_keys) = self.reborrow().inline_keys()
        {
            // This typed view keeps leaf capacity constant, as for the
            // ordered inline_equal_run. Empty masks need no bound extraction.
            let run = if let Some(mask) = inline_keys.point_mask(target.inline_key()) {
                mask.run(self.len())?
            } else {
                inline_keys.equal_run(target.inline_key())
            };
            if run.is_empty() {
                return None;
            } else if run.len() == 1 {
                // Point lookup needs no insertion edge or ordering result.
                // Arrays use full equality; strings retain the prefix skip.
                let keys = self.reborrow().keys();
                let stored = &keys[run.start];
                let matches = if Self::plan().has_whole_key_equality()
                    && let Some(query) = key.whole_bytes_query()
                {
                    query.matches(stored)
                } else {
                    key.cmp_tied::<Leaf>(target, stored) == Ordering::Equal
                };
                matches.then_some(run.start)
            } else if let Some(tag) = key.length_tag()
                && let Some(tags) = self.length_tags()
            {
                match LengthTags::new(tags).matching(run.clone(), tag) {
                    LengthMatch::None => return None,
                    LengthMatch::One(index) => {
                        // The mask only includes indexes in the live run;
                        // indexing keeps this fact checked locally too.
                        if key.cmp_tied::<Leaf>(target, &self.reborrow().keys()[index])
                            != Ordering::Equal
                        {
                            return None;
                        }
                        Some(index)
                    }
                    LengthMatch::Two { lower, upper } => {
                        // Binary search over the two filtered keys: the
                        // upper key is the midpoint. This avoids reading
                        // unrelated strings in the original primary run.
                        let keys = self.reborrow().keys();
                        match key.cmp_tied::<Leaf>(target, &keys[upper]) {
                            Ordering::Equal => Some(upper),
                            Ordering::Less => (key.cmp_tied::<Leaf>(target, &keys[lower])
                                == Ordering::Equal)
                                .then_some(lower),
                            Ordering::Greater => None,
                        }
                    }
                    LengthMatch::Many => self.search_run(run, key, target).ok(),
                }
            } else {
                self.search_run(run, key, target).ok()
            }
        } else {
            self.search_index(key).ok()
        }?;
        // SAFETY: both searches return only a live key index; a tag result
        // was bounded to the live run and checked by indexing above.
        Some(unsafe { Kv::new_unchecked(self, found) })
    }

    /// The leaf's head, or `None` if it has none: in a tree built by the node
    /// operations, only the leftmost leaf has none (D17).
    pub(crate) fn head(self) -> Option<Head<Self>> {
        // SAFETY: the node is allocated and its header written (invariant 1).
        let has_head = unsafe { self.node.has_head() };
        has_head.then_some(Head { leaf: self })
    }

    fn value_slots(&self) -> Slots<V> {
        // SAFETY: the node is an allocated leaf (invariant 1).
        unsafe { self.node.value_slots() }
    }

    /// Slot 0 is the head, slot `i + 1` value `i`.
    fn head_and_value_slots(&self) -> Slots<V> {
        // SAFETY: the node is an allocated leaf (invariant 1).
        unsafe { self.node.head_and_value_slots() }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> NodeRef<Immut<'_>, K, V, S, Internal> {
    /// The window of a key beside edge `index`, the one left of it if there
    /// is one: a bound of the child there, which shares the child's prefix,
    /// at this node's prefix length. `None` if the node has no keys.
    ///
    /// # Panics
    ///
    /// If `index > len`.
    pub(super) fn bound_window(self, index: usize) -> Option<Window> {
        let len = self.len();
        assert!(index <= len, "edge index out of bounds");
        let beside = index.checked_sub(1).or((index < len).then_some(index))?;
        Some(self.key_window(beside))
    }

    /// `key`, from the child at edge `index` (a separator from its split),
    /// with its window moved back to this node's prefix length, filled from
    /// [`Self::bound_window`]. A node without keys is a new root, whose
    /// only child, the old root, has prefix 0, as it does.
    fn window_from_child(self, index: usize, key: InlinedKey<K, S>) -> InlinedKey<K, S> {
        match self.bound_window(index) {
            Some(fill) => key.moved_back_to(fill),
            None => {
                debug_assert_eq!(key.window().prefix_len(), self.prefix_len());
                key
            }
        }
    }
}

impl<BorrowType, K, V, S> NodeRef<BorrowType, K, V, S, Internal> {
    /// Whether the node's children are leaves, as those of a node at height
    /// 1 are: the one decision about their kind, after which code for that
    /// kind takes over ([`Edge::descend_as`],
    /// [`Kv::into_balancing_context`]).
    pub(crate) fn has_leaf_children(&self) -> bool {
        self.height == 1
    }
}

impl<BorrowType, K, V, S: InlineKeyStrategy<K>> NodeRef<BorrowType, K, V, S, Internal> {
    fn edge_slots(&self) -> Slots<NodePtr<K, V, S>> {
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

impl<'a, K, V, S: InlineKeyStrategy<K>, Kind: PageKind> NodeRef<Immut<'a>, K, V, S, Kind> {
    /// The keys `0..len`, in order.
    pub(crate) fn keys(self) -> NodeKeys<'a, K> {
        // SAFETY: keys `0..len` are initialized and unchanged for `'a`
        // (invariant 1), and the last key's stride ends within the page
        // (`layout.rs`).
        unsafe { NodeKeys::new(self.key_slots().keys(), self.len()) }
    }

    /// The node's inline keys, if it has them (D18): inline key `i` is key
    /// `i`'s, for `i < len`, and a search may read the slots past them.
    pub(super) fn inline_keys(self) -> Option<NodeInlineKeys<'a>> {
        let slots = self.key_slots().inline_keys()?;
        // SAFETY: the node's inline-key slots number its capacity, all are
        // initialized (invariant 6), and none changes for `'a` (invariant
        // 1); they grow up, so memory order is index order.
        Some(unsafe { NodeInlineKeys::new(slots, self.capacity(), self.len()) })
    }

    /// Key `index`'s window: its inline key at the node's prefix length, or
    /// [`Window::NONE`] in a node without inline keys.
    ///
    /// # Panics
    ///
    /// If `index >= len`.
    fn key_window(self, index: usize) -> Window {
        assert!(index < self.len(), "key index out of bounds");
        if self.local_prefix_len().is_some() {
            let inliner = Self::plan().inliner().expect("local byte-string prefix");
            return Window::new(
                self.prefix_len(),
                inliner.inline_key_at(&self.keys()[0], self.prefix_len()),
            );
        }
        // SAFETY: `index < len` and the node has no active local windows.
        // The slots rehydrate narrow heap-byte windows before export.
        unsafe { self.key_slots().window(index) }
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

impl<K, V, S: InlineKeyStrategy<K>, Kind: PageKind> NodeMut<'_, K, V, S, Kind> {
    /// Records a longer initialized prefix. Slots beyond the new length
    /// already hold the greatest inline key, so none needs refilling.
    ///
    /// # Safety
    ///
    /// `self.len() <= len <= capacity`, and all keys, inline keys, values
    /// or edges described by `len` are initialized, as for `set_len`.
    unsafe fn grow_len(&mut self, len: usize) {
        debug_assert!(self.len() <= len && len <= self.capacity());
        // SAFETY: the node is allocated and this handle is exclusive
        // (invariant 1); the length and initialization requirements are
        // forwarded. Growing preserves the untouched inline-key padding.
        unsafe { self.node.set_len(len) }
        self.refresh_local_prefix();
    }

    /// Sets the length; the inline-key slots a shorter length leaves get
    /// the greatest inline key, as all slots past the length hold
    /// (invariant 6). Called once the slots are where the length says.
    ///
    /// # Safety
    ///
    /// `len` is at most the capacity, and keys `0..len` (with their inline
    /// keys, and in a leaf, values `0..len`; in an internal node, edges
    /// `0..=len`) are initialized and valid at this call: refreshing an
    /// internal node's local prefix may immediately read its live keys.
    /// Any active local windows have been normalized before key mutation.
    unsafe fn set_len(&mut self, len: usize) {
        debug_assert!(len <= self.capacity());
        let old_len = self.len();
        // SAFETY: `old_len <= capacity`, and this handle is exclusive.
        unsafe { self.key_slots().columns().mark_vacant(len..old_len) };
        // SAFETY: the node is allocated and this handle is exclusive
        // (invariant 1); `len <= capacity` is forwarded.
        unsafe { self.node.set_len(len) }
        self.refresh_local_prefix();
    }
}

impl<K, V, S: InlineKeyStrategy<K>, Kind: PageKind> NodeMut<'_, K, V, S, Kind> {
    /// Installs prepared local search windows, or conservative fence windows
    /// when `prefix_len` is zero. No key or user callback is read here.
    pub(super) fn set_local_windows(
        &mut self,
        prefix_len: usize,
        windows: impl ExactSizeIterator<Item = InlineWord>,
    ) {
        assert_eq!(windows.len(), self.len());
        let slots = self
            .key_slots()
            .inline_keys()
            .expect("local byte-string prefix");
        // SAFETY: one plain-data word per initialized key, within capacity;
        // this handle owns exclusive access to slots and the inline header.
        unsafe {
            for (index, window) in (0..self.len()).zip(windows) {
                slots.write(index, window);
            }
            self.node.set_search_prefix_len(prefix_len);
        }
    }

    /// Moves the node's prefix length back to `fill`'s, and every inline key
    /// with it ([`Window::moved_back_to`]), given `fill`, the window there of
    /// a key whose encoding shares the node's prefix: one of the node's
    /// fences, or one of its keys. An active local search prefix first
    /// reads one built-in byte string to restore fence windows; subsequent
    /// shifts read no key and invoke no user code. Does nothing if the node's
    /// prefix is no longer than `fill`'s, which includes every node without
    /// inline keys.
    pub(super) fn move_prefix_back_to(&mut self, fill: Window) {
        self.normalize_local_prefix();
        let prefix_len = self.prefix_len();
        if prefix_len <= fill.prefix_len() {
            return;
        }
        let len = self.len();
        let Some(inline_keys) = self.key_slots().inline_keys() else {
            return;
        };
        // SAFETY: inline keys `0..len` are initialized (invariants 1 and 6),
        // each is rewritten in place with the window of the same key, and
        // the header with them; this handle is exclusive.
        unsafe {
            for index in 0..len {
                let window = Window::new(prefix_len, inline_keys.read(index)).moved_back_to(fill);
                inline_keys.write(index, window.inline_key());
            }
            self.node.set_prefix_len(fill.prefix_len());
        }
    }

    /// Makes `prefix_len` the node's prefix length, and `inline_keys` its
    /// keys' inline keys, in order, which must be their windows there: how a
    /// prefix grows, once its windows are computed (`prefix.rs`). Calls no
    /// `InlineKey`. Does nothing to a node without inline keys.
    ///
    /// # Panics
    ///
    /// If `inline_keys` does not say it has one inline key per key; the
    /// node is unchanged then.
    pub(super) fn set_windows(
        &mut self,
        prefix_len: usize,
        inline_keys: impl ExactSizeIterator<Item = InlineWord>,
    ) {
        let len = self.len();
        assert_eq!(inline_keys.len(), len, "one inline key per key");
        let Some(slots) = self.key_slots().inline_keys() else {
            return;
        };
        // SAFETY: each slot written has an index below `len <= capacity`,
        // however many inline keys the iterator gives; inline keys are plain
        // data, and this handle is exclusive.
        unsafe {
            for (index, inline_key) in (0..len).zip(inline_keys) {
                slots.write(index, inline_key);
            }
            self.node.set_prefix_len(prefix_len);
        }
        self.refresh_local_prefix();
    }

    /// Gives a node with no keys `prefix_len`, which, with no inline keys to
    /// match, any prefix length is: as a copy of a node starts (`clone.rs`).
    ///
    /// # Panics
    ///
    /// If the node has keys.
    pub(super) fn set_empty_prefix_len(&mut self, prefix_len: usize) {
        assert_eq!(self.len(), 0, "a node with keys keeps their prefix length");
        // SAFETY: the node is allocated and this handle is exclusive.
        unsafe { self.node.set_prefix_len(prefix_len) }
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

impl<K, V, S: InlineKeyStrategy<K>> NodeMut<'_, K, V, S, Leaf> {
    /// Cuts the leaf at edge `index`: moves the entries from `index` on, with
    /// their inline keys, into a new leaf in `page`, without a head, and
    /// returns it. This leaf keeps the entries before `index`, and its head.
    /// Either may be left with fewer than its minimum, down to none.
    ///
    /// # Panics
    ///
    /// If `index > len`; the leaf is unchanged then.
    pub(crate) fn cut_off_at(
        &mut self,
        index: usize,
        page: FreshPage<K, V, S, Leaf>,
    ) -> Subtree<K, V, S, Leaf> {
        let len = self.len();
        assert!(index <= len, "cutting a leaf past its last edge");
        let count = len - index;
        let right = NodePtr::from_fresh_page(page);
        // SAFETY: `index <= len`, so keys (with their inline keys) and values
        // `index..len` are initialized; they move to `0..count` of `right`, a
        // fresh leaf of the same map, so `true` says its kind, at this leaf's
        // prefix length. This handle is exclusive, and `right` is reached by
        // nothing else yet. Afterwards this leaf holds `0..index` and its
        // head, and `right` `0..count` and none.
        unsafe {
            right.set_prefix_len(self.prefix_len());
            self.key_slots()
                .move_run_to(index, count, right.key_slots::<Leaf>(), 0);
            self.value_slots()
                .move_run_to(index, count, right.value_slots(), 0);
            right.set_len(count);
            self.set_len(index);
        }
        // SAFETY: `right` holds a well-formed leaf, made just above, which
        // the returned subtree owns.
        unsafe { Subtree::from_raw_parts(right, 0) }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> NodeMut<'_, K, V, S, Internal> {
    /// Cuts the node at edge `index`: moves the keys from `index` on, with
    /// their inline keys, and the edges right of them, into a new node in
    /// `page`, after `first_child`, the part of the child at `index` that
    /// went right, and returns it. This node keeps the keys before `index`
    /// and the edges up to it. Either may be left with fewer keys than its
    /// minimum, down to none.
    ///
    /// # Panics
    ///
    /// If `index > len`, or if `first_child` is not exactly one level below
    /// this node; the node is unchanged then.
    pub(crate) fn cut_off_at(
        &mut self,
        index: usize,
        first_child: Subtree<K, V, S>,
        page: FreshPage<K, V, S, Internal>,
    ) -> Subtree<K, V, S, Internal> {
        let len = self.len();
        assert!(index <= len, "cutting a node past its last edge");
        self.assert_child_level(&first_child);
        self.normalize_local_prefix();
        let count = len - index;
        let right = NodePtr::from_fresh_page(page);
        // SAFETY: `index <= len`, so keys (with their inline keys)
        // `index..len` and edges `index + 1..=len` are initialized; they move
        // to keys `0..count` and edges `1..=count` of `right`, a fresh
        // internal node of the same map, so `false` says its kind, after
        // `first_child` as its edge 0, a subtree one level down (checked
        // above), which `right` owns from now on (invariant 2). This handle
        // is exclusive, and `right` is reached by nothing else yet.
        // Afterwards this node holds keys `0..index` and edges `0..=index`.
        // The keys keep their windows, at this node's prefix length, which
        // `right` takes.
        unsafe {
            right.set_prefix_len(self.prefix_len());
            right.edge_slots().slot(0).write(first_child.node);
            self.key_slots()
                .move_run_to(index, count, right.key_slots::<Internal>(), 0);
            self.edge_slots()
                .move_run_to(index + 1, count, right.edge_slots(), 1);
            right.set_len(count);
            self.set_len(index);
        }
        // SAFETY: `right` holds a well-formed node at this node's height,
        // made just above, which the returned subtree owns.
        unsafe { Subtree::from_raw_parts(right, self.height) }
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

/// A validated interval in one leaf, whose borrow keeps its slot positions
/// unchanged.
/// Paired entries with a positive stride use key pointers as their cursor
/// and end; advancing needs neither the leaf pointer nor an index multiply.
/// Other layouts keep the key origin in `front` and use `start..end`.
/// Which representation applies depends only on the map's layout.
///
/// The originating leaf stays allocated with borrow `B`. Its path retains
/// the leaf handle while this run retains the same borrow proof through its
/// marker. Pointers preserve the page's provenance, including its key end.
pub(super) struct LeafRun<B, K, V, S> {
    front: NonNull<K>,
    back: NonNull<K>,
    start: usize,
    end: usize,
    _borrow: PhantomData<NodeRef<B, K, V, S, Leaf>>,
}

// SAFETY: the cached pointers grant exactly the originating leaf handle's
// access. Moving or sharing them requires the same key/value borrow bounds.
unsafe impl<B, K, V, S> Send for LeafRun<B, K, V, S> where NodeRef<B, K, V, S, Leaf>: Send {}
// SAFETY: as for Send, with the leaf handle's shared-access bounds.
unsafe impl<B, K, V, S> Sync for LeafRun<B, K, V, S> where NodeRef<B, K, V, S, Leaf>: Sync {}

impl<B: CopyableBorrow, K, V, S> Clone for LeafRun<B, K, V, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<B: CopyableBorrow, K, V, S> Copy for LeafRun<B, K, V, S> {}

impl<B: CopyableBorrow, K, V, S: InlineKeyStrategy<K>> LeafRun<B, K, V, S> {
    fn uses_pointers() -> bool {
        NodePtr::<K, V, S>::has_paired_entries::<Leaf>()
            && NodePtr::<K, V, S>::leaf_layout().keys.key_stride != 0
    }

    pub(super) fn new(leaf: NodeRef<B, K, V, S, Leaf>, range: Range<usize>) -> Self {
        assert!(range.start <= range.end && range.end <= leaf.len());
        // SAFETY: every leaf has positive capacity, so slot 0 is allocated
        // even when uninitialized. No reference to its contents is formed.
        let origin = unsafe { leaf.key_slots().keys().slot(0) };
        let (front, back) = if Self::uses_pointers() {
            let stride = NodePtr::<K, V, S>::leaf_layout().keys.key_stride;
            // SAFETY: the layout bounds even the key at index capacity by
            // the page's end. A value end need not fit, so only key pointers
            // serve as sentinels; values are derived from live keys below.
            unsafe {
                (
                    origin.byte_add(range.start * stride),
                    origin.byte_add(range.end * stride),
                )
            }
        } else {
            (origin, origin)
        };
        Self {
            front,
            back,
            start: range.start,
            end: range.end,
            _borrow: PhantomData,
        }
    }

    pub(super) fn next(&mut self) -> Option<LeafEntry<B, K, V, S>> {
        if Self::uses_pointers() {
            if self.front == self.back {
                return None;
            }
            let key = self.front;
            // SAFETY: the nonempty interval contains this key; its next
            // key pointer is at most the validated end of the run.
            unsafe {
                self.front = key.byte_add(NodePtr::<K, V, S>::leaf_layout().keys.key_stride);
                return Some(self.entry(key, 0));
            }
        }
        if self.start == self.end {
            return None;
        }
        let index = self.start;
        self.start += 1;
        // SAFETY: the nonempty, validated interval contains this index.
        Some(unsafe { self.entry(self.front, index) })
    }

    pub(super) fn next_back(&mut self) -> Option<LeafEntry<B, K, V, S>> {
        if Self::uses_pointers() {
            if self.front == self.back {
                return None;
            }
            // SAFETY: the nonempty interval contains the preceding key.
            unsafe {
                self.back = self
                    .back
                    .byte_sub(NodePtr::<K, V, S>::leaf_layout().keys.key_stride);
                return Some(self.entry(self.back, 0));
            }
        }
        if self.start == self.end {
            return None;
        }
        self.end -= 1;
        // SAFETY: the nonempty, validated interval contains this index.
        Some(unsafe { self.entry(self.front, self.end) })
    }

    /// An initialized entry: `origin` is either this entry's key in paired
    /// mode (with index 0), or the leaf's first key in indexed mode.
    unsafe fn entry(&self, origin: NonNull<K>, index: usize) -> LeafEntry<B, K, V, S> {
        let layout = NodePtr::<K, V, S>::leaf_layout();
        let value_offset = layout.head_offset + layout.value_stride - layout.keys.keys_offset;
        // SAFETY: the caller supplies a live slot. Keys and values follow
        // these aligned strides and never overlap. In paired mode they use
        // the same stride, so the value is at a fixed offset from this key.
        unsafe {
            LeafEntry {
                key: origin.byte_add(index * layout.keys.key_stride),
                value: origin
                    .cast::<u8>()
                    .byte_add(value_offset + index * layout.value_stride)
                    .cast(),
                _borrow: PhantomData,
            }
        }
    }

    /// # Safety
    /// `leaf` is the same leaf this run was made from, kept allocated with
    /// unchanged slot positions. Its cursor has stayed in that leaf while the run
    /// advanced, so the pointer difference recovers the new logical edge.
    pub(super) unsafe fn edge_index(
        &self,
        leaf: NodeRef<B, K, V, S, Leaf>,
        reverse: bool,
    ) -> usize {
        if Self::uses_pointers() {
            // SAFETY: slot 0 exists, and the caller gives the originating
            // allocation. Both pointers lie within it or at its key end.
            let offset = unsafe {
                let origin = leaf.key_slots().keys().slot(0);
                let edge = if reverse { self.back } else { self.front };
                edge.cast::<u8>().offset_from_unsigned(origin.cast())
            };
            offset / NodePtr::<K, V, S>::leaf_layout().keys.key_stride
        } else if reverse {
            self.end
        } else {
            self.start
        }
    }

    pub(super) fn reborrow(&self) -> LeafRun<Immut<'_>, K, V, S> {
        LeafRun {
            front: self.front,
            back: self.back,
            start: self.start,
            end: self.end,
            _borrow: PhantomData,
        }
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>> LeafRun<Immut<'a>, K, V, S> {
    /// The keys left in the run, which it moves past, staying in its leaf
    /// with none left: for a loop to visit them without the walk.
    pub(super) fn take_keys(&mut self) -> KeyRun<'a, K> {
        let stride = NodePtr::<K, V, S>::leaf_layout().keys.key_stride;
        let (next, len) = if Self::uses_pointers() {
            // SAFETY: both pointers lie in the same leaf's key slots, or at
            // their end, `front` not past `back`, a whole number of strides
            // apart.
            let bytes = unsafe {
                self.back
                    .cast::<u8>()
                    .offset_from_unsigned(self.front.cast())
            };
            let next = self.front;
            self.front = self.back;
            (next, bytes / stride)
        } else {
            // SAFETY: `start <= end <= len` (validated when made), so key
            // `start` is a slot of the leaf, or its end.
            let next = unsafe { self.front.byte_add(self.start * stride) };
            let len = self.end - self.start;
            self.start = self.end;
            (next, len)
        };
        // SAFETY: the run's keys are initialized and shared for `'a`, `len`
        // of them from `next`, `stride` bytes apart (the leaf's layout).
        unsafe { KeyRun::new(next, stride, len) }
    }
}

/// Keys of one node, in order, which a walk has moved past: for a loop to
/// visit them without the walk, which it touches once per run rather than
/// once per key (a merge of two sets). Copied into a local, the run lives
/// in registers.
///
/// Invariant: `left` keys from `next`, `stride` bytes apart, are
/// initialized and shared for `'a`.
pub(crate) struct KeyRun<'a, K> {
    next: *const K,
    stride: usize,
    left: usize,
    _borrow: PhantomData<&'a K>,
}

impl<K> Clone for KeyRun<'_, K> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K> Copy for KeyRun<'_, K> {}

impl<'a, K> KeyRun<'a, K> {
    /// No keys.
    pub(crate) const EMPTY: Self = Self {
        next: core::ptr::null(),
        stride: 0,
        left: 0,
        _borrow: PhantomData,
    };

    /// # Safety
    ///
    /// The type invariant holds for `next`, `stride` and `len`.
    unsafe fn new(next: NonNull<K>, stride: usize, len: usize) -> Self {
        Self {
            next: next.as_ptr(),
            stride,
            left: len,
            _borrow: PhantomData,
        }
    }

    /// `key` alone.
    pub(crate) fn one(key: &'a K) -> Self {
        // SAFETY: one key, which `key` keeps shared for `'a`.
        unsafe { Self::new(NonNull::from(key), 0, 1) }
    }

    /// How many keys are left.
    pub(crate) fn len(&self) -> usize {
        self.left
    }

    /// The next key, staying before it.
    #[inline]
    pub(crate) fn peek(&self) -> Option<&'a K> {
        // SAFETY: with a key left, `next` is that key (type invariant).
        (self.left > 0).then(|| unsafe { &*self.next })
    }

    /// Moves past the next key, if any.
    #[inline]
    pub(crate) fn advance(&mut self) {
        if self.left > 0 {
            self.left -= 1;
            // Past the last key, the pointer is never read.
            self.next = self.next.wrapping_byte_add(self.stride);
        }
    }
}

impl<'a, K> Iterator for KeyRun<'a, K> {
    type Item = &'a K;

    #[inline]
    fn next(&mut self) -> Option<&'a K> {
        let key = self.peek()?;
        self.advance();
        Some(key)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.left, Some(self.left))
    }
}

/// Initialized key/value slots from a validated leaf run, with that run's
/// borrow. It caches only pointers, never references to previously yielded
/// mutable values. The walk proves each logical entry is handed out once.
pub(crate) struct LeafEntry<B, K, V, S> {
    key: NonNull<K>,
    value: NonNull<V>,
    _borrow: PhantomData<NodeRef<B, K, V, S, Leaf>>,
}

impl<'a, K, V, S> LeafEntry<Immut<'a>, K, V, S> {
    pub(super) fn into_key_value(self) -> (&'a K, &'a V) {
        // SAFETY: the slots are initialized and shared for the run's borrow.
        unsafe { (self.key.as_ref(), self.value.as_ref()) }
    }
}

impl<'a, K, V, S> LeafEntry<ValMut<'a, K, V>, K, V, S> {
    /// # Safety
    /// This entry's value is handed out only once for the run's borrow.
    pub(super) unsafe fn into_key_value_mut(mut self) -> (&'a K, &'a mut V) {
        // SAFETY: the initialized slots are disjoint, the key stays shared,
        // and the caller permits this value's unique reference.
        unsafe { (self.key.as_ref(), self.value.as_mut()) }
    }
}

impl<K, V, S> LeafEntry<Dying<K, V>, K, V, S> {
    /// # Safety
    /// This entry is moved out only once and its leaf has not been freed.
    pub(super) unsafe fn take_key_value(self) -> (K, V) {
        // SAFETY: the caller permits moving these initialized slots once.
        unsafe { (self.key.read(), self.value.read()) }
    }
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

    /// [`Self::descend`] to a child of kind `ChildKind`, as the caller
    /// decided once for this node's children
    /// ([`NodeRef::has_leaf_children`]).
    ///
    /// # Panics
    ///
    /// If the children are of the other kind.
    pub(crate) fn descend_as<ChildKind: PageKind>(self) -> NodeRef<BorrowType, K, V, S, ChildKind> {
        assert_eq!(
            self.node.height == 1,
            ChildKind::IS_LEAF,
            "descending to a child of the other kind"
        );
        // SAFETY: as for `descend`; the child's height says its kind,
        // `ChildKind`, checked just above.
        unsafe {
            let child = self.node.edge_slots().slot(self.index).read();
            NodeRef::from_raw_parts(child, self.node.height - 1)
        }
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>, Kind: PageKind> Edge<NodeMut<'a, K, V, S, Kind>> {
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

impl<BorrowType, K, V, S: InlineKeyStrategy<K>, Kind: PageKind>
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

impl<'a, K, V, S: InlineKeyStrategy<K>, Kind: PageKind> Kv<NodeRef<Immut<'a>, K, V, S, Kind>> {
    pub(crate) fn into_key(self) -> &'a K {
        // SAFETY: `index < len`, so key `index` is initialized, and it stays
        // unchanged for `'a` (invariants 1 and 3).
        unsafe { self.node.key_slots().keys().slot(self.index).as_ref() }
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>, Kind: PageKind>
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

impl<BorrowType, K, V, S: InlineKeyStrategy<K>, Kind: PageKind>
    Kv<NodeRef<BorrowType, K, V, S, Kind>>
{
    /// The key's window: its inline key at the node's prefix length, or
    /// [`Window::NONE`] in a node without inline keys.
    pub(super) fn window(&self) -> Window {
        self.node.reborrow().key_window(self.index)
    }
}

impl<K, V, S: InlineKeyStrategy<K>, Kind: PageKind> Kv<NodeMut<'_, K, V, S, Kind>> {
    /// Replaces the key and its inline key, returning the old key. Keeping
    /// the keys in order is the caller's business.
    ///
    /// The new key's window may be at a longer prefix length, if it comes
    /// from a node below this key whose bounds it is, such as the leaf right
    /// of a separator: it moves back to this node's, filled from the old
    /// key's window.
    pub(crate) fn replace_key(&mut self, key: InlinedKey<K, S>) -> K {
        self.node.normalize_local_prefix();
        let key = key.moved_back_to(self.window());
        // SAFETY: `index < len`, so key `index` is initialized, with its
        // inline key, and this handle is exclusive.
        let old = unsafe { self.node.key_slots().replace(self.index, key).into_key() };
        self.node.refresh_local_prefix();
        old
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
        // `0..len` are initialized; afterwards `0..len - 1` are. Paired, key
        // and value `index` share an entry, read out before the entries after
        // it shift down whole (D29). The edge has `index <= len - 1`.
        unsafe {
            let (key, value) = if NodePtr::<K, V, S>::has_paired_entries::<Leaf>() {
                let value = leaf.value_slots().slot(index).read();
                let key = leaf.key_slots().remove_shifting_entries(index, len);
                (key.into_key(), value)
            } else {
                let key = leaf.key_slots().remove(index, len);
                (key.into_key(), leaf.value_slots().remove(index, len))
            };
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
        parent.normalize_local_prefix();
        let len = parent.len();
        // SAFETY: `index < len`, so key `index` (with its inline key) and edge
        // `index + 1` are initialized; afterwards keys `0..len - 1` and edges
        // `0..len` are. Paired, they share an entry, read out before the
        // entries after it shift down whole (D29). The edge was the root of a
        // well-formed subtree one level down that the node owned, and the
        // returned `Subtree` owns now (invariant 2).
        unsafe {
            let (key, child) = if NodePtr::<K, V, S>::has_paired_entries::<Internal>() {
                let child = parent.edge_slots().slot(index + 1).read();
                let key = parent.key_slots().remove_shifting_entries(index, len);
                (key.into_key(), child)
            } else {
                let key = parent.key_slots().remove(index, len);
                (
                    key.into_key(),
                    parent.edge_slots().remove(index + 1, len + 1),
                )
            };
            parent.set_len(len - 1);
            (key, Subtree::from_raw_parts(child, parent.height - 1))
        }
    }

    /// The balancing context of this separator and the two children around
    /// it, which are of kind `ChildKind`, as the caller decided once for
    /// this node's children ([`NodeRef::has_leaf_children`]).
    ///
    /// # Panics
    ///
    /// If the children are of the other kind; or if they are leaves and the
    /// right one has no head to hold the separator's value, which never
    /// happens in a tree built by the node operations (D17).
    pub(crate) fn into_balancing_context<ChildKind: TrailingSlots>(
        self,
    ) -> BalancingContext<'a, K, V, S, ChildKind> {
        let child_height = self.node.height - 1;
        assert_eq!(
            child_height == 0,
            ChildKind::IS_LEAF,
            "balancing children of the other kind"
        );
        // SAFETY: `index < len`, so edges `index` and `index + 1` are
        // initialized: roots of two distinct, disjoint subtrees one level down
        // that the parent owns (invariant 2), and neither includes the
        // parent's page. So exclusive handles to both children can live next to
        // the parent's, as long as the context doesn't change those two edges
        // while the children are in use: only `merge` changes one, and it
        // consumes the context. Their height says their kind, `ChildKind`,
        // checked just above.
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
        // SAFETY: the right child is an allocated node of kind `ChildKind`.
        let right_has_first_slot = unsafe { ChildKind::has_first_slot(context.right_child.node) };
        assert!(
            right_has_first_slot,
            "the leaf right of a separator has no head"
        );
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

impl<K, V, S: InlineKeyStrategy<K>> Kv<NodeRef<Dying<K, V>, K, V, S, Leaf>> {
    /// Moves the key and value out of their slots, which the leaf keeps
    /// counting: only the walk taking the tree apart knows they are gone.
    ///
    /// # Safety
    ///
    /// They are moved out only this once, and nothing reads their slots
    /// afterward.
    pub(crate) unsafe fn take_key_value(self) -> (K, V) {
        // SAFETY: `index < len`, so key and value `index` are initialized
        // unless moved out (invariants 1 and 3), which the caller vouches
        // they are not, and only now.
        unsafe {
            (
                self.node.key_slots().keys().slot(self.index).read(),
                self.node.value_slots().slot(self.index).read(),
            )
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Kv<NodeRef<Dying<K, V>, K, V, S, Internal>> {
    /// Moves the separator key out of its slot; see
    /// [`Kv::take_key_value`].
    ///
    /// # Safety
    ///
    /// As for [`Kv::take_key_value`].
    pub(crate) unsafe fn take_key(self) -> K {
        // SAFETY: as for `take_key_value`.
        unsafe { self.node.key_slots().keys().slot(self.index).read() }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Head<NodeRef<Dying<K, V>, K, V, S, Leaf>> {
    /// Moves the head out of its slot, which the leaf's header still
    /// flags; see [`Kv::take_key_value`].
    ///
    /// # Safety
    ///
    /// As for [`Kv::take_key_value`].
    pub(crate) unsafe fn take_value(self) -> V {
        // SAFETY: the head is initialized unless moved out (invariants 3 and
        // 5), which the caller vouches it is not, and only now.
        unsafe { self.leaf.head_and_value_slots().slot(0).read() }
    }
}

impl<K, V, S, Kind: PageKind> NodeRef<Dying<K, V>, K, V, S, Kind> {
    /// Gives the node's page back, sorted by the node's kind.
    ///
    /// # Safety
    ///
    /// Every key, value and head the node held has been moved out, and every
    /// child it led to freed, and nothing uses the node, through this handle
    /// or a copy, again.
    pub(crate) unsafe fn free(self) -> PageByKind<K, V, S> {
        // SAFETY: the node is of kind `Kind` (invariant 1), and the caller
        // hands over everything in it and every use of it.
        Kind::page_by_kind(unsafe { self.node.into_fresh_page::<Kind>() })
    }
}

impl<K, V, S> NodeByKind<Dying<K, V>, K, V, S> {
    /// [`NodeRef::free`], for a node whose kind was decided.
    ///
    /// # Safety
    ///
    /// As for [`NodeRef::free`].
    pub(crate) unsafe fn free(self) -> PageByKind<K, V, S> {
        // SAFETY: forwarded.
        unsafe {
            match self {
                ForceResult::Leaf(leaf) => leaf.free(),
                ForceResult::Internal(internal) => internal.free(),
            }
        }
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

impl<'a, K, V, S: InlineKeyStrategy<K>, Kind: PageKind> FittingEdge<NodeMut<'a, K, V, S, Kind>> {
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
        // `0..len + 1` are afterwards. Paired, the entries from key and value
        // `index` on shift up whole, and the new pair goes into the entry
        // that frees (D29).
        unsafe {
            if NodePtr::<K, V, S>::has_paired_entries::<Leaf>() {
                leaf.key_slots().insert_shifting_entries(index, len, key);
                leaf.value_slots().slot(index).write(value);
            } else {
                leaf.key_slots().insert(index, len, key);
                leaf.value_slots().insert(index, len, value);
            }
            leaf.grow_len(len + 1);
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
        parent.normalize_local_prefix();
        let len = parent.len();
        let key = parent.reborrow().window_from_child(index, key);
        // SAFETY: `index <= len < capacity` (invariants 3 and 4), so
        // `index + 1 <= len + 1 < capacity + 1`; keys (with their inline keys)
        // `0..len` and edges `0..=len` are initialized, and one more of each
        // afterwards. The new edge is the root of a well-formed subtree one
        // level down (the caller), owned by the `Subtree` given up here
        // (invariant 2). Paired, the entries from key `index` and edge
        // `index + 1` on shift up whole, and the new pair goes into the entry
        // that frees (D29).
        unsafe {
            if NodePtr::<K, V, S>::has_paired_entries::<Internal>() {
                parent.key_slots().insert_shifting_entries(index, len, key);
                parent.edge_slots().slot(index + 1).write(right_child.node);
            } else {
                parent.key_slots().insert(index, len, key);
                parent
                    .edge_slots()
                    .insert(index + 1, len + 1, right_child.node);
            }
            parent.grow_len(len + 1);
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
        let mut split = self.split_for_insert(right_page);
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
        let mut split = self.split_for_insert(right_page);
        // SAFETY: `split` was just made, and neither half changed since; both
        // halves are at the original height, checked against `right_child`
        // above.
        unsafe {
            split.fitting_edge().insert_fit_unchecked(key, right_child);
        }
        split
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>, Kind: TrailingSlots> FullEdge<NodeMut<'a, K, V, S, Kind>> {
    /// Leave room where entries are arriving. An insertion at either end
    /// leaves that side about one quarter full and the other side denser;
    /// an interior insertion divides the free space evenly. The active side
    /// keeps another minimum's worth of entries above the removal threshold,
    /// so deleting the new entry cannot immediately merge the halves again.
    /// Small nodes fall back to an even split to keep headroom on both sides.
    ///
    /// Both sides meet `minimum` after the new entry is inserted, and
    /// the returned index is below `capacity`.
    fn separator_index(capacity: usize, insertion_index: usize, minimum: usize) -> usize {
        let arriving_len = (2 * minimum).min(capacity / 2);
        if insertion_index == 0 {
            arriving_len - 1
        } else if insertion_index == capacity {
            capacity - arriving_len
        } else {
            capacity / 2
        }
    }

    /// Splits the full node around the key `separator_index` picks, and
    /// tells where the new entry goes: left if this edge is left of the
    /// separator, right otherwise. Both sides have room.
    ///
    /// The separator key moves up, with its inline key. In a leaf, its value
    /// stays behind as the head of the right leaf (D17). The right half goes
    /// into `right_page`, a page for the same kind of node as this one.
    fn split_for_insert(self, right_page: FreshPage<K, V, S, Kind>) -> Split<'a, K, V, S, Kind> {
        let Edge {
            node: mut left,
            index,
        } = self.0;
        left.normalize_local_prefix();
        let len = left.len();
        let separator_index = Self::separator_index(len, index, left.min_len());
        debug_assert!(separator_index < len);
        let right_len = len - separator_index - 1;
        let right = NodePtr::from_fresh_page(right_page);
        // SAFETY: `right` is a fresh node, reached by nothing else; it takes
        // the prefix length of the keys and inline keys it is about to get.
        unsafe { right.set_prefix_len(left.prefix_len()) };
        // SAFETY: `separator_index < len == capacity` (invariant 4 and the
        // policy). Keys `separator_index + 1..len` are initialized and move,
        // with their inline keys, to the start of `right`, a fresh page of the
        // same kind and map (both are `Kind`, `K`, `V`, `S`). So do trailing
        // slots `separator_index + 1..=len`, to slots `0..=right_len`: in a
        // leaf, the values of keys `separator_index..len`, so that the
        // separator's becomes the right leaf's head, which its header then
        // flags (invariant 5); in an internal node, the edges right of the
        // separator. Key `separator_index` is then the last initialized one
        // on the left and is read out, with its inline key, leaving
        // `separator_index` keys and `separator_index + 1` trailing slots
        // there; the left node's slot 0 is untouched.
        let separator = unsafe {
            left.key_slots().move_run_to(
                separator_index + 1,
                right_len,
                right.key_slots::<Kind>(),
                0,
            );
            Kind::move_run(left.node, separator_index + 1, right_len + 1, right, 0);
            Kind::set_has_first_slot(right);
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

impl<K, V, S: InlineKeyStrategy<K>, Kind: PageKind> Split<'_, K, V, S, Kind> {
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

/// A fitting edge at the physically last leaf, lent together with its
/// owner's cleared insertion hint. Dropping this token changes no entries
/// and leaves the hint empty; consuming it appends and restores the hint.
pub(crate) struct RightmostAppend<'a, K, V, S> {
    edge: FittingEdge<NodeMut<'a, K, V, S, Leaf>>,
    hint: &'a mut Option<NodePtr<K, V, S>>,
}

impl<K, V, S: InlineKeyStrategy<K>> RightmostAppend<'_, K, V, S> {
    /// Appends the same key that was validated by
    /// [`Subtree::rightmost_append`], with its value. Key identity is a
    /// logical ordering requirement, not a memory-safety premise: the edge
    /// itself proves its position and available room.
    #[inline]
    pub(crate) fn insert(self, key: K, value: V) {
        let Self { edge, hint } = self;
        let node = edge.0.node.node_ptr();
        let prefix_len = edge.0.node.prefix_len();
        // Compute the summary before mutation. A panic or an unused token
        // leaves the hint cleared, without changing the tree.
        edge.insert_fit(InlinedKey::new(key, prefix_len), value);
        // Fitting appends preserve this leaf and all ancestors. There are
        // no callbacks after insertion; the caller counts the entry next.
        *hint = Some(node);
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
/// If present, `rightmost_leaf` points to the tree's physically last leaf.
/// Every general exclusive borrow clears it before lending mutation access;
/// only the owner's fitting append preserves it. It holds no key reference,
/// cached length, or assertion about the behavior of `Ord`.
pub(crate) struct Subtree<K, V, S, Kind = LeafOrInternal> {
    node: NodePtr<K, V, S>,
    height: usize,
    rightmost_leaf: Option<NodePtr<K, V, S>>,
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
            rightmost_leaf: None,
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
        self.clear_insertion_hint();
        // SAFETY: the tree is well-formed (type invariant), and `&mut self`
        // gives the handle exclusive access.
        unsafe { NodeRef::from_raw_parts(self.node, self.height) }
    }

    /// A handle for a walk that hands out values mutably (see [`ValMut`]).
    pub(crate) fn borrow_valmut(&mut self) -> NodeRef<ValMut<'_, K, V>, K, V, S, Kind> {
        self.clear_insertion_hint();
        // SAFETY: the tree is well-formed (type invariant), and `&mut self`
        // gives exclusive access, which the handle and its copies only use
        // as `ValMut` allows.
        unsafe { NodeRef::from_raw_parts(self.node, self.height) }
    }

    /// Ends the owner's permission to append directly before another
    /// exclusive borrower can change or release the remembered leaf.
    pub(super) fn clear_insertion_hint(&mut self) {
        if self.rightmost_leaf.is_some() {
            self.rightmost_leaf = None;
        }
    }

    #[cfg(test)]
    pub(crate) fn has_insertion_hint(&self) -> bool {
        self.rightmost_leaf.is_some()
    }

    /// The tree, for a walk to take apart (see [`Dying`]).
    pub(crate) fn into_dying(self) -> NodeRef<Dying<K, V>, K, V, S, Kind> {
        // SAFETY: the tree is well-formed (type invariant), and the
        // `Subtree` given up was the only owner of it.
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
}

impl<K, V, S: InlineKeyStrategy<K>> Subtree<K, V, S> {
    /// Publishes a path's proof after it relinquishes its mutation access.
    ///
    /// # Safety
    ///
    /// `leaf` is the physically last leaf owned by this tree. No outstanding
    /// handle may change it after this call; every subsequent structural
    /// mutation must first clear the hint.
    pub(super) unsafe fn remember_rightmost_leaf(&mut self, leaf: NodePtr<K, V, S>) {
        self.rightmost_leaf = Some(leaf);
    }

    /// Validates a fitting append to the remembered last leaf when `key`
    /// is greater than its current last key. Clears an active hint before
    /// comparison, and retains no borrow of `key`: the returned token can
    /// consume it on insertion. Failure leaves ordinary search to the caller.
    #[inline]
    pub(crate) fn rightmost_append(&mut self, key: &K) -> Option<RightmostAppend<'_, K, V, S>>
    where
        K: Ord,
    {
        let node = self.rightmost_leaf?;
        self.rightmost_leaf = None;
        // SAFETY: the hint's invariant identifies a live leaf owned by
        // `self`; the exclusive owner borrow is its only mutation access.
        // No ordering result is used to justify the pointer or its bounds.
        // The returned handle reaches only this heap page, disjoint from
        // the owner's hint field lent alongside it for the same lifetime.
        let leaf: NodeMut<'_, K, V, S, Leaf> = unsafe { NodeRef::from_raw_parts(node, 0) };
        let EdgeRoom::Fits(edge) = leaf.last_edge().check_room() else {
            return None;
        };
        let last = edge.0.reborrow().left_kv().ok()?;
        if SearchKey::<K, S, K>::new(key).cmp_full(last.key()) != Ordering::Greater {
            return None;
        }
        Some(RightmostAppend {
            edge,
            hint: &mut self.rightmost_leaf,
        })
    }

    /// Puts a new root with no keys above the current one, which becomes its
    /// only child, and returns the new root.
    pub(crate) fn push_internal_level(
        &mut self,
        page: FreshPage<K, V, S, Internal>,
    ) -> NodeMut<'_, K, V, S, Internal> {
        self.clear_insertion_hint();
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
        self.clear_insertion_hint();
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

/// A separator of an internal node and the two children around it, of kind
/// `ChildKind`, ready to be merged or to move an entry from one child to the
/// other. Every operation is the same for both kinds of children, through
/// their trailing slots ([`TrailingSlots`]).
///
/// Invariant: `left_child` and `right_child` are the children at the edges
/// left and right of `parent`, of kind `ChildKind`, and the right one's
/// trailing slot 0 is initialized: for leaves, the head that holds the
/// separator's value (D17).
pub(crate) struct BalancingContext<'a, K, V, S, ChildKind> {
    parent: Kv<NodeMut<'a, K, V, S, Internal>>,
    left_child: NodeMut<'a, K, V, S, ChildKind>,
    right_child: NodeMut<'a, K, V, S, ChildKind>,
}

/// A [`BalancingContext`] whose children fit in one node together with the
/// parent's separator. Made only by [`BalancingContext::can_merge`].
pub(crate) struct Mergeable<'a, K, V, S, ChildKind>(BalancingContext<'a, K, V, S, ChildKind>);

/// Why a steal moved nothing.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StealError {
    /// The child to take an entry from has none.
    DonorEmpty,
    /// The child to put an entry into is full.
    ReceiverFull,
}

impl<'a, K, V, S: InlineKeyStrategy<K>, ChildKind: TrailingSlots>
    BalancingContext<'a, K, V, S, ChildKind>
{
    pub(crate) fn left_child_len(&self) -> usize {
        self.left_child.len()
    }

    pub(crate) fn right_child_len(&self) -> usize {
        self.right_child.len()
    }

    /// The fewest keys either child holds between removals (D22).
    pub(crate) fn child_min_len(&self) -> usize {
        self.left_child.min_len()
    }

    pub(crate) fn into_parent(self) -> Kv<NodeMut<'a, K, V, S, Internal>> {
        self.parent
    }

    pub(crate) fn into_left_child(self) -> NodeMut<'a, K, V, S, ChildKind> {
        self.left_child
    }

    pub(crate) fn into_right_child(self) -> NodeMut<'a, K, V, S, ChildKind> {
        self.right_child
    }

    /// The merge decision: whether both children and the parent's separator
    /// fit in one node; `Err(self)` if not.
    pub(crate) fn can_merge(self) -> Result<Mergeable<'a, K, V, S, ChildKind>, Self> {
        let merged_len = self.left_child.len() + 1 + self.right_child.len();
        if merged_len <= self.left_child.capacity() {
            Ok(Mergeable(self))
        } else {
            Err(self)
        }
    }

    /// Rotates one entry rightwards: the left child's last key goes up into
    /// the parent, and the parent's separator comes down to the front of the
    /// right child, each with its inline key; the left child's last trailing
    /// slot becomes the right child's first.
    ///
    /// Between leaves, the value rotates through the right child's head: the
    /// left child's last value becomes the head, and the old head joins the
    /// separator that came down (D17). Between internal children, the left
    /// child's last edge becomes the right child's first.
    ///
    /// The right child takes the parent's prefix length (05-fence-key.md),
    /// at which the separator's inline key is. After local search windows
    /// are normalized, shifting reads no further key: its
    /// inline keys, and the key going up, move back to it, filled from the
    /// separator's, a fence of both children. Fitting both children's
    /// prefixes to their new fences, which reads keys, is the caller's
    /// business, once its operation is done (`prefix.rs`).
    pub(crate) fn steal_left(&mut self) -> Result<(), StealError> {
        let left_len = self.left_child.len();
        let right_len = self.right_child.len();
        if left_len == 0 {
            return Err(StealError::DonorEmpty);
        }
        if right_len == self.right_child.capacity() {
            return Err(StealError::ReceiverFull);
        }
        self.parent.node.normalize_local_prefix();
        self.left_child.normalize_local_prefix();
        self.right_child.normalize_local_prefix();
        let fill = self.parent.window();
        self.right_child.move_prefix_back_to(fill);
        let parent = &mut self.parent;
        // SAFETY: checked just above: the left child has key `left_len - 1`
        // and trailing slot `left_len` (its value, or its last edge) to give,
        // and the right child room for one more key and trailing slot. Both
        // are of kind `ChildKind`, in the parent's map. The parent's
        // separator is initialized (invariant 3). Keys move with their inline
        // keys, all three nodes being of the same map (invariant 6). The
        // right child's trailing slots `0..=right_len` are initialized (the
        // context's invariant), so inserting at slot 0 shifts them up: between
        // leaves, the old head becomes the value of the separator now at key
        // 0, and the left child's last value the head, so the right child
        // keeps one; between internal children, the moved edge stays one level
        // below both (invariant 2). Each child keeps keys `0..len` and
        // trailing slots `1..=len` initialized, and its slot 0 as it was.
        unsafe {
            let key = self.left_child.key_slots().remove(left_len - 1, left_len);
            let separator = parent
                .node
                .key_slots()
                .replace(parent.index, key.moved_back_to(fill));
            self.right_child.key_slots().insert(0, right_len, separator);
            ChildKind::move_slot(
                self.left_child.node,
                left_len,
                left_len + 1,
                self.right_child.node,
                0,
                right_len + 1,
            );
            self.left_child.set_len(left_len - 1);
            self.right_child.set_len(right_len + 1);
        }
        self.parent.node.refresh_local_prefix();
        Ok(())
    }

    /// Rotates one entry leftwards: the right child's first key goes up into
    /// the parent, and the parent's separator comes down to the end of the
    /// left child, each with its inline key; the right child's first
    /// trailing slot becomes the left child's last. The left child takes the
    /// parent's prefix length, as in [`Self::steal_left`].
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
        self.parent.node.normalize_local_prefix();
        self.left_child.normalize_local_prefix();
        self.right_child.normalize_local_prefix();
        let fill = self.parent.window();
        self.left_child.move_prefix_back_to(fill);
        let parent = &mut self.parent;
        // SAFETY: as in `steal_left`, mirrored: the right child gives key 0
        // and trailing slot 0 (its head, after which the value of the key
        // that went up is its head; or its first edge), shifting the rest
        // down, and the left child has room at key `left_len` and trailing
        // slot `left_len + 1`. The left child's slot 0 is untouched.
        unsafe {
            let key = self.right_child.key_slots().remove(0, right_len);
            let separator = parent
                .node
                .key_slots()
                .replace(parent.index, key.moved_back_to(fill));
            self.left_child
                .key_slots()
                .insert(left_len, left_len, separator);
            ChildKind::move_slot(
                self.right_child.node,
                0,
                right_len + 1,
                self.left_child.node,
                left_len + 1,
                left_len + 1,
            );
            self.left_child.set_len(left_len + 1);
            self.right_child.set_len(right_len - 1);
        }
        self.parent.node.refresh_local_prefix();
        Ok(())
    }
}

impl<'a, K, V, S, ChildKind> Mergeable<'a, K, V, S, ChildKind> {
    /// Gives up the token.
    pub(crate) fn into_context(self) -> BalancingContext<'a, K, V, S, ChildKind> {
        self.0
    }
}

impl<'a, K, V, S: InlineKeyStrategy<K>, ChildKind: TrailingSlots>
    Mergeable<'a, K, V, S, ChildKind>
{
    /// Moves the parent's separator and all of the right child into the left
    /// child, and removes the separator and the edge to the right child from
    /// the parent. Returns the parent's edge to the merged child, and the
    /// right child's emptied page, sorted by the children's kind.
    ///
    /// Between leaves, the separator rejoins its value, the right child's
    /// head, as an ordinary entry, and the left child keeps its own head, if
    /// any (D17). Keys move with their inline keys.
    ///
    /// The merged child takes the parent's prefix length (05-fence-key.md),
    /// at which the separator's inline key is. After local search windows
    /// are normalized, shifting reads no further key: both
    /// children's inline keys move back to it, filled from the separator's,
    /// a fence of both. Fitting the merged child's prefix to its fences,
    /// which reads keys, is the caller's business, as for
    /// [`BalancingContext::steal_left`].
    pub(crate) fn merge(self) -> (EdgeMut<'a, K, V, S, Internal>, PageByKind<K, V, S>) {
        let fill = self.0.parent.window();
        let BalancingContext {
            parent: Kv {
                node: mut parent,
                index,
            },
            mut left_child,
            right_child,
        } = self.0;
        parent.normalize_local_prefix();
        let parent_len = parent.len();
        // SAFETY: `index < parent_len` (invariant 3). Key `index`, with its
        // inline key, at the parent's prefix length, `fill`'s, and edge
        // `index + 1`, the right child, come out; the parent keeps keys
        // `0..parent_len - 1` and edges `0..parent_len`.
        let separator = unsafe {
            let separator = parent.key_slots().remove(index, parent_len);
            parent.edge_slots().remove(index + 1, parent_len + 1);
            parent.set_len(parent_len - 1);
            separator
        };
        // SAFETY: both children and the separator fit in one node (invariant
        // 4). The right child is another node of kind `ChildKind`, at the
        // left one's height, in the same map, with its trailing slot 0
        // initialized (the context's invariant); its edge is gone from the
        // parent, so nothing reaches its page but the one returned. The
        // parent's prefix length, `fill`'s, is at most either child's.
        let freed_page = unsafe { left_child.absorb_unchecked(fill, separator, right_child) };
        // SAFETY: `index <= parent_len - 1`, the parent's new length.
        let merged_edge = unsafe { Edge::new_unchecked(parent, index) };
        (merged_edge, freed_page)
    }
}

/// What [`NodeMut::absorb`] gives back when the node, the separator and the
/// tree's root don't fit in one node: the separator and the tree.
pub(crate) type Unabsorbed<K, V, S> = (InlinedKey<K, S>, Subtree<K, V, S>);

impl<'a, K, V, S: InlineKeyStrategy<K>, Kind: TrailingSlots> NodeMut<'a, K, V, S, Kind> {
    /// Moves `separator`, and every key and trailing slot of `right` after
    /// it, to the end of this node, as a merge moves its parent's separator
    /// and right child into the left one, and returns `right`'s emptied
    /// page: for a node and the root of another tree, which share no
    /// parent, as when two trees join (`join.rs`). For leaves, `right`'s
    /// head is the separator's value, which rejoins it as an entry (D17).
    ///
    /// Both nodes take the separator's prefix length, as merged children
    /// take their parent's. If they don't fit in one node with the
    /// separator, nothing changes, and both come back.
    ///
    /// # Panics
    ///
    /// If `right` is not at this node's height, if it is a leaf without a
    /// head, or if the separator's prefix length is longer than either
    /// node's; nothing changes then.
    pub(crate) fn absorb(
        &mut self,
        separator: InlinedKey<K, S>,
        right: Subtree<K, V, S>,
    ) -> Result<PageByKind<K, V, S>, Unabsorbed<K, V, S>> {
        assert_eq!(
            right.height, self.height,
            "a node absorbs a node of its own height"
        );
        if self.len() + 1 + Subtree::borrow(&right).len() > self.capacity() {
            return Err((separator, right));
        }
        // SAFETY: `right` is the root of a well-formed tree (its type
        // invariant) at this node's height, so of this kind, and the
        // `Subtree` given up below was its only owner: no other handle to it
        // lives.
        let right_node: NodeMut<'_, K, V, S, Kind> =
            unsafe { NodeRef::from_raw_parts(right.node, right.height) };
        // SAFETY: an allocated node of kind `Kind`.
        let right_has_first_slot = unsafe { Kind::has_first_slot(right_node.node) };
        assert!(
            right_has_first_slot,
            "the leaf right of a separator has no head"
        );
        let fill = separator.window();
        assert!(
            fill.prefix_len() <= self.prefix_len().min(right_node.prefix_len()),
            "a separator past the prefix of a node it joins"
        );
        // SAFETY: checked just above: the fit, `right`'s kind, height and
        // initialized trailing slot 0, and the prefix lengths. `right` is
        // another tree's root, so another node, of the same map type, and
        // its owner is given up: nothing reaches its page but the one
        // returned.
        Ok(unsafe { self.absorb_unchecked(fill, separator, right_node) })
    }

    /// Moves `separator` to key slot `len`, and every key and trailing slot
    /// of `right` after it, both nodes first taking `fill`'s prefix length,
    /// and returns `right`'s emptied page: the core of [`Mergeable::merge`]
    /// and [`Self::absorb`]. After local search windows are normalized,
    /// shifting reads no further key: both nodes' inline keys move back,
    /// filled from the separator's, a fence of both. Fitting the node's
    /// prefix to its fences, which reads keys, is the caller's business.
    ///
    /// # Safety
    ///
    /// The node, `separator` and `right` fit in one node: `len + 1 +
    /// right.len() <= capacity`. `right` is another node of this kind and
    /// height, in a tree of the same map type, whose trailing slot 0 is
    /// initialized (a leaf's head), and nothing reaches its page afterwards
    /// but through the one returned. `separator`'s window is at `fill`'s
    /// prefix length, at most either node's.
    unsafe fn absorb_unchecked(
        &mut self,
        fill: Window,
        separator: InlinedKey<K, S>,
        mut right: NodeMut<'_, K, V, S, Kind>,
    ) -> PageByKind<K, V, S> {
        self.move_prefix_back_to(fill);
        right.move_prefix_back_to(fill);
        let left_len = self.len();
        let right_len = right.len();
        let merged_len = left_len + 1 + right_len;
        // SAFETY:
        // - This node: `merged_len <= capacity` (the caller). The separator
        //   goes into key slot `left_len`, and `right`'s keys `0..right_len`
        //   into `left_len + 1..merged_len`, all with their inline keys at
        //   this node's prefix length now (invariant 6). `right`'s trailing
        //   slots `0..=right_len`, all initialized (the caller), go into this
        //   node's `left_len + 1..=merged_len`: between leaves, its head
        //   beside the separator as its value, and its values; between
        //   internal nodes, its edges, from a distinct page and one level
        //   below, like this node's own. This node's slot 0 is untouched.
        // - `right`: of kind `Kind`, as its page is released; everything
        //   moved out, and nothing reaches its page anymore (the caller).
        unsafe {
            self.key_slots().write(left_len, separator);
            right
                .key_slots()
                .move_run_to(0, right_len, self.key_slots(), left_len + 1);
            Kind::move_run(right.node, 0, right_len + 1, self.node, left_len + 1);
            self.set_len(merged_len);
            Kind::page_by_kind(right.node.into_fresh_page())
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Edge<NodeMut<'_, K, V, S, Internal>> {
    /// Puts `child`, the root of a tree one level below this node, at this
    /// edge in place of the subtree there, and returns that one, as two
    /// trees join (`join.rs`). Keeping keys in order, heads where separators
    /// expect them (D17), and the new child's prefix at least this node's,
    /// is the caller's business.
    ///
    /// # Panics
    ///
    /// If `child` is not exactly one level below this node; the node is
    /// unchanged then.
    pub(crate) fn replace_child(&mut self, child: Subtree<K, V, S>) -> Subtree<K, V, S> {
        self.node.assert_child_level(&child);
        // SAFETY: `index <= len` (invariant 3), so edge `index` is
        // initialized: the only pointer to a well-formed subtree one level
        // down (invariant 2), whose ownership goes to the `Subtree`
        // returned. `child`, checked to be one level down, takes its place,
        // owned by the node from now on, its `Subtree` given up.
        unsafe {
            let old = self.node.edge_slots().slot(self.index).replace(child.node);
            Subtree::from_raw_parts(old, self.node.height - 1)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::vec::Vec;

    use core::cmp::Ordering;
    use core::ptr::NonNull;
    use std::vec;

    use super::{InlineKeySlots, InlineSlot, LiveSlots, NodeInlineKeys};
    use crate::inline_key::{InlineWidth, InlineWord};
    use crate::node::test_support::TestRng;

    /// `stale_count` random slots after `live`, as no node holds past its
    /// length: a search must stay in bounds whatever they hold.
    fn slots_with_stale_tail<W: InlineSlot + From<u8>>(
        rng: &TestRng,
        live: &[W],
        stale_count: usize,
    ) -> Vec<W> {
        let stale = (0..stale_count).map(|_| W::from(rng.below(20) as u8));
        live.iter().copied().chain(stale).collect()
    }

    /// The greatest inline key, which slots past a node's length hold.
    fn greatest<W: InlineSlot>() -> W {
        W::of_window(InlineWord::MAX)
    }

    /// On sorted inline keys, the run is exactly the equal ones, wherever
    /// they are and however many, over several words of slots, the
    /// greatest too, by every way of finding it, with the slots past `len`
    /// holding the greatest (invariant 6), and by the vector count whatever
    /// they hold.
    fn assert_equal_runs_of_sorted_inline_keys<W: InlineSlot + From<u8>>(seed: u64) {
        let rng = TestRng::new(seed);
        for _ in 0..if cfg!(miri) { 50 } else { 2000 } {
            let len = rng.below(140);
            let mut inline_keys: Vec<W> = (0..len)
                .map(|_| match rng.below(9) {
                    8 => greatest(),
                    small => W::from(small as u8 * 2),
                })
                .collect();
            inline_keys.sort_unstable();
            let stale = core::iter::repeat_n(greatest(), rng.below(8));
            let slots: Vec<W> = inline_keys.iter().copied().chain(stale).collect();
            let node = LiveSlots::new(&slots, len);
            let unpadded = slots_with_stale_tail(&rng, &inline_keys, rng.below(8));
            for target in (0..18).map(W::from).chain([greatest()]) {
                let start = inline_keys.partition_point(|&probe| probe < target);
                let end = inline_keys.partition_point(|&probe| probe <= target);
                assert_eq!(node.equal_run(target), start..end);
                assert_eq!(node.equal_run_by_count(target), start..end);
                assert_eq!(node.equal_run_by_search(target), start..end);
                for slots in [&slots, &unpadded] {
                    if let Some(run) = W::equal_run_in_vectors(slots, len, target) {
                        assert_eq!(run, start..end);
                    }
                }
            }
        }
    }

    #[test]
    fn equal_runs_of_sorted_inline_keys() {
        assert_equal_runs_of_sorted_inline_keys::<u16>(51);
        assert_equal_runs_of_sorted_inline_keys::<u32>(53);
        assert_equal_runs_of_sorted_inline_keys::<u64>(54);
    }

    /// On any inline keys, the run stays within the live ones.
    fn assert_equal_runs_stay_in_bounds<W: InlineSlot + From<u8>>(seed: u64) {
        let rng = TestRng::new(seed);
        for _ in 0..if cfg!(miri) { 50 } else { 2000 } {
            let len = rng.below(40);
            let inline_keys: Vec<W> = (0..len).map(|_| W::from(rng.below(4) as u8)).collect();
            let slots = slots_with_stale_tail(&rng, &inline_keys, rng.below(8));
            let node = LiveSlots::new(&slots, len);
            for target in (0..5).map(W::from) {
                let runs = [
                    node.equal_run(target),
                    node.equal_run_by_count(target),
                    node.equal_run_by_search(target),
                ];
                for run in runs
                    .into_iter()
                    .chain(W::equal_run_in_vectors(&slots, len, target))
                {
                    assert!(run.start <= run.end && run.end <= len);
                }
            }
        }
    }

    #[test]
    fn equal_runs_stay_in_bounds_for_unsorted_inline_keys() {
        assert_equal_runs_stay_in_bounds::<u16>(52);
        assert_equal_runs_stay_in_bounds::<u32>(55);
        assert_equal_runs_stay_in_bounds::<u64>(56);
    }

    /// A node's inline keys at each width keep the first bytes of the
    /// windows written, compare by them, and find the runs of windows that
    /// tie in them: three sorted windows that share 2 bytes, two of which
    /// share 4.
    #[test]
    fn inline_keys_of_each_width_keep_the_first_bytes() {
        let windows: [InlineWord; 3] = [
            0x0102_0304_0506_0708,
            0x0102_0304_ffff_ffff,
            0x0102_ffff_0000_0000,
        ];
        for (width, kept_bytes) in [
            (InlineWidth::Two, 2),
            (InlineWidth::Four, 4),
            (InlineWidth::Eight, 8),
        ] {
            let capacity = 1024 / kept_bytes;
            // All ones: the greatest inline key at every width, which slots
            // past a node's length hold (invariant 6).
            let mut buffer = vec![u64::MAX; 128];
            let anchor = NonNull::new(buffer.as_mut_ptr().cast::<u8>()).unwrap();
            // SAFETY: `capacity` slots of `kept_bytes` fill the 1 KB buffer,
            // which is 8-aligned and outlives them.
            let slots = unsafe { InlineKeySlots::new(width, anchor, capacity) };
            for (index, &window) in windows.iter().enumerate() {
                // SAFETY: `index < capacity`.
                unsafe { slots.write(index, window) };
            }
            // SAFETY: the buffer was filled, so every slot is initialized,
            // and nothing writes it while `inline_keys` lives.
            let inline_keys = unsafe { NodeInlineKeys::new(slots, capacity, windows.len()) };
            let kept_mask = !(InlineWord::MAX
                .checked_shr(8 * kept_bytes as u32)
                .unwrap_or(0));
            for (index, &window) in windows.iter().enumerate() {
                let kept = window & kept_mask;
                assert_eq!(inline_keys.inline_key(index), kept);
                assert_eq!(inline_keys.cmp_at(index, window), Ordering::Equal);
                let below = windows.iter().filter(|&&other| other & kept_mask < kept);
                let ties = windows.iter().filter(|&&other| other & kept_mask == kept);
                let (below, ties) = (below.count(), ties.count());
                assert_eq!(inline_keys.equal_run(window), below..below + ties);
            }
        }
    }
}
