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
//! - follows no pointer it read before checking it can: an edge no writer
//!   wrote is null, as pages start zeroed (`tree/latched`), and a pointer to
//!   a node is to a live page, as a latched tree frees none while threads
//!   share it;
//! - runs no user code on what it read but a separator's key, a leaf's
//!   head (D49), and only once the node's version vouches for the edge it
//!   walked down: a head key never changes while its leaf is in a tree that
//!   only grows.

use core::borrow::Borrow;
use core::cmp::Ordering;
use core::ops::Range;
use core::ptr::NonNull;

use super::Node;
use super::inline_key_search::NodeInlineKeys;
use super::inline_keys::SearchKey;
use super::internal::Internal;
use super::latch::Version;
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
            match cmp(self.racing_separator_key(middle, version)?) {
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

    /// Separator `index`'s key, the head key of the leftmost leaf right of
    /// it (D49), if the node, read at `version`, still is: its edge is read,
    /// then the version checked, before the walk down first edges, which a
    /// published node never changes. `None` if the node changed, or the walk
    /// meets a null edge or a leaf without a head.
    ///
    /// The reference stays valid while threads share the tree: a head key
    /// never changes while its leaf is in a tree that only grows.
    fn racing_separator_key<'a>(self, index: usize, version: Version) -> Option<&'a K> {
        let child = self.racing_child(index + 1)?;
        // SAFETY: the tree is latched (`route_racing`'s caller), and the page
        // allocated.
        if !unsafe { self.node_ptr().latch() }.is_unchanged(version) {
            return None;
        }
        let mut node = child;
        loop {
            node = match node {
                Node::Leaf(leaf) => return leaf.has_head().then(|| leaf.head_key()),
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
