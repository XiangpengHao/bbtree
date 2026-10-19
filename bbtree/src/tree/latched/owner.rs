//! What a latched tree's owner does with it, through `&mut` or by value.
//! No other thread reaches the tree then, so the unsync tree's own lookups
//! and walks run on it without a latch. They hand out references for as
//! long as the owner lends the tree.

use core::borrow::Borrow;
use core::ops::Bound;
use core::ptr;

use super::super::node::inline_keys::SearchKey;
use super::super::node::internal::Internal;
use super::super::node::latch::Latched;
use super::super::node::leaf::Leaf;
use super::super::node::page::PageType;
use super::super::node_stack::ValMut;
use super::super::search::Found;
use super::super::walk::{DyingWalk, RangeWalk};
use super::LatchedTree;
use crate::allocator::Allocator;
use crate::inline_key::InlineKeyStrategy;

impl<K, V, S: InlineKeyStrategy<K>> LatchedTree<K, V, S> {
    /// The value of the entry whose key equals `key`, exclusive, as the
    /// unsync tree's lookup finds it; `None` without one.
    pub(crate) fn find_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let root_page = self.root_page()?;
        let key = SearchKey::new(key);
        if root_page.shared_prefix().place_outside(&key).is_some() {
            return None;
        }
        root_page.node().find(&key, ()).map(Found::value_mut)
    }

    /// A walk over every entry, values exclusive, from both ends, as
    /// `unsync::BBTreeMap::range_mut(..)` walks.
    pub(crate) fn walk_mut(&mut self) -> RangeWalk<ValMut<'_, K, V>, K, V, Latched<S>>
    where
        K: Ord,
    {
        match self.root_page() {
            Some(root_page) => RangeWalk::new::<K>(root_page, Bound::Unbounded, Bound::Unbounded),
            None => RangeWalk::EMPTY,
        }
    }

    /// Takes the tree out, leaving none, as a walk that hands out its
    /// entries in key order and gives each page back to `alloc` once past
    /// it, as `unsync::BBTreeMap::into_iter` does. The pages on the free
    /// lists go back to `alloc` first.
    pub(crate) fn take_dying_walk<A: Allocator>(
        &mut self,
        alloc: A,
    ) -> DyingWalk<K, V, Latched<S>, A> {
        let root_page = self.root_page();
        *self.root.get_mut() = ptr::null_mut();
        *self.rightmost_leaf.get_mut() = ptr::null_mut();
        self.free.deallocate_all(
            &alloc,
            Leaf::<K, V, Latched<S>>::page_layout(),
            Internal::<K, V, Latched<S>>::page_layout(),
        );
        // A walk that only its front moves needs no count of the entries,
        // which the tree keeps none of.
        DyingWalk::new(root_page, usize::MAX, alloc)
    }
}
