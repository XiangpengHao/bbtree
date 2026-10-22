//! [`RangeCopies`]: copies of a range of a store's entries, from either
//! end, a few at a time, with no latch held between calls.
//!
//! Each end copies up to [`RangeCopies::BATCH`] entries at a time by a scan
//! that starts past what it handed out last, and ends before what the other
//! end handed out last. It hands a copy out only if the other end hasn't
//! handed it, or anything past it, out since. So, as a scan does, it hands
//! out what other threads insert ahead of it, and nothing twice.

use core::fmt;
use core::iter::FusedIterator;
use core::ops::{Bound, ControlFlow};
use std::vec::Vec;

use bbtree::{Allocator, NodeConfig};

use crate::store::BBTreeStore;

/// A copied entry: its key and value.
type Entry = (Vec<u8>, Vec<u8>);

/// Copies of the entries of a range of a [`BBTreeStore`], in key order from
/// the front, in reverse from the back, from [`BBTreeStore::range`].
pub struct RangeCopies<'s, A: Allocator + Clone, C: NodeConfig> {
    store: &'s BBTreeStore<A, C>,
    /// Where the front goes on: past what it handed out last, or the
    /// range's start.
    start: Bound<Vec<u8>>,
    /// Where the back goes on, as `start` for the front.
    end: Bound<Vec<u8>>,
    /// Entries the front copied ahead, the next one last.
    front: Vec<Entry>,
    /// Entries the back copied ahead, the next one last.
    back: Vec<Entry>,
}

impl<'s, A: Allocator + Clone, C: NodeConfig> RangeCopies<'s, A, C> {
    /// The most entries a copy takes.
    const BATCH: usize = 16;

    /// Copies of `store`'s entries between `start` and `end`, which a scan
    /// checked.
    pub(crate) fn new(
        store: &'s BBTreeStore<A, C>,
        start: Bound<Vec<u8>>,
        end: Bound<Vec<u8>>,
    ) -> Self {
        Self {
            store,
            start,
            end,
            front: Vec::new(),
            back: Vec::new(),
        }
    }

    /// Whether no key lies between the ends.
    fn is_empty(&self) -> bool {
        match (&self.start, &self.end) {
            (Bound::Included(start), Bound::Included(end)) => start > end,
            (
                Bound::Included(start) | Bound::Excluded(start),
                Bound::Included(end) | Bound::Excluded(end),
            ) => start >= end,
            _ => false,
        }
    }

    /// The bounds left, as slices.
    fn bounds(&self) -> (Bound<&[u8]>, Bound<&[u8]>) {
        (
            self.start.as_ref().map(Vec::as_slice),
            self.end.as_ref().map(Vec::as_slice),
        )
    }

    /// Copies up to a batch of entries from the front if `ascending`, else
    /// from the back, the next one last.
    fn copy(&self, ascending: bool) -> Vec<Entry> {
        let mut copied = Vec::new();
        if self.is_empty() {
            return copied;
        }
        let mut take = |key: &[u8], value: &[u8]| {
            copied.push((key.to_vec(), value.to_vec()));
            if copied.len() == Self::BATCH {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        if ascending {
            self.store.scan::<[u8], ()>(self.bounds(), &mut take);
        } else {
            self.store.scan_rev::<[u8], ()>(self.bounds(), &mut take);
        }
        copied.reverse();
        copied
    }

    /// Whether `key` still lies between the ends: the other end may have
    /// handed it out, or one past it, since the copy.
    fn lies_between(&self, key: &[u8]) -> bool {
        let after_start = match &self.start {
            Bound::Included(start) => key >= start.as_slice(),
            Bound::Excluded(start) => key > start.as_slice(),
            Bound::Unbounded => true,
        };
        let before_end = match &self.end {
            Bound::Included(end) => key <= end.as_slice(),
            Bound::Excluded(end) => key < end.as_slice(),
            Bound::Unbounded => true,
        };
        after_start && before_end
    }

    /// Stops both ends: they meet.
    fn finish(&mut self) -> Option<Entry> {
        self.front.clear();
        self.back.clear();
        self.start = Bound::Excluded(Vec::new());
        self.end = Bound::Excluded(Vec::new());
        None
    }
}

impl<A: Allocator + Clone, C: NodeConfig> Iterator for RangeCopies<'_, A, C> {
    type Item = Entry;

    fn next(&mut self) -> Option<Entry> {
        if self.front.is_empty() {
            self.front = self.copy(true);
        }
        let Some(entry) = self.front.pop() else {
            return self.finish();
        };
        if !self.lies_between(&entry.0) {
            return self.finish();
        }
        self.start = Bound::Excluded(entry.0.clone());
        Some(entry)
    }
}

impl<A: Allocator + Clone, C: NodeConfig> DoubleEndedIterator for RangeCopies<'_, A, C> {
    fn next_back(&mut self) -> Option<Entry> {
        if self.back.is_empty() {
            self.back = self.copy(false);
        }
        let Some(entry) = self.back.pop() else {
            return self.finish();
        };
        if !self.lies_between(&entry.0) {
            return self.finish();
        }
        self.end = Bound::Excluded(entry.0.clone());
        Some(entry)
    }
}

impl<A: Allocator + Clone, C: NodeConfig> FusedIterator for RangeCopies<'_, A, C> {}

impl<A: Allocator + Clone, C: NodeConfig> fmt::Debug for RangeCopies<'_, A, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RangeCopies").finish_non_exhaustive()
    }
}
