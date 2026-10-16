//! Internal nodes of a latched tree, which threads read while others write
//! them: optimistic lock coupling (`latch.rs`, `doc/09-concurrency.md`).
//!
//! A reader takes no latch on an internal node: it reads the node's version,
//! reads the node in place, and checks the version after. A writer holding
//! the node changes it in place, with the node code as it is. What a reader
//! reads meanwhile may be half of one change and half of another, which the
//! check then throws away. So a read here:
//!
//! - goes through raw pointers and forms no reference into the node
//!   ([`NodeInlineKeys`] over its slots, edges read from their slots);
//! - reads the node's length once, and clamps it to the capacity, so that
//!   whatever it read, the search stays in bounds (D14);
//! - follows no edge it read before the node's version vouches for it, and
//!   then reaches a page of the kind the edge's tag says: every page but
//!   the root's keeps its kind while threads share the tree, and a node
//!   moved or freed keeps its page's latch word, whose version only grows
//!   (`tree/latched/pages.rs`);
//! - runs no user code on what it read: where inline keys tie, it compares
//!   a separator's key, a leaf's head (D49), once the node's version vouches
//!   for the edge it walked down, with that leaf latched shared. A head key
//!   changes only while its leaf is latched exclusively, and the key a
//!   removal takes out may be dropped, its memory freed, at once after.
//!   Keys ordered by their own bytes alone, std integers and byte arrays,
//!   it compares as a copy instead, unlatched, and checks the node's
//!   version after: a head changes only while the node holding its
//!   separator is held.

use core::borrow::Borrow;
use core::cmp::Ordering;
use core::mem::ManuallyDrop;
use core::ops::Range;
use core::ptr::NonNull;

use super::Node;
use super::inline_key_search::NodeInlineKeys;
use super::inline_keys::{KeyInliner, SearchKey};
use super::internal::Internal;
use super::latch::{NodeLatch, Version};
use super::leaf::Leaf;
use super::page::NodePtr;
use crate::inline_key::InlineKeyStrategy;

/// Where a key goes from an internal node read optimistically: down `edge`,
/// right of separator `edge - 1` if the key equals it, whose entry is the
/// head of the leftmost leaf below (D17); and the node's length, as read.
#[derive(Clone, Copy)]
pub(in crate::tree) struct Routed {
    pub(in crate::tree) edge: usize,
    pub(in crate::tree) at_separator: bool,
    pub(in crate::tree) len: usize,
}

impl<K, V, S: InlineKeyStrategy<K>> Leaf<K, V, S> {
    /// Whether `key` may sort after every key of this leaf, read in place,
    /// without its latch, while writers may change it: `false` if the leaf,
    /// as read, has no key, or a last key whose inline key alone sorts it
    /// after `key`. A hint for an append, which latches the leaf to know
    /// (`tree/latched/append.rs`), so that an insert that won't append
    /// writes nothing to the last leaf.
    #[inline(always)]
    pub(in crate::tree) fn may_go_last_racing<Q>(self, key: &SearchKey<'_, K, S, Q>) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let page = self.node_ptr();
        let capacity = Self::capacity();
        // SAFETY: the page is a leaf's, allocated while threads share the
        // tree; the fields are read once each and the length clamped, and
        // every inline-key slot up to the capacity is initialized, read
        // through its pointer, as for an internal node (`route_racing`).
        let (len, prefix_len, inline_keys) = unsafe {
            let (len, prefix_len) = page.load_racing_header();
            let len = len.min(capacity);
            let slots = page
                .key_columns::<Self>()
                .inline_keys()
                .expect("a latched tree's keys have inline keys");
            (len, prefix_len, NodeInlineKeys::new(slots, capacity, len))
        };
        let Some(last) = len.checked_sub(1) else {
            return false;
        };
        key.inline_target(prefix_len)
            .is_none_or(|target| !inline_keys.is_above(last, target.inline_key()))
    }

    /// A copy of the head's key, read in place while writers may change it,
    /// for keys ordered by their own bytes alone
    /// ([`KeyInliner::orders_by_own_bytes`]): whatever bytes it holds make a
    /// key that compares by them, and a version check after tells whether
    /// the comparison held. `None` if the leaf, as read, has no head.
    #[inline(always)]
    fn load_racing_head_key(self) -> Option<ManuallyDrop<K>> {
        debug_assert!(KeyInliner::<K>::orders_by_own_bytes::<S>());
        let page = self.node_ptr();
        // SAFETY: the page is a leaf's, allocated while threads share the
        // tree, in a map whose separator keys are heads (D49); its head key
        // slot is in the page, and initialized once the leaf had a head,
        // which every bit pattern of such a key is.
        unsafe {
            page.load_racing_has_head()
                .then(|| ManuallyDrop::new(page.head_key_slot().read()))
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Internal<K, V, S> {
    /// Routes `key` through this node, read at `version`, as
    /// [`Internal::search`] does, in place while writers may change it: what
    /// it returns holds only if the version is unchanged after. `None` if a
    /// tie found the node changed, or what it read is no node any writer
    /// left: the caller starts over.
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
        let page = self.node_ptr();
        let capacity = Self::capacity();
        // SAFETY: the page is an internal node's, allocated while threads
        // share the tree. The fields are read once each, and the length
        // clamped: a search over any inline keys, up to the capacity, stays
        // in bounds. Every inline-key slot up to the capacity is
        // initialized (invariant 4), and the view reads them through their
        // pointer.
        let (len, prefix_len, inline_keys) = unsafe {
            let (len, prefix_len) = page.load_racing_header();
            let len = len.min(capacity);
            let slots = page
                .key_columns::<Self>()
                .inline_keys()
                .expect("a latched tree's keys have inline keys");
            (len, prefix_len, NodeInlineKeys::new(slots, capacity, len))
        };
        if let Some(target) = key.inline_target(prefix_len) {
            let candidates = inline_keys.equal_range(target.inline_key());
            // Most searches tie with no separator, and go no further than
            // the inline keys, nor into a call (D49).
            if candidates.is_empty() {
                return Some(Routed {
                    edge: candidates.start,
                    at_separator: false,
                    len,
                });
            }
            return self.route_tied(candidates, len, version, |stored| {
                key.cmp_tied::<Self>(target, stored)
            });
        }
        // A lookup through a form the nodes can't encode compares full keys.
        self.route_tied(0..len, len, version, |stored| {
            key.key().cmp(stored.borrow())
        })
    }

    /// A binary search among `candidates`, separators whose keys `cmp`
    /// orders the search key against, each read where it is, in a leaf's
    /// head (D49).
    #[inline(never)]
    fn route_tied(
        self,
        candidates: Range<usize>,
        len: usize,
        version: Version,
        cmp: impl Fn(&K) -> Ordering,
    ) -> Option<Routed> {
        let (mut low, mut high) = (candidates.start, candidates.end);
        while low < high {
            let middle = low + (high - low) / 2;
            match self.cmp_separator(middle, version, &cmp)? {
                Ordering::Less => high = middle,
                Ordering::Greater => low = middle + 1,
                Ordering::Equal => {
                    return Some(Routed {
                        edge: middle + 1,
                        at_separator: true,
                        len,
                    });
                }
            }
        }
        Some(Routed {
            edge: low,
            at_separator: false,
            len,
        })
    }

    /// `cmp` of separator `index`'s key, the head key of the leftmost leaf
    /// right of it (D49), read with that leaf latched shared, or as a copy
    /// for keys ordered by their own bytes, if the node, read at `version`,
    /// still is: its edge is read, then the version checked, before the walk
    /// down first edges, which only a thread holding the node changes; and
    /// checked again once the head is read. `None` if the node changed, or
    /// the walk meets a null edge or a leaf without a head.
    ///
    /// The caller holds no latch, so waiting for the leaf's waits for no
    /// thread that waits for it.
    fn cmp_separator(
        self,
        index: usize,
        version: Version,
        cmp: &impl Fn(&K) -> Ordering,
    ) -> Option<Ordering> {
        // SAFETY: the tree is latched (`route_racing`'s caller), and its
        // pages stay node pages of their kind (`tree/latched`).
        let latch_of = |node: NodePtr<K, V, S>| unsafe { node.latch() };
        let child = self.racing_child(index + 1)?;
        if !latch_of(self.node_ptr()).is_unchanged(version) {
            return None;
        }
        let mut node = child;
        let leaf = loop {
            node = match node {
                Node::Leaf(leaf) => break leaf,
                Node::Internal(internal) => internal.racing_child(0)?,
            };
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
    /// clamped to the capacity: what it holds only if the node's version is
    /// unchanged after.
    #[inline(always)]
    pub(in crate::tree) fn racing_len(self) -> usize {
        // SAFETY: the page is an internal node's, allocated while threads
        // share the tree, its header initialized.
        let (len, _) = unsafe { self.node_ptr().load_racing_header() };
        len.min(Self::capacity())
    }

    /// The child at edge `index`, read in place while writers may change
    /// it: what a writer stored there, or `None` for an edge none has.
    ///
    /// # Panics
    ///
    /// In debug builds, if `index > capacity`.
    #[inline(always)]
    pub(in crate::tree) fn racing_child(self, index: usize) -> Option<Node<K, V, S>> {
        debug_assert!(index <= Self::capacity());
        // SAFETY: `index <= capacity`, so the edge slot is in the page, which
        // is allocated while threads share the tree, aligned for a pointer,
        // and initialized, zeroed when allocated; read as a raw pointer, as
        // a zeroed one is no `NodePtr`. A non-null edge is a pointer to a
        // node of this map, tagged.
        unsafe {
            let slot = self.node_ptr().edge_slots().slot(index);
            let tagged = slot.cast::<*mut u8>().read();
            NonNull::new(tagged).map(|tagged| Node::at(NodePtr::from_tagged(tagged)))
        }
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
