//! Reads of a latched tree's nodes while other threads write them: internal
//! nodes always, by optimistic lock coupling, and leaves for lookups and
//! hints (D57; `latch.rs`, `design/09-concurrency.md`).
//!
//! A racing reader takes no latch. It reads the node's version, reads the
//! node in place, and checks the version after. A writer holding the node
//! changes it in place, with the ordinary node code. The reader may
//! meanwhile read half of one change and half of another, which the check
//! then throws away. So a read here:
//!
//! - goes through raw pointers and forms no reference into the node
//!   ([`NodeInlineKeys`] over its slots, edges read from their slots);
//! - reads the node's length once and clamps it to the capacity, so the
//!   search stays in bounds whatever it read (D14);
//! - follows no edge before the node's version vouches for it, touching
//!   neither the child's latch word nor its bytes: the check orders the
//!   child's page, which a split may have just allocated, before the reader
//!   (`tree/latched/descent.rs`). The edge then reaches a page of the kind
//!   its tag says. Every page but the root's keeps its kind while threads
//!   share the tree. A node moved or freed keeps its page's latch word,
//!   whose version only grows (`tree/latched/pages.rs`);
//! - runs no user code on what it read. Where inline keys tie, it compares a
//!   separator's key, a leaf's head (D49), with that leaf latched shared,
//!   once the node's version vouches for the edge it walked down. A head key
//!   changes only while its leaf is latched exclusively, and the key a
//!   removal takes out may be dropped, and its memory freed, right after.
//!   Keys ordered by their own bytes alone (std integers and byte arrays) it
//!   compares as an unlatched copy instead, and checks the node's version
//!   after: a head changes only while the node holding its separator is
//!   held;
//! - tells ThreadSanitizer to ignore it ([`RacingReads`]). By Rust's rules,
//!   a plain read that races with a write is undefined behavior, checked or
//!   not, and Rust has no way yet to read bytes that a writer changes,
//!   padding included: RFC 3301's `AtomicPerByte` would be one. CI checks
//!   the compiled reads instead (`bbtree/racing-reads`).

use core::borrow::Borrow;
use core::cmp::Ordering;
use core::mem::{ManuallyDrop, MaybeUninit};
use core::ops::Range;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU8, Ordering as AtomicOrdering};

use super::Node;
use super::hash_tags::HashTags;
use super::inline_key_search::NodeInlineKeys;
use super::inline_keys::{KeyInliner, SearchKey, Window};
use super::internal::Internal;
use super::latch::{NodeLatch, Version};
use super::leaf::{CandidateLine, Leaf};
use super::page::{NodeKind, NodePtr};
use crate::inline_key::InlineKeyStrategy;

/// Where a key goes from an internal node read optimistically, as far as
/// its inline keys tell ([`Internal::route_racing_inline`]).
enum RacingRoute {
    Routed(Routed),
    /// The key ties with separators, whose keys decide.
    Tied(Tie),
}

/// Separators of an internal node, read optimistically, that tie with a key:
/// `candidates`, among the node's `len` as read. Without an inline key to
/// compare (`target` is `None`), all of them tie.
struct Tie {
    candidates: Range<usize>,
    len: usize,
    target: Option<Window>,
}

impl Tie {
    /// Down `edge`, right of a separator equal to the key if `at_separator`.
    fn routed(&self, edge: usize, at_separator: bool) -> Routed {
        Routed {
            edge,
            at_separator,
            len: self.len,
        }
    }
}

/// Where a key goes from an internal node read optimistically: down `edge`,
/// with the node's `len` as read. `at_separator` if the key equals separator
/// `edge - 1`, whose entry is the head of the leftmost leaf below (D17).
#[derive(Clone, Copy)]
pub(in crate::tree) struct Routed {
    pub(in crate::tree) edge: usize,
    pub(in crate::tree) at_separator: bool,
    pub(in crate::tree) len: usize,
}

impl<K, V, S: InlineKeyStrategy<K>> Leaf<K, V, S> {
    /// Whether `key` may sort after every key of this leaf, read in place
    /// without its latch while writers may change it. `false` if the leaf, as
    /// read, has no key, or a last key whose inline key alone sorts it after
    /// `key`. A hint for an append, which latches the leaf to be sure
    /// (`tree/latched/append.rs`): an insert that won't append then writes
    /// nothing to the last leaf.
    #[inline(always)]
    pub(in crate::tree) fn may_go_last_racing<Q>(self, key: &SearchKey<'_, K, S, Q>) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let _reads = RacingReads::begin();
        // SAFETY: the page is a leaf's, allocated while threads share the
        // tree.
        let (len, prefix_len, inline_keys) =
            unsafe { self.node_ptr().load_racing_inline_keys::<Self>() };
        let Some(last) = len.checked_sub(1) else {
            return false;
        };
        key.inline_target(prefix_len)
            .is_none_or(|target| !inline_keys.is_above(last, target.inline_key()))
    }

    /// [`Self::candidate_line`], read in place while writers may change the
    /// leaf, and the prefix length it read the leaf at. Both hold only if the
    /// leaf's version is unchanged after. A batch's lane reads it before it
    /// latches the leaf, to fetch the candidate's entry first
    /// (`tree/latched/batch.rs`).
    ///
    /// `key` shares the leaf's prefix if it routed here through nodes whose
    /// versions held.
    #[inline(always)]
    pub(in crate::tree) fn candidate_line_racing<Q>(
        self,
        key: &SearchKey<'_, K, S, Q>,
    ) -> (CandidateLine, usize)
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let _reads = RacingReads::begin();
        // SAFETY: the page is a leaf's, allocated while threads share the
        // tree.
        let (len, prefix_len, inline_keys) =
            unsafe { self.node_ptr().load_racing_inline_keys::<Self>() };
        let candidate = match key.inline_target(prefix_len) {
            Some(target) => self.candidate_line_among(target, inline_keys, len),
            None => CandidateLine::Unknown,
        };
        (candidate, prefix_len)
    }

    /// The line of the entry an insertion of `key` would shift first, as the
    /// leaf's inline keys say, read in place while writers may change them.
    /// A batch's lane fetches it before its insert searches the leaf latched
    /// (`tree/latched/batch.rs`). `None` if the key would go last, shifting
    /// nothing, or the inline keys can't place it.
    #[inline(always)]
    pub(in crate::tree) fn racing_insertion_line<Q>(
        self,
        key: &SearchKey<'_, K, S, Q>,
    ) -> Option<*const u8>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let _reads = RacingReads::begin();
        // SAFETY: the page is a leaf's, allocated while threads share the
        // tree.
        let (len, prefix_len, inline_keys) =
            unsafe { self.node_ptr().load_racing_inline_keys::<Self>() };
        let edge = inline_keys
            .equal_range(key.inline_target(prefix_len)?.inline_key())
            .start;
        // SAFETY: `edge < len`, which is clamped to the capacity, so the slot
        // is in the page; only its address is taken.
        (edge < len).then(|| unsafe {
            self.node_ptr()
                .key_slots::<Self>()
                .keys()
                .slot(edge)
                .cast::<u8>()
                .as_ptr()
                .cast_const()
        })
    }

    /// A copy of key `index` and its value, read in place while writers may
    /// change them: an entry of the leaf only if its version is unchanged
    /// after ([`RacingEntry`]).
    ///
    /// # Panics
    ///
    /// In debug builds, if `index` is not below the capacity.
    #[inline(always)]
    pub(in crate::tree) fn load_racing_entry(self, index: usize) -> RacingEntry<K, V> {
        let _reads = RacingReads::begin();
        debug_assert!(index < Self::capacity());
        let page = self.node_ptr();
        // SAFETY: `index` is below the capacity, so key slot `index` and
        // value slot `index` are in the page, which is a leaf's, allocated
        // while threads share the tree. Both are read as plain bytes, which
        // `MaybeUninit` holds whatever they are.
        unsafe {
            RacingEntry {
                key: page.key_slots::<Self>().keys().slot(index).cast().read(),
                value: page.value_slots().slot(index).cast().read(),
            }
        }
    }

    /// A copy of value `index`, read in place while writers may change it:
    /// the value of key `index` only if the leaf's version is unchanged
    /// after.
    ///
    /// # Panics
    ///
    /// In debug builds, if `index` is not below the capacity.
    #[inline(always)]
    pub(in crate::tree) fn load_racing_value(self, index: usize) -> MaybeUninit<V> {
        let _reads = RacingReads::begin();
        debug_assert!(index < Self::capacity());
        // SAFETY: `index` is below the capacity, so value slot `index` is in
        // the page, which is a leaf's, allocated while threads share the
        // tree, read as plain bytes.
        unsafe { self.node_ptr().value_slots().slot(index).cast().read() }
    }

    /// Whether key `index` is `key`, decided by the leaf alone, read in place
    /// while writers may change it. `index` is a candidate: its inline key,
    /// at the leaf's `prefix_len`, equals `key`'s. `Some` for a byte string
    /// that ends within its inline key: equal lengths then make the keys
    /// equal, and an exact hash tag tells the length (D36). `None` where
    /// only the keys' bytes can tell. The answer holds only if the leaf's
    /// version is unchanged after.
    ///
    /// `key` shares the leaf's prefix if it routed here through nodes whose
    /// versions held.
    ///
    /// # Panics
    ///
    /// In debug builds, if `index` is not below the capacity.
    #[inline(always)]
    pub(in crate::tree) fn racing_candidate_is<Q>(
        self,
        key: &SearchKey<'_, K, S, Q>,
        index: usize,
        prefix_len: usize,
    ) -> Option<bool>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let _reads = RacingReads::begin();
        debug_assert!(index < Self::capacity());
        if !key.ends_within_window::<Self>(prefix_len) {
            return None;
        }
        let query_tag = key
            .hash_tag()
            .filter(|&tag| HashTags::is_exact_length(tag))?;
        // SAFETY: the page is a leaf's, allocated while threads share the
        // tree, and its hash tags were initialized with it (invariant 5).
        // Tag `index`, below the capacity, is a byte in the page, read as an
        // atomic.
        let tag = unsafe {
            let tags = self.node_ptr().key_columns::<Self>().hash_tags()?;
            AtomicU8::from_ptr(tags.slot(index).as_ptr()).load(AtomicOrdering::Relaxed)
        };
        Some(tag == query_tag)
    }

    /// A copy of the head's key and value, read in place while writers may
    /// change them, as [`Self::load_racing_entry`]; `None` if the leaf, as
    /// read, has no head.
    #[inline(always)]
    pub(in crate::tree) fn load_racing_head_entry(self) -> Option<RacingEntry<K, V>> {
        let _reads = RacingReads::begin();
        let page = self.node_ptr();
        // SAFETY: the page is a leaf's, allocated while threads share the tree,
        // in a map with inline keys (D49); its head key slot and head slot are
        // in it, read as plain bytes.
        unsafe {
            page.load_racing_has_head().then(|| RacingEntry {
                key: page.head_key_slot().cast().read(),
                value: page.head_and_value_slots().slot(0).cast().read(),
            })
        }
    }

    /// A copy of the head's key, read in place while writers may change it,
    /// for keys ordered by their own bytes alone
    /// ([`KeyInliner::orders_by_own_bytes`]). Whatever bytes the copy holds
    /// make a key that compares by them, and a version check after tells
    /// whether the comparison held. `None` if the leaf, as read, has no head.
    #[inline(always)]
    fn load_racing_head_key(self) -> Option<ManuallyDrop<K>> {
        let _reads = RacingReads::begin();
        debug_assert!(KeyInliner::<K>::orders_by_own_bytes::<S>());
        let page = self.node_ptr();
        // SAFETY: the page is a leaf's, allocated while threads share the tree,
        // in a map with inline keys (D49). Its head key slot is in the page,
        // and initialized once the leaf has had a head. Every bit pattern is a
        // valid key of such a type.
        unsafe {
            page.load_racing_has_head()
                .then(|| ManuallyDrop::new(page.head_key_slot().read()))
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Internal<K, V, S> {
    /// Routes `key` through this node, read at `version`, as
    /// [`Internal::search`] does, but in place while writers may change it.
    /// The result holds only if the version is unchanged after. `None` if a
    /// tie found the node changed, or what it read is no node any writer
    /// left; the caller starts over.
    ///
    /// `key` shares the node's prefix if it routed here through nodes whose
    /// versions held.
    #[inline(always)]
    pub(in crate::tree) fn route_racing<Q>(
        self,
        key: &SearchKey<'_, K, S, Q>,
        version: Version,
    ) -> Option<Routed>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        match self.route_racing_inline(key) {
            RacingRoute::Routed(routed) => Some(routed),
            RacingRoute::Tied(tie) => self.route_tied(tie, version, key),
        }
    }

    /// Routes `key` through this node, read in place while writers may
    /// change it, as far as its inline keys tell: where the key goes, or the
    /// separators it ties with, whose keys in leaf heads decide (D49). The
    /// result holds only if the node's version is unchanged after.
    #[inline(always)]
    fn route_racing_inline<Q>(self, key: &SearchKey<'_, K, S, Q>) -> RacingRoute
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let _reads = RacingReads::begin();
        // SAFETY: the page is an internal node's, allocated while threads
        // share the tree.
        let (len, prefix_len, inline_keys) =
            unsafe { self.node_ptr().load_racing_inline_keys::<Self>() };
        let Some(target) = key.inline_target(prefix_len) else {
            // A lookup through a form the nodes can't encode compares full
            // keys.
            return RacingRoute::Tied(Tie {
                candidates: 0..len,
                len,
                target: None,
            });
        };
        let candidates = inline_keys.equal_range(target.inline_key());
        // Most searches tie with no separator: they stop at the inline keys,
        // without a call (D49).
        if candidates.is_empty() {
            return RacingRoute::Routed(Routed {
                edge: candidates.start,
                at_separator: false,
                len,
            });
        }
        RacingRoute::Tied(Tie {
            candidates,
            len,
            target: Some(target),
        })
    }

    /// A binary search among the separators `tie` names, each key read in
    /// its leaf's head (D49).
    #[inline(never)]
    fn route_tied<Q>(
        self,
        tie: Tie,
        version: Version,
        key: &SearchKey<'_, K, S, Q>,
    ) -> Option<Routed>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let (mut low, mut high) = (tie.candidates.start, tie.candidates.end);
        while low < high {
            let middle = low + (high - low) / 2;
            let leaf = self.racing_leftmost_leaf(middle + 1, version)?;
            match self.cmp_separator_head(leaf, version, key, tie.target)? {
                Ordering::Less => high = middle,
                Ordering::Greater => low = middle + 1,
                Ordering::Equal => return Some(tie.routed(middle + 1, true)),
            }
        }
        Some(tie.routed(low, false))
    }

    /// How `key` orders against the key of a separator of this node: the
    /// head key of `leaf`, the leftmost leaf right of it (D49). The head is
    /// read with the leaf latched shared, or as a copy for keys ordered by
    /// their own bytes. The result stands only if the node, read at
    /// `version`, is unchanged once the head is read: the walk to the leaf
    /// went down first edges, which only a thread holding the node changes.
    /// `target` is the key's window the separators tie with, if it has one.
    /// `None` if the node changed, or the leaf has no head.
    ///
    /// The caller holds no latch, so waiting for the leaf's latch waits for
    /// no thread that waits for the caller.
    #[inline(always)]
    fn cmp_separator_head<Q>(
        self,
        leaf: Leaf<K, V, S>,
        version: Version,
        key: &SearchKey<'_, K, S, Q>,
        target: Option<Window>,
    ) -> Option<Ordering>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        // SAFETY: the tree is latched (`route_racing`'s caller), and its
        // pages stay node pages of their kind (`tree/latched`).
        let latch_of = |node: NodePtr<K, V, S>| unsafe { node.latch() };
        let cmp = |stored: &K| match target {
            Some(target) => key.cmp_tied::<Self>(target, stored),
            None => key.key().cmp(stored.borrow()),
        };
        if KeyInliner::<K>::orders_by_own_bytes::<S>() {
            // Without the latch: the head changes only while this node is
            // held (`tree/latched/held.rs`), which its version tells.
            let head = leaf.load_racing_head_key()?;
            let ordering = cmp(&head);
            return latch_of(self.node_ptr())
                .is_unchanged(version)
                .then_some(ordering);
        }
        let latched = SharedLatch::lock(latch_of(leaf.node_ptr()));
        let ordering = (latch_of(self.node_ptr()).is_unchanged(version) && leaf.has_head())
            .then(|| cmp(leaf.head_key()));
        drop(latched);
        ordering
    }

    /// The node's length, read in place while writers may change it, and
    /// clamped to the capacity. It is the node's only if the node's version
    /// is unchanged after.
    #[inline(always)]
    pub(in crate::tree) fn racing_len(self) -> usize {
        // SAFETY: the page is an internal node's, allocated while threads
        // share the tree, its header initialized.
        let (len, _) = unsafe { self.node_ptr().load_racing_header() };
        len.min(Self::capacity())
    }

    /// The node's prefix length, read in place while writers may change it.
    /// It is the node's only if the node's version is unchanged after.
    #[inline(always)]
    pub(in crate::tree) fn racing_prefix_len(self) -> usize {
        // SAFETY: as for `racing_len`.
        let (_, prefix_len) = unsafe { self.node_ptr().load_racing_header() };
        prefix_len
    }

    /// The leftmost leaf below edge `edge`, down first edges from there,
    /// each read in place while writers may change it. Only a thread holding
    /// this node, read at `version`, changes them, so the walk follows each
    /// once the version vouches for it (`tree/latched/descent.rs`). `None` if
    /// the node changed, or the walk meets a null edge.
    #[inline(always)]
    fn racing_leftmost_leaf(self, edge: usize, version: Version) -> Option<Leaf<K, V, S>> {
        // SAFETY: the tree is latched (`route_racing`'s caller), and its
        // pages stay node pages of their kind (`tree/latched`).
        let latch = unsafe { self.node_ptr().latch() };
        let mut node = self.racing_child(edge)?;
        loop {
            if !latch.is_unchanged(version) {
                return None;
            }
            node = match node {
                Node::Leaf(leaf) => return Some(leaf),
                Node::Internal(internal) => internal.racing_child(0)?,
            };
        }
    }

    /// The child at edge `index`, read in place while writers may change
    /// it: what a writer stored there, or `None` for an edge none has.
    ///
    /// # Panics
    ///
    /// In debug builds, if `index > capacity`.
    #[inline(always)]
    pub(in crate::tree) fn racing_child(self, index: usize) -> Option<Node<K, V, S>> {
        let _reads = RacingReads::begin();
        debug_assert!(index <= Self::capacity());
        // SAFETY: `index <= capacity`, so the edge slot is in the page, which
        // is allocated while threads share the tree. The slot is aligned for a
        // pointer and initialized, zeroed when allocated. It is read as a raw
        // pointer, as a zeroed one is no `NodePtr`. A non-null edge is a
        // tagged pointer to a node of this map.
        unsafe {
            let slot = self.node_ptr().edge_slots().slot(index);
            let tagged = slot.cast::<*mut u8>().read();
            NonNull::new(tagged).map(|tagged| Node::at(NodePtr::from_tagged(tagged)))
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> NodePtr<K, V, S> {
    /// The length and prefix length of the `Kind` node on this page, and its
    /// inline keys, read in place while writers may change them. Each field
    /// is read once, and the length is clamped to the capacity, so a search
    /// over the inline keys stays in bounds whatever it read (D14).
    ///
    /// # Safety
    ///
    /// The page is a `Kind` node's, allocated while threads share the tree.
    #[inline(always)]
    unsafe fn load_racing_inline_keys<'a, Kind: NodeKind>(
        self,
    ) -> (usize, usize, NodeInlineKeys<'a>) {
        let capacity = Self::capacity::<Kind>();
        // SAFETY: the page is an allocated `Kind` node's, its header
        // initialized (caller); every inline-key slot up to the capacity is
        // initialized (invariant 4), and the view reads them through their
        // pointer.
        unsafe {
            let (len, prefix_len) = self.load_racing_header();
            let len = len.min(capacity);
            let slots = self
                .key_columns::<Kind>()
                .inline_keys()
                .expect("a latched tree's keys have inline keys");
            (len, prefix_len, NodeInlineKeys::new(slots, capacity, len))
        }
    }
}

/// A copy of a leaf's entry, its key and value, read in place while writers
/// may change them. It may be torn, or of slots no entry fills; it is an
/// entry the leaf held only if the leaf's version is unchanged after the
/// read.
pub(in crate::tree) struct RacingEntry<K, V> {
    key: MaybeUninit<K>,
    value: MaybeUninit<V>,
}

/// A key and value copied out of a leaf, which the copy must not drop: the
/// leaf's own stay where they are.
pub(in crate::tree) type CopiedEntry<K, V> = (ManuallyDrop<K>, ManuallyDrop<V>);

impl<K, V> RacingEntry<K, V> {
    /// The key and value read.
    ///
    /// # Safety
    ///
    /// The leaf stayed at one version, with no thread holding it
    /// exclusively, from before the read to after it. The read was of an
    /// entry the leaf held at that version: a key below its length, or its
    /// head, if it had one. Both are copies of the tree's, which the caller
    /// must not drop.
    #[inline(always)]
    pub(in crate::tree) unsafe fn assume_read(self) -> CopiedEntry<K, V> {
        // SAFETY: the slots held the entry's key and value throughout the
        // read (caller).
        unsafe {
            (
                ManuallyDrop::new(self.key.assume_init()),
                ManuallyDrop::new(self.value.assume_init()),
            )
        }
    }
}

/// One of the reads above, while it lasts: a TSan build (`--cfg tsan`,
/// `.github/workflows/sanitizer.yml`) ignores the thread's memory accesses
/// meanwhile, which are racing reads the version check after vouches for,
/// and only those. TSan still checks every write, and the latch words read
/// outside it. Nothing in other builds.
#[must_use]
pub(in crate::tree) struct RacingReads(());

impl RacingReads {
    #[inline(always)]
    pub(in crate::tree) fn begin() -> Self {
        #[cfg(tsan)]
        tsan::AnnotateIgnoreReadsBegin(c"bbtree".as_ptr(), 0);
        Self(())
    }
}

#[cfg(tsan)]
impl Drop for RacingReads {
    fn drop(&mut self) {
        tsan::AnnotateIgnoreReadsEnd(c"bbtree".as_ptr(), 0);
    }
}

/// TSan's runtime, which a `-Zsanitizer=thread` build links: its dynamic
/// annotations for reads that race on purpose.
#[cfg(tsan)]
mod tsan {
    use core::ffi::{c_char, c_int};

    unsafe extern "C" {
        pub(super) safe fn AnnotateIgnoreReadsBegin(file: *const c_char, line: c_int);
        pub(super) safe fn AnnotateIgnoreReadsEnd(file: *const c_char, line: c_int);
    }
}

/// A node latched shared until dropped, unwinding included.
struct SharedLatch<'a>(&'a NodeLatch);

impl<'a> SharedLatch<'a> {
    fn lock(latch: &'a NodeLatch) -> Self {
        latch.lock_shared();
        Self(latch)
    }
}

impl Drop for SharedLatch<'_> {
    fn drop(&mut self) {
        self.0.unlock_shared();
    }
}
