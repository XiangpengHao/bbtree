//! Leaves as the tree uses them: insert, splitting a full leaf first. Also
//! the tests of the leaf operations in `handle.rs`.

#![forbid(unsafe_code)]

use super::handle::{Edge, EdgeRoom, Insertion, Leaf, NodeMut};
use super::inline_keys::InlinedKey;
use super::page::FreshPage;
use crate::inline_key::InlineKeyStrategy;

impl<'a, K, V, S: InlineKeyStrategy<K>> Edge<NodeMut<'a, K, V, S, Leaf>> {
    /// Inserts `key`, with its inline key, and `value` at this edge,
    /// splitting the leaf first if it is full, with the right half in a page
    /// from `new_page`.
    pub(crate) fn insert(
        self,
        key: InlinedKey<K, S>,
        value: V,
        new_page: impl FnOnce() -> FreshPage<K, V, S, Leaf>,
    ) -> Insertion<'a, K, V, S, Leaf> {
        match self.check_room() {
            EdgeRoom::Fits(edge) => Insertion::Fit(edge.insert_fit(key, value)),
            EdgeRoom::Full(edge) => Insertion::Split(edge.split_and_insert(new_page(), key, value)),
        }
    }
}

#[cfg(test)]
#[cfg(test)]
mod tests {
    use core::cmp::Ordering;
    use core::panic::AssertUnwindSafe;
    use std::format;
    use std::panic::catch_unwind;
    use std::string::String;
    use std::vec::Vec;

    use crate::inline_key::{Auto, InlineKey, InlineKeyStrategy, InlineWord, WithInlineKey};
    use crate::node::handle::{
        EdgeRoom, ForceResult, Immut, Insertion, Leaf, NodeMut, NodeRef, SearchResult, Split,
        Subtree,
    };
    use crate::node::inline_keys::{InlinedKey, SearchKey};
    use crate::node::page::{NodePtr, TestPages};
    use crate::node::test_support::{
        Bulky, CacheLineAligned, DropLedger, FromTracked, PageAligned, TestRng, TestTree, Tracked,
    };

    /// Appends an entry to a leaf that has room.
    fn push_back<K, V, S: InlineKeyStrategy<K>>(
        leaf: NodeMut<'_, K, V, S, Leaf>,
        key: K,
        value: V,
    ) {
        match leaf.last_edge().check_room() {
            EdgeRoom::Fits(edge) => {
                edge.insert_fit(InlinedKey::new(key), value);
            }
            EdgeRoom::Full(_) => panic!("the leaf is full"),
        }
    }

    /// The root of a tree that must be a leaf.
    fn root_leaf<K, V, S: InlineKeyStrategy<K>>(
        tree: &mut Subtree<K, V, S>,
    ) -> NodeMut<'_, K, V, S, Leaf> {
        match tree.borrow_mut().force() {
            ForceResult::Leaf(leaf) => leaf,
            ForceResult::Internal(_) => panic!("the root is internal"),
        }
    }

    /// Shared access to the root of a tree that must be a leaf.
    fn root_leaf_ref<K, V, S: InlineKeyStrategy<K>>(
        tree: &Subtree<K, V, S>,
    ) -> NodeRef<Immut<'_>, K, V, S, Leaf> {
        match tree.borrow().force() {
            ForceResult::Leaf(leaf) => leaf,
            ForceResult::Internal(_) => panic!("the root is internal"),
        }
    }

    /// Where a search of `leaf` for `key` ends: `Ok` at an equal key, `Err`
    /// at the edge where it belongs, as `slice::binary_search` says.
    fn search_position<K: core::borrow::Borrow<Q>, V, S: InlineKeyStrategy<K>, Q: Ord + ?Sized>(
        leaf: NodeRef<Immut<'_>, K, V, S, Leaf>,
        key: &Q,
    ) -> Result<usize, usize> {
        match leaf.search(&SearchKey::new(key)) {
            SearchResult::Found(kv) => Ok(kv.index()),
            SearchResult::GoDown(edge) => Err(edge.index()),
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
            if let SearchResult::Found(kv) = leaf.borrow().search(&SearchKey::new(&probe)) {
                assert_eq!(*kv.value(), probe + 1000);
            }
            assert_eq!(
                search_position(leaf.borrow(), &probe),
                keys.binary_search(&probe)
            );
        }
        leaf.destroy(|page| pages.release(page));
        pages.assert_all_released();
    }

    /// The same check for keys that take the plain binary search, or, with
    /// inline keys, the search over them: whole short strings, which their
    /// inline keys hold, and long ones that tie on them.
    #[test]
    fn string_search_matches_slice_binary_search() {
        for (prefix, width) in [("", 4), ("", 8), ("", 9), ("tied/prefix/", 4)] {
            let pages = TestPages::<String, u64>::new();
            assert!(NodePtr::<String, u64, Auto>::has_inline_keys());
            let mut leaf = Subtree::new_leaf(pages.fresh());
            let key = |number: usize| format!("{prefix}{number:0width$}");
            let keys: Vec<String> = (0..25).map(|number| key(number * 2)).collect();
            for (number, key) in keys.iter().enumerate() {
                push_back(leaf.borrow_mut(), key.clone(), number as u64);
            }
            leaf.borrow().assert_inline_keys_match();
            for probe in (0..53).map(key) {
                let expected = keys.binary_search(&probe);
                assert_eq!(search_position(leaf.borrow(), probe.as_str()), expected);
                assert_eq!(search_position(leaf.borrow(), &probe), expected);
            }
            // Shorter and longer keys with the same inline key.
            for probe in [key(4) + "\0", key(4) + "0", String::from(&key(4)[..1])] {
                let expected = keys.binary_search(&probe);
                assert_eq!(search_position(leaf.borrow(), probe.as_str()), expected);
            }
            leaf.destroy(|page| pages.release(page));
            pages.assert_all_released();
        }
    }

    #[test]
    fn search_borrows_keys() {
        let pages = TestPages::<String, ()>::new();
        let mut leaf = Subtree::new_leaf(pages.fresh());
        for key in ["apple", "banana", "cherry"] {
            push_back(leaf.borrow_mut(), String::from(key), ());
        }
        match leaf.borrow().search(&SearchKey::new("banana")) {
            SearchResult::Found(kv) => assert_eq!(kv.key(), "banana"),
            SearchResult::GoDown(_) => panic!("banana is there"),
        }
        match leaf.borrow().search(&SearchKey::new("blueberry")) {
            SearchResult::Found(_) => panic!("blueberry is not there"),
            SearchResult::GoDown(edge) => assert_eq!(edge.index(), 2),
        }
        leaf.destroy(|page| pages.release(page));
        pages.assert_all_released();
    }

    /// A key whose `Ord` and `InlineKey` answer at random. `PADDING` picks
    /// the search path without inline keys: 0 keeps it small enough for the
    /// branchless search, 32 makes it take the plain binary search.
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

    /// Few distinct values, so that runs of equal inline keys are common.
    impl<const PADDING: usize> InlineKey for Chaotic<'_, PADDING> {
        fn inline_key(&self) -> InlineWord {
            self.0.below(4) as InlineWord
        }
    }

    #[test]
    fn search_stays_in_bounds_for_any_ord() {
        search_stays_in_bounds::<0, Auto>();
        search_stays_in_bounds::<32, Auto>();
        search_stays_in_bounds::<0, WithInlineKey>();
        search_stays_in_bounds::<32, WithInlineKey>();
    }

    fn search_stays_in_bounds<const PADDING: usize, S>()
    where
        for<'r> S: InlineKeyStrategy<Chaotic<'r, PADDING>>,
    {
        let rng = TestRng::new(7);
        let ledger = DropLedger::new();
        let pages = TestPages::<Chaotic<'_, PADDING>, Tracked<'_>, S>::new();
        let mut leaf = Subtree::new_leaf(pages.fresh());
        let searches = if cfg!(miri) { 60 } else { 1000 };
        for _ in 0..searches {
            let probe = Chaotic::new(&rng);
            let found = if rng.below(2) == 0 {
                leaf.borrow_mut().search(&SearchKey::new(&probe))
            } else {
                leaf.borrow_mut()
                    .search_expecting_append(&SearchKey::new(&probe))
            };
            let edge = match found {
                SearchResult::Found(kv) => {
                    assert!(kv.index() < kv.reborrow().into_node().len());
                    let _ = kv.value().id();
                    kv.right_edge()
                }
                SearchResult::GoDown(edge) => edge,
            };
            assert!(edge.index() <= edge.reborrow().into_node().len());
            if let EdgeRoom::Fits(edge) = edge.check_room() {
                edge.insert_fit(InlinedKey::new(Chaotic::new(&rng)), ledger.track());
            }
        }
        leaf.destroy(|page| pages.release(page));
        pages.assert_all_released();
        ledger.assert_all_dropped_once();
    }

    #[test]
    fn a_tree_stays_memory_safe_under_any_ord() {
        tree_stays_memory_safe::<0, Auto>();
        tree_stays_memory_safe::<32, Auto>();
        tree_stays_memory_safe::<0, WithInlineKey>();
        tree_stays_memory_safe::<32, WithInlineKey>();
    }

    fn tree_stays_memory_safe<const PADDING: usize, S>()
    where
        for<'r> S: InlineKeyStrategy<Chaotic<'r, PADDING>>,
    {
        let rng = TestRng::new(8);
        let ledger = DropLedger::new();
        let pages = TestPages::<Chaotic<'_, PADDING>, Bulky<'_>, S>::new();
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

    /// What one leaf should hold: the ids of its entries in order, and of its
    /// head, if any.
    struct LeafModel {
        entries: Vec<(usize, usize)>,
        head: Option<usize>,
    }

    impl LeafModel {
        /// Checks the leaf's entries and head, and its inline keys, if its
        /// map has them, against its keys.
        fn assert_matches<'l, V: FromTracked<'l>, S: InlineKeyStrategy<Tracked<'l>>>(
            &self,
            leaf: NodeRef<Immut<'_>, Tracked<'l>, V, S, Leaf>,
        ) {
            assert_eq!(leaf.tracked_ids(), self.entries);
            assert_eq!(leaf.head_id(), self.head);
            leaf.assert_inline_keys_match();
        }

        /// Checks the halves of a split of the modelled leaf: the left keeps
        /// the head, the separator's value is the right half's head, and the
        /// halves differ by at most two entries.
        fn assert_split_halves<'l, V: FromTracked<'l>, S: InlineKeyStrategy<Tracked<'l>>>(
            &self,
            split: &Split<'_, Tracked<'l>, V, S, Leaf>,
        ) {
            let right = split.right.borrow();
            let separator_value = right.head_id().expect("the right half has a head");
            let mut halves = split.left.reborrow().tracked_ids();
            halves.push((split.separator.key().id(), separator_value));
            halves.extend(right.tracked_ids());
            assert_eq!(halves, self.entries);
            assert_eq!(split.left.reborrow().head_id(), self.head);
            assert!(split.left.len().abs_diff(right.len()) <= 2);
            assert_eq!(right.height(), 0);
            split.left.reborrow().assert_inline_keys_match();
            right.assert_inline_keys_match();
        }
    }

    /// Random operations on one leaf, its head included, checked against a
    /// model after each. A split of the full leaf is put back together
    /// through a parent. Returns how many times the leaf split.
    fn random_operations<'l, V: FromTracked<'l>, S: InlineKeyStrategy<Tracked<'l>>>(
        ledger: &'l DropLedger,
        steps: usize,
    ) -> usize {
        let pages = TestPages::<Tracked<'l>, V, S>::new();
        let rng = TestRng::new(1);
        let mut tree = Subtree::new_leaf(pages.fresh()).forget_type();
        let mut model = LeafModel {
            entries: Vec::new(),
            head: None,
        };
        let mut splits = 0;

        for _ in 0..steps {
            let len = model.entries.len();
            match rng.below(11) {
                0..=3 => {
                    let index = rng.below(len + 1);
                    let (key, value) = (ledger.track(), V::from_tracked(ledger.track()));
                    model.entries.insert(index, (key.id(), value.tracked_id()));
                    let edge = root_leaf(&mut tree).edge_at(index);
                    match edge.insert(InlinedKey::new(key), value, || pages.fresh()) {
                        Insertion::Fit(kv) => assert_eq!(kv.index(), index),
                        Insertion::Split(split) => {
                            model.assert_split_halves(&split);
                            let parts = SplitLeaf {
                                separator: split.separator,
                                right: split.right,
                                left_len: split.left.len(),
                            };
                            tree = parts.reassemble(tree, &pages, &rng, &mut model.entries);
                            splits += 1;
                        }
                    }
                }
                4 | 5 if len > 0 => {
                    let index = rng.below(len);
                    let ((key, value), edge) = root_leaf(&mut tree).kv_at(index).remove();
                    assert_eq!((key.id(), value.tracked_id()), model.entries.remove(index));
                    assert_eq!(edge.index(), index);
                }
                6 if len > 0 => {
                    let index = rng.below(len);
                    let (key, value) = (ledger.track(), V::from_tracked(ledger.track()));
                    let new_ids = (key.id(), value.tracked_id());
                    let (old_key, old_value) = root_leaf(&mut tree)
                        .kv_at(index)
                        .replace_key_value(InlinedKey::new(key), value);
                    assert_eq!((old_key.id(), old_value.tracked_id()), model.entries[index]);
                    model.entries[index] = new_ids;
                }
                7 if len > 0 => {
                    let index = rng.below(len);
                    let value = V::from_tracked(ledger.track());
                    model.entries[index].1 = value.tracked_id();
                    let mut kv = root_leaf(&mut tree).kv_at(index);
                    let old_value = if rng.below(2) == 0 {
                        core::mem::replace(kv.value_mut(), value)
                    } else {
                        core::mem::replace(kv.into_value_mut(), value)
                    };
                    drop(old_value);
                }
                8 => {
                    let leaf = root_leaf_ref(&tree);
                    let index = rng.below(len + 1);
                    match leaf.edge_at(index).left_kv() {
                        Ok(kv) => assert_eq!(kv.key().id(), model.entries[index - 1].0),
                        Err(edge) => assert_eq!(edge.index(), 0),
                    }
                    match leaf.edge_at(index).right_kv() {
                        Ok(kv) => {
                            assert_eq!(kv.key().id(), model.entries[index].0);
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
                9 => {
                    let mut leaf = root_leaf(&mut tree);
                    match (model.head, rng.below(3)) {
                        (Some(head_id), 0) => {
                            let head = leaf.head().expect("the model says there is a head");
                            let (value, _) = head.remove();
                            assert_eq!(value.tracked_id(), head_id);
                            model.head = None;
                        }
                        (Some(head_id), 1) => {
                            let head = leaf.head().expect("the model says there is a head");
                            match head.remove_with_first_key() {
                                Some((value, key)) => {
                                    assert_eq!(value.tracked_id(), head_id);
                                    let (first_key_id, first_value_id) = model.entries.remove(0);
                                    assert_eq!(key.key().id(), first_key_id);
                                    model.head = Some(first_value_id);
                                }
                                None => assert!(model.entries.is_empty()),
                            }
                        }
                        (old_head, _) => {
                            let value = V::from_tracked(ledger.track());
                            model.head = Some(value.tracked_id());
                            let replaced = leaf.replace_head(value);
                            assert_eq!(replaced.map(|value| value.tracked_id()), old_head);
                        }
                    }
                }
                _ => {}
            }
            model.assert_matches(root_leaf_ref(&tree));
        }
        tree.destroy(|page| pages.release(page));
        pages.assert_all_released();
        splits
    }

    /// The leaf operations without inline keys, and with them, on the same
    /// operations.
    #[test]
    fn random_operations_match_a_model() {
        let ledger = DropLedger::new();
        let (bulky_steps, tracked_steps) = if cfg!(miri) {
            (200, 200)
        } else {
            (5_000, 20_000)
        };
        assert!(random_operations::<Bulky<'_>, Auto>(&ledger, bulky_steps) > 0);
        assert!(random_operations::<Bulky<'_>, WithInlineKey>(&ledger, bulky_steps) > 0);
        let tracked_splits = random_operations::<Tracked<'_>, Auto>(&ledger, tracked_steps);
        assert!(cfg!(miri) || tracked_splits > 0);
        let tracked_splits =
            random_operations::<Tracked<'_>, WithInlineKey>(&ledger, tracked_steps);
        assert!(cfg!(miri) || tracked_splits > 0);
        ledger.assert_all_dropped_once();
    }

    /// The parts of a root leaf that split, besides the leaf itself.
    struct SplitLeaf<K, V, S> {
        separator: InlinedKey<K, S>,
        right: Subtree<K, V, S, Leaf>,
        left_len: usize,
    }

    impl<'l, V: FromTracked<'l>, S: InlineKeyStrategy<Tracked<'l>>> SplitLeaf<Tracked<'l>, V, S> {
        /// Puts the split root leaf `tree` back together in one leaf: hangs
        /// both halves under a new root, removes one random entry other than
        /// the separator so that they fit, merges them, which brings the
        /// separator back beside its value, and pops the root. `entries`
        /// loses the removed entry.
        fn reassemble(
            self,
            mut tree: Subtree<Tracked<'l>, V, S>,
            pages: &TestPages<Tracked<'l>, V, S>,
            rng: &TestRng,
            entries: &mut Vec<(usize, usize)>,
        ) -> Subtree<Tracked<'l>, V, S> {
            let root = tree.push_internal_level(pages.fresh());
            assert_eq!(root.len(), 0);
            let EdgeRoom::Fits(edge) = root.last_edge().check_room() else {
                panic!("a new root has room");
            };
            let mut root = edge
                .insert_fit(self.separator, self.right.forget_type())
                .into_node();
            root.reborrow().assert_inline_keys_match();

            let right_len = entries.len() - self.left_len - 1;
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
            assert_eq!((key.id(), value), entries.remove(model_index));

            let context = root.kv_at(0).into_balancing_context();
            assert_eq!(
                context.left_child_len() + 1 + context.right_child_len(),
                entries.len()
            );
            let Ok(mergeable) = context.can_merge() else {
                panic!("the halves fit again");
            };
            let (merged, page) = mergeable.merge();
            pages.release(page);
            assert_eq!(merged.index(), 0);
            assert_eq!(merged.into_node().len(), 0);

            pages.release(tree.pop_internal_level());
            assert_eq!(tree.height(), 0);
            tree
        }
    }

    /// Builds a full leaf of tracked entries, with a head if `with_head`,
    /// inserts one more at `position`, and checks the split against the
    /// model.
    fn split_at<'l, V: FromTracked<'l>, S: InlineKeyStrategy<Tracked<'l>>>(
        ledger: &'l DropLedger,
        position: usize,
        with_head: bool,
    ) {
        let pages = TestPages::<Tracked<'l>, V, S>::new();
        let mut leaf = Subtree::new_leaf(pages.fresh());
        let mut model = LeafModel {
            entries: Vec::new(),
            head: None,
        };
        if with_head {
            let value = V::from_tracked(ledger.track());
            model.head = Some(value.tracked_id());
            assert!(leaf.borrow_mut().replace_head(value).is_none());
        }
        while let EdgeRoom::Fits(edge) = leaf.borrow_mut().last_edge().check_room() {
            let (key, value) = (ledger.track(), V::from_tracked(ledger.track()));
            model.entries.push((key.id(), value.tracked_id()));
            edge.insert_fit(InlinedKey::new(key), value);
        }
        let (key, value) = (ledger.track(), V::from_tracked(ledger.track()));
        model
            .entries
            .insert(position, (key.id(), value.tracked_id()));
        let edge = leaf.borrow_mut().edge_at(position);
        let Insertion::Split(split) = edge.insert(InlinedKey::new(key), value, || pages.fresh())
        else {
            panic!("a full leaf splits");
        };
        model.assert_split_halves(&split);
        let Split {
            separator, right, ..
        } = split;
        drop(separator);
        right.destroy(|page| pages.release(page));
        leaf.destroy(|page| pages.release(page));
        pages.assert_all_released();
    }

    #[test]
    fn split_and_insert_at_every_position() {
        fn every_position<'l, V: FromTracked<'l>, S: InlineKeyStrategy<Tracked<'l>>>(
            ledger: &'l DropLedger,
            heads: &[bool],
        ) {
            for position in 0..=NodePtr::<Tracked<'l>, V, S>::capacity(true) {
                for &with_head in heads {
                    split_at::<V, S>(ledger, position, with_head);
                }
            }
        }
        let ledger = DropLedger::new();
        if cfg!(miri) {
            every_position::<Bulky<'_>, Auto>(&ledger, &[true]);
            every_position::<Bulky<'_>, WithInlineKey>(&ledger, &[false]);
        } else {
            every_position::<Bulky<'_>, Auto>(&ledger, &[false, true]);
            every_position::<Bulky<'_>, WithInlineKey>(&ledger, &[false, true]);
            every_position::<Tracked<'_>, Auto>(&ledger, &[false, true]);
            every_position::<Tracked<'_>, WithInlineKey>(&ledger, &[false, true]);
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
        assert_eq!(leaf.borrow().inline_keys(), None);
        leaf.destroy(|page| pages.release(page));
        pages.assert_all_released();
    }

    #[test]
    fn zero_sized_keys_and_values() {
        let pages = TestPages::<(), ()>::new();
        let mut leaf = Subtree::new_leaf(pages.fresh());
        let mut len = 0;
        while let EdgeRoom::Fits(edge) = leaf.borrow_mut().first_edge().check_room() {
            edge.insert_fit(InlinedKey::new(()), ());
            len += 1;
        }
        assert_eq!(len, 1023);
        let middle = leaf.borrow_mut().edge_at(len / 2);
        let Insertion::Split(split) = middle.insert(InlinedKey::new(()), (), || pages.fresh())
        else {
            panic!("a full leaf splits");
        };
        assert_eq!(split.left.len() + 1 + split.right.borrow().len(), len + 1);
        assert!(split.right.borrow().head().is_some());
        split.right.destroy(|page| pages.release(page));
        assert!(matches!(
            leaf.borrow().search(&SearchKey::new(&())),
            SearchResult::Found(_)
        ));
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
