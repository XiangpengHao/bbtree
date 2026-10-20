//! [`RangeCloned`]: clones of a range of a [`BBTreeMap`]'s entries, from
//! either end, one at a time, with no latch held between calls: a cursor
//! over a map that threads change meanwhile.
//!
//! Each end copies a few entries at a time during a scan
//! (`tree/latched/scan.rs`), each with its leaf's stamp. It hands a copy out
//! only while the stamps of its leaf and of the last entry handed out are
//! current: then those leaves still hold both entries, and nothing lies
//! between them. Otherwise it scans again from the last key it handed out.
//! So, as a scan does, it hands out what other threads insert ahead of it,
//! and nothing they removed before it got there.
//!
//! While that leaf is unchanged, the next copy goes on where the last one's
//! scan stopped, along the path that scan kept, without a descent
//! ([`ScanResume`]). An end copies one entry first, then four times as many
//! each time, up to [`RangeEnd::MAX_COPY_LEN`], so that a caller that takes
//! a few entries clones a few.

use alloc::vec::Vec;
use core::borrow::Borrow;
use core::cmp::Ordering as KeyOrdering;
use core::marker::PhantomData;
use core::ops::{Bound, ControlFlow, RangeBounds};

use super::BBTreeMap;
use crate::allocator::Allocator;
use crate::inline_key::InlineKeyStrategy;
use crate::tree::latched::{LeafStamp, ScanResume};

/// An iterator over clones of the entries of a range of a [`BBTreeMap`]:
/// in key order from the front, in reverse from the back. See
/// [`BBTreeMap::range_cloned`].
pub struct RangeCloned<'m, K, V, S: InlineKeyStrategy<K>, A: Allocator, Q: ?Sized, R> {
    map: &'m BBTreeMap<K, V, S, A>,
    range: R,
    front: RangeEnd<'m, K, V, S>,
    back: RangeEnd<'m, K, V, S>,
    _query: PhantomData<fn(&Q)>,
}

/// An entry copied ahead, and its leaf's stamp then.
type Copied<'m, K, V> = (K, V, LeafStamp<'m>);

/// One end of a [`RangeCloned`].
struct RangeEnd<'m, K, V, S: InlineKeyStrategy<K>> {
    /// The key this end handed out last, which it goes on past, and its
    /// leaf's stamp when copied.
    last: Option<(K, LeafStamp<'m>)>,
    /// Entries copied ahead, the next one last.
    copied: Vec<Copied<'m, K, V>>,
    /// Where the scan that copied them stopped, to go on from.
    resume: ScanResume<'m, K, V, S>,
    /// How many entries the next copy takes at most.
    copy_len: usize,
    /// Whether this end is past the range, or past the other end.
    finished: bool,
}

impl<'m, K, V, S: InlineKeyStrategy<K>> RangeEnd<'m, K, V, S> {
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
    fn next_copied(&mut self) -> Option<Copied<'m, K, V>> {
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

impl<'m, K, V, S, A, Q, R> RangeCloned<'m, K, V, S, A, Q, R>
where
    K: Borrow<Q> + Clone,
    V: Clone,
    S: InlineKeyStrategy<K>,
    A: Allocator,
    Q: Ord + ?Sized,
    R: RangeBounds<Q>,
{
    pub(super) fn new(map: &'m BBTreeMap<K, V, S, A>, range: R) -> Self {
        Self {
            map,
            range,
            front: RangeEnd::new(),
            back: RangeEnd::new(),
            _query: PhantomData,
        }
    }

    /// The next entry from the front if `ascending`, else from the back.
    fn advance(&mut self, ascending: bool) -> Option<(K, V)> {
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
            None => self.copy(ascending),
        };
        let (end, other) = if ascending {
            (&mut self.front, &self.back)
        } else {
            (&mut self.back, &self.front)
        };
        // The other end may have handed the key out since the copy.
        let met_other = |key: &K| {
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
    fn copy(&mut self, ascending: bool) -> Option<Copied<'m, K, V>> {
        let Self {
            map,
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
        if copy_len == RangeEnd::<K, V, S>::MAX_COPY_LEN {
            end.resume.keep_path();
        }
        end.copy_len = (copy_len * 4).min(RangeEnd::<K, V, S>::MAX_COPY_LEN);
        let copied = &mut end.copied;
        copied.clear();
        let visit = |key: &K, value: &V, stamp: LeafStamp<'m>| {
            copied.push((key.clone(), value.clone(), stamp));
            if copied.len() == copy_len {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        };
        if ascending {
            map.tree.scan(from, to, Some(&mut end.resume), visit);
        } else {
            map.tree.scan_rev(to, from, Some(&mut end.resume), visit);
        }
        end.copied.reverse();
        end.copied.pop()
    }
}

impl<K, V, S, A, Q, R> Iterator for RangeCloned<'_, K, V, S, A, Q, R>
where
    K: Borrow<Q> + Clone,
    V: Clone,
    S: InlineKeyStrategy<K>,
    A: Allocator,
    Q: Ord + ?Sized,
    R: RangeBounds<Q>,
{
    type Item = (K, V);

    fn next(&mut self) -> Option<(K, V)> {
        self.advance(true)
    }
}

impl<K, V, S, A, Q, R> DoubleEndedIterator for RangeCloned<'_, K, V, S, A, Q, R>
where
    K: Borrow<Q> + Clone,
    V: Clone,
    S: InlineKeyStrategy<K>,
    A: Allocator,
    Q: Ord + ?Sized,
    R: RangeBounds<Q>,
{
    fn next_back(&mut self) -> Option<(K, V)> {
        self.advance(false)
    }
}
