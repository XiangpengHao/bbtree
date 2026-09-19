//! Taking an owned tree apart: [`Subtree::destroy`].

#![forbid(unsafe_code)]

use super::handle::{ForceResult, NodeKind, Subtree};
use super::page::FreshPage;

impl<K, V, Kind: NodeKind> Subtree<K, V, Kind> {
    /// Drops every key and value, in descending key order, and hands every
    /// page to `release`.
    ///
    /// If a key's or value's `Drop` panics, the rest of the tree is still
    /// taken apart before the panic goes on, as in std's collections: either
    /// way, every key and value is dropped once and every page released once.
    /// A second panic while doing so aborts, as any panic during unwinding
    /// does.
    pub(crate) fn destroy(self, mut release: impl FnMut(FreshPage<K, V>)) {
        Teardown {
            rest: Some(self.forget_type()),
            release: &mut release,
        }
        .run();
    }
}

/// What is left of a tree being taken apart, and where its pages go.
///
/// Dropping it takes apart whatever is left: that is how a panicking key or
/// value `Drop` still leaves nothing behind.
struct Teardown<'r, K, V, Release: FnMut(FreshPage<K, V>)> {
    rest: Option<Subtree<K, V>>,
    release: &'r mut Release,
}

impl<K, V, Release: FnMut(FreshPage<K, V>)> Teardown<'_, K, V, Release> {
    /// Removes the root's entries from last to first, taking apart the child
    /// right of each, then releases the root and goes on with its only child.
    ///
    /// User `Drop` code runs only on entries already out of the tree, while
    /// `rest` holds what is left of it.
    fn run(&mut self) {
        while let Some(subtree) = self.rest.take() {
            match subtree.force() {
                ForceResult::Leaf(mut leaf) => match leaf.borrow_mut().last_kv() {
                    Some(last) => {
                        let (key_value, _) = last.remove();
                        self.rest = Some(leaf.forget_type());
                        drop(key_value);
                    }
                    None => (self.release)(leaf.into_page()),
                },
                ForceResult::Internal(mut internal) => match internal.borrow_mut().last_kv() {
                    Some(last) => {
                        let (key_value, right_child) = last.remove_with_right_child();
                        self.rest = Some(internal.forget_type());
                        Teardown {
                            rest: Some(right_child),
                            release: &mut *self.release,
                        }
                        .run();
                        drop(key_value);
                    }
                    None => {
                        let (only_child, page) = internal.pop_internal_level();
                        self.rest = Some(only_child);
                        (self.release)(page);
                    }
                },
            }
        }
    }
}

impl<K, V, Release: FnMut(FreshPage<K, V>)> Drop for Teardown<'_, K, V, Release> {
    fn drop(&mut self) {
        // Nothing is left unless a panic cut `run` short.
        self.run();
    }
}

#[cfg(test)]
mod tests {
    use core::panic::AssertUnwindSafe;
    use std::panic::catch_unwind;
    use std::vec::Vec;

    use crate::node::page::TestPages;
    use crate::node::test_support::{Bulky, DropLedger, TestRng, TestTree, Tracked, TrackedValue};

    type BulkyTree<'p, 'l> = TestTree<'p, Tracked<'l>, Bulky<'l>>;

    /// What, if anything, panics when dropped.
    #[derive(Clone, Copy)]
    enum PanickingDrop {
        Nothing,
        Key(usize),
        Value(usize),
    }

    /// A tree of `count` entries inserted in random order. Keys are ordered
    /// by id, so key ids ascend with key order.
    fn random_tree<'p, 'l>(
        pages: &'p TestPages<Tracked<'l>, Bulky<'l>>,
        ledger: &'l DropLedger,
        rng: &TestRng,
        count: usize,
        panicking: PanickingDrop,
    ) -> BulkyTree<'p, 'l> {
        let mut entries: Vec<(Tracked<'l>, Bulky<'l>)> = (0..count)
            .map(|index| match panicking {
                PanickingDrop::Key(position) if position == index => (
                    ledger.track_panicking_on_drop(),
                    Bulky::from_tracked(ledger.track()),
                ),
                _ => (ledger.track(), Bulky::from_tracked(ledger.track())),
            })
            .collect();
        if let PanickingDrop::Value(position) = panicking {
            entries[position].1 = Bulky::from_tracked(ledger.track_panicking_on_drop());
        }
        rng.shuffle(&mut entries);
        let mut tree = TestTree::new(pages);
        for (key, value) in entries {
            assert!(tree.insert(key, value).is_none());
        }
        tree
    }

    #[test]
    fn destroy_drops_every_entry_and_releases_every_page() {
        let rng = TestRng::new(9);
        let largest = if cfg!(miri) { 150 } else { 3000 };
        for count in [0, 1, 12, 13, 100, largest] {
            let ledger = DropLedger::new();
            let pages = TestPages::new();
            let tree = random_tree(&pages, &ledger, &rng, count, PanickingDrop::Nothing);
            assert_eq!(tree.entries(|_, _| ()).len(), count);
            assert!(pages.in_use() >= 1);
            tree.destroy();
            pages.assert_all_released();
            ledger.assert_all_dropped_once();
        }
    }

    #[test]
    fn destroy_drops_keys_in_descending_order() {
        let ledger = DropLedger::new();
        let pages = TestPages::new();
        let tree = random_tree(
            &pages,
            &ledger,
            &TestRng::new(10),
            300,
            PanickingDrop::Nothing,
        );
        assert!(tree.height() >= 2);
        let key_ids: Vec<usize> = tree.entries(|key, _| key.id());
        tree.destroy();
        let dropped_keys: Vec<usize> = ledger
            .drop_order()
            .into_iter()
            .filter(|id| key_ids.contains(id))
            .collect();
        assert_eq!(dropped_keys, key_ids.into_iter().rev().collect::<Vec<_>>());
        pages.assert_all_released();
        ledger.assert_all_dropped_once();
    }

    #[test]
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
                let pages = TestPages::new();
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
        let pages = TestPages::new();
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
