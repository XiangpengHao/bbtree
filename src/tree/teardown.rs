//! Taking an owned tree apart: [`Subtree::destroy`], a walk that moves
//! every entry out from the front and frees each node once past it
//! ([`DyingWalk`]), dropped at once.

#![forbid(unsafe_code)]

use super::node::handle::{ForceResult, LeafOrInternal, NodeKind, NodeMut, Subtree};
use super::node::page::PageByKind;
use super::walk::DyingWalk;
use crate::inline_key::InlineKeyStrategy;

impl<K, V, S: InlineKeyStrategy<K>, Kind: NodeKind> Subtree<K, V, S, Kind> {
    /// Drops every key and value in ascending key order, each key before its
    /// value, as std's `BTreeMap` does (D1), and hands every page to
    /// `release`, sorted by the kind of node it held. A separator's value is
    /// its right leaf's head (D17), dropped right after the separator's key;
    /// a head on the leftmost leaf, the value of a separator kept elsewhere,
    /// as a split's right half has, goes first, its key, where separator keys
    /// are heads (D49), before it.
    ///
    /// The tree must be well-formed: every leaf right of a separator has a
    /// head (D17).
    ///
    /// If a key's or value's `Drop` panics, the rest of the tree is still
    /// taken apart, in the same order, before the panic goes on, as in std's
    /// collections: either way, every key and value is dropped once and every
    /// page released once. A second panic while doing so aborts, as any panic
    /// during unwinding does.
    pub(crate) fn destroy(self, release: impl FnMut(PageByKind<K, V, S>)) {
        let mut tree = self.forget_type();
        let head = tree.borrow_mut().take_leftmost_head();
        // Without a count of the entries, the walk ends where the tree does:
        // only its front moves.
        let walk = DyingWalk::new(Some(tree), usize::MAX, release);
        drop(head);
        drop(walk);
    }
}

impl<K, V, S: InlineKeyStrategy<K>> NodeMut<'_, K, V, S, LeafOrInternal> {
    /// Takes the head out of the leftmost leaf under this node, if it has
    /// one, with its key where separator keys are heads (D49).
    fn take_leftmost_head(self) -> Option<(Option<K>, V)> {
        let mut node = self;
        loop {
            node = match node.force() {
                ForceResult::Leaf(leaf) => {
                    return leaf.head().map(|head| {
                        let (key, value, _) = head.remove();
                        (key, value)
                    });
                }
                ForceResult::Internal(internal) => internal.first_edge().descend(),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use core::panic::AssertUnwindSafe;
    use std::panic::catch_unwind;
    use std::vec::Vec;

    use crate::inline_key::{Auto, InlineKeyStrategy, WithInlineKey};
    use crate::tree::node::page::TestPages;
    use crate::tree::test_support::{Bulky, DropLedger, FromTracked, TestRng, TestTree, Tracked};

    /// What, if anything, panics when dropped.
    #[derive(Clone, Copy)]
    enum PanickingDrop {
        Nothing,
        Key(usize),
        Value(usize),
    }

    /// A tree of `count` entries inserted in random order. Keys are ordered
    /// by id, so key ids ascend with key order.
    fn random_tree<'p, 'l, K: FromTracked<'l> + Ord, S: InlineKeyStrategy<K>>(
        pages: &'p TestPages<K, Tracked<'l>, S>,
        ledger: &'l DropLedger,
        rng: &TestRng,
        count: usize,
        panicking: PanickingDrop,
    ) -> TestTree<'p, K, Tracked<'l>, S> {
        let mut entries: Vec<(K, Tracked<'l>)> = (0..count)
            .map(|index| match panicking {
                PanickingDrop::Key(position) if position == index => (
                    K::from_tracked(ledger.track_panicking_on_drop()),
                    ledger.track(),
                ),
                PanickingDrop::Value(position) if position == index => (
                    K::from_tracked(ledger.track()),
                    ledger.track_panicking_on_drop(),
                ),
                _ => (K::from_tracked(ledger.track()), ledger.track()),
            })
            .collect();
        rng.shuffle(&mut entries);
        let mut tree = TestTree::new(pages);
        for (key, value) in entries {
            assert!(tree.insert(key, value).is_none());
        }
        tree
    }

    /// Builds a random tree of `count` entries and destroys it: every entry
    /// is dropped once, and every page released.
    fn destroy_random_tree<'l, K: FromTracked<'l> + Ord, S: InlineKeyStrategy<K>>(
        ledger: &'l DropLedger,
        rng: &TestRng,
        count: usize,
    ) {
        let pages = TestPages::<K, Tracked<'l>, S>::new();
        let tree = random_tree(&pages, ledger, rng, count, PanickingDrop::Nothing);
        assert_eq!(tree.entries(|_, _| ()).len(), count);
        assert!(pages.in_use() >= 1);
        tree.destroy();
        pages.assert_all_released();
        ledger.assert_all_dropped_once();
    }

    #[test]
    fn destroy_drops_every_entry_and_releases_every_page() {
        let rng = TestRng::new(9);
        let largest = if cfg!(miri) { 150 } else { 3000 };
        for count in [0, 1, 12, 13, 100, largest] {
            destroy_random_tree::<Bulky<'_>, Auto>(&DropLedger::new(), &rng, count);
            destroy_random_tree::<Tracked<'_>, WithInlineKey>(&DropLedger::new(), &rng, count);
        }
    }

    /// Builds a random tree of `count` entries, at least 2 levels high, and
    /// checks that destroying it drops its entries in ascending order.
    fn destroy_in_order<'l, K: FromTracked<'l> + Ord, S: InlineKeyStrategy<K>>(
        ledger: &'l DropLedger,
        count: usize,
    ) {
        let pages = TestPages::<K, Tracked<'l>, S>::new();
        let rng = TestRng::new(10);
        let tree = random_tree(&pages, ledger, &rng, count, PanickingDrop::Nothing);
        assert!(tree.height() >= 2);
        let entry_ids: Vec<usize> = tree
            .entries(|key, value| [key.tracked_id(), value.id()])
            .concat();
        tree.destroy();
        assert_eq!(ledger.drop_order(), entry_ids);
        pages.assert_all_released();
        ledger.assert_all_dropped_once();
    }

    #[test]
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
    fn destroy_drops_entries_in_ascending_order() {
        destroy_in_order::<Bulky<'_>, Auto>(&DropLedger::new(), 300);
        // 25 entries in a leaf with inline keys, and 63 children in an
        // internal node, which keeps no keys (D49): height 2 takes more. The
        // tree is the same under Miri, so the native run checks its height.
        destroy_in_order::<Tracked<'_>, WithInlineKey>(&DropLedger::new(), 2000);
    }

    #[test]
    #[cfg_attr(panic = "abort", ignore = "needs unwinding")]
    fn destroy_keeps_ascending_order_after_a_panicking_drop() {
        let rng = TestRng::new(13);
        for position in [0, 123, 299] {
            for panicking in [PanickingDrop::Key(position), PanickingDrop::Value(position)] {
                let ledger = DropLedger::new();
                let pages = TestPages::<Bulky<'_>, Tracked<'_>>::new();
                let tree = random_tree(&pages, &ledger, &rng, 300, panicking);
                assert!(tree.height() >= 2);
                let entry_ids: Vec<usize> = tree
                    .entries(|key, value| [key.tracked_id(), value.id()])
                    .concat();
                let unwound = catch_unwind(AssertUnwindSafe(|| tree.destroy()));
                assert!(unwound.is_err());
                assert_eq!(ledger.drop_order(), entry_ids);
                pages.assert_all_released();
                ledger.assert_all_dropped_once();
            }
        }
    }

    #[test]
    fn destroy_releases_every_page_of_entries_without_drop_glue() {
        let rng = TestRng::new(14);
        let count = if cfg!(miri) { 300 } else { 3000 };
        let mut keys: Vec<u64> = (0..count).collect();
        rng.shuffle(&mut keys);
        // 64-byte keys: 14 entries per node of either kind, so a tall tree.
        let pages = TestPages::<[u64; 8], u64>::new();
        let mut tree = TestTree::new(&pages);
        for key in keys {
            assert!(tree.insert([key; 8], key).is_none());
        }
        assert!(tree.height() >= 2);
        tree.destroy();
        pages.assert_all_released();
    }

    #[test]
    #[cfg_attr(panic = "abort", ignore = "needs unwinding")]
    fn destroy_finishes_after_a_panicking_drop() {
        let rng = TestRng::new(11);
        let count = if cfg!(miri) { 60 } else { 300 };
        let positions = if cfg!(miri) {
            std::vec![0, count / 2, count - 1]
        } else {
            (0..count).step_by(7).collect()
        };
        for position in positions {
            for panicking in [PanickingDrop::Key(position), PanickingDrop::Value(position)] {
                let ledger = DropLedger::new();
                let pages = TestPages::<Bulky<'_>, Tracked<'_>>::new();
                let tree = random_tree(&pages, &ledger, &rng, count, panicking);
                assert!(tree.height() >= 1);
                let unwound = catch_unwind(AssertUnwindSafe(|| tree.destroy()));
                assert!(unwound.is_err());
                pages.assert_all_released();
                ledger.assert_all_dropped_once();
            }
        }
    }

    #[test]
    fn released_pages_build_the_next_tree() {
        let ledger = DropLedger::new();
        let pages = TestPages::<Bulky<'_>, Tracked<'_>>::new();
        let rng = TestRng::new(12);
        let tree = random_tree(&pages, &ledger, &rng, 100, PanickingDrop::Nothing);
        let allocated = pages.allocated_count();
        tree.destroy();
        assert_eq!(pages.in_use(), 0);

        let tree = random_tree(&pages, &ledger, &rng, 100, PanickingDrop::Nothing);
        assert!(pages.allocated_count() <= allocated + 2);
        assert!(pages.in_use() >= allocated / 2);
        tree.destroy();
        pages.assert_all_released();
        ledger.assert_all_dropped_once();
    }
}
