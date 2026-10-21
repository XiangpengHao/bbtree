//! [`RangeCloned`]: clones of a range of a [`BBTreeMap`]'s entries, from
//! either end, one at a time, with no latch held between calls: the latched
//! tree's copies of a range (`tree/latched/copied_range.rs`), each entry
//! cloned.

use core::borrow::Borrow;
use core::marker::PhantomData;
use core::ops::RangeBounds;

use super::BBTreeMap;
use crate::allocator::Allocator;
use crate::inline_key::InlineKeyStrategy;
use crate::tree::latched::CopiedRange;

/// An iterator over clones of the entries of a range of a [`BBTreeMap`]:
/// in key order from the front, in reverse from the back. See
/// [`BBTreeMap::range_cloned`].
pub struct RangeCloned<'m, K, V, S: InlineKeyStrategy<K>, A: Allocator, Q: ?Sized, R> {
    entries: CopiedRange<'m, K, V, S, K, V, Q, R>,
    _map: PhantomData<&'m BBTreeMap<K, V, S, A>>,
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
            entries: CopiedRange::new(&map.tree, range),
            _map: PhantomData,
        }
    }

    /// A map's entry, copied out of its leaf as clones.
    fn clone_entry(key: &K, value: &V) -> (K, V) {
        (key.clone(), value.clone())
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
        self.entries.advance(true, Self::clone_entry)
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
        self.entries.advance(false, Self::clone_entry)
    }
}
