//! [`LeafAnnex`]: what a [`BBTreeMap`] keeps in each leaf for the code that
//! owns where its keys and values point, such as `bbtree-store`'s chain of
//! heap pages; and [`WithLeafAnnex`], the strategy that gives a map one.
//!
//! [`BBTreeMap`]: crate::BBTreeMap

use core::fmt;
use core::marker::PhantomData;

use crate::allocator::Allocator;
use crate::inline_key::InlineKeyStrategy;
use crate::inline_key::sealed::{InlineKeySource, Sealed};

/// Data a [`BBTreeMap`] keeps in each of its leaves, right after the leaf's
/// header, for the code that owns where its keys and values point: it reads
/// and changes it while an insert or a removal holds the leaf
/// ([`AnnexedLeaf`](crate::AnnexedLeaf)). The map keeps it with its leaf:
///
/// - a new tree's leaf gets `Default`'s;
/// - a split gives the new leaf, right of the one that splits, `Default`'s,
///   then calls [`Self::split`];
/// - a merge calls [`Self::merge`] on the leaf that stays with the one that
///   goes;
/// - dropping the map, or draining it, calls [`Self::release`] on each.
///
/// Entries also move between neighbouring leaves without a call, as a
/// removal rebalances them. Only a [`BBTreeMap`] keeps annexes: a map for
/// one thread or a file map with one fails to compile.
///
/// [`BBTreeMap`]: crate::BBTreeMap
pub trait LeafAnnex: Default + Send + Sync {
    /// `self`'s leaf split, into itself and `right`'s leaf, which took the
    /// entries right of the split.
    fn split(&mut self, right: &mut Self);

    /// `right`'s leaf merged into `self`'s, and goes.
    fn merge(&mut self, right: Self);

    /// `self`'s leaf goes with the map: gives what it holds back to
    /// `alloc`, the map's allocator.
    fn release(self, alloc: &impl Allocator);
}

/// No annex: what every map keeps but one with [`WithLeafAnnex`]. It takes
/// no room in a leaf.
impl LeafAnnex for () {
    #[inline(always)]
    fn split(&mut self, _right: &mut Self) {}

    #[inline(always)]
    fn merge(&mut self, _right: Self) {}

    #[inline(always)]
    fn release(self, _alloc: &impl Allocator) {}
}

/// The strategy whose inline keys and node config are strategy `S`'s, and
/// whose leaves keep an annex `L`: for a [`BBTreeMap`](crate::BBTreeMap).
pub struct WithLeafAnnex<S, L>(PhantomData<fn() -> (S, L)>);

impl<S, L> WithLeafAnnex<S, L> {
    #[must_use]
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}

// By hand: derives would bound `S` and `L`, which the marker doesn't hold.

impl<S, L> Clone for WithLeafAnnex<S, L> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<S, L> Copy for WithLeafAnnex<S, L> {}

impl<S, L> Default for WithLeafAnnex<S, L> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S, L> fmt::Debug for WithLeafAnnex<S, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WithLeafAnnex")
    }
}

impl<K, S: InlineKeyStrategy<K>, L: LeafAnnex> Sealed<K> for WithLeafAnnex<S, L> {
    const SOURCE: InlineKeySource<K> = S::SOURCE;
    type Config = S::Config;
}

impl<K, S: InlineKeyStrategy<K>, L: LeafAnnex> InlineKeyStrategy<K> for WithLeafAnnex<S, L> {
    type LeafAnnex = L;
}
