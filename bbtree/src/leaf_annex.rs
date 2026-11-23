//! [`LeafAnnex`]: what a [`BBTreeMap`] keeps in each leaf for the code that
//! owns where its keys and values point, such as `bbtree-store`'s chain of
//! heap pages; and [`WithLeafAnnex`], the strategy that gives a map one.
//!
//! [`BBTreeMap`]: crate::BBTreeMap
//! [`WithLeafAnnex`]: crate::WithLeafAnnex

use crate::allocator::Allocator;

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
///
/// [`WithLeafAnnex`]: crate::WithLeafAnnex
impl LeafAnnex for () {
    #[inline(always)]
    fn split(&mut self, _right: &mut Self) {}

    #[inline(always)]
    fn merge(&mut self, _right: Self) {}

    #[inline(always)]
    fn release(self, _alloc: &impl Allocator) {}
}
