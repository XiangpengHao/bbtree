//! A leaf's annex (`leaf_annex.rs`) as the node code keeps it: made with
//! the leaf ([`NodePtr::initialize`]), moved with its bytes, split, merged
//! and released here; and [`AnnexedLeaf`], the leaf an insert or a removal
//! hands its caller's closure, with its annex.
//!
//! [`NodePtr::initialize`]: super::page::NodePtr

use core::fmt;
use core::marker::PhantomData;

use super::latch::Latched;
use super::leaf::Leaf;
use crate::allocator::Allocator;
use crate::inline_key::InlineKeyStrategy;
use crate::leaf_annex::LeafAnnex;

impl<K, V, S: InlineKeyStrategy<K>> Leaf<K, V, S> {
    /// Calls the annex's split, this leaf's with `right`'s, the new leaf
    /// right of it, once the entries right of the split moved there.
    #[inline(always)]
    pub(super) fn split_annex(self, right: Self) {
        // SAFETY: both leaves are initialized, so are their annexes, and
        // they are apart; the split has exclusive access to both.
        unsafe {
            (self.node_ptr().leaf_annex().as_mut()).split(right.node_ptr().leaf_annex().as_mut())
        }
    }

    /// Calls the annex's merge, this leaf's with `right`'s, which merged
    /// into this one and goes: its annex moves out.
    #[inline(always)]
    pub(super) fn merge_annex(self, right: Self) {
        // SAFETY: as for `split_annex`; `right`'s page is freed next, and
        // nothing reads its annex again.
        unsafe {
            let right = right.node_ptr().leaf_annex().read();
            self.node_ptr().leaf_annex().as_mut().merge(right);
        }
    }

    /// Releases the annex, its leaf's page about to be freed with the map,
    /// to `alloc`, the map's allocator: it moves out.
    ///
    /// # Safety
    ///
    /// The leaf is initialized, the caller's alone, and freed next without
    /// its annex being read again.
    #[inline(always)]
    pub(in crate::tree) unsafe fn release_annex(self, alloc: &impl Allocator) {
        // SAFETY: the annex is initialized, and moves out once (caller).
        unsafe { self.node_ptr().leaf_annex().read() }.release(alloc);
    }
}

/// A leaf of a [`BBTreeMap`](crate::BBTreeMap) with strategy `S`, that an
/// insert or a removal holds exclusively for `'a`, while it runs its
/// caller's closure ([`BBTreeMap::update_or_insert_with_leaf`],
/// [`BBTreeMap::remove_with_leaf`]): the leaf's annex ([`LeafAnnex`]), and
/// its keys. No other thread reads or changes them meanwhile.
///
/// [`BBTreeMap::update_or_insert_with_leaf`]: crate::BBTreeMap::update_or_insert_with_leaf
/// [`BBTreeMap::remove_with_leaf`]: crate::BBTreeMap::remove_with_leaf
pub struct AnnexedLeaf<'a, K, V, S: InlineKeyStrategy<K>> {
    leaf: Leaf<K, V, Latched<S>>,
    _held: PhantomData<&'a mut S::LeafAnnex>,
}

impl<'a, K, V, S: InlineKeyStrategy<K>> AnnexedLeaf<'a, K, V, S> {
    /// `leaf`, which the caller holds exclusively for `'a`.
    pub(in crate::tree) fn new(leaf: Leaf<K, V, Latched<S>>) -> Self {
        Self {
            leaf,
            _held: PhantomData,
        }
    }

    /// The leaf's annex.
    pub fn annex(&mut self) -> &mut S::LeafAnnex {
        // SAFETY: the leaf is initialized, so is its annex, which the holder
        // alone reaches, through `self` alone, for `'a` (type invariant).
        unsafe { self.leaf.node_ptr().leaf_annex().as_mut() }
    }

    /// The leaf's keys, in order: its head's first, if it has one, the
    /// separator's left of it (D49). They stay put while the leaf is held.
    pub fn keys(&self) -> impl Iterator<Item = &'a K> + use<'a, K, V, S>
    where
        K: 'a,
    {
        let leaf = self.leaf;
        let head = leaf.has_head().then(|| leaf.head_key());
        head.into_iter()
            .chain((0..leaf.len()).map(move |index| leaf.key(index)))
    }
}

impl<K, V, S: InlineKeyStrategy<K>> fmt::Debug for AnnexedLeaf<'_, K, V, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnnexedLeaf").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;
    use core::alloc::Layout;
    use core::ptr::NonNull;
    use std::collections::BTreeMap;

    use crate::allocator::Allocator;
    use crate::inline_key::Auto;
    use crate::leaf_annex::{LeafAnnex, WithLeafAnnex};
    use crate::map::BBTreeMap;
    use crate::tree::node::page::TestPages;
    use crate::tree::test_support::TestRng;

    /// The layout of the test annex's heap pages.
    const PAGE: Layout = Layout::new::<[u64; 8]>();

    /// An annex that holds a heap page for each entry its leaf took, as a
    /// store's chain holds its records' pages: a split hands the new leaf
    /// half of them, a merge takes the other leaf's, and the map's teardown
    /// gives them back.
    #[derive(Default)]
    struct PagePerEntry {
        pages: Vec<NonNull<u8>>,
    }

    // SAFETY: the pages are the map's allocator's, which only the leaf's
    // holder reaches.
    unsafe impl Send for PagePerEntry {}

    // SAFETY: as for `Send`.
    unsafe impl Sync for PagePerEntry {}

    impl LeafAnnex for PagePerEntry {
        fn split(&mut self, right: &mut Self) {
            right.pages = self.pages.split_off(self.pages.len() / 2);
        }

        fn merge(&mut self, right: Self) {
            self.pages.extend(right.pages);
        }

        fn release(self, alloc: &impl Allocator) {
            for page in self.pages {
                // SAFETY: the page came from the map's allocator, for
                // `PAGE`, and the annex held it alone.
                unsafe { alloc.deallocate_heap(page, PAGE) };
            }
        }
    }

    type AnnexedMap<'p> =
        BBTreeMap<u64, u64, WithLeafAnnex<Auto, PagePerEntry>, &'p TestPages<u64, u64>>;

    /// Inserts `key`, taking a heap page into the annex of the leaf it goes
    /// in; whether it was new.
    fn insert(map: &AnnexedMap<'_>, pages: &TestPages<u64, u64>, key: u64, value: u64) -> bool {
        let inserted = map.update_or_insert_with_leaf(
            key,
            value,
            |leaf, stored, stored_value| {
                assert!(leaf.keys().is_sorted() && leaf.keys().any(|key| key == stored));
                *stored_value = value;
                Ok(false)
            },
            |mut leaf, new, _| {
                assert!(leaf.keys().is_sorted() && leaf.keys().all(|key| key != new));
                let page = pages.allocate_heap(PAGE).map_err(|_| PAGE)?;
                leaf.annex().pages.push(page);
                Ok(true)
            },
        );
        inserted.expect("the test's pages have no budget")
    }

    /// Removes `key`, giving a heap page of its leaf's annex back, if it
    /// holds one: the map's teardown gives the rest back.
    fn remove(map: &AnnexedMap<'_>, pages: &TestPages<u64, u64>, key: u64) -> Option<u64> {
        let removed = map.remove_with_leaf(&key, |mut leaf, stored, _| {
            assert!(leaf.keys().is_sorted() && leaf.keys().any(|key| key == stored));
            if let Some(page) = leaf.annex().pages.pop() {
                // SAFETY: the page came from `pages`, for `PAGE`, and the
                // annex held it alone.
                unsafe { pages.deallocate_heap(page, PAGE) };
            }
        });
        removed.map(|(_, value)| value)
    }

    #[test]
    fn annexes_follow_their_leaves_through_splits_merges_and_teardown() {
        let pages = TestPages::<u64, u64>::new();
        let mut map: AnnexedMap<'_> = BBTreeMap::new_in(&pages);
        let rng = TestRng::new(7);
        let mut model = BTreeMap::new();
        let steps = if cfg!(miri) { 2_000 } else { 30_000 };
        for step in 0..steps {
            let key = rng.below_u64(3_000);
            if rng.below(3) < 2 {
                let new = insert(&map, &pages, key, step);
                assert_eq!(new, model.insert(key, step).is_none());
            } else {
                assert_eq!(remove(&map, &pages, key), model.remove(&key));
            }
        }
        map.check_invariants(model.len());
        // Half the entries move out, which frees leaves as the walk passes
        // them; the rest drop with the walk.
        let mut drained = map.into_iter();
        for expected in model.iter().take(model.len() / 2) {
            assert_eq!(drained.next(), Some((*expected.0, *expected.1)));
        }
        drop(drained);
        pages.assert_all_released();
    }

    #[test]
    fn a_refused_insert_changes_nothing() {
        let pages = TestPages::<u64, u64>::new();
        let mut map: AnnexedMap<'_> = BBTreeMap::new_in(&pages);
        let rng = TestRng::new(8);
        let mut model = BTreeMap::new();
        for step in 0..2_000 {
            let key = rng.below_u64(4_000);
            insert(&map, &pages, key, step);
            model.insert(key, step);
        }
        // New keys land in leaves with room, full leaves that would split,
        // and after the last key; held keys refuse on the update.
        for key in (0..4_100).step_by(3) {
            let refused = map.update_or_insert_with_leaf(
                key,
                0,
                |_, _, _| -> Result<(), Layout> { Err(PAGE) },
                |_, _, _| Err(PAGE),
            );
            let (refused_key, _) = refused.expect_err("the closures refuse").into_entry();
            assert_eq!(refused_key, key);
        }
        map.check_invariants(model.len());
        for (key, value) in &model {
            assert_eq!(map.get_copied(key), Some(*value));
        }
        drop(map);
        pages.assert_all_released();
    }
}
