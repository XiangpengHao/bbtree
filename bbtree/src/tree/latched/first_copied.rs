//! The first entry of a range of a latched tree, read without latching its
//! leaf, for keys and values that are copies of their bytes: `Copy`, and
//! owning nothing. A seek: what a versioned read does many times a query,
//! "the first entry at or after this key", into the leaves every thread is
//! reading. As a lookup copies one value unlatched (`lookup.rs`, D57), a
//! copied first places the range's start among a leaf's keys, reads the few
//! entries that may be the first, checks the version, and settles among the
//! copies.
//!
//! - **Placed by the inline keys**, as a latched search is (D18): the keys
//!   whose inline keys sort below the bound's are before it, those above are
//!   past it, and the few that tie are compared in full, on copies.
//! - **One leaf.** The descent's leaf holds the first entry unless the bound
//!   sorts past every key in it, when the next leaf's head does. That, a run
//!   of ties longer than the copies taken, a range that starts before every
//!   key, or a leaf that changed meanwhile, sends the read the latched way
//!   (`scan.rs`).
//!
//! No latch is taken on this path, so seekers write nothing to the leaves
//! they read (`design/09-concurrency.md`, principle 3).

use core::borrow::Borrow;
use core::cmp::Ordering as KeyOrdering;
use core::mem::MaybeUninit;
use core::ops::{Bound, ControlFlow};

use super::super::node::inline_keys::SearchKey;
use super::super::node::leaf::Leaf;
use super::super::node::optimistic::RacingEntry;
use super::LatchedTree;
use super::descent::Optimistic;
use crate::inline_key::InlineKeyStrategy;

/// How many entries are read at most: the keys whose inline keys tie with
/// the bound's, and the one after them. Ties are rare and short (D18); a
/// longer run goes the latched way.
const READ: usize = 3;

impl<K: Copy, V: Copy, S: InlineKeyStrategy<K>> LatchedTree<K, V, S> {
    /// A copy of the first entry whose key lies between `start` and `end`,
    /// in key order, without latching its leaf where its version holds
    /// (module documentation). `None` if no key does.
    pub(crate) fn first_copied<Q>(&self, start: Bound<&Q>, end: Bound<&Q>) -> Option<(K, V)>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.root_page()?;
        let key = match start {
            Bound::Unbounded => return self.first_latched(start, end),
            Bound::Included(key) | Bound::Excluded(key) => SearchKey::new(key),
        };
        let (held, at_separator) = match self.descend::<Q, Optimistic, ()>(&key, &mut ()) {
            Ok(at) => at,
            // The bound lacks what every key shares and sorts before them
            // all: the first entry is the tree's first, found the latched way.
            Err(KeyOrdering::Less) => return self.first_latched(start, end),
            // The bound sorts after every key.
            Err(_) => return None,
        };
        let leaf = held.leaf();
        // The bound matched the separator left of the leaf, the head's key:
        // the head is the first entry if the bound includes it. Otherwise
        // the head is before the range: the bound routed past it.
        if at_separator && matches!(start, Bound::Included(_)) {
            let Some(head) = leaf.load_racing_head_entry() else {
                return self.first_latched(start, end);
            };
            if !held.is_unchanged() {
                return self.first_latched(start, end);
            }
            // SAFETY: the leaf stayed at the version the descent read, with
            // no thread holding it exclusively, from before the read to
            // after it, and it had a head as read. A `Copy` key and value
            // own nothing.
            let (key, value) = unsafe { head.assume_read() };
            return Self::within_end(*key, *value, end);
        }
        let Some(placed) = leaf.racing_place(&key) else {
            return self.first_latched(start, end);
        };
        let (ties, len) = (placed.ties.clone(), placed.len);
        if ties.start >= len {
            // Every key sorts before the bound: the first entry is the next
            // leaf's head, if any.
            return self.first_latched(start, end);
        }
        // The ties, compared in full, and the key after them, which is the
        // first if no tie is: ties are rare and short (D18), so `READ` reads
        // settle nearly every seek, each of one entry, in place (a copy
        // sized at run time would be a call); a longer run goes the latched
        // way.
        let taking = (ties.end + 1).min(len) - ties.start;
        if taking > READ {
            return self.first_latched(start, end);
        }
        let mut read: [MaybeUninit<RacingEntry<K, V>>; READ] =
            [const { MaybeUninit::uninit() }; READ];
        read[0].write(leaf.load_racing_entry(ties.start));
        if taking > 1 {
            read[1].write(leaf.load_racing_entry(ties.start + 1));
        }
        if taking > 2 {
            read[2].write(leaf.load_racing_entry(ties.start + 2));
        }
        if !held.is_unchanged() {
            return self.first_latched(start, end);
        }
        for (offset, slot) in read[..taking].iter().enumerate() {
            // SAFETY: the slot was written above, with a key below the
            // length as read. The leaf stayed at the version the descent
            // read, with no thread holding it exclusively, from before the
            // reads to after them (checked just above). `Copy` keys and
            // values own nothing.
            let (stored, value) = unsafe { slot.assume_init_ref().assume_read_ref() };
            // A tie compares from past the bytes it shares with the bound;
            // the key after the ties is past the bound.
            if ties.start + offset < ties.end {
                let before = match Leaf::<K, V, _>::cmp_tied(&key, &placed, stored) {
                    KeyOrdering::Greater => true,
                    KeyOrdering::Equal => matches!(start, Bound::Excluded(_)),
                    KeyOrdering::Less => false,
                };
                if before {
                    continue;
                }
            }
            return Self::within_end(*stored, *value, end);
        }
        // Every tie sorts before the bound, and no key follows them here.
        self.first_latched(start, end)
    }

    /// The entry, if its key is not past `end`.
    fn within_end<Q>(key: K, value: V, end: Bound<&Q>) -> Option<(K, V)>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        (!Self::past_end(&key, end)).then_some((key, value))
    }

    /// The first entry the latched way (`scan.rs`).
    #[cold]
    #[inline(never)]
    fn first_latched<Q>(&self, start: Bound<&Q>, end: Bound<&Q>) -> Option<(K, V)>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.scan(start, end, None, |key, value, _| {
            ControlFlow::Break((*key, *value))
        })
    }

    /// Whether `key` is past the bound a scan stops at.
    fn past_end<Q>(key: &K, to: Bound<&Q>) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        match to {
            Bound::Included(to) => key.borrow() > to,
            Bound::Excluded(to) => key.borrow() >= to,
            Bound::Unbounded => false,
        }
    }
}
