//! Leaves: the nodes that hold the entries. A leaf's page holds its keys,
//! each paired with its value where the keys have inline keys (D29), the
//! inline keys a search runs over first (D18), and in a leaf of heap byte
//! strings, the keys' length tags (D36). It also holds a head: the value of
//! the separator just left of the leaf, and where separator keys are heads,
//! that separator's key (D17, D49). Every leaf but the tree's leftmost has a
//! head.
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
use super::inline_key_search::NodeInlineKeys;
use super::inline_keys::{InlineTarget, SearchKey, Separator, Window, WindowedKey};
use super::internal::Internal;
use super::length_tags::{LengthTags, SlotMatches};
use super::node_plan::NodePlan;
use super::page::{FreshPage, NodeKind, NodePtr, PageByType, PageType};
use super::siblings::Siblings;
use super::slots::{KeyColumns, KeySlots, NodeKeys, Slots};
use super::{Child, EntrySlots, HeadSuccession, InsertionSide, Split};
use crate::inline_key::{InlineKeyStrategy, InlineWord};

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
    fn page_layout() -> Layout {
        NodePtr::<K, V, S>::leaf_layout().page
    }
}

// SAFETY: as for `PageType`; leaves are leaves.
unsafe impl<K, V, S: InlineKeyStrategy<K>> NodeKind for Leaf<K, V, S> {
    const IS_LEAF: bool = true;
}

impl<K, V, S> Leaf<K, V, S> {
    /// The leaf at `node`, which must hold one.
    #[inline(always)]
    pub(in crate::tree) fn at(node: NodePtr<K, V, S>) -> Self {
        debug_assert!(node.is_leaf(), "the node is internal");
        Self { node }
    }

    /// The leaf's page, for a path or an edge to keep.
    #[inline(always)]
    pub(in crate::tree) fn node_ptr(self) -> NodePtr<K, V, S> {
        self.node
    }

    /// The number of entries.
    #[inline(always)]
    pub(crate) fn len(self) -> usize {
        // SAFETY: the node is allocated and initialized (invariant 1), and
        // nothing writes its header meanwhile (the handles' contract).
        unsafe { self.node.len() }
    }

    /// Whether the leaf has a head: in a tree built by the node operations,
    /// every leaf but the leftmost has one (D17).
    pub(in crate::tree) fn has_head(self) -> bool {
        // SAFETY: the node is allocated and its header written (invariant 1).
        unsafe { self.node.has_head() }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Leaf<K, V, S> {
    /// A new, empty leaf in `page`, without a head.
    pub(in crate::tree) fn new(page: FreshPage<Self>) -> Self {
        Self::at(NodePtr::from_fresh_page(page))
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

    /// Panics if the leaf has no head.
    fn assert_has_head(self) {
        assert!(self.has_head(), "the leaf has no head");
    }

    /// The plan of the map's leaves (`node_plan.rs`).
    #[inline(always)]
    pub(in crate::tree) fn plan() -> NodePlan<K> {
        NodePlan::of::<S, Self>()
    }

    /// The most entries a leaf holds.
    #[inline(always)]
    pub(crate) fn capacity() -> usize {
        NodePtr::<K, V, S>::capacity::<Self>()
    }

    /// Whether the leaf holds fewer entries than [`Child::min_len`].
    #[cfg(test)]
    pub(in crate::tree) fn is_underfull(self) -> bool {
        self.len() < <Self as Child<K, V, S>>::min_len()
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

    /// The arrays beside the keys: inline keys and length tags.
    #[inline(always)]
    fn columns(self) -> KeyColumns {
        // SAFETY: the node is an allocated leaf (invariant 1).
        unsafe { self.node.key_columns::<Self>() }
    }

    fn value_slots(self) -> Slots<V> {
        // SAFETY: the node is an allocated leaf (invariant 1).
        unsafe { self.node.value_slots() }
    }

    /// The head key slot, where the key of the separator left of the leaf
    /// is, if it has a head, in a map whose separator keys are heads (D49).
    ///
    /// # Panics
    ///
    /// If separator keys aren't heads in this map.
    pub(super) fn head_key_slot(self) -> NonNull<K> {
        assert!(
            NodePtr::<K, V, S>::has_separator_keys_in_heads(),
            "separator keys are heads only with inline keys"
        );
        // SAFETY: the node is an allocated leaf (invariant 1), in a map whose
        // separator keys are heads (checked just above).
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

    /// Key `index`.
    ///
    /// # Panics
    ///
    /// If `index >= len`.
    pub(in crate::tree) fn key<'a>(self, index: usize) -> &'a K {
        assert!(index < self.len(), "key index out of bounds");
        // SAFETY: checked just above.
        unsafe { self.key_unchecked(index) }
    }

    /// Key `index`, with `index < len`, unchecked.
    ///
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

    /// Key `index` and its value.
    ///
    /// # Panics
    ///
    /// If `index >= len`.
    pub(in crate::tree) fn key_value<'a>(self, index: usize) -> (&'a K, &'a V) {
        assert!(index < self.len(), "entry index out of bounds");
        // SAFETY: `index < len`, checked just above.
        unsafe { self.key_value_unchecked(index) }
    }

    /// Key `index` and its value, with `index < len`, unchecked.
    ///
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

    /// Value `index`, exclusive, with `index < len`, unchecked.
    ///
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

    /// Value `index`, exclusive.
    ///
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

    /// The keys' length tags, if the plan and layout keep them (D36): all
    /// `capacity` slots, tag `i` key `i`'s for `i < len`.
    #[inline(always)]
    pub(in crate::tree) fn length_tags<'a>(self) -> Option<&'a [u16]> {
        let tags = self.columns().length_tags()?;
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
    /// `key`'s inline key at this leaf's prefix length, if the leaf has
    /// inline keys and `key` one to compare with them (D18).
    ///
    /// Each search takes it where it uses it, rather than having it handed
    /// in: handed across a call, it went through the stack, written right
    /// before the search read it back, a store the CPU can't always
    /// forward, which stalled every level. Std byte strings read the
    /// window directly from the search key.
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

    /// [`Self::search`], always inlined, whatever the plan says: for an
    /// insert's search (`Node::search_for_insert`), which, unlike a lookup,
    /// gains more from its leaf search inline than it loses in code size
    /// (`NodePlan::inlines_node_search`). For std heap byte strings, the
    /// search out of line cost a random insert 40 instructions more.
    ///
    /// If the leaf has inline keys and `key` has one to compare (D18), taken
    /// at the leaf's prefix length, the search runs over them; otherwise over
    /// the keys, as the leaf's plan says (D19). The key shares the leaf's
    /// prefix: it lies between the leaf's fences, or within what every key
    /// of the tree shares, which a search checks at the top (D50). After
    /// monomorphization, a map without inline keys keeps only the search
    /// over its keys.
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
            // A key the leaf lacks, as most an insert searches for, mostly
            // has none: it goes no further than the inline keys, nor into a
            // call.
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
    /// key searched for; the others differ in their inline keys. The count
    /// over the inline keys runs over the leaf's capacity, a constant, which
    /// the compiler unrolls; over a capacity chosen at run time, it
    /// compiled to a loop of several times the instructions, and over two
    /// capacities, a leaf's and an internal node's, to one that summed the
    /// compares' bits one at a time. Keep this small typed view in its
    /// callers: outlining shared uses also made LLVM expand 64-bit summary
    /// counts into scalar lane-bit sums.
    #[inline(always)]
    fn candidates(self, target: InlineTarget) -> Option<Range<usize>> {
        Some(self.inline_keys()?.equal_range(target.inline_key()))
    }

    /// The search among `candidates`, the keys whose inline keys equal
    /// `key`'s (D18). The other keys differ from `key` in their inline keys,
    /// which order them (the `InlineKey` contract), so if `key` is none of
    /// the candidates, its edge is where they start, even when there are
    /// none.
    ///
    /// It runs as the leaf's plan says. Keys with their own `InlineKey`
    /// impl compare by `Ord`, and search as their type picks (D19):
    /// branchless for plain data such as integers, whose candidates are the
    /// whole leaf where their kept bytes all tie. Std byte strings search
    /// them with an early exit, comparing as [`SearchKey::cmp_tied`] does, a
    /// word at a time from past the bytes the candidates share.
    ///
    /// In bounds for any `Ord` and `InlineKey`: the candidates lie within
    /// `0..len` whatever the inline keys hold
    /// ([`NodeInlineKeys::equal_range`]), taking it out of the keys checks
    /// that, and either search stays within it.
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
            self.keys().slice(candidates.clone()),
            key.key(),
            |stored: &K| key.cmp_tied::<Self>(target, stored),
        );
        found
            .map(|index| candidates.start + index)
            .map_err(|index| candidates.start + index)
    }

    /// The index of the key equal to `key`, if the leaf holds one: an exact-key
    /// lookup, which may reject unequal lengths among the candidates (D36) and
    /// confirm a lone candidate by equality alone (D38). Ordered searches
    /// still use [`Self::search`]: a length tag gives no edge.
    pub(in crate::tree) fn find<Q>(self, key: &SearchKey<'_, K, S, Q>) -> Option<usize>
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
            } else if let Some(tag) = key.length_tag()
                && let Some(tags) = self.length_tags()
            {
                match LengthTags::new(tags).matching(candidates.clone(), tag) {
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
    /// The head's value.
    ///
    /// # Panics
    ///
    /// If the leaf has no head, which only the tree's leftmost has.
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

    /// The head's key, the key of the separator left of the leaf, where
    /// separator keys are heads (D49).
    ///
    /// # Panics
    ///
    /// If they aren't, in this map, or the leaf has no head.
    pub(in crate::tree) fn head_key<'a>(self) -> &'a K {
        let slot = self.head_key_slot();
        self.assert_has_head();
        // SAFETY: separator keys are heads (checked by `head_key_slot`) and
        // the leaf has a head (checked just above), so the head's key slot
        // is initialized (invariant 3) and stays unchanged while the caller
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

    /// Gives the leaf, which has no head, `value` as its head, the value of
    /// the separator left of it, and `key`, that separator's key, where
    /// separator keys are heads (D49); `key` is `None` where internal nodes
    /// keep them. Keeping heads where separators expect them is the caller's
    /// business.
    ///
    /// # Panics
    ///
    /// If the leaf has a head, or if `key` is not what this map keeps in
    /// heads; the leaf is unchanged then.
    pub(in crate::tree) fn put_head(self, key: Option<K>, value: V) {
        assert!(!self.has_head(), "the leaf has a head");
        assert_eq!(
            key.is_some(),
            NodePtr::<K, V, S>::has_separator_keys_in_heads(),
            "a head's key goes where the map keeps separator keys"
        );
        // SAFETY: the access is exclusive (the handles' contract), and the
        // leaf has no head (checked above), so its head slot, and its head
        // key slot where separator keys are heads (also checked), hold
        // nothing: written, then flagged (invariant 3).
        unsafe {
            if let Some(key) = key {
                self.head_key_slot().write(key);
            }
            self.head_and_value_slots().slot(0).write(value);
            self.node.set_has_head(true);
        }
    }

    /// Gives the leaf, which has no head, the head of `separator`, the
    /// separator just left of it: `value`, and where separator keys are
    /// heads, the key (D49). Returns the separator as its internal node takes
    /// it: the key where internal nodes keep keys, or its window.
    ///
    /// # Panics
    ///
    /// If the leaf has a head; it is unchanged then.
    pub(in crate::tree) fn put_separator_head(
        self,
        separator: WindowedKey<K, S>,
        value: V,
    ) -> Separator<K, S> {
        if NodePtr::<K, V, S>::has_separator_keys_in_heads() {
            let (key, window) = separator.into_parts();
            self.put_head(Some(key), value);
            Separator::without_key(window)
        } else {
            self.put_head(None, value);
            Separator::with_key(separator)
        }
    }

    /// Takes the head out of the leaf, which then has none, and returns it,
    /// with its key where separator keys are heads (D49).
    ///
    /// # Panics
    ///
    /// If the leaf has no head.
    pub(in crate::tree) fn take_head(self) -> (Option<K>, V) {
        self.assert_has_head();
        // SAFETY: the head is initialized, and so is its key where separator
        // keys are heads (invariant 3); both are read out, and the header
        // then says the leaf has none; the access is exclusive.
        unsafe {
            let key = NodePtr::<K, V, S>::has_separator_keys_in_heads()
                .then(|| self.head_key_slot().read());
            let value = self.head_and_value_slots().slot(0).read();
            self.node.set_has_head(false);
            (key, value)
        }
    }

    /// Puts `key` in place of the head's key, where separator keys are heads
    /// (D49), and returns the old one. Keeping the keys in order is the
    /// caller's business.
    ///
    /// # Panics
    ///
    /// If separator keys aren't heads in this map, or the leaf has no head.
    pub(in crate::tree) fn replace_head_key(self, key: K) -> K {
        let slot = self.head_key_slot();
        self.assert_has_head();
        // SAFETY: the head's key slot is initialized (checked just above,
        // invariant 3), and the access is exclusive.
        unsafe { slot.replace(key) }
    }

    /// Takes out the head and the first key, whose value, and where
    /// separator keys are heads, the key itself, become the new head (D49),
    /// and returns what the separator being removed gives up and what takes
    /// its place; `None`, changing nothing, if the leaf has no keys. This is
    /// how the leaf right of a separator being removed gives up the
    /// separator's value, its head, and hands up the next key, to take the
    /// separator's place (D17).
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
        // SAFETY: `0 < len`, so key 0 is initialized, with its inline key,
        // and is read out, shifting keys `1..len` down. The head is
        // initialized (checked above, invariant 3), so head-and-value slots
        // `0..=len` are: slot 0, the head, is read out, and value 0 shifts
        // into it, so the leaf still has a head. Where separator keys are
        // heads, so is the head's key slot, swapped for key 0. Keys and
        // values `0..len - 1` stay initialized; the access is exclusive.
        unsafe {
            let first_key = self.key_slots().remove(0, len);
            let value = self.head_and_value_slots().remove(0, len + 1);
            let (removed_key, successor) = if NodePtr::<K, V, S>::has_separator_keys_in_heads() {
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
    /// Raises the length to `len`, for entries just written past it: the
    /// slots past `len` already hold the greatest inline key (invariant 4),
    /// so none needs refilling.
    ///
    /// # Safety
    ///
    /// `self.len() <= len <= capacity`, and all keys, inline keys and
    /// values described by `len` are initialized, as for `set_len`.
    unsafe fn grow_len(self, len: usize) {
        debug_assert!(self.len() <= len && len <= Self::capacity());
        // SAFETY: the node is allocated and the access exclusive; the length
        // and initialization requirements are forwarded. Growing preserves
        // the untouched inline-key padding.
        unsafe { self.node.set_len(len) }
    }

    /// Sets the length; the inline-key slots a shorter length leaves get the
    /// greatest inline key, as all slots past the length hold (invariant
    /// 4). Called once the slots are where the length says.
    ///
    /// # Safety
    ///
    /// `len` is at most the capacity, and keys `0..len`, with their inline
    /// keys, and values `0..len` are initialized and valid at this call.
    unsafe fn set_len(self, len: usize) {
        debug_assert!(len <= Self::capacity());
        let old_len = self.len();
        // SAFETY: `old_len <= capacity`, and the access is exclusive.
        unsafe { self.columns().mark_vacant(len..old_len) };
        // SAFETY: the node is allocated and the access exclusive; `len <=
        // capacity` is forwarded.
        unsafe { self.node.set_len(len) }
    }

    /// Inserts `key`, with its inline key, and `value` at edge `index`,
    /// shifting later entries right. The leaf has room: `len < capacity`,
    /// and `index <= len`.
    #[inline]
    pub(in crate::tree) fn insert_fit(self, index: usize, key: WindowedKey<K, S>, value: V) {
        let len = self.len();
        debug_assert!(index <= len && len < Self::capacity());
        // SAFETY: `index <= len < capacity` (the caller); keys (with their
        // inline keys) and values `0..len` are initialized, and `0..len + 1`
        // are afterwards. Paired, the entries from key and value `index` on
        // shift up whole, and the new pair goes into the entry that frees
        // (D29).
        unsafe {
            if NodePtr::<K, V, S>::has_paired_leaf_entries() {
                self.key_slots().insert_shifting_entries(index, len, key);
                self.value_slots().slot(index).write(value);
            } else {
                self.key_slots().insert(index, len, key);
                self.value_slots().insert(index, len, value);
            }
            self.grow_len(len + 1);
        }
    }

    /// Inserts `key`, with its inline key, and `value` at edge `index`,
    /// splitting the leaf first if it is full, with the right half in a page
    /// from `new_page`.
    #[cfg(test)]
    pub(in crate::tree) fn insert(
        self,
        index: usize,
        key: WindowedKey<K, S>,
        value: V,
        new_page: impl FnOnce() -> FreshPage<Self>,
    ) -> Insertion<K, S, Self> {
        if self.len() < Self::capacity() {
            self.insert_fit(index, key, value);
            Insertion::Fit(index)
        } else {
            Insertion::Split(self.split_insert(index, key, value, new_page()))
        }
    }

    /// Splits the full leaf and inserts `key`/`value` at edge `index`, on
    /// the side where that edge ended up (`InsertionSide::of_split`). The
    /// separator goes up; its value becomes the right leaf's head, with its
    /// key where separator keys are heads (D49), and the left leaf keeps its
    /// own head, if any (D17). The right half goes into `right_page`.
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
        let right = Self::new(right_page);
        // SAFETY: `right` is a fresh leaf of the same map, reached by nothing
        // else, at this leaf's prefix length once set here.
        unsafe { right.node.set_prefix_len(self.prefix_len()) };
        let separator = self.split_around(separator_index, right);
        // A full leaf of `capacity` entries split around one leaves
        // `separator_index < capacity` on the left and `capacity -
        // separator_index - 1 < capacity` on the right, so both have room,
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

    /// Moves the entries right of `separator_index` into `right`, a fresh
    /// leaf at this leaf's prefix length, and takes out key
    /// `separator_index`, for the parent; returns it. Its value becomes
    /// `right`'s head, with its key where separator keys are heads (D49).
    fn split_around(self, separator_index: usize, right: Self) -> Separator<K, S> {
        let len = self.len();
        debug_assert!(separator_index < len);
        let right_len = len - separator_index - 1;
        // SAFETY: `separator_index < len`. Keys `separator_index + 1..len`
        // are initialized and move, with their inline keys, to the start of
        // `right`, a fresh leaf of the same map at the same prefix length;
        // so do head-and-value slots `separator_index + 1..=len`, the values
        // of keys `separator_index..len`, to slots `0..=right_len`, so that
        // the separator's value becomes the right leaf's head. Key
        // `separator_index` is then the last initialized one on the left and
        // is read out, with its inline key: where separator keys are heads,
        // into the right leaf's head key slot (D49), so that the head is
        // whole, which the header then flags (invariant 3). The left leaf
        // keeps `separator_index` keys and values and its head.
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
            let separator = if NodePtr::<K, V, S>::has_separator_keys_in_heads() {
                let (key, window) = key.into_parts();
                right.head_key_slot().write(key);
                Separator::without_key(window)
            } else {
                Separator::with_key(key)
            };
            right.node.set_has_head(true);
            right.set_len(right_len);
            self.set_len(separator_index);
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
        // and value `index` share an entry, read out before the entries after
        // it shift down whole (D29).
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

    /// Replaces key `index` and its inline key, returning the old key.
    /// Keeping the keys in order is the caller's business.
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
    /// old key and value. Keeping the keys in order is the caller's
    /// business.
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
        let right = Self::new(page);
        // SAFETY: `index <= len`, so keys (with their inline keys) and values
        // `index..len` are initialized; they move to `0..count` of `right`, a
        // fresh leaf of the same map, at this leaf's prefix length. The
        // access is exclusive, and `right` is reached by nothing else yet.
        // Afterwards this leaf holds `0..index` and its head, and `right`
        // `0..count` and none.
        unsafe {
            right.node.set_prefix_len(self.prefix_len());
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

/// A leaf's prefix (05-fence-key.md).
impl<K, V, S: InlineKeyStrategy<K>> Leaf<K, V, S> {
    /// Moves the leaf's prefix length back to `fill`'s, and every inline key
    /// with it ([`Window::moved_back_to`]), given `fill`, the window there of
    /// a key whose encoding shares the leaf's prefix: one of its fences, one
    /// of its keys, or what every key of the tree shares (D50). Reads no key
    /// and invokes no user code. Does nothing if the prefix is no longer
    /// than `fill`'s, which includes every leaf without inline keys.
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

    /// Makes `inline_keys` the leaf's keys' inline keys, in order, which
    /// must be their windows at `prefix_len`, and then `prefix_len` its
    /// prefix length: how a prefix is refit (`tree/prefix.rs`). Does nothing
    /// to a leaf without inline keys.
    ///
    /// # Panics
    ///
    /// If `inline_keys` does not say it has one inline key per key; the leaf
    /// is unchanged then.
    pub(in crate::tree) fn set_windows(
        self,
        prefix_len: usize,
        inline_keys: impl ExactSizeIterator<Item = InlineWord>,
    ) {
        let len = self.len();
        assert_eq!(inline_keys.len(), len, "one inline key per key");
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

    /// Sets the prefix length of a leaf with no keys, which any prefix length
    /// fits: how a copied leaf, or a new root, starts.
    ///
    /// # Panics
    ///
    /// If the leaf has keys.
    pub(in crate::tree) fn set_empty_prefix_len(self, prefix_len: usize) {
        assert_eq!(self.len(), 0, "a leaf with keys keeps their prefix length");
        // SAFETY: the node is allocated and the access exclusive.
        unsafe { self.node.set_prefix_len(prefix_len) }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Leaf<K, V, S> {
    /// Puts `key`, leaving a sibling, in place of the key of the parent's
    /// separator `index`, of which this leaf is the right child's leftmost
    /// leaf, and returns that separator's key, with its window at `fill`,
    /// the parent's: where separator keys are heads (D49), `key` takes this
    /// leaf's head key, and the parent keeps its window alone; otherwise the
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
        if NodePtr::<K, V, S>::has_separator_keys_in_heads() {
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

    fn move_prefix_back_to(self, fill: Window) {
        Leaf::move_prefix_back_to(self, fill);
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
        // siblings' invariant), so inserting at slot 0 shifts them up: the
        // old head becomes the value of the key now at key 0, and the left
        // leaf's last value the head. The key going to key 0 is the old
        // separator, at the right leaf's prefix length, `fill`'s (caller);
        // where separator keys are heads (D49), it comes from the head key
        // slot, which the left leaf's last key takes, and the parent keeps
        // that key's window alone.
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
        // SAFETY: as in `steal_left_unchecked`, mirrored: the right leaf
        // gives key 0 and head-and-value slot 0 (its head, after which value
        // 0 shifts into the head), and the left leaf has room at key
        // `left_len` and head-and-value slot `left_len + 1`. The key going
        // there is the old separator, at the left leaf's prefix length,
        // `fill`'s; where separator keys are heads, from the right leaf's
        // head key slot, which the right leaf's old key 0 takes.
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
        // length, `fill`'s; where separator keys are heads (D49), it is the
        // right leaf's head key, with `fill` as its window, every byte of it
        // a leaf keeps (caller). `right`'s keys `0..right_len` go into
        // `left_len + 1..`, and its head-and-value slots `0..=right_len`, all
        // initialized (caller), into `left_len + 1..=left_len + 1 +
        // right_len`: its head beside the separator as its value. `left`'s
        // head is untouched.
        unsafe {
            let separator = if NodePtr::<K, V, S>::has_separator_keys_in_heads() {
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
    }

    fn key<'a>(self, index: usize) -> &'a K {
        Leaf::key(self, index)
    }

    fn inline_keys<'a>(self) -> Option<NodeInlineKeys<'a>> {
        Leaf::inline_keys(self)
    }

    #[cfg(any(test, fuzzing))]
    fn length_tags<'a>(self) -> Option<&'a [u16]> {
        Leaf::length_tags(self)
    }

    /// As far as its fences share: nothing below a leaf bounds it.
    fn allowed_prefix_len(self, prefix_len: usize) -> usize {
        prefix_len
    }

    fn set_windows(
        self,
        prefix_len: usize,
        inline_keys: impl ExactSizeIterator<Item = InlineWord>,
    ) {
        Leaf::set_windows(self, prefix_len, inline_keys);
    }
}

/// A validated interval of one leaf's entries, which a walk hands out
/// without going back to the leaf. Paired entries use key pointers as their
/// cursor and end: `front` and `back` are the next keys at either end, and
/// advancing needs neither the leaf pointer nor an index multiply. Other
/// layouts keep the leaf's first key in `front` and the interval in
/// `start..end`, leaving `back` unused. Which representation applies
/// depends only on the map's layout.
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

/// Keys of one leaf, in order, which a walk has moved past: for a loop to
/// visit them without the walk, which it touches once per run rather than
/// once per key (a merge of two sets). Copied into a local, the run lives
/// in registers.
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
