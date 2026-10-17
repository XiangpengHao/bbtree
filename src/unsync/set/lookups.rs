//! Looking up an iterator's elements in a large set a batch at a time, for
//! the operations on two sets that search one for the other's elements
//! (`intersection`, `difference`, `is_subset`): a batch's lookups overlap
//! their waits for memory (doc 12). The iterator still hands out its
//! elements one by one, so the operations print and hint as std's, which
//! look each up alone.

#![forbid(unsafe_code)]

use super::{BBTreeSet, Iter};
use crate::allocator::Allocator;
use crate::inline_key::InlineKeyStrategy;

/// Whether each of an iterator's next elements is in a set, looked up ahead
/// of it: bit `i` of `found` for its `i`th next element, of the first
/// `looked_up`.
#[derive(Clone, Copy, Default)]
pub(super) struct LookupsAhead {
    found: u32,
    looked_up: u8,
}

impl LookupsAhead {
    /// How many elements one batch looks up.
    const BATCH: usize = 32;

    /// The next element of `elements`, and whether `set` holds it. While
    /// the lookups ahead last, `elements` must be the iterator they were
    /// made for, at the same position.
    #[inline]
    pub(super) fn next<'a, T, S, A>(
        &mut self,
        elements: &mut Iter<'a, T, S>,
        set: &BBTreeSet<T, S, A>,
    ) -> Option<(&'a T, bool)>
    where
        T: Ord,
        S: InlineKeyStrategy<T>,
        A: Allocator + Clone,
    {
        if self.looked_up == 0 && elements.len() >= Self::BATCH && set.map.batches() {
            self.look_up(elements.clone(), set);
        }
        let element = elements.next()?;
        if self.looked_up == 0 {
            return Some((element, set.contains(element)));
        }
        let found = self.found & 1 != 0;
        self.found >>= 1;
        self.looked_up -= 1;
        Some((element, found))
    }

    /// Looks up the next batch of `ahead`, which has one, in `set`.
    #[inline(never)]
    fn look_up<T, S, A>(&mut self, mut ahead: Iter<'_, T, S>, set: &BBTreeSet<T, S, A>)
    where
        T: Ord,
        S: InlineKeyStrategy<T>,
        A: Allocator + Clone,
    {
        let batch: [&T; Self::BATCH] =
            core::array::from_fn(|_| ahead.next().expect("a whole batch is left"));
        self.found = (set.contains_many(batch).into_iter().enumerate())
            .fold(0, |bits, (index, found)| bits | u32::from(found) << index);
        self.looked_up = Self::BATCH as u8;
    }
}
