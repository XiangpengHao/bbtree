//! Leaves as the tree uses them: insert, splitting a full leaf first. Also
//! the tests of the leaf operations in `handle.rs`.

#![forbid(unsafe_code)]

use super::handle::{Edge, EdgeRoom, Insertion, Leaf, NodeMut};
use super::page::FreshPage;

impl<'a, K, V> Edge<NodeMut<'a, K, V, Leaf>> {
    /// Inserts `key`/`value` at this edge, splitting the leaf first if it is
    /// full, with the right half in a page from `new_page`.
    pub(crate) fn insert(
        self,
        key: K,
        value: V,
        new_page: impl FnOnce() -> FreshPage<K, V>,
    ) -> Insertion<'a, K, V, Leaf> {
        match self.check_room() {
            EdgeRoom::Fits(edge) => Insertion::Fit(edge.insert_fit(key, value)),
            EdgeRoom::Full(edge) => Insertion::Split(edge.split_and_insert(new_page(), key, value)),
        }
    }
}

#[cfg(test)]
mod tests {
    use core::cmp::Ordering;
    use core::panic::AssertUnwindSafe;
    use std::format;
    use std::panic::catch_unwind;
    use std::string::String;
    use std::vec::Vec;

    use crate::node::handle::{
        EdgeRoom, ForceResult, Insertion, Leaf, NodeMut, SearchResult, Split, Subtree,
    };
    use crate::node::layout::NodeLayout;
    use crate::node::page::TestPages;
    use crate::node::test_support::{
        Bulky, CacheLineAligned, DropLedger, PageAligned, TestRng, TestTree, Tracked, TrackedValue,
    };

    /// Appends an entry to a leaf that has room.
    fn push_back<K, V>(leaf: NodeMut<'_, K, V, Leaf>, key: K, value: V) {
        match leaf.last_edge().check_room() {
            EdgeRoom::Fits(edge) => {
                edge.insert_fit(key, value);
            }
            EdgeRoom::Full(_) => panic!("the leaf is full"),
        }
    }

    /// The root of a tree that must be a leaf.
    fn root_leaf<K, V>(tree: &mut Subtree<K, V>) -> NodeMut<'_, K, V, Leaf> {
        match tree.borrow_mut().force() {
            ForceResult::Leaf(leaf) => leaf,
            ForceResult::Internal(_) => panic!("the root is internal"),
        }
    }

    #[test]
    fn search_matches_slice_binary_search() {
        let pages = TestPages::<u64, u64>::new();
        let mut leaf = Subtree::new_leaf(pages.fresh());
        let keys: Vec<u64> = (0..40).map(|key| key * 2).collect();
        for &key in &keys {
            push_back(leaf.borrow_mut(), key, key + 1000);
        }
        for probe in 0..85 {
            let position = match leaf.borrow().search(&probe) {
                SearchResult::Found(kv) => {
                    assert_eq!(*kv.value(), probe + 1000);
                    Ok(kv.index())
                }
                SearchResult::GoDown(edge) => Err(edge.index()),
            };
            assert_eq!(position, keys.binary_search(&probe));
        }
        leaf.destroy(|page| pages.release(page));
        pages.assert_all_released();
    }

    /// The same check for keys that take the narrow-then-scan search.
    #[test]
    fn string_search_matches_slice_binary_search() {
        let pages = TestPages::<String, u64>::new();
        let mut leaf = Subtree::new_leaf(pages.fresh());
        let keys: Vec<String> = (0..30).map(|key| format!("{:04}", key * 2)).collect();
        for (number, key) in keys.iter().enumerate() {
            push_back(leaf.borrow_mut(), key.clone(), number as u64);
        }
        for probe in 0..65 {
            let probe = format!("{probe:04}");
            let position = match leaf.borrow().search(probe.as_str()) {
                SearchResult::Found(kv) => {
                    assert_eq!(kv.key(), &probe);
                    Ok(kv.index())
                }
                SearchResult::GoDown(edge) => Err(edge.index()),
            };
            assert_eq!(position, keys.binary_search(&probe));
        }
        leaf.destroy(|page| pages.release(page));
        pages.assert_all_released();
    }

    #[test]
    fn search_borrows_keys() {
        let pages = TestPages::<String, ()>::new();
        let mut leaf = Subtree::new_leaf(pages.fresh());
        for key in ["apple", "banana", "cherry"] {
            push_back(leaf.borrow_mut(), String::from(key), ());
        }
        match leaf.borrow().search("banana") {
            SearchResult::Found(kv) => assert_eq!(kv.key(), "banana"),
            SearchResult::GoDown(_) => panic!("banana is there"),
        }
        match leaf.borrow().search("blueberry") {
            SearchResult::Found(_) => panic!("blueberry is not there"),
            SearchResult::GoDown(edge) => assert_eq!(edge.index(), 2),
        }
        leaf.destroy(|page| pages.release(page));
        pages.assert_all_released();
    }

    /// A key whose `Ord` answers at random. `PADDING` picks the search path:
    /// 0 keeps it small enough for the branchless search, 32 makes it take
    /// the narrow-then-scan search.
    struct Chaotic<'r, const PADDING: usize>(&'r TestRng, [u8; PADDING]);

    impl<'r, const PADDING: usize> Chaotic<'r, PADDING> {
        fn new(rng: &'r TestRng) -> Self {
            Self(rng, [0; PADDING])
        }
    }

    impl<const PADDING: usize> PartialEq for Chaotic<'_, PADDING> {
        fn eq(&self, _: &Self) -> bool {
            self.0.below(2) == 0
        }
    }

    impl<const PADDING: usize> Eq for Chaotic<'_, PADDING> {}

    impl<const PADDING: usize> PartialOrd for Chaotic<'_, PADDING> {
        fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
            Some(self.cmp(other))
        }
    }

    impl<const PADDING: usize> Ord for Chaotic<'_, PADDING> {
        fn cmp(&self, _: &Self) -> Ordering {
            [Ordering::Less, Ordering::Equal, Ordering::Greater][self.0.below(3)]
        }
    }

    #[test]
    fn search_stays_in_bounds_for_any_ord() {
        search_stays_in_bounds::<0>();
        search_stays_in_bounds::<32>();
    }

    fn search_stays_in_bounds<const PADDING: usize>() {
        let rng = TestRng::new(7);
        let ledger = DropLedger::new();
        let pages = TestPages::<Chaotic<'_, PADDING>, Tracked<'_>>::new();
        let mut leaf = Subtree::new_leaf(pages.fresh());
        let searches = if cfg!(miri) { 60 } else { 1000 };
        for _ in 0..searches {
            let edge = match leaf.borrow_mut().search(&Chaotic::new(&rng)) {
                SearchResult::Found(kv) => {
                    assert!(kv.index() < kv.reborrow().into_node().len());
                    let _ = kv.value().id();
                    kv.right_edge()
                }
                SearchResult::GoDown(edge) => edge,
            };
            assert!(edge.index() <= edge.reborrow().into_node().len());
            if let EdgeRoom::Fits(edge) = edge.check_room() {
                edge.insert_fit(Chaotic::new(&rng), ledger.track());
            }
        }
        leaf.destroy(|page| pages.release(page));
        pages.assert_all_released();
        ledger.assert_all_dropped_once();
    }

    #[test]
    fn a_tree_stays_memory_safe_under_any_ord() {
        tree_stays_memory_safe::<0>();
        tree_stays_memory_safe::<32>();
    }

    fn tree_stays_memory_safe<const PADDING: usize>() {
        let rng = TestRng::new(8);
        let ledger = DropLedger::new();
        let pages = TestPages::<Chaotic<'_, PADDING>, Bulky<'_>>::new();
        let mut tree = TestTree::new(&pages);
        let inserts = if cfg!(miri) { 120 } else { 3000 };
        for _ in 0..inserts {
            tree.insert(Chaotic::new(&rng), Bulky::from_tracked(ledger.track()));
        }
        assert!(tree.height() >= 1);
        let ids = tree.entries(|_, value| value.tracked_id());
        assert!(!ids.is_empty() && ids.len() <= inserts);
        tree.destroy();
        pages.assert_all_released();
        ledger.assert_all_dropped_once();
    }

    /// Random operations on one leaf, checked against a `Vec` after each.
    /// A split of the full leaf is put back together through a parent.
    /// Returns how many times the leaf split.
    fn random_operations<'l, V: TrackedValue<'l>>(ledger: &'l DropLedger, steps: usize) -> usize {
        let pages = TestPages::<Tracked<'l>, V>::new();
        let rng = TestRng::new(1);
        let mut tree = Subtree::new_leaf(pages.fresh()).forget_type();
        let mut model: Vec<(usize, usize)> = Vec::new();
        let mut splits = 0;

        for _ in 0..steps {
            let len = model.len();
            match rng.below(10) {
                0..=3 => {
                    let index = rng.below(len + 1);
                    let (key, value) = (ledger.track(), V::from_tracked(ledger.track()));
                    model.insert(index, (key.id(), value.tracked_id()));
                    let edge = root_leaf(&mut tree).edge_at(index);
                    match edge.insert(key, value, || pages.fresh()) {
                        Insertion::Fit(kv) => assert_eq!(kv.index(), index),
                        Insertion::Split(Split {
                            left,
                            separator,
                            right,
                        }) => {
                            let mut halves = left.reborrow().tracked_ids();
                            halves.push((separator.0.id(), separator.1.tracked_id()));
                            halves.extend(right.borrow().tracked_ids());
                            assert_eq!(halves, model);
                            assert!(left.len().abs_diff(right.borrow().len()) <= 2);
                            let parts = SplitLeaf {
                                separator,
                                right,
                                left_len: left.len(),
                            };
                            tree = parts.reassemble(tree, &pages, &rng, &mut model);
                            splits += 1;
                        }
                    }
                }
                4 | 5 if len > 0 => {
                    let index = rng.below(len);
                    let ((key, value), edge) = root_leaf(&mut tree).kv_at(index).remove();
                    assert_eq!((key.id(), value.tracked_id()), model.remove(index));
                    assert_eq!(edge.index(), index);
                }
                6 if len > 0 => {
                    let index = rng.below(len);
                    let (key, value) = (ledger.track(), V::from_tracked(ledger.track()));
                    let new_ids = (key.id(), value.tracked_id());
                    let (old_key, old_value) = root_leaf(&mut tree)
                        .kv_at(index)
                        .replace_key_value(key, value);
                    assert_eq!((old_key.id(), old_value.tracked_id()), model[index]);
                    model[index] = new_ids;
                }
                7 if len > 0 => {
                    let index = rng.below(len);
                    let value = V::from_tracked(ledger.track());
                    model[index].1 = value.tracked_id();
                    let mut kv = root_leaf(&mut tree).kv_at(index);
                    let old_value = if rng.below(2) == 0 {
                        core::mem::replace(kv.value_mut(), value)
                    } else {
                        core::mem::replace(kv.into_value_mut(), value)
                    };
                    drop(old_value);
                }
                8 => {
                    let leaf = tree.borrow();
                    let ForceResult::Leaf(leaf) = leaf.force() else {
                        panic!("the root is a leaf")
                    };
                    let index = rng.below(len + 1);
                    match leaf.edge_at(index).left_kv() {
                        Ok(kv) => assert_eq!(kv.key().id(), model[index - 1].0),
                        Err(edge) => assert_eq!(edge.index(), 0),
                    }
                    match leaf.edge_at(index).right_kv() {
                        Ok(kv) => {
                            assert_eq!(kv.key().id(), model[index].0);
                            assert_eq!(kv.left_edge().index(), index);
                            assert_eq!(kv.right_edge().index(), index + 1);
                        }
                        Err(edge) => assert_eq!(edge.index(), len),
                    }
                    assert_eq!(leaf.first_edge().index(), 0);
                    assert_eq!(leaf.last_edge().index(), len);
                    assert_eq!(leaf.first_kv().map(|kv| kv.index()), (len > 0).then_some(0));
                    assert_eq!(leaf.last_kv().map(|kv| kv.index()), len.checked_sub(1));
                }
                _ => {}
            }
            assert_eq!(tree.borrow().tracked_ids(), model);
        }
        tree.destroy(|page| pages.release(page));
        pages.assert_all_released();
        splits
    }

    #[test]
    fn random_operations_match_a_vec_model() {
        let ledger = DropLedger::new();
        let (bulky_steps, tracked_steps) = if cfg!(miri) {
            (200, 200)
        } else {
            (5_000, 20_000)
        };
        assert!(random_operations::<Bulky<'_>>(&ledger, bulky_steps) > 0);
        let tracked_splits = random_operations::<Tracked<'_>>(&ledger, tracked_steps);
        assert!(cfg!(miri) || tracked_splits > 0);
        ledger.assert_all_dropped_once();
    }

    /// The parts of a root leaf that split, besides the leaf itself.
    struct SplitLeaf<K, V> {
        separator: (K, V),
        right: Subtree<K, V, Leaf>,
        left_len: usize,
    }

    impl<'l, V: TrackedValue<'l>> SplitLeaf<Tracked<'l>, V> {
        /// Puts the split root leaf `tree` back together in one leaf: hangs
        /// both halves under a new root, removes one random entry so that
        /// they fit, merges them and pops the root. `model` loses the entry.
        fn reassemble(
            self,
            mut tree: Subtree<Tracked<'l>, V>,
            pages: &TestPages<Tracked<'l>, V>,
            rng: &TestRng,
            model: &mut Vec<(usize, usize)>,
        ) -> Subtree<Tracked<'l>, V> {
            let root = tree.push_internal_level(pages.fresh());
            assert_eq!(root.len(), 0);
            let EdgeRoom::Fits(edge) = root.last_edge().check_room() else {
                panic!("a new root has room");
            };
            let mut root = edge
                .insert_fit(self.separator.0, self.separator.1, self.right.forget_type())
                .into_node();

            let right_len = model.len() - self.left_len - 1;
            let in_left = self.left_len > 0 && (right_len == 0 || rng.below(2) == 0);
            let child = root.reborrow_mut().edge_at(usize::from(!in_left)).descend();
            let ForceResult::Leaf(child) = child.force() else {
                panic!("the children are leaves");
            };
            let index = rng.below(child.len());
            let ((key, value), _) = child.kv_at(index).remove();
            let value = value.tracked_id();
            let model_index = if in_left {
                index
            } else {
                self.left_len + 1 + index
            };
            assert_eq!((key.id(), value), model.remove(model_index));

            let context = root.kv_at(0).into_balancing_context();
            assert_eq!(
                context.left_child_len() + 1 + context.right_child_len(),
                model.len()
            );
            let Ok(mergeable) = context.can_merge() else {
                panic!("the halves fit again");
            };
            let (merged, page) = mergeable.merge();
            pages.release(page);
            assert_eq!(merged.index(), 0);
            assert_eq!(merged.into_node().len(), 0);

            let ForceResult::Internal(root) = tree.force() else {
                panic!("the root is internal");
            };
            let (leaf, page) = root.pop_internal_level();
            pages.release(page);
            assert_eq!(leaf.height(), 0);
            leaf
        }
    }

    /// Builds a full leaf of tracked entries, inserts one more at `position`,
    /// and checks the split against the model.
    fn split_at<'l, V: TrackedValue<'l>>(ledger: &'l DropLedger, position: usize) {
        let pages = TestPages::<Tracked<'l>, V>::new();
        let mut leaf = Subtree::new_leaf(pages.fresh());
        let mut model = Vec::new();
        while let EdgeRoom::Fits(edge) = leaf.borrow_mut().last_edge().check_room() {
            let (key, value) = (ledger.track(), V::from_tracked(ledger.track()));
            model.push((key.id(), value.tracked_id()));
            edge.insert_fit(key, value);
        }
        let (key, value) = (ledger.track(), V::from_tracked(ledger.track()));
        model.insert(position, (key.id(), value.tracked_id()));
        let edge = leaf.borrow_mut().edge_at(position);
        let Insertion::Split(Split {
            left,
            separator,
            right,
        }) = edge.insert(key, value, || pages.fresh())
        else {
            panic!("a full leaf splits");
        };
        let mut halves = left.reborrow().tracked_ids();
        halves.push((separator.0.id(), separator.1.tracked_id()));
        halves.extend(right.borrow().tracked_ids());
        assert_eq!(halves, model);
        assert!(left.len().abs_diff(right.borrow().len()) <= 2);
        assert_eq!(right.height(), 0);
        drop(separator);
        right.destroy(|page| pages.release(page));
        leaf.destroy(|page| pages.release(page));
        pages.assert_all_released();
    }

    #[test]
    fn split_and_insert_at_every_position() {
        let ledger = DropLedger::new();
        for position in 0..=NodeLayout::<Tracked<'_>, Bulky<'_>>::LEAF_CAPACITY {
            split_at::<Bulky<'_>>(&ledger, position);
        }
        if !cfg!(miri) {
            for position in 0..=NodeLayout::<Tracked<'_>, Tracked<'_>>::LEAF_CAPACITY {
                split_at::<Tracked<'_>>(&ledger, position);
            }
        }
        ledger.assert_all_dropped_once();
    }

    #[test]
    fn checked_positions_reject_out_of_bounds_indices() {
        let pages = TestPages::<u64, u64>::new();
        let mut leaf = Subtree::new_leaf(pages.fresh());
        assert!(leaf.borrow().first_kv().is_none());
        assert!(leaf.borrow().last_kv().is_none());
        assert!(catch_unwind(AssertUnwindSafe(|| leaf.borrow_mut().kv_at(0).index())).is_err());
        push_back(leaf.borrow_mut(), 1, 10);
        push_back(leaf.borrow_mut(), 2, 20);
        assert!(catch_unwind(AssertUnwindSafe(|| leaf.borrow_mut().edge_at(3).index())).is_err());
        assert!(catch_unwind(AssertUnwindSafe(|| leaf.borrow_mut().kv_at(2).index())).is_err());
        assert!(leaf.borrow().first_edge().left_kv().is_err());
        assert!(leaf.borrow().last_edge().right_kv().is_err());
        assert_eq!(leaf.borrow().keys(), &[1, 2]);
        leaf.destroy(|page| pages.release(page));
        pages.assert_all_released();
    }

    #[test]
    fn zero_sized_keys_and_values() {
        let pages = TestPages::<(), ()>::new();
        let mut leaf = Subtree::new_leaf(pages.fresh());
        let mut len = 0;
        while let EdgeRoom::Fits(edge) = leaf.borrow_mut().first_edge().check_room() {
            edge.insert_fit((), ());
            len += 1;
        }
        assert_eq!(len, 1023);
        let middle = leaf.borrow_mut().edge_at(len / 2);
        let Insertion::Split(split) = middle.insert((), (), || pages.fresh()) else {
            panic!("a full leaf splits");
        };
        assert_eq!(split.left.len() + 1 + split.right.borrow().len(), len + 1);
        split.right.destroy(|page| pages.release(page));
        assert!(matches!(leaf.borrow().search(&()), SearchResult::Found(_)));
        leaf.destroy(|page| pages.release(page));
        pages.assert_all_released();
    }

    #[test]
    fn over_aligned_keys_and_values() {
        let rng = TestRng::new(3);
        let count = if cfg!(miri) { 40 } else { 400 };
        let mut keys: Vec<u64> = (0..count).collect();
        rng.shuffle(&mut keys);

        let pages = TestPages::<CacheLineAligned, CacheLineAligned>::new();
        let mut tree = TestTree::new(&pages);
        for &key in &keys {
            let key = CacheLineAligned::new(key);
            assert!(
                tree.insert(key, CacheLineAligned::new(key.get() * 3))
                    .is_none()
            );
        }
        let entries = tree.entries(|key, value| (key.get(), value.get()));
        assert_eq!(
            entries,
            (0..count).map(|key| (key, key * 3)).collect::<Vec<_>>()
        );
        assert!(tree.height() >= 1);
        tree.destroy();
        pages.assert_all_released();

        let pages = TestPages::<u64, PageAligned>::new();
        let mut tree = TestTree::new(&pages);
        let count = if cfg!(miri) { 20 } else { 100 };
        for &key in keys.iter().filter(|&&key| key < count) {
            tree.insert(key, PageAligned::new(key + 1));
        }
        let entries = tree.entries(|key, value| (*key, value.get()));
        assert_eq!(
            entries,
            (0..count).map(|key| (key, key + 1)).collect::<Vec<_>>()
        );
        assert!(tree.height() >= 1);
        tree.destroy();
        pages.assert_all_released();
    }
}
