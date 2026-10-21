//! [`CopiedRange`]: copies of a range of a latched tree's entries, from
//! either end, one at a time, with no latch held between calls: a cursor
//! over a tree that threads change meanwhile. A map's `range_cloned` clones
//! its entries ([`map/range.rs`]); a store's copies its records' bytes
//! (`store.rs`): each says how, by the copy it passes.
//!
//! Each end copies a few entries at a time during a scan (`scan.rs`), each
//! with its leaf's stamp. It hands a copy out only while the stamps of its
//! leaf and of the last entry handed out are current: then those leaves
//! still hold both entries, and nothing lies between them. Otherwise it
//! scans again from the last key it handed out. So, as a scan does, it
//! hands out what other threads insert ahead of it, and nothing they
//! removed before it got there.
//!
//! While that leaf is unchanged, the next copy goes on where the last one's
//! scan stopped, along the path that scan kept, without a descent
//! ([`ScanResume`]). An end copies one entry first, then four times as many
//! each time, up to [`RangeEnd::MAX_COPY_LEN`], so that a caller that takes
//! a few entries copies a few.
//!
//! [`map/range.rs`]: crate::map

#![forbid(unsafe_code)]

use alloc::vec::Vec;
use core::borrow::Borrow;
use core::cmp::Ordering as KeyOrdering;
use core::marker::PhantomData;
use core::ops::{Bound, ControlFlow, RangeBounds};

use super::LatchedTree;
use super::scan::{LeafStamp, ScanResume};
use crate::inline_key::InlineKeyStrategy;

/// Copies of the entries of a range of a latched tree, in key order from
/// the front, in reverse from the back: keys copied as `CK`, which also
/// bound where the next copies start, values as `CV`. The copied keys
/// borrow as `Q`, the range's queries.
pub(crate) struct CopiedRange<'t, K, V, S: InlineKeyStrategy<K>, CK, CV, Q: ?Sized, R> {
    tree: &'t LatchedTree<K, V, S>,
    range: R,
    front: RangeEnd<'t, K, V, S, CK, CV>,
    back: RangeEnd<'t, K, V, S, CK, CV>,
    _query: PhantomData<fn(&Q)>,
}

/// An entry copied ahead, and its leaf's stamp then.
type Copied<'t, CK, CV> = (CK, CV, LeafStamp<'t>);

/// One end of a [`CopiedRange`].
struct RangeEnd<'t, K, V, S: InlineKeyStrategy<K>, CK, CV> {
    /// The key this end handed out last, which it goes on past, and its
    /// leaf's stamp when copied.
    last: Option<(CK, LeafStamp<'t>)>,
    /// Entries copied ahead, the next one last.
    copied: Vec<Copied<'t, CK, CV>>,
    /// Where the scan that copied them stopped, to go on from.
    resume: ScanResume<'t, K, V, S>,
    /// How many entries the next copy takes at most.
    copy_len: usize,
    /// Whether this end is past the range, or past the other end.
    finished: bool,
}

impl<'t, K, V, S: InlineKeyStrategy<K>, CK, CV> RangeEnd<'t, K, V, S, CK, CV> {
    /// The most entries a copy takes.
    const MAX_COPY_LEN: usize = 16;

    fn new() -> Self {
        Self {
            last: None,
            copied: Vec::new(),
            resume: ScanResume::new(),
            copy_len: 1,
            finished: false,
        }
    }

    /// The next entry copied ahead, if its leaf, and the last one's, hold
    /// the copies still; otherwise forgets the copies and where their scan
    /// stopped.
    fn next_copied(&mut self) -> Option<Copied<'t, CK, CV>> {
        let (_, _, stamp) = self.copied.last()?;
        let last_current = (self.last.as_ref()).is_none_or(|(_, last)| last.is_current());
        if !(last_current && stamp.is_current()) {
            self.copied.clear();
            self.resume.forget();
            return None;
        }
        self.copied.pop()
    }

    /// Stops this end: it hands out nothing more.
    fn finish(&mut self) {
        self.finished = true;
        self.copied.clear();
        self.resume.forget();
    }
}

impl<'t, K, V, S, CK, CV, Q, R> CopiedRange<'t, K, V, S, CK, CV, Q, R>
where
    K: Borrow<Q>,
    S: InlineKeyStrategy<K>,
    CK: Borrow<Q> + Clone,
    Q: Ord + ?Sized,
    R: RangeBounds<Q>,
{
    /// The copies of `tree`'s entries in `range`, which
    /// [`LatchedTree::checked_bounds`] passed.
    pub(crate) fn new(tree: &'t LatchedTree<K, V, S>, range: R) -> Self {
        Self {
            tree,
            range,
            front: RangeEnd::new(),
            back: RangeEnd::new(),
            _query: PhantomData,
        }
    }

    /// The next entry from the front if `ascending`, else from the back,
    /// copied out of its leaf by `copy`, which runs while the leaf is
    /// latched, on entries copied ahead too.
    pub(crate) fn advance(
        &mut self,
        ascending: bool,
        copy: impl FnMut(&K, &V) -> (CK, CV),
    ) -> Option<(CK, CV)> {
        let end = if ascending {
            &mut self.front
        } else {
            &mut self.back
        };
        if end.finished {
            return None;
        }
        let entry = match end.next_copied() {
            Some(entry) => Some(entry),
            None => self.copy(ascending, copy),
        };
        let (end, other) = if ascending {
            (&mut self.front, &self.back)
        } else {
            (&mut self.back, &self.front)
        };
        // The other end may have handed the key out since the copy.
        let met_other = |key: &CK| {
            other.last.as_ref().is_some_and(|(other, _)| {
                let ordering = key.borrow().cmp(other.borrow());
                ordering == KeyOrdering::Equal || (ordering == KeyOrdering::Greater) == ascending
            })
        };
        match entry {
            Some((key, value, stamp)) if !met_other(&key) => {
                end.last = Some((key.clone(), stamp));
                Some((key, value))
            }
            _ => {
                end.finish();
                None
            }
        }
    }

    /// Copies entries for the front if `ascending`, else for the back, and
    /// returns the first. The copy starts where the last copy's scan
    /// stopped, if its leaf is unchanged; else past this end's last key, or
    /// at this end's bound of the range. It stops at the other end's last
    /// key, or the other bound.
    fn copy(
        &mut self,
        ascending: bool,
        mut copy: impl FnMut(&K, &V) -> (CK, CV),
    ) -> Option<Copied<'t, CK, CV>> {
        let Self {
            tree,
            range,
            front,
            back,
            ..
        } = self;
        let (end, other) = if ascending {
            (front, &*back)
        } else {
            (back, &*front)
        };
        let (from, to) = if ascending {
            (range.start_bound(), range.end_bound())
        } else {
            (range.end_bound(), range.start_bound())
        };
        let from = (end.last.as_ref()).map_or(from, |(last, _)| Bound::Excluded(last.borrow()));
        let to = (other.last.as_ref()).map_or(to, |(last, _)| Bound::Excluded(last.borrow()));
        let copy_len = end.copy_len;
        // A caller that took a full copy scans on: keep the path down to
        // where the copies stop, so that the next ones go past their leaves
        // without a descent.
        if copy_len == RangeEnd::<K, V, S, CK, CV>::MAX_COPY_LEN {
            end.resume.keep_path();
        }
        end.copy_len = (copy_len * 4).min(RangeEnd::<K, V, S, CK, CV>::MAX_COPY_LEN);
        let copied = &mut end.copied;
        copied.clear();
        let visit = |key: &K, value: &V, stamp: LeafStamp<'t>| {
            let (key, value) = copy(key, value);
            copied.push((key, value, stamp));
            if copied.len() == copy_len {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        if ascending {
            tree.scan(from, to, Some(&mut end.resume), visit);
        } else {
            tree.scan_rev(to, from, Some(&mut end.resume), visit);
        }
        end.copied.reverse();
        end.copied.pop()
    }
}
