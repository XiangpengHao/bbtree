//! A [`BBTreeMap`]'s operations that hand their closure the leaf they hold,
//! with its annex ([`AnnexedLeaf`], [`LeafAnnex`]): for the code that owns
//! where the map's keys and values point, such as `bbtree-store`, which
//! keeps each leaf's records in heap pages its annex chains.
//!
//! [`LeafAnnex`]: crate::LeafAnnex

use core::alloc::Layout;
use core::borrow::Borrow;

use super::BBTreeMap;
use crate::allocator::{Allocator, InsertError};
use crate::inline_key::InlineKeyStrategy;
use crate::tree::latched::WithLeaf;
use crate::tree::node::annexed_leaf::AnnexedLeaf;

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator> BBTreeMap<K, V, S, A> {
    /// Runs `occupied` on the entry whose key equals `key`, its value
    /// exclusive, or, if the map holds none, `vacant` on `key` and `value`,
    /// which go in right after; each with the leaf the entry is in, or goes
    /// in, and returns what it returned. Either runs while no other thread
    /// reads or changes the leaf. Once `occupied` runs, `key` and `value`
    /// drop.
    ///
    /// # Errors
    ///
    /// If the allocator has no page for a node the insert makes, or the
    /// closure refuses, for want of a page of the layout it returns, having
    /// changed nothing: the map is unchanged, and the entry comes back
    /// ([`InsertError`]). A node page is taken before `vacant` runs.
    #[inline]
    pub fn update_or_insert_with_leaf<R>(
        &self,
        key: K,
        value: V,
        occupied: impl FnOnce(AnnexedLeaf<'_, K, V, S>, &K, &mut V) -> Result<R, Layout>,
        vacant: impl FnOnce(AnnexedLeaf<'_, K, V, S>, &K, &V) -> Result<R, Layout>,
    ) -> Result<R, InsertError<K, V>>
    where
        K: Ord,
    {
        self.tree
            .insert(key, value, WithLeaf(occupied, vacant), &self.alloc)
    }

    /// Removes the entry whose key equals `key`, and returns it, with the
    /// key as the map stored it; `None` without one. `f` runs once on it,
    /// with the leaf it is in, right before it comes out, while no other
    /// thread reads or changes the leaf.
    #[inline]
    pub fn remove_with_leaf<Q>(
        &self,
        key: &Q,
        f: impl FnOnce(AnnexedLeaf<'_, K, V, S>, &K, &V),
    ) -> Option<(K, V)>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.tree.remove_entry(key, |_, _| true, f, &self.alloc)
    }
}
