//! The first entry of a range of a latched tree, read without latching its
//! leaf, for keys and values that are copies of their bytes: `Copy`, and
//! owning nothing. A seek: what a versioned read does many times a query,
//! "the first entry at or after this key", into the leaves every thread is
//! reading. As a lookup copies one value unlatched (`lookup.rs`, D57), a
//! copied first places the range's start among a leaf's keys, reads the few
//! entries that may be the first, one at a time with the version checked
//! after each, and settles among the copies.
//!
//! - **Placed by the inline keys**, as a latched search is (D18): the keys
//!   whose inline keys sort below the bound's are before it, those above are
//!   past it, and the few that tie are compared in full, on copies.
//! - **One leaf.** The descent's leaf holds the first entry unless the bound
//!   sorts past every key in it, when the next leaf's head does. That, more
//!   ties than [`READ`] reads settle, a range unbounded below or starting
//!   before every key, or a leaf that changed meanwhile, sends the read the
//!   latched way (`scan.rs`).
//!
//! No latch is taken on this path, so seekers write nothing to the leaves
//! they read (`design/09-concurrency.md`, principle 3).

use core::borrow::Borrow;
use core::cmp::Ordering as KeyOrdering;
use core::ops::{Bound, ControlFlow, RangeBounds};

use super::super::node::inline_keys::SearchKey;
use super::LatchedTree;
use super::descent::Optimistic;
use crate::inline_key::InlineKeyStrategy;

/// How many entries are read at most, one at a time: the keys whose inline
/// keys tie with the bound's, and the one after them. Ties are rare and
/// short (D18); a longer run goes the latched way.
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
        if at_separator {
            // The bound matched the separator left of the leaf: the head's
            // key, unless the head went since (`descend`), so the head is
            // confirmed on a copy. A head at the bound is the first entry if
            // the bound includes it, and before the range otherwise; a head
            // past the bound is the first entry, every other key of the leaf
            // being past the head.
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
            let (stored, value) = unsafe { head.assume_read() };
            match key.cmp_full(&stored) {
                KeyOrdering::Equal if matches!(start, Bound::Excluded(_)) => {}
                KeyOrdering::Equal | KeyOrdering::Less => {
                    return Self::within_end(*stored, *value, end);
                }
                // A head before the bound: not the leaf the bound belongs
                // to any more; the latched way finds it.
                KeyOrdering::Greater => return self.first_latched(start, end),
            }
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
        // settle nearly every seek. Each reads one entry in place (a copy
        // sized at run time would be a call) and checks the version, so a
        // seek holds one entry however large the entries are; a longer run
        // goes the latched way.
        let after = (ties.end + 1).min(len);
        if after - ties.start > READ {
            return self.first_latched(start, end);
        }
        for index in ties.start..after {
            let entry = leaf.load_racing_entry(index);
            if !held.is_unchanged() {
                return self.first_latched(start, end);
            }
            // SAFETY: the index is below the length as read, and the leaf
            // stayed at the version the descent read, with no thread holding
            // it exclusively, from before the read to after it (checked just
            // above). `Copy` keys and values own nothing.
            let (stored, value) = unsafe { entry.assume_read() };
            // A tie compares from past the bytes it shares with the bound;
            // the key after the ties is past the bound.
            if index < ties.end {
                let before = match placed.cmp_tied(&key, &stored) {
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
        (Bound::Unbounded, end)
            .contains::<Q>(key.borrow())
            .then_some((key, value))
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
}
