//! Taking an owned tree apart: [`Subtree::destroy`].

#![forbid(unsafe_code)]

use core::mem::needs_drop;

use super::handle::{ForceResult, LeafOrInternal, NodeKind, NodeMut, Subtree};
use super::page::PageByKind;

impl<K, V, Kind: NodeKind> Subtree<K, V, Kind> {
    /// Drops every key and value in ascending key order, each key before its
    /// value, as std's `BTreeMap` does (D1), and hands every page to
    /// `release`, sorted by the kind of node it held. A separator's value is
    /// its right leaf's head (D17), dropped right after the separator.
    ///
    /// If a key's or value's `Drop` panics, the rest of the tree is still
    /// taken apart, in the same order, before the panic goes on, as in std's
    /// collections: either way, every key and value is dropped once and every
    /// page released once. A second panic while doing so aborts, as any panic
    /// during unwinding does.
    pub(crate) fn destroy(self, mut release: impl FnMut(PageByKind<K, V>)) {
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
struct Teardown<'r, K, V, Release: FnMut(PageByKind<K, V>)> {
    rest: Option<Subtree<K, V>>,
    release: &'r mut Release,
}

impl<K, V, Release: FnMut(PageByKind<K, V>)> Teardown<'_, K, V, Release> {
    /// Drops the entries of `rest` in ascending order, then releases its
    /// pages.
    ///
    /// Entries without drop glue skip the in-order pass: the order of their
    /// drops is unobservable, and removing every entry from the front of its
    /// node shifts the rest of the node each time, which the release pass,
    /// removing from the back, avoids.
    fn run(&mut self) {
        if (needs_drop::<K>() || needs_drop::<V>())
            && let Some(tree) = &mut self.rest
        {
            Self::drop_entries_in_order(tree.borrow_mut(), self.release);
        }
        self.release_pages();
    }

    /// Drops every entry below `node` in ascending order, leaving its nodes
    /// empty and still linked through their first edges.
    ///
    /// The first child is emptied in place; each later child comes out with
    /// the separator left of it, whose key is dropped before the child is
    /// taken apart. The child's teardown starts at its leftmost leaf, with
    /// the head: the separator's value (D17). User `Drop` code runs only on
    /// keys and values already out of the tree, and while a [`Teardown`] owns
    /// every detached child.
    fn drop_entries_in_order(node: NodeMut<'_, K, V, LeafOrInternal>, release: &mut Release) {
        match node.force() {
            ForceResult::Leaf(mut leaf) => {
                if let Some(head) = leaf.reborrow_mut().head() {
                    let (value, _) = head.remove();
                    drop(value);
                }
                while let Some(first) = leaf.reborrow_mut().first_kv() {
                    let (key_value, _) = first.remove();
                    drop(key_value);
                }
            }
            ForceResult::Internal(mut internal) => {
                let first_child = internal.reborrow_mut().first_edge().descend();
                Self::drop_entries_in_order(first_child, release);
                while let Some(first) = internal.reborrow_mut().first_kv() {
                    let (separator, right_child) = first.remove_with_right_child();
                    let mut right_child = Teardown {
                        rest: Some(right_child),
                        release: &mut *release,
                    };
                    drop(separator);
                    right_child.run();
                }
            }
        }
    }

    /// Removes the root's entries from last to first, taking apart the child
    /// right of each, then releases the root and goes on with its only child.
    /// A leaf gives up its head last.
    ///
    /// Any key or value still in the tree has no drop glue (see
    /// [`Self::run`]), so dropping it here does nothing.
    fn release_pages(&mut self) {
        while let Some(subtree) = self.rest.take() {
            match subtree.force() {
                ForceResult::Leaf(mut leaf) => match leaf.borrow_mut().last_kv() {
                    Some(last) => {
                        let (key_value, _) = last.remove();
                        self.rest = Some(leaf.forget_type());
                        drop(key_value);
                    }
                    None => match leaf.borrow_mut().head() {
                        Some(head) => {
                            let (value, _) = head.remove();
                            self.rest = Some(leaf.forget_type());
                            drop(value);
                        }
                        None => (self.release)(leaf.into_page().into()),
                    },
                },
                ForceResult::Internal(mut internal) => match internal.borrow_mut().last_kv() {
                    Some(last) => {
                        let (separator, right_child) = last.remove_with_right_child();
                        self.rest = Some(internal.forget_type());
                        drop(separator);
                        Teardown {
                            rest: Some(right_child),
                            release: &mut *self.release,
                        }
                        .release_pages();
                    }
                    None => {
                        let (only_child, page) = internal.pop_internal_level();
                        self.rest = Some(only_child);
                        (self.release)(page.into());
                    }
                },
            }
        }
    }
}

impl<K, V, Release: FnMut(PageByKind<K, V>)> Drop for Teardown<'_, K, V, Release> {
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
    use crate::node::test_support::{Bulky, DropLedger, FromTracked, TestRng, TestTree, Tracked};

    type BulkyTree<'p, 'l> = TestTree<'p, Bulky<'l>, Tracked<'l>>;

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
        pages: &'p TestPages<Bulky<'l>, Tracked<'l>>,
        ledger: &'l DropLedger,
        rng: &TestRng,
        count: usize,
        panicking: PanickingDrop,
    ) -> BulkyTree<'p, 'l> {
        let mut entries: Vec<(Bulky<'l>, Tracked<'l>)> = (0..count)
            .map(|index| match panicking {
                PanickingDrop::Key(position) if position == index => (
                    Bulky::from_tracked(ledger.track_panicking_on_drop()),
                    ledger.track(),
                ),
                _ => (Bulky::from_tracked(ledger.track()), ledger.track()),
            })
            .collect();
        if let PanickingDrop::Value(position) = panicking {
            entries[position].1 = ledger.track_panicking_on_drop();
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
    fn destroy_drops_entries_in_ascending_order() {
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
        let entry_ids: Vec<usize> = tree
            .entries(|key, value| [key.tracked_id(), value.id()])
            .concat();
        tree.destroy();
        assert_eq!(ledger.drop_order(), entry_ids);
        pages.assert_all_released();
        ledger.assert_all_dropped_once();
    }

    #[test]
    fn destroy_keeps_ascending_order_after_a_panicking_drop() {
        let rng = TestRng::new(13);
        for position in [0, 123, 299] {
            let ledger = DropLedger::new();
            let pages = TestPages::new();
            let tree = random_tree(&pages, &ledger, &rng, 300, PanickingDrop::Key(position));
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
