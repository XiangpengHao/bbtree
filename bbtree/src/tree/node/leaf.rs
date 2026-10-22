//! Leaves: the nodes that hold the entries. A leaf's page holds its keys and
//! values, each key paired with its value with inline keys (D29). It also
//! holds the inline keys a search runs over first (D18), and in a leaf of
//! heap byte strings, the keys' hash tags (D36).
//!
//! A leaf also holds a *head*: the value of the separator just left of the
//! leaf, and with inline keys, that separator's key too (D17, D49). Every
//! leaf but the tree's leftmost has a head.
//!
//! [`Leaf`] is a handle to one, with every operation on one leaf: searching
//! it, reading and changing its entries and its head, splitting it, cutting
//! it, and balancing it with a sibling ([`Child`]). The handle's contracts
//! are the node module's (`tree/node/mod.rs`).

use core::alloc::Layout;
use core::borrow::Borrow;
use core::cmp::Ordering;
use core::marker::PhantomData;
use core::ops::Range;
use core::ptr::NonNull;

#[cfg(test)]
use super::Insertion;
use super::hash_tags::{HashTags, SlotMatches};
use super::inline_key_search::NodeInlineKeys;
use super::inline_keys::{SearchKey, Separator, Window, WindowedKey};
use super::internal::Internal;
use super::node_plan::NodePlan;
#[cfg(test)]
use super::page::PageAllocator;
use super::page::{FreshPage, NodeKind, NodePtr, PageByType, PageType};
use super::siblings::Siblings;
use super::slots::{KeyColumns, KeySlots, NodeKeys, Slots};
use super::{Child, EntrySlots, HeadSuccession, InsertionSide, Split};
use crate::allocator::PageKind;
use crate::inline_key::{InlineKeyStrategy, InlineWidth};

/// A leaf, by its page.
pub(crate) struct Leaf<K, V, S> {
    node: NodePtr<K, V, S>,
}

impl<K, V, S> Clone for Leaf<K, V, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K, V, S> Copy for Leaf<K, V, S> {}

// SAFETY: leaves have the leaf layout.
unsafe impl<K, V, S: InlineKeyStrategy<K>> PageType for Leaf<K, V, S> {
    const KIND: PageKind = PageKind::Leaf;

    fn page_layout() -> Layout {
        NodePtr::<K, V, S>::leaf_layout().page
    }
}

// SAFETY: as for `PageType`; leaves are leaves.
unsafe impl<K, V, S: InlineKeyStrategy<K>> NodeKind for Leaf<K, V, S> {
    const IS_LEAF: bool = true;

    #[inline(always)]
    fn has_room_for_hash_tags(inline_width: InlineWidth) -> bool {
        NodePtr::<K, V, S>::leaf_has_room_for_hash_tags(inline_width)
    }
}

impl<K, V, S> Leaf<K, V, S> {
    /// The leaf at `node`, which must hold one.
    #[inline(always)]
    pub(in crate::tree) fn at(node: NodePtr<K, V, S>) -> Self {
        debug_assert!(node.is_leaf(), "the node is internal");
        Self { node }
    }

    /// The leaf's page, for a node stack or an edge to keep.
    #[inline(always)]
    pub(in crate::tree) fn node_ptr(self) -> NodePtr<K, V, S> {
        self.node
    }

    #[inline(always)]
    pub(crate) fn len(self) -> usize {
        // SAFETY: the node is allocated and initialized (invariant 1), and
        // nothing writes its header meanwhile (the handles' contract).
        unsafe { self.node.len() }
    }

    /// Whether the leaf has a head, as every leaf but the tree's leftmost
    /// does in a tree built by the node operations (D17).
    pub(in crate::tree) fn has_head(self) -> bool {
        // SAFETY: the node is allocated and its header written (invariant 1).
        unsafe { self.node.has_head() }
    }
}

/// What the batch can do before the final leaf search: finish an absent
/// query, prefetch its unique candidate's metadata, or search normally.
pub(in crate::tree) enum CandidateLine {
    Absent,
    One(usize, *const u8),
    Unknown,
}

impl<K, V, S: InlineKeyStrategy<K>> Leaf<K, V, S> {
    /// A new, empty leaf in `page`, without a head, at `prefix_len`: what
    /// its fences share, or on the tree's border, what every key of the
    /// tree shares (05-fence-key.md, D50).
    pub(in crate::tree) fn new(page: FreshPage<Self>, prefix_len: usize) -> Self {
        let leaf = Self::at(NodePtr::from_fresh_page(page));
        leaf.set_empty_prefix_len(prefix_len);
        leaf
    }

    /// Gives the leaf's page back.
    ///
    /// # Safety
    ///
    /// The leaf is on a page allocated as a leaf's: not a tree's root, which
    /// is in its root page ([`RootPage::free`]). Every key, value and head
    /// the leaf held has been moved out, dropped or deliberately leaked, and
    /// nothing uses the leaf, through this handle or a copy, again.
    ///
    /// [`RootPage::free`]: super::root_page::RootPage::free
    pub(in crate::tree) unsafe fn free(self) -> FreshPage<Self> {
        // SAFETY: the page was allocated as a leaf's (caller), and the caller
        // hands over everything in it and every use of it.
        unsafe { self.node.into_fresh_page() }
    }

    fn assert_has_head(self) {
        assert!(self.has_head(), "the leaf has no head");
    }

    /// The plan of the map's leaves (`node_plan.rs`).
    #[inline(always)]
    pub(in crate::tree) fn plan() -> NodePlan<K> {
        NodePlan::of::<S, Self>()
    }

    /// The header's line, for the batch's next leaf search.
    #[inline(always)]
    pub(in crate::tree) fn search_line(self) -> *const u8 {
        self.node.page().as_ptr()
    }

    /// An initialized key slot that the batch's pending shift will read.
    #[inline(always)]
    pub(in crate::tree) fn entry_line(self, index: usize) -> *const u8 {
        core::ptr::from_ref(self.key(index)).cast()
    }

    /// What an inline-key search alone tells a batch about `key`: that the
    /// leaf lacks it, or the one candidate entry, whose line the next
    /// comparison reads. Ties, and searches without inline keys, give
    /// [`CandidateLine::Unknown`].
    #[inline(always)]
    pub(in crate::tree) fn candidate_line<Q>(self, key: &SearchKey<'_, K, S, Q>) -> CandidateLine
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let Some(target) = self.inline_target(key) else {
            return CandidateLine::Unknown;
        };
        let Some(inline_keys) = self.inline_keys() else {
            return CandidateLine::Unknown;
        };
        self.candidate_line_among(target, inline_keys, self.len())
    }

    /// [`Self::candidate_line`] for `target`, the key's window at the leaf's
    /// prefix length, among the leaf's `inline_keys`, `len` of them live.
    #[inline(always)]
    pub(super) fn candidate_line_among(
        self,
        target: Window,
        inline_keys: NodeInlineKeys<'_>,
        len: usize,
    ) -> CandidateLine {
        let index = if let Some(mask) = inline_keys.equality_mask(target.inline_key()) {
            match mask.unique(len) {
                SlotMatches::None => return CandidateLine::Absent,
                SlotMatches::One(index) => index,
                _ => return CandidateLine::Unknown,
            }
        } else {
            let candidates = inline_keys.equal_range(target.inline_key());
            if candidates.is_empty() {
                return CandidateLine::Absent;
            }
            if candidates.len() != 1 {
                return CandidateLine::Unknown;
            }
            candidates.start
        };
        // SAFETY: both searches clip to len; a unique match names an
        // initialized key of this live leaf.
        let entry = unsafe { self.key_slots().keys().slot(index) }.cast::<u8>();
        CandidateLine::One(index, entry.as_ptr())
    }

    /// The first heap line a unique candidate's comparison reads. The
    /// batch calls this only after prefetching that candidate's metadata.
    ///
    /// # Safety
    ///
    /// `index` and `prefix_len` were saved by a search of this leaf, which
    /// has not changed since. `index` names an initialized key.
    #[inline(always)]
    pub(in crate::tree) unsafe fn candidate_bytes_line<Q>(
        self,
        key: &SearchKey<'_, K, S, Q>,
        index: usize,
        prefix_len: usize,
    ) -> Option<*const u8>
    where
        K: Borrow<Q>,
        Q: ?Sized,
    {
        // SAFETY: the saved index names an initialized key of this leaf.
        let stored = unsafe { self.key_slots().keys().slot(index).as_ref() };
        key.tied_bytes_line::<Self>(prefix_len, stored)
    }

    /// Confirms the unique candidate kept by a batch's earlier mask step.
    /// Taking the saved `prefix_len` avoids a dependent header read after the
    /// entry prefetch, which matters when a batch's headers compete in L1.
    ///
    /// # Safety
    ///
    /// `index` and `prefix_len` were saved by a search of this leaf, which
    /// has not changed since. `index` names an initialized key.
    #[inline(always)]
    pub(in crate::tree) unsafe fn find_candidate<Q>(
        self,
        key: &SearchKey<'_, K, S, Q>,
        index: usize,
        prefix_len: usize,
    ) -> Option<usize>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let target = key.inline_target(prefix_len)?;
        // SAFETY: the saved index names an initialized key of this leaf.
        let stored = unsafe { self.key_slots().keys().slot(index).as_ref() };
        let matches = if Self::plan().has_whole_key_equality()
            && let Some(equal) = key.equals_whole_bytes(stored)
        {
            equal
        } else {
            key.cmp_tied::<Self>(target, stored) == Ordering::Equal
        };
        matches.then_some(index)
    }

    /// The most entries a leaf holds.
    #[inline(always)]
    pub(crate) fn capacity() -> usize {
        NodePtr::<K, V, S>::capacity::<Self>()
    }

    /// Whether the leaf holds fewer entries than [`Child::min_len`].
    pub(in crate::tree) fn is_underfull(self) -> bool {
        self.len() < <Self as Child<K, V, S>>::min_len()
    }

    /// Whether the leaf keeps its [`Child::min_len`] after losing an entry.
    #[inline(always)]
    pub(in crate::tree) fn keeps_its_minimum(self) -> bool {
        self.len() > <Self as Child<K, V, S>>::min_len()
    }

    /// How many leading bytes the leaf's fences share, and so every key in
    /// it, which its inline keys skip (05-fence-key.md); 0 for a leaf
    /// without inline keys.
    #[inline(always)]
    pub(in crate::tree) fn prefix_len(self) -> usize {
        // SAFETY: the node is allocated and initialized, and nothing writes
        // its header meanwhile (the handles' contract).
        unsafe { self.node.prefix_len() }
    }

    /// The keys and the columns beside them.
    #[inline(always)]
    fn key_slots(self) -> KeySlots<K, S, Self> {
        // SAFETY: the node is an allocated leaf (invariant 1).
        unsafe { self.node.key_slots::<Self>() }
    }

    /// The arrays beside the keys: inline keys and hash tags.
    #[inline(always)]
    fn columns(self) -> KeyColumns {
        // SAFETY: the node is an allocated leaf (invariant 1).
        unsafe { self.node.key_columns::<Self>() }
    }

    fn value_slots(self) -> Slots<V> {
        // SAFETY: the node is an allocated leaf (invariant 1).
        unsafe { self.node.value_slots() }
    }

    /// The head key slot, which holds the head's key while the leaf has a
    /// head (D49).
    ///
    /// # Panics
    ///
    /// If the map has no inline keys.
    pub(super) fn head_key_slot(self) -> NonNull<K> {
        assert!(
            NodePtr::<K, V, S>::has_inline_keys(),
            "a leaf has a head key only with inline keys"
        );
        // SAFETY: the node is an allocated leaf (invariant 1), in a map with
        // inline keys (checked just above).
        unsafe { self.node.head_key_slot() }
    }

    /// Slot 0 is the head, slot `i + 1` value `i`.
    fn head_and_value_slots(self) -> Slots<V> {
        // SAFETY: the node is an allocated leaf (invariant 1).
        unsafe { self.node.head_and_value_slots() }
    }

    /// The leaf's keys `0..len`, in order.
    #[inline(always)]
    pub(in crate::tree) fn keys<'a>(self) -> NodeKeys<'a, K> {
        // SAFETY: keys `0..len` are initialized and unchanged while the
        // caller borrows them (invariant 1 and the handles' contract), and
        // the last key's stride ends within the page (`layout.rs`).
        unsafe { NodeKeys::new(self.key_slots().keys(), self.len()) }
    }

    /// # Panics
    ///
    /// If `index >= len`.
    pub(in crate::tree) fn key<'a>(self, index: usize) -> &'a K {
        assert!(index < self.len(), "key index out of bounds");
        // SAFETY: checked just above.
        unsafe { self.key_unchecked(index) }
    }

    /// # Safety
    ///
    /// `index < len`.
    #[inline(always)]
    pub(in crate::tree) unsafe fn key_unchecked<'a>(self, index: usize) -> &'a K {
        debug_assert!(index < self.len());
        // SAFETY: key `index` is initialized (caller, invariant 1), and
        // unchanged while the caller borrows it (the handles' contract).
        unsafe { self.key_slots().keys().slot(index).as_ref() }
    }

    /// # Panics
    ///
    /// If `index >= len`.
    pub(in crate::tree) fn key_value<'a>(self, index: usize) -> (&'a K, &'a V) {
        assert!(index < self.len(), "entry index out of bounds");
        // SAFETY: `index < len`, checked just above.
        unsafe { self.key_value_unchecked(index) }
    }

    /// # Safety
    ///
    /// `index < len`.
    #[inline(always)]
    pub(in crate::tree) unsafe fn key_value_unchecked<'a>(self, index: usize) -> (&'a K, &'a V) {
        debug_assert!(index < self.len());
        // SAFETY: `index < len` (caller), so key and value `index` are
        // initialized, and they stay unchanged while the caller borrows them
        // (invariant 1 and the handles' contract).
        unsafe {
            (
                self.key_slots().keys().slot(index).as_ref(),
                self.value_slots().slot(index).as_ref(),
            )
        }
    }

    /// # Safety
    ///
    /// `index < len`.
    #[inline(always)]
    pub(in crate::tree) unsafe fn value_mut_unchecked<'a>(self, index: usize) -> &'a mut V {
        debug_assert!(index < self.len());
        // SAFETY: `index < len` (caller), so value `index` is initialized;
        // the caller has exclusive access to it for as long as it borrows it
        // (the handles' contract).
        unsafe { self.value_slots().slot(index).as_mut() }
    }

    /// # Panics
    ///
    /// If `index >= len`.
    pub(in crate::tree) fn value_mut<'a>(self, index: usize) -> &'a mut V {
        assert!(index < self.len(), "entry index out of bounds");
        // SAFETY: `index < len`, checked just above.
        unsafe { self.value_mut_unchecked(index) }
    }

    /// Where key `index` and its value are, for a walk to hand them out as
    /// its borrow allows (`tree/walk.rs`).
    ///
    /// # Safety
    ///
    /// `index < len`.
    #[inline(always)]
    pub(in crate::tree) unsafe fn entry_slots(self, index: usize) -> EntrySlots<K, V> {
        debug_assert!(index < self.len());
        // SAFETY: `index < len <= capacity` (caller), so both slots are in
        // the page; nothing is dereferenced.
        unsafe {
            EntrySlots {
                key: self.key_slots().keys().slot(index),
                value: self.value_slots().slot(index),
            }
        }
    }

    /// The leaf's inline keys, if it has them (D18): inline key `i` is key
    /// `i`'s, for `i < len`, and a search may read the slots past them.
    #[inline(always)]
    pub(in crate::tree) fn inline_keys<'a>(self) -> Option<NodeInlineKeys<'a>> {
        let slots = self.columns().inline_keys()?;
        // SAFETY: the inline-key slots number the capacity, all are
        // initialized (invariant 4), and none changes while the caller
        // borrows them (the handles' contract); they grow up, so memory
        // order is index order.
        Some(unsafe { NodeInlineKeys::new(slots, Self::capacity(), self.len()) })
    }

    /// The keys' hash tags, if the plan and layout keep them (D36): all
    /// `capacity` slots, tag `i` key `i`'s for `i < len`.
    #[inline(always)]
    pub(in crate::tree) fn hash_tags<'a>(self) -> Option<&'a [u8]> {
        let tags = self.columns().hash_tags()?;
        // SAFETY: every tag was initialized when the leaf was made and can
        // be read while nothing changes it (invariant 5 and the handles'
        // contract).
        unsafe { Some(tags.slice_ptr(Self::capacity()).as_ref()) }
    }

    /// Key `index`'s window: its inline key at the leaf's prefix length, or
    /// [`Window::NONE`] in a leaf without inline keys.
    ///
    /// # Panics
    ///
    /// If `index >= len`.
    pub(in crate::tree) fn window(self, index: usize) -> Window {
        assert!(index < self.len(), "key index out of bounds");
        let Some(inline_keys) = self.inline_keys() else {
            return Window::NONE;
        };
        Window::new(self.prefix_len(), inline_keys.inline_key(index))
    }
}

/// Searching a leaf.
impl<K, V, S: InlineKeyStrategy<K>> Leaf<K, V, S> {
    /// `key`'s window at this leaf's prefix length, if the leaf has inline
    /// keys and `key` has one to compare with them (D18).
    ///
    /// Each search computes it where it uses it, rather than taking it as an
    /// argument. Passed across a call, it would go through the stack right
    /// before the search reads it back, a store the CPU can't always
    /// forward, stalling every level. Std byte strings read the window
    /// directly from the search key.
    #[inline(always)]
    pub(in crate::tree) fn inline_target<Q: ?Sized>(
        self,
        key: &SearchKey<'_, K, S, Q>,
    ) -> Option<Window>
    where
        K: Borrow<Q>,
    {
        key.inline_target(self.prefix_len())
    }

    /// Binary search for `key` among the leaf's keys: `Ok(i)` if key `i`
    /// equals it, otherwise `Err(i)` with `i` the edge where it belongs.
    /// With inline keys (D18), over those first.
    ///
    /// The position is in bounds for any `Ord` and `InlineKey`, consistent
    /// or not.
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

    /// [`Self::search`], always inlined, whatever the plan says. An insert's
    /// search (`Node::search_for_insert`) uses it: unlike a lookup, it gains
    /// more from an inlined leaf search than it loses in code size
    /// (`NodePlan::inlines_node_search`).
    ///
    /// If the leaf has inline keys and `key` has a window at the leaf's
    /// prefix length (D18), the search runs over the inline keys; otherwise
    /// over the keys, as the leaf's plan says (D19). The key shares the
    /// leaf's prefix: it lies between the leaf's fences, or within what every
    /// key of the tree shares, which a search checks at the top (D50). A map
    /// without inline keys monomorphizes to the search over its keys alone.
    #[inline(always)]
    pub(in crate::tree) fn search_always_inlined<Q>(
        self,
        key: &SearchKey<'_, K, S, Q>,
    ) -> Result<usize, usize>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if let Some(target) = self.inline_target(key)
            && let Some(candidates) = self.candidates(target)
        {
            // Most keys an insert searches for are absent and have no
            // candidates: they stop at the inline keys, without a call.
            if candidates.is_empty() {
                return Err(candidates.start);
            }
            return self.search_candidates(candidates, key, target);
        }
        Self::plan()
            .all_keys_search()
            .search(self.keys(), key.key(), |probe: &K| {
                key.key().cmp(probe.borrow())
            })
    }

    /// The candidates for `target`: the positions of the leaf's keys whose
    /// inline keys equal `target`'s in the bytes the leaf keeps (D18), or
    /// `None` if the leaf has no inline keys. Only those keys can equal the
    /// key searched for; the others differ in their inline keys.
    ///
    /// Counts over `Self::capacity()`, a constant, so it unrolls. Passing the
    /// capacity in, sharing it with internal nodes, or outlining it
    /// de-vectorizes the count.
    #[inline(always)]
    fn candidates(self, target: Window) -> Option<Range<usize>> {
        Some(self.inline_keys()?.equal_range(target.inline_key()))
    }

    /// The search among `candidates`, the keys whose inline keys equal
    /// `key`'s (D18). The other keys differ from `key` in their inline keys,
    /// which order them (the `InlineKey` contract), so `key`'s position lies
    /// among the candidates, or where they start when there are none.
    ///
    /// It runs as the leaf's plan says (D19). Keys with their own `InlineKey`
    /// impl compare by `Ord`, and search as their type picks: branchless for
    /// plain data such as integers, whose candidates are the whole leaf when
    /// their kept bytes all tie. Std byte strings search with an early exit,
    /// comparing as [`SearchKey::cmp_tied`] does: a word at a time, from past
    /// the bytes the candidates share.
    ///
    /// In bounds for any `Ord` and `InlineKey`: the candidates lie within
    /// `0..len` whatever the inline keys hold
    /// ([`NodeInlineKeys::equal_range`]), slicing the keys by them checks
    /// that, and either search stays within the slice.
    fn search_candidates<Q>(
        self,
        candidates: Range<usize>,
        key: &SearchKey<'_, K, S, Q>,
        target: Window,
    ) -> Result<usize, usize>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let found = Self::plan().candidate_search().search(
            self.keys().slice(candidates.clone()),
            key.key(),
            |stored: &K| key.cmp_tied::<Self>(target, stored),
        );
        found
            .map(|index| candidates.start + index)
            .map_err(|index| candidates.start + index)
    }

    /// The index of the key equal to `key`, if the leaf holds one. As an
    /// exact-key lookup, it may reject candidates of other lengths (D36), and
    /// confirm a lone candidate by equality alone (D38). Ordered searches use
    /// [`Self::search`]: a hash tag gives no edge.
    pub(in crate::tree) fn find<Q, CallSite>(
        self,
        key: &SearchKey<'_, K, S, Q>,
        _call_site: CallSite,
    ) -> Option<usize>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if let Some(target) = self.inline_target(key)
            && let Some(inline_keys) = self.inline_keys()
        {
            // This typed view keeps the leaf's capacity constant, as
            // `Self::candidates` does. Empty masks need no bound
            // extraction.
            let candidates = if let Some(mask) = inline_keys.equality_mask(target.inline_key()) {
                mask.equal_range(self.len())?
            } else {
                inline_keys.equal_range(target.inline_key())
            };
            if candidates.is_empty() {
                None
            } else if candidates.len() == 1 {
                // An exact-key lookup needs no insertion edge or ordering.
                // Arrays use full equality; strings retain the prefix skip.
                let stored = &self.keys()[candidates.start];
                let matches = if Self::plan().has_whole_key_equality()
                    && let Some(equal) = key.equals_whole_bytes(stored)
                {
                    equal
                } else {
                    key.cmp_tied::<Self>(target, stored) == Ordering::Equal
                };
                matches.then_some(candidates.start)
            } else if let Some(tag) = key.hash_tag()
                && let Some(tags) = self.hash_tags()
            {
                match HashTags::new(tags).matching(candidates.clone(), tag) {
                    SlotMatches::None => None,
                    SlotMatches::One(index) => {
                        // The mask only includes live candidates;
                        // indexing keeps this fact checked locally too.
                        (key.cmp_tied::<Self>(target, &self.keys()[index]) == Ordering::Equal)
                            .then_some(index)
                    }
                    SlotMatches::Two { lower, upper } => {
                        // Binary search over the two filtered keys: the
                        // upper key is the midpoint. This avoids reading
                        // the other candidates' strings.
                        let keys = self.keys();
                        match key.cmp_tied::<Self>(target, &keys[upper]) {
                            Ordering::Equal => Some(upper),
                            Ordering::Less => (key.cmp_tied::<Self>(target, &keys[lower])
                                == Ordering::Equal)
                                .then_some(lower),
                            Ordering::Greater => None,
                        }
                    }
                    SlotMatches::Many => self.search_candidates(candidates, key, target).ok(),
                }
            } else {
                self.search_candidates(candidates, key, target).ok()
            }
        } else {
            self.search(key).ok()
        }
    }
}

/// A leaf's head: the entry of the separator just left of it (D17, D49).
impl<K, V, S: InlineKeyStrategy<K>> Leaf<K, V, S> {
    /// # Panics
    ///
    /// If the leaf has no head: only the tree's leftmost leaf lacks one.
    pub(in crate::tree) fn head_value<'a>(self) -> &'a V {
        self.assert_has_head();
        // SAFETY: the head is initialized (checked just above, invariant 3)
        // and stays unchanged while the caller borrows it (the handles'
        // contract).
        unsafe { self.head_and_value_slots().slot(0).as_ref() }
    }

    /// The head's value, exclusive; see [`Self::head_value`].
    pub(in crate::tree) fn head_value_mut<'a>(self) -> &'a mut V {
        self.assert_has_head();
        // SAFETY: the head is initialized (checked just above, invariant 3);
        // the caller has exclusive access to it for as long as it borrows it
        // (the handles' contract).
        unsafe { self.head_and_value_slots().slot(0).as_mut() }
    }

    /// The head's key, which it has only with inline keys (D49).
    ///
    /// # Panics
    ///
    /// If the map has no inline keys, or the leaf has no head.
    pub(in crate::tree) fn head_key<'a>(self) -> &'a K {
        let slot = self.head_key_slot();
        self.assert_has_head();
        // SAFETY: the map has inline keys (checked by `head_key_slot`), and the
        // leaf has a head (checked just above). So the head's key slot is
        // initialized (invariant 3), and stays unchanged while the caller
        // borrows it (the handles' contract).
        unsafe { slot.as_ref() }
    }

    /// Where the head's value is, for a walk to hand it out as its borrow
    /// allows (`tree/walk.rs`).
    ///
    /// # Panics
    ///
    /// If the leaf has no head.
    pub(super) fn head_value_slot(self) -> NonNull<V> {
        self.assert_has_head();
        // SAFETY: slot 0 of the head-and-value slots is in the page; nothing
        // is dereferenced.
        unsafe { self.head_and_value_slots().slot(0) }
    }

    /// Sets a headless leaf's head to `value`, plus `key` with inline keys
    /// (D49); otherwise `key` must be `None`. The caller keeps heads
    /// consistent with separators.
    ///
    /// # Panics
    ///
    /// If the leaf has a head, or `key` is `Some` without inline keys or
    /// `None` with them; the leaf is unchanged then.
    pub(in crate::tree) fn put_head(self, key: Option<K>, value: V) {
        assert!(!self.has_head(), "the leaf has a head");
        assert_eq!(
            key.is_some(),
            NodePtr::<K, V, S>::has_inline_keys(),
            "a head's key goes where the map keeps separator keys"
        );
        // SAFETY: the access is exclusive (the handles' contract). The leaf
        // has no head (checked above), so its head slot, and with inline keys
        // (also checked) its head key slot, hold nothing (invariant 3). Both
        // are written, then flagged.
        unsafe {
            if let Some(key) = key {
                self.head_key_slot().write(key);
            }
            self.head_and_value_slots().slot(0).write(value);
            self.node.set_has_head(true);
        }
    }

    /// Makes `separator`, the separator just left of this headless leaf, its
    /// head: `value`, and with inline keys `separator`'s key (D49). Returns
    /// the separator as its internal node keeps it: the key without inline
    /// keys, its window with them.
    ///
    /// # Panics
    ///
    /// If the leaf has a head; it is unchanged then.
    pub(in crate::tree) fn put_separator_head(
        self,
        separator: WindowedKey<K, S>,
        value: V,
    ) -> Separator<K, S> {
        if NodePtr::<K, V, S>::has_inline_keys() {
            let (key, window) = separator.into_parts();
            self.put_head(Some(key), value);
            Separator::without_key(window)
        } else {
            self.put_head(None, value);
            Separator::with_key(separator)
        }
    }

    /// Takes the head out of the leaf, which then has none: its value, and
    /// with inline keys its key (D49).
    ///
    /// # Panics
    ///
    /// If the leaf has no head.
    pub(in crate::tree) fn take_head(self) -> (Option<K>, V) {
        self.assert_has_head();
        // SAFETY: the head is initialized, and with inline keys so is its key
        // (invariant 3). Both are read out, then the header says the leaf has
        // none; the access is exclusive.
        unsafe {
            let key = NodePtr::<K, V, S>::has_inline_keys().then(|| self.head_key_slot().read());
            let value = self.head_and_value_slots().slot(0).read();
            self.node.set_has_head(false);
            (key, value)
        }
    }

    /// Replaces the head's key with `key` and returns the old one (D49). The
    /// caller keeps the keys in order.
    ///
    /// # Panics
    ///
    /// If the map has no inline keys, or the leaf has no head.
    pub(in crate::tree) fn replace_head_key(self, key: K) -> K {
        let slot = self.head_key_slot();
        self.assert_has_head();
        // SAFETY: the head's key slot is initialized (checked just above,
        // invariant 3), and the access is exclusive.
        unsafe { slot.replace(key) }
    }

    /// How the leaf right of a separator being removed gives up its head, the
    /// separator's entry, and hands up its first key to take the separator's
    /// place (D17). The first key's value, and with inline keys the key
    /// itself, become the new head (D49). `None`, changing nothing, if the
    /// leaf has no keys.
    ///
    /// # Panics
    ///
    /// If the leaf has no head.
    pub(in crate::tree) fn take_head_and_first_key(self) -> Option<HeadSuccession<K, V, S>> {
        self.assert_has_head();
        let len = self.len();
        if len == 0 {
            return None;
        }
        // SAFETY: `0 < len`, so key 0 is initialized, with its inline key; it
        // is read out, and keys `1..len` shift down. The head is initialized
        // (checked above, invariant 3), so head-and-value slots `0..=len` are.
        // Slot 0, the head, is read out and value 0 shifts into it, so the
        // leaf still has a head. With inline keys, the head's key slot is
        // initialized too, and is swapped for key 0. Keys and values
        // `0..len - 1` stay initialized; the access is exclusive.
        unsafe {
            let first_key = self.key_slots().remove(0, len);
            let value = self.head_and_value_slots().remove(0, len + 1);
            let (removed_key, successor) = if NodePtr::<K, V, S>::has_inline_keys() {
                let (first_key, window) = first_key.into_parts();
                let removed_key = self.head_key_slot().replace(first_key);
                (Some(removed_key), Separator::without_key(window))
            } else {
                (None, Separator::with_key(first_key))
            };
            self.set_len(len - 1);
            Some(HeadSuccession {
                removed_key,
                value,
                successor,
            })
        }
    }
}

/// Changing a leaf's entries.
impl<K, V, S: InlineKeyStrategy<K>> Leaf<K, V, S> {
    /// # Safety
    ///
    /// As for [`NodePtr::set_len_refilling`]: keys `0..len`, with their
    /// inline keys, and values `0..len` are initialized and valid at this
    /// call.
    unsafe fn set_len(self, len: usize) {
        // SAFETY: forwarded; the node is a leaf (invariant 1).
        unsafe { self.node.set_len_refilling::<Self>(len) }
    }

    /// Inserts `key`, with its inline key, and `value` at edge `index`,
    /// shifting later entries right. The leaf has room: `len < capacity`,
    /// and `index <= len`.
    #[inline]
    pub(in crate::tree) fn insert_fit(self, index: usize, key: WindowedKey<K, S>, value: V) {
        let len = self.len();
        debug_assert!(index <= len && len < Self::capacity());
        // SAFETY: `index <= len < capacity` (the caller). Keys (with their
        // inline keys) and values `0..len` are initialized, and `0..len + 1`
        // are afterwards. Paired, the entries from `index` on shift up whole,
        // and the new pair goes into the entry that frees (D29).
        unsafe {
            if NodePtr::<K, V, S>::has_paired_leaf_entries() {
                self.key_slots().insert_shifting_entries(index, len, key);
                self.value_slots().slot(index).write(value);
            } else {
                self.key_slots().insert(index, len, key);
                self.value_slots().insert(index, len, value);
            }
            // The slots past it already hold the greatest inline key.
            self.node.set_len(len + 1);
        }
    }

    /// Inserts `key`, with its inline key, and `value` at edge `index`,
    /// splitting the leaf first if it is full, with the right half in a page
    /// from `alloc`.
    #[cfg(test)]
    pub(in crate::tree) fn insert(
        self,
        index: usize,
        key: WindowedKey<K, S>,
        value: V,
        alloc: &impl PageAllocator,
    ) -> Insertion<K, S, Self> {
        if self.len() < Self::capacity() {
            self.insert_fit(index, key, value);
            Insertion::Fit(index)
        } else {
            let right_page = FreshPage::allocate(alloc);
            Insertion::Split(self.split_insert(index, key, value, right_page))
        }
    }

    /// Splits the full leaf and inserts `key`/`value` at edge `index`, on the
    /// side where that edge ended up (`InsertionSide::of_split`). The
    /// separator goes up, and its entry becomes the right leaf's head (D17,
    /// D49); the left leaf keeps its own head, if any. The right half goes
    /// into `right_page`.
    pub(in crate::tree) fn split_insert(
        self,
        index: usize,
        key: WindowedKey<K, S>,
        value: V,
        right_page: FreshPage<Self>,
    ) -> Split<K, S, Self> {
        debug_assert_eq!(self.len(), Self::capacity());
        let (separator_index, inserted) =
            InsertionSide::of_split(Self::capacity(), index, Self::min_len());
        let right = Self::new(right_page, self.prefix_len());
        let separator = self.split_around(separator_index, right);
        // A full leaf of `capacity` entries split around one leaves
        // `separator_index < capacity` on the left and `capacity -
        // separator_index - 1 < capacity` on the right. So both have room,
        // and the edge is within its half.
        match inserted {
            InsertionSide::Left(index) => self.insert_fit(index, key, value),
            InsertionSide::Right(index) => right.insert_fit(index, key, value),
        }
        Split {
            separator,
            right,
            inserted,
        }
    }

    /// Moves the entries right of `separator_index` into `right`, a fresh leaf
    /// at this leaf's prefix length. Takes out key `separator_index`, whose
    /// entry becomes `right`'s head (D49), and returns it as the parent's
    /// separator. Then the leaves' annexes split (`annexed_leaf.rs`).
    fn split_around(self, separator_index: usize, right: Self) -> Separator<K, S> {
        let len = self.len();
        debug_assert!(separator_index < len);
        let right_len = len - separator_index - 1;
        // SAFETY: `separator_index < len`. Keys `separator_index + 1..len` are
        // initialized and move, with their inline keys, to the start of
        // `right`, a fresh leaf of the same map at the same prefix length.
        // Head-and-value slots `separator_index + 1..=len`, the values of keys
        // `separator_index..len`, move to its slots `0..=right_len`, so the
        // separator's value becomes `right`'s head. Key `separator_index` is
        // then the last initialized key on the left, and is read out with its
        // inline key. With inline keys, the key goes into `right`'s head key
        // slot (D49), completing the head, which the header then flags
        // (invariant 3). The left leaf keeps `separator_index` keys and
        // values, and its head.
        unsafe {
            self.key_slots()
                .move_run_to(separator_index + 1, right_len, right.key_slots(), 0);
            self.head_and_value_slots().move_run_to(
                separator_index + 1,
                right_len + 1,
                right.head_and_value_slots(),
                0,
            );
            let key = self.key_slots().read(separator_index);
            let separator = if NodePtr::<K, V, S>::has_inline_keys() {
                let (key, window) = key.into_parts();
                right.head_key_slot().write(key);
                Separator::without_key(window)
            } else {
                Separator::with_key(key)
            };
            right.node.set_has_head(true);
            right.set_len(right_len);
            self.set_len(separator_index);
            self.split_annex(right);
            separator
        }
    }

    /// Removes key/value `index`, shifting later ones left, and returns it.
    ///
    /// # Panics
    ///
    /// If `index >= len`.
    pub(in crate::tree) fn remove(self, index: usize) -> (K, V) {
        let len = self.len();
        assert!(index < len, "entry index out of bounds");
        // SAFETY: `index < len`, and keys (with their inline keys) and values
        // `0..len` are initialized; afterwards `0..len - 1` are. Paired, key
        // and value `index` share an entry, which is read out before the
        // entries after it shift down whole (D29).
        unsafe {
            let (key, value) = if NodePtr::<K, V, S>::has_paired_leaf_entries() {
                let value = self.value_slots().slot(index).read();
                let key = self.key_slots().remove_shifting_entries(index, len);
                (key.into_key(), value)
            } else {
                let key = self.key_slots().remove(index, len);
                (key.into_key(), self.value_slots().remove(index, len))
            };
            self.set_len(len - 1);
            (key, value)
        }
    }

    /// Replaces key `index` and its inline key, returning the old key. The
    /// caller keeps the keys in order.
    ///
    /// # Panics
    ///
    /// If `index >= len`.
    pub(in crate::tree) fn replace_key(self, index: usize, key: WindowedKey<K, S>) -> K {
        assert!(index < self.len(), "key index out of bounds");
        // SAFETY: `index < len`, so key `index` is initialized, with its
        // inline key, and the access is exclusive.
        unsafe { self.key_slots().replace(index, key).into_key() }
    }

    /// Replaces key `index`, its inline key, and its value, returning the
    /// old key and value. The caller keeps the keys in order.
    ///
    /// # Panics
    ///
    /// If `index >= len`.
    #[cfg(test)]
    pub(in crate::tree) fn replace_key_value(
        self,
        index: usize,
        key: WindowedKey<K, S>,
        value: V,
    ) -> (K, V) {
        assert!(index < self.len(), "key index out of bounds");
        // SAFETY: `index < len`, so key and value `index` are initialized,
        // with the key's inline key, and the access is exclusive.
        unsafe {
            (
                self.key_slots().replace(index, key).into_key(),
                self.value_slots().slot(index).replace(value),
            )
        }
    }

    /// Cuts the leaf at edge `index`: moves the entries from `index` on, with
    /// their inline keys, into a new leaf in `page`, without a head, and
    /// returns it. This leaf keeps the entries before `index`, and its head.
    /// Either may be left with fewer than its minimum, down to none.
    ///
    /// # Panics
    ///
    /// If `index > len`; the leaf is unchanged then.
    pub(in crate::tree) fn cut_off_at(self, index: usize, page: FreshPage<Self>) -> Self {
        let len = self.len();
        assert!(index <= len, "cutting a leaf past its last edge");
        let count = len - index;
        let right = Self::new(page, self.prefix_len());
        // SAFETY: `index <= len`, so keys (with their inline keys) and values
        // `index..len` are initialized. They move to `0..count` of `right`, a
        // fresh leaf of the same map, at this leaf's prefix length. The
        // access is exclusive, and nothing else reaches `right` yet.
        // Afterwards this leaf holds `0..index` and its head, and `right`
        // `0..count` and no head.
        unsafe {
            self.key_slots()
                .move_run_to(index, count, right.key_slots(), 0);
            self.value_slots()
                .move_run_to(index, count, right.value_slots(), 0);
            right.set_len(count);
            self.set_len(index);
        }
        right
    }
}

/// Moving a key through the parent's separator, for a steal.
impl<K, V, S: InlineKeyStrategy<K>> Leaf<K, V, S> {
    /// Puts `key`, which is leaving a sibling, in place of the key of the
    /// parent's separator `index`, and returns the old key with `fill` as its
    /// window, at the parent's prefix length. This leaf is the leftmost leaf
    /// right of that separator. With inline keys (D49), `key` replaces this
    /// leaf's head key and the parent keeps only its window; otherwise the
    /// parent keeps `key`.
    ///
    /// # Safety
    ///
    /// This leaf has a head, and `key`'s window is at a prefix length no
    /// shorter than `fill`'s.
    unsafe fn replace_separator_key(
        self,
        parent: Internal<K, V, S>,
        index: usize,
        key: WindowedKey<K, S>,
        fill: Window,
    ) -> WindowedKey<K, S> {
        if NodePtr::<K, V, S>::has_inline_keys() {
            let (key, window) = key.into_parts();
            // SAFETY: the leaf has a head (caller), so its head key slot is
            // initialized (invariant 3).
            let old_head_key = unsafe { self.head_key_slot().replace(key) };
            parent.replace_separator(index, Separator::without_key(window.moved_back_to(fill)));
            WindowedKey::from_parts(old_head_key, fill)
        } else {
            parent
                .replace_separator(index, Separator::with_key(key.moved_back_to(fill)))
                .into_windowed_key()
                .expect("a separator of a node that keeps keys is a key")
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Child<K, V, S> for Leaf<K, V, S> {
    fn child_of(parent: Internal<K, V, S>, index: usize) -> Self {
        parent.leaf_child(index)
    }

    fn node_ptr(self) -> NodePtr<K, V, S> {
        self.node
    }

    fn len(self) -> usize {
        Leaf::len(self)
    }

    #[inline(always)]
    fn capacity() -> usize {
        Leaf::<K, V, S>::capacity()
    }

    #[inline(always)]
    fn plan() -> NodePlan<K> {
        Leaf::<K, V, S>::plan()
    }

    fn prefix_len(self) -> usize {
        Leaf::prefix_len(self)
    }

    fn has_first_trailing_slot(self) -> bool {
        self.has_head()
    }

    unsafe fn free_page(self) -> PageByType<K, V, S> {
        // SAFETY: forwarded.
        unsafe { self.free() }.into()
    }

    unsafe fn steal_left_unchecked(siblings: &Siblings<K, V, S, Self>, fill: Window) {
        let (parent, index) = (siblings.parent(), siblings.index());
        let (left, right) = (siblings.left(), siblings.right());
        let (left_len, right_len) = (left.len(), right.len());
        // SAFETY: the left leaf has key and value `left_len - 1` to give, and
        // the right leaf room for one more (caller). Both are leaves of the
        // parent's map, and the parent's separator is initialized. The right
        // leaf's head-and-value slots `0..=right_len` are initialized (the
        // siblings' invariant), so inserting at slot 0 shifts them up. The old
        // head becomes the value of the new key 0, and the left leaf's last
        // value becomes the head. The new key 0 is the old separator, at the
        // right leaf's prefix length, `fill`'s (caller). With inline keys
        // (D49), it comes from the head key slot, which the left leaf's last
        // key takes, and the parent keeps only that key's window.
        unsafe {
            let key = left.key_slots().remove(left_len - 1, left_len);
            let value = left.head_and_value_slots().remove(left_len, left_len + 1);
            right.head_and_value_slots().insert(0, right_len + 1, value);
            let separator = right.replace_separator_key(parent, index, key, fill);
            right.key_slots().insert(0, right_len, separator);
            left.set_len(left_len - 1);
            right.set_len(right_len + 1);
        }
    }

    unsafe fn steal_right_unchecked(siblings: &Siblings<K, V, S, Self>, fill: Window) {
        let (parent, index) = (siblings.parent(), siblings.index());
        let (left, right) = (siblings.left(), siblings.right());
        let (left_len, right_len) = (left.len(), right.len());
        // SAFETY: as in `steal_left_unchecked`, mirrored. The right leaf gives
        // key 0 and head-and-value slot 0, its head; value 0 then shifts into
        // the head. The left leaf has room at key `left_len` and head-and-value
        // slot `left_len + 1`. The key going there is the old separator, at
        // the left leaf's prefix length, `fill`'s. With inline keys, it comes
        // from the right leaf's head key slot, which the right leaf's old key 0
        // takes.
        unsafe {
            let key = right.key_slots().remove(0, right_len);
            let value = right.head_and_value_slots().remove(0, right_len + 1);
            left.head_and_value_slots()
                .insert(left_len + 1, left_len + 1, value);
            let separator = right.replace_separator_key(parent, index, key, fill);
            left.key_slots().insert(left_len, left_len, separator);
            left.set_len(left_len + 1);
            right.set_len(right_len - 1);
        }
    }

    unsafe fn append(left: Self, fill: Window, separator: Separator<K, S>, right: Self) {
        let (left_len, right_len) = (left.len(), right.len());
        // SAFETY: `left_len + 1 + right_len <= capacity` (caller). The
        // separator goes into key slot `left_len`, at the leaves' prefix
        // length, `fill`'s. With inline keys (D49), it is the right leaf's head
        // key, with `fill` as its window, holding every byte a leaf keeps of
        // it (caller). `right`'s keys `0..right_len` go into `left_len + 1..`.
        // Its head-and-value slots `0..=right_len`, all initialized (caller),
        // go into `left_len + 1..=left_len + 1 + right_len`, so its head lands
        // beside the separator as its value. `left`'s head is untouched.
        unsafe {
            let separator = if NodePtr::<K, V, S>::has_inline_keys() {
                WindowedKey::from_parts(right.head_key_slot().read(), fill)
            } else {
                separator
                    .into_windowed_key()
                    .expect("a separator into a node that keeps keys is a key")
            };
            left.key_slots().write(left_len, separator);
            right
                .key_slots()
                .move_run_to(0, right_len, left.key_slots(), left_len + 1);
            right.head_and_value_slots().move_run_to(
                0,
                right_len + 1,
                left.head_and_value_slots(),
                left_len + 1,
            );
            left.set_len(left_len + 1 + right_len);
        }
        left.merge_annex(right);
    }

    fn key<'a>(self, index: usize) -> &'a K {
        Leaf::key(self, index)
    }

    fn inline_keys<'a>(self) -> Option<NodeInlineKeys<'a>> {
        Leaf::inline_keys(self)
    }

    #[cfg(any(test, fuzzing))]
    fn hash_tags<'a>(self) -> Option<&'a [u8]> {
        Leaf::hash_tags(self)
    }

    /// As far as its fences share: nothing below a leaf bounds it.
    fn allowed_prefix_len(self, prefix_len: usize) -> usize {
        prefix_len
    }
}

/// A validated interval of one leaf's entries, which a walk hands out
/// without going back to the leaf. Which representation it uses depends
/// only on the map's layout. With paired entries, `front` and `back` are key
/// pointers, the next keys at either end, so advancing needs neither the
/// leaf pointer nor an index multiply. Otherwise `front` is the leaf's first
/// key, the interval is `start..end`, and `back` is unused.
///
/// Invariant: the run's leaf stays allocated with its slot positions
/// unchanged while the run is used, which the walk that holds it sees to.
/// Pointers preserve the page's provenance, including its key end.
pub(in crate::tree) struct LeafRun<K, V, S> {
    front: NonNull<K>,
    back: NonNull<K>,
    start: usize,
    end: usize,
    _leaf: PhantomData<Leaf<K, V, S>>,
}

impl<K, V, S> Clone for LeafRun<K, V, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K, V, S> Copy for LeafRun<K, V, S> {}

impl<K, V, S: InlineKeyStrategy<K>> LeafRun<K, V, S> {
    /// Whether the run walks key pointers: in a leaf of paired entries.
    fn uses_pointers() -> bool {
        NodePtr::<K, V, S>::has_paired_leaf_entries()
    }

    /// The entries `range` of `leaf`.
    ///
    /// # Panics
    ///
    /// If `range` is not within `0..len`.
    pub(in crate::tree) fn new(leaf: Leaf<K, V, S>, range: Range<usize>) -> Self {
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
            _leaf: PhantomData,
        }
    }

    pub(in crate::tree) fn next(&mut self) -> Option<EntrySlots<K, V>> {
        if Self::uses_pointers() {
            if self.front == self.back {
                return None;
            }
            let key = self.front;
            // SAFETY: the nonempty interval contains this key; its next
            // key pointer is at most the validated end of the run.
            unsafe {
                self.front = key.byte_add(NodePtr::<K, V, S>::leaf_layout().keys.key_stride);
                return Some(Self::entry_slots_at(key, 0));
            }
        }
        if self.start == self.end {
            return None;
        }
        let index = self.start;
        self.start += 1;
        // SAFETY: the nonempty, validated interval contains this index.
        Some(unsafe { Self::entry_slots_at(self.front, index) })
    }

    pub(in crate::tree) fn next_back(&mut self) -> Option<EntrySlots<K, V>> {
        if Self::uses_pointers() {
            if self.front == self.back {
                return None;
            }
            // SAFETY: the nonempty interval contains the preceding key.
            unsafe {
                self.back = self
                    .back
                    .byte_sub(NodePtr::<K, V, S>::leaf_layout().keys.key_stride);
                return Some(Self::entry_slots_at(self.back, 0));
            }
        }
        if self.start == self.end {
            return None;
        }
        self.end -= 1;
        // SAFETY: the nonempty, validated interval contains this index.
        Some(unsafe { Self::entry_slots_at(self.front, self.end) })
    }

    /// Where entry `index` from `origin` is: `origin` is the entry's own
    /// key with `index` 0 when the run walks pointers, or the leaf's first
    /// key otherwise.
    ///
    /// # Safety
    ///
    /// `origin` is a key slot of a leaf of this map's layout, and entry
    /// `index` from it is within that leaf's capacity.
    unsafe fn entry_slots_at(origin: NonNull<K>, index: usize) -> EntrySlots<K, V> {
        let layout = NodePtr::<K, V, S>::leaf_layout();
        let value_offset = layout.values_offset() - layout.keys.keys_offset;
        // SAFETY: the caller supplies a slot in the leaf. Keys and values
        // follow these aligned strides and never overlap. In paired mode they
        // use the same stride, so the value is at a fixed offset from this
        // key.
        unsafe {
            EntrySlots {
                key: origin.byte_add(index * layout.keys.key_stride),
                value: origin
                    .cast::<u8>()
                    .byte_add(value_offset + index * layout.value_stride)
                    .cast(),
            }
        }
    }

    /// The edge the run has reached in `leaf`, from the front, or from the
    /// back if `reverse`.
    ///
    /// # Safety
    ///
    /// `leaf` is the leaf this run was made from, kept allocated with
    /// unchanged slot positions.
    pub(in crate::tree) unsafe fn edge_index(&self, leaf: Leaf<K, V, S>, reverse: bool) -> usize {
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

    /// The keys left in the run, which it moves past, staying in its leaf
    /// with none left: for a loop to visit them without the walk.
    ///
    /// # Safety
    ///
    /// The run's keys stay initialized and unchanged for `'a`.
    pub(in crate::tree) unsafe fn take_keys<'a>(&mut self) -> KeyRun<'a, K> {
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
        // SAFETY: the run's keys are initialized and shared for `'a`
        // (caller), `len` of them from `next`, `stride` bytes apart (the
        // leaf's layout).
        unsafe { KeyRun::new(next, stride, len) }
    }
}

/// Keys of one leaf, in order, that a walk has moved past, for a loop to
/// visit without the walk. The loop touches the walk once per run rather
/// than once per key (as in a merge of two sets). Copied into a local, the
/// run lives in registers.
///
/// Invariant: `remaining` keys from `next`, `stride` bytes apart, are
/// initialized and shared for `'a`.
pub(crate) struct KeyRun<'a, K> {
    next: *const K,
    stride: usize,
    remaining: usize,
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
        remaining: 0,
        _borrow: PhantomData,
    };

    /// # Safety
    ///
    /// The type invariant holds for `next`, `stride` and `len`.
    unsafe fn new(next: NonNull<K>, stride: usize, len: usize) -> Self {
        Self {
            next: next.as_ptr(),
            stride,
            remaining: len,
            _borrow: PhantomData,
        }
    }

    /// `key` alone.
    pub(in crate::tree) fn one(key: &'a K) -> Self {
        // SAFETY: one key, which `key` keeps shared for `'a`.
        unsafe { Self::new(NonNull::from(key), 0, 1) }
    }

    /// How many keys are left.
    pub(crate) fn len(&self) -> usize {
        self.remaining
    }

    /// The next key, staying before it.
    #[inline]
    pub(crate) fn peek(&self) -> Option<&'a K> {
        // SAFETY: with a key left, `next` is that key (type invariant).
        (self.remaining > 0).then(|| unsafe { &*self.next })
    }

    /// Moves past the next key, if any.
    #[inline]
    pub(crate) fn advance(&mut self) {
        if self.remaining > 0 {
            self.remaining -= 1;
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
        (self.remaining, Some(self.remaining))
    }
}
