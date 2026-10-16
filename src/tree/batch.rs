//! Batched lookups and inserts (doc 12). Each key descends in its own lane,
//! an async block that reads like the single-key search: before it reads a
//! line it may not have, it prefetches that line and yields. A private
//! executor polls the lanes round-robin, so that one lane's wait for memory
//! overlaps the other lanes' searches. No future leaves this module: the
//! batch methods of the map and the set take and return plain arrays.
//!
//! Only this module prefetches (D46), and only lines a lane reads next: a
//! node's header, the chosen edge, the query's window, a unique leaf
//! candidate's entry and the first key bytes its comparison reads, and the
//! slot an insertion shifts. Never unchosen edges or whole keys.
//!
//! Level-by-level lanes, which search every lane at one depth before any
//! follows its edge, are the measured ceiling: at 10M these lanes take
//! 12–30% longer (doc 12).

use core::borrow::Borrow;
use core::future::Future;
use core::mem;
use core::pin::{Pin, pin};
use core::task::{Context, Poll, Waker};

use super::node::Node;
use super::node::inline_keys::SearchKey;
use super::node::internal::Internal;
use super::node::leaf::{CandidateLine, Leaf};
use super::node::page::{NodePtr, PageType};
use super::root::Tree;
use super::search::Found;
use super::shared_prefix::SharedPrefix;
use crate::allocator::Allocator;
use crate::inline_key::InlineKeyStrategy;

/// Separates the batch's leaf searches from the single-key `get`'s in
/// compilation, without changing node layouts: sharing one search
/// outlined the single-key one.
struct BatchSearch;

/// One key's descent. With the index a lane keeps between stages, its
/// fields are all a suspended lane holds, so successive stages reuse them:
/// one node for the internal levels and the leaf, and one prefix length,
/// the root's for the query window and then the leaf's. Node lengths and
/// prefixes fit in `u16`.
///
/// Each lane is one flat async block, the internal levels' loop included:
/// awaiting a nested future for them added a second dispatch to every
/// resume, 124 instructions and 13 ns per 10M `u64` hit.
struct Lane<'q, K, V, S, Q: ?Sized> {
    key: SearchKey<'q, K, S, Q>,
    node: NodePtr<K, V, S>,
    prefix_len: u16,
    /// False for a key that lacks what every key of the tree shares (D50),
    /// which no node holds.
    active: bool,
}

impl<'q, K, V, S, Q> Lane<'q, K, V, S, Q>
where
    K: Borrow<Q>,
    S: InlineKeyStrategy<K>,
    Q: Ord + ?Sized,
{
    #[inline(always)]
    fn new(key: &'q Q, root: Internal<K, V, S>, shared: &SharedPrefix) -> Self {
        let key = SearchKey::new(key);
        Self {
            active: shared.len() == 0 || shared.place_outside(&key).is_none(),
            key,
            node: root.node_ptr(),
            prefix_len: root.prefix_len() as u16,
        }
    }

    /// Looks the key up, writing its value to `output` if the tree has it.
    /// Returns unit rather than the value: LLVM leaves a returned
    /// `Poll<Option<&V>>` merged as one aggregate, testing a constant tag at
    /// each suspension (doc 12).
    #[expect(
        clippy::manual_async_fn,
        reason = "the block's attribute reaches its poll"
    )]
    fn find<'a>(mut self, output: &mut Option<&'a V>) -> impl Future<Output = ()>
    where
        K: 'a,
        V: 'a,
    {
        #[inline(always)]
        async move {
            // The chosen edge, then the leaf's candidate.
            let mut index: u16;
            if !self.active {
                return;
            }
            if let Some(line) = self.key.window_line(usize::from(self.prefix_len)) {
                Prefetcher::fetch(line).await;
            }
            loop {
                index = match Internal::at(self.node).search(&self.key) {
                    Ok(index) => {
                        let separator = Found::Separator(Internal::at(self.node), index);
                        *output = Some(separator.key_value().1);
                        return;
                    }
                    Err(edge) => edge as u16,
                };
                Prefetcher::fetch(Internal::at(self.node).edge_line(usize::from(index))).await;
                // SAFETY: the search returned an initialized edge, and the
                // whole batch borrows the tree: no node or index changes.
                match unsafe { Internal::at(self.node).child_unchecked(usize::from(index)) } {
                    Node::Leaf(leaf) => {
                        self.node = leaf.node_ptr();
                        break;
                    }
                    Node::Internal(child) => {
                        self.node = child.node_ptr();
                        Prefetcher::fetch(child.search_line()).await;
                    }
                }
            }
            Prefetcher::fetch(Leaf::at(self.node).search_line()).await;
            let found = if Leaf::<K, V, S>::plan().inline_width().is_some() {
                match Leaf::at(self.node).candidate_line(&self.key) {
                    CandidateLine::Absent => return,
                    CandidateLine::One(candidate, line) => {
                        index = candidate as u16;
                        self.prefix_len = Leaf::at(self.node).prefix_len() as u16;
                        Prefetcher::fetch(line).await;
                        if Leaf::<K, V, S>::plan().has_length_tags() {
                            // SAFETY: the candidate is an initialized key of
                            // this leaf, and the prefix the leaf's; neither
                            // changes while the tree is borrowed.
                            let line = unsafe {
                                Leaf::at(self.node).candidate_bytes_line(
                                    &self.key,
                                    usize::from(index),
                                    usize::from(self.prefix_len),
                                )
                            };
                            if let Some(line) = line {
                                Prefetcher::fetch(line).await;
                            }
                        }
                        // SAFETY: as above.
                        unsafe {
                            Leaf::at(self.node).find_candidate(
                                &self.key,
                                usize::from(index),
                                usize::from(self.prefix_len),
                            )
                        }
                    }
                    CandidateLine::Unknown => Leaf::at(self.node).find(&self.key, BatchSearch),
                }
            } else {
                Leaf::at(self.node).find(&self.key, BatchSearch)
            };
            *output = found.map(|index| Found::InLeaf(Leaf::at(self.node), index).key_value().1);
        }
    }

    /// Finds where the key goes, writing it to `output`: the separator equal
    /// to it, or its leaf and that leaf's search. Descends as [`Self::find`]
    /// does. An inactive lane leaves `output` at [`Destination::FromRoot`].
    #[expect(
        clippy::manual_async_fn,
        reason = "the block's attribute reaches its poll"
    )]
    fn find_destination(mut self, output: &mut Destination<K, V, S>) -> impl Future<Output = ()> {
        #[inline(always)]
        async move {
            if !self.active {
                return;
            }
            loop {
                let edge = match Internal::at(self.node).search(&self.key) {
                    Ok(index) => {
                        *output = Destination::Separator(Internal::at(self.node), index);
                        return;
                    }
                    Err(edge) => edge,
                };
                Prefetcher::fetch(Internal::at(self.node).edge_line(edge)).await;
                // SAFETY: as in `find`.
                match unsafe { Internal::at(self.node).child_unchecked(edge) } {
                    Node::Leaf(leaf) => {
                        self.node = leaf.node_ptr();
                        break;
                    }
                    Node::Internal(child) => {
                        self.node = child.node_ptr();
                        Prefetcher::fetch(child.search_line()).await;
                    }
                }
            }
            Prefetcher::fetch(Leaf::at(self.node).search_line()).await;
            let leaf = Leaf::at(self.node);
            let found = leaf.search_always_inlined(&self.key);
            if let Err(edge) = found
                && edge < leaf.len()
            {
                // The insertion shifts the keys from here. Reading ahead
                // through all it shifts increased fills from memory.
                Prefetcher::hint(leaf.entry_line(edge));
            }
            *output = Destination::Leaf(leaf, leaf.len(), found);
        }
    }
}

/// Where a batch's key goes, from a read-only descent. An insertion that
/// fits invalidates the searches saved in its leaf; a split invalidates the
/// leaf itself and every separator, which can move while it propagates.
enum Destination<K, V, S> {
    /// Unknown: insert as a single key does, from the root.
    FromRoot,
    /// The leaf, its length when searched, and the search.
    Leaf(Leaf<K, V, S>, usize, Result<usize, usize>),
    /// A separator equal to the key.
    Separator(Internal<K, V, S>, usize),
}

impl<K, V, S> Destination<K, V, S> {
    /// Splitting moves entries out of `split` and can move every separator.
    /// Other leaves keep their pages, even when the root rises: a batch
    /// starts at an internal root, never a root leaf.
    fn invalidate_after_split(&mut self, split: Leaf<K, V, S>) {
        if let Self::Leaf(leaf, _, _) = self
            && !leaf.node_ptr().is(split.node_ptr())
        {
            return;
        }
        *self = Self::FromRoot;
    }
}

/// Polls a batch's lanes round-robin until all finish. A lane yields only
/// after prefetching what it reads next, so none needs waking: the executor
/// polls every live lane again. No allocation, vtable or runtime.
struct Executor;

impl Executor {
    /// Up to 64 lanes, a bit per live lane, so that a finished lane needs no
    /// second discriminant beside its state.
    fn run<F: Future<Output = ()>, const N: usize>(lanes: [F; N]) {
        if N > u64::BITS as usize {
            return Self::run_slots(lanes);
        }
        let mut lanes = pin!(lanes);
        let mut live = if N == 64 { u64::MAX } else { (1_u64 << N) - 1 };
        let mut context = Context::from_waker(Waker::noop());
        while live != 0 {
            let mut round = live;
            while round != 0 {
                let index = round.trailing_zeros() as usize;
                round &= round - 1;
                // SAFETY: the array stays pinned through every poll and its
                // drop. A cleared bit keeps a finished lane from a repoll.
                let lane = unsafe { lanes.as_mut().map_unchecked_mut(|lanes| &mut lanes[index]) };
                if lane.poll(&mut context).is_ready() {
                    live &= !(1_u64 << index);
                }
            }
        }
    }

    /// More than 64 lanes, each in an `Option` emptied when it finishes.
    fn run_slots<F: Future<Output = ()>, const N: usize>(lanes: [F; N]) {
        let mut slots = pin!(lanes.map(Some));
        let mut remaining = N;
        let mut context = Context::from_waker(Waker::noop());
        while remaining != 0 {
            for index in 0..N {
                // SAFETY: the array stays pinned through every poll and its
                // drop. `Pin::set` drops a finished lane in place, never
                // moving it.
                let mut slot =
                    unsafe { slots.as_mut().map_unchecked_mut(|slots| &mut slots[index]) };
                if let Some(lane) = slot.as_mut().as_pin_mut()
                    && lane.poll(&mut context).is_ready()
                {
                    slot.set(None);
                    remaining -= 1;
                }
            }
        }
    }
}

/// The only software prefetch (D46): a hint about a line of a live node,
/// the query or a chosen candidate, which reads nothing. Other targets and
/// Miri do nothing.
struct Prefetcher {
    yielded: bool,
}

impl Prefetcher {
    /// Prefetches `line` and yields once, on the first poll: the lane
    /// resumes once the others have had a turn while the line arrives.
    #[inline(always)]
    fn fetch(line: *const u8) -> Self {
        Self::hint(line);
        Self { yielded: false }
    }

    /// Prefetches `line` without yielding.
    #[inline(always)]
    fn hint(line: *const u8) {
        #[cfg(all(target_arch = "x86_64", not(miri)))]
        // SAFETY: x86-64 has SSE, and the intrinsic doesn't dereference
        // its address.
        unsafe {
            core::arch::x86_64::_mm_prefetch(line.cast(), core::arch::x86_64::_MM_HINT_T0);
        }
        #[cfg(any(not(target_arch = "x86_64"), miri))]
        let _ = line;
    }
}

impl Future for Prefetcher {
    type Output = ();

    #[inline(always)]
    fn poll(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
        if self.yielded {
            Poll::Ready(())
        } else {
            self.yielded = true;
            Poll::Pending
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Tree<K, V, S> {
    /// Whether a tree of `length` entries is large enough for batches to
    /// pay for their lanes: below about an L2 of leaf pages, at 70%
    /// occupancy, there are too few misses to overlap. Keys' heap bytes are
    /// unknown; the pages give a floor.
    pub(crate) fn batches(length: usize) -> bool {
        let pages_in_l2 = (1024 * 1024 / Leaf::<K, V, S>::page_layout().size()).max(1);
        let entries_per_leaf = Leaf::<K, V, S>::capacity() * 7 / 10 + 1;
        cfg!(fuzzing) || length >= pages_in_l2 * entries_per_leaf
    }

    /// One key of a batch too small to schedule, through the batch's own
    /// leaf search: the single-key `get` keeps its inlined descent.
    #[inline(never)]
    fn find_in_batch<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let root_page = self.root_page()?;
        let key = SearchKey::<K, S, Q>::new(key);
        if root_page.shared_prefix().place_outside(&key).is_some() {
            return None;
        }
        root_page
            .node()
            .find(&key, BatchSearch)
            .map(|found| found.key_value().1)
    }

    /// Each key's value, in input order, as separate [`Self::find`]s give
    /// them; `length` is the tree's.
    pub(crate) fn find_many<Q, const N: usize>(
        &self,
        keys: [&Q; N],
        length: usize,
    ) -> [Option<&V>; N]
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if N < 2 || !Self::batches(length) {
            return keys.map(|key| self.find_in_batch(key));
        }
        let Some(root_page) = self.root_page() else {
            return [None; N];
        };
        let Node::Internal(root) = root_page.node() else {
            return keys.map(|key| self.find_in_batch(key));
        };
        let lanes = keys.map(|key| Lane::new(key, root, root_page.shared_prefix()));
        let mut values = [None; N];
        let mut outputs = values.iter_mut();
        Executor::run(lanes.map(|lane| lane.find(outputs.next().expect("one value per key"))));
        values
    }

    /// Inserts `entries` in input order, returning each previous value, as
    /// separate [`Self::insert`]s do; `length` is the tree's.
    pub(crate) fn insert_many<const N: usize>(
        &mut self,
        entries: [(K, V); N],
        alloc: &impl Allocator,
        length: &mut usize,
    ) -> [Option<V>; N]
    where
        K: Ord,
    {
        if N < 2 || !Self::batches(*length) {
            return entries.map(|(key, value)| self.insert(key, value, alloc, length));
        }
        let mut entries = entries.into_iter();
        let (key, value) = entries.next().expect("N >= 2");
        let first = match self.rightmost_append(key) {
            Ok(append) => {
                // Ascending keys: the rest most likely append too, which
                // the single-key insert does without a descent.
                append.insert(value);
                *length += 1;
                return core::array::from_fn(|index| {
                    if index == 0 {
                        return None;
                    }
                    let (key, value) = entries.next().expect("one entry per key");
                    self.insert(key, value, alloc, length)
                });
            }
            Err(key) => (key, value),
        };
        let mut first = Some(first);
        let entries = core::array::from_fn(|_| {
            first
                .take()
                .unwrap_or_else(|| entries.next().expect("one entry per key"))
        });
        self.insert_batch(entries, alloc, length)
    }

    /// Descends for every key first, then inserts them in input order, each
    /// at its destination while that is still valid. Shrinks the shared
    /// prefix for each key just before it goes in: shrinking for all first
    /// could change the prefixes an earlier key's split fits. Outlined, to
    /// keep the lanes out of appends and small trees.
    #[inline(never)]
    fn insert_batch<const N: usize>(
        &mut self,
        entries: [(K, V); N],
        alloc: &impl Allocator,
        length: &mut usize,
    ) -> [Option<V>; N]
    where
        K: Ord,
    {
        let Some(root_page) = self.root_page_mut() else {
            return entries.map(|(key, value)| self.insert(key, value, alloc, length));
        };
        let Node::Internal(root) = root_page.node() else {
            return entries.map(|(key, value)| self.insert(key, value, alloc, length));
        };
        let shared = root_page.shared_prefix();
        let mut destinations = core::array::from_fn::<_, N, _>(|_| Destination::FromRoot);
        let mut outputs = destinations.iter_mut();
        Executor::run(entries.each_ref().map(|(key, _)| {
            let output = outputs.next().expect("one destination per key");
            Lane::new(key, root, shared).find_destination(output)
        }));
        let mut destinations = destinations.into_iter();
        let mut descents_valid = true;
        entries.map(|(key, value)| {
            let destination = destinations.next().expect("one destination per key");
            if !descents_valid || matches!(destination, Destination::FromRoot) {
                // This insertion can split a leaf no destination names, so
                // no later key can trust the descents.
                descents_valid = false;
                return self.insert(key, value, alloc, length);
            }
            let key = self.shrink_shared_prefix_for(key, alloc);
            let found = match destination {
                Destination::Separator(internal, index) => Ok(Found::Separator(internal, index)),
                Destination::Leaf(leaf, searched_len, found) => {
                    // An insertion that fit since changed the leaf's length;
                    // a replacement doesn't, and a split made `FromRoot`.
                    let found = if leaf.len() == searched_len {
                        found
                    } else {
                        leaf.search_always_inlined(&SearchKey::new(key.key()))
                    };
                    match found {
                        Ok(index) => Ok(Found::InLeaf(leaf, index)),
                        Err(edge) => Err((leaf, edge)),
                    }
                }
                Destination::FromRoot => unreachable!("inserted from the root above"),
            };
            match found {
                Ok(found) => {
                    // As the single-key insert: the new key drops before
                    // the value changes.
                    drop(key);
                    Some(mem::replace(found.value_mut(), value))
                }
                Err((leaf, edge)) if leaf.len() < Leaf::<K, V, S>::capacity() => {
                    leaf.insert_fit(edge, key.window_at(leaf.prefix_len()), value);
                    *length += 1;
                    None
                }
                Err((leaf, _)) => {
                    let previous = self.insert_with_stack(key, value, alloc, length);
                    for destination in destinations.as_mut_slice() {
                        destination.invalidate_after_split(leaf);
                    }
                    previous
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;
    use core::marker::PhantomPinned;
    use std::fmt::Debug;
    use std::format;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::string::String;
    use std::vec::Vec;

    use super::*;
    use crate::inline_key::{Auto, WithInlineKey};
    use crate::tree::node::page::TestPages;
    use crate::tree::test_support::TestRng;

    #[test]
    fn lookups_match_single_key_finds_at_every_depth() {
        let pages = TestPages::<u64, u64>::new();
        let mut tree = Tree::<_, _, Auto>::EMPTY;
        let mut length = 0;
        assert_eq!(tree.find_many::<u64, 0>([], usize::MAX), []);
        assert_eq!(tree.find_many([&1, &1], usize::MAX), [None, None]);
        let count = if cfg!(miri) { 200 } else { 10_000 };
        let mut keys: Vec<u64> = (0..count).map(|key| key * 2).collect();
        TestRng::new(53).shuffle(&mut keys);
        for (index, &key) in keys.iter().enumerate() {
            tree.insert(key, key, &pages, &mut length);
            if index % 37 == 0 {
                let queries = [key, key + 1, 0, count * 2, key, key / 2, 42, 1];
                assert_eq!(
                    tree.find_many(queries.each_ref(), usize::MAX),
                    queries.map(|key| tree.find(&key).map(|(_, value)| value))
                );
            }
        }
        // Every key, separators included, not only a sample.
        for chunk in keys.chunks(8) {
            let queries: [&u64; 8] = core::array::from_fn(|i| &chunk[i % chunk.len()]);
            assert_eq!(
                tree.find_many(queries, usize::MAX),
                queries.map(|key| tree.find(key).map(|(_, value)| value))
            );
        }
        let separator = *tree.root().expect_internal().separator_key(0);
        assert_eq!(
            tree.find_many([&separator; 8], usize::MAX),
            [Some(&separator); 8]
        );
        tree.check_invariants(length);
        tree.destroy(&pages);
        pages.assert_all_released();
    }

    #[test]
    fn borrowed_queries_outside_the_shared_prefix_find_nothing() {
        let pages = TestPages::<String, usize>::new();
        let mut tree = Tree::<_, _, Auto>::EMPTY;
        let mut length = 0;
        for key in 0..if cfg!(miri) { 100 } else { 1000 } {
            tree.insert(format!("shared/{key:06}"), key, &pages, &mut length);
        }
        let queries = [
            "before",
            "shared/000004",
            "zz",
            "shared/000004",
            "shared/000002",
            "sh",
            "shared/999999",
            "",
        ];
        assert_eq!(
            tree.find_many(queries, usize::MAX),
            queries.map(|key| tree.find(key).map(|(_, value)| value))
        );
        assert_eq!(tree.find_many(["", "z"], usize::MAX), [None, None]);
        tree.destroy(&pages);
        pages.assert_all_released();
    }

    #[test]
    fn heap_candidates_match_with_long_and_unequal_tails() {
        let pages = TestPages::<String, usize>::new();
        let mut tree = Tree::<_, _, Auto>::EMPTY;
        let mut length = 0;
        let count = if cfg!(miri) { 64 } else { 512 };
        let tail = "abc0123456789".repeat(10);
        let keys: Vec<_> = (0..count)
            .map(|index| {
                let head = (index as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
                format!("{head:016x}/{tail}")
            })
            .collect();
        for (index, key) in keys.iter().enumerate() {
            tree.insert(key.clone(), index, &pages, &mut length);
        }
        for chunk in keys.as_chunks::<8>().0 {
            let queries: [_; 8] = core::array::from_fn(|lane| {
                let mut key = chunk[lane].clone();
                if lane % 2 != 0 {
                    key.pop();
                    key.push('z');
                }
                key
            });
            let queries = queries.each_ref().map(String::as_str);
            assert_eq!(
                tree.find_many(queries, usize::MAX),
                queries.map(|key| tree.find(key).map(|(_, value)| value))
            );
        }
        tree.destroy(&pages);
        pages.assert_all_released();
    }

    #[test]
    fn fixed_array_queries_match_at_short_and_long_prefixes() {
        for prefix_len in [0, 24, 25, 30] {
            let pages = TestPages::<[u8; 32], usize>::new();
            let mut tree = Tree::<_, _, Auto>::EMPTY;
            let mut length = 0;
            let count = if cfg!(miri) { 64 } else { 256 };
            let mut keys: Vec<_> = (0..count)
                .map(|index| {
                    let mut key = [0; 32];
                    key[..prefix_len].fill(0xa5);
                    key[prefix_len..prefix_len + 2]
                        .copy_from_slice(&(2 * index as u16).to_be_bytes());
                    key
                })
                .collect();
            TestRng::new(71).shuffle(&mut keys);
            for (index, &key) in keys.iter().enumerate() {
                tree.insert(key, index, &pages, &mut length);
            }
            tree.check_invariants(length);
            for chunk in keys.as_chunks::<8>().0 {
                let queries: [_; 8] = core::array::from_fn(|lane| {
                    let mut key = chunk[lane];
                    if lane % 2 != 0 {
                        key[prefix_len + 1] |= 1;
                    }
                    key
                });
                assert_eq!(
                    tree.find_many(queries.each_ref(), usize::MAX),
                    queries
                        .each_ref()
                        .map(|key| tree.find(key).map(|(_, value)| value))
                );
            }
            tree.destroy(&pages);
            pages.assert_all_released();
        }
    }

    #[test]
    fn more_than_64_lanes_include_absent_keys() {
        let pages = TestPages::<[u8; 32], usize>::new();
        let mut tree = Tree::<_, _, Auto>::EMPTY;
        let mut length = 0;
        let count = if cfg!(miri) { 80 } else { 2000 };
        for index in 0..count {
            let mut key = [0xa5; 32];
            key[28..].copy_from_slice(&(2 * index as u32).to_be_bytes());
            tree.insert(key, index, &pages, &mut length);
        }
        for start in (0..2 * count).step_by(65) {
            let keys = core::array::from_fn::<_, 65, _>(|i| {
                let mut key = [0xa5; 32];
                key[28..].copy_from_slice(&((start + i) as u32).to_be_bytes());
                key
            });
            assert_eq!(
                tree.find_many(keys.each_ref(), usize::MAX),
                keys.each_ref()
                    .map(|key| tree.find(key).map(|(_, value)| value))
            );
        }
        tree.check_invariants(length);
        tree.destroy(&pages);
        pages.assert_all_released();
    }

    struct InsertBatches;

    impl InsertBatches {
        /// Inserts `keys` a batch of `N` at a time and one at a time, into
        /// two trees that must match node for node after every batch.
        fn check<K: Ord + Clone + Debug, S: InlineKeyStrategy<K>, const N: usize>(keys: Vec<K>) {
            let pages = TestPages::<K, usize, S>::new();
            let mut batched = Tree::<_, _, S>::EMPTY;
            let mut individual = Tree::<_, _, S>::EMPTY;
            let (mut batch_len, mut individual_len) = (0, 0);
            for (batch_index, keys) in keys.chunks(N).enumerate() {
                let entries: [_; N] = core::array::from_fn(|lane| {
                    (keys[lane % keys.len()].clone(), batch_index * N + lane)
                });
                let expected = entries
                    .clone()
                    .map(|(key, value)| individual.insert(key, value, &pages, &mut individual_len));
                // The lanes even on small trees, which keeps the cases
                // affordable under Miri.
                assert_eq!(
                    batched.insert_batch(entries, &pages, &mut batch_len),
                    expected
                );
                assert_eq!(batch_len, individual_len);
                batched.check_invariants(batch_len);
                individual.check_invariants(individual_len);
                batched.check_same_nodes(&individual);
                let queries = core::array::from_fn::<_, N, _>(|lane| &keys[lane % keys.len()]);
                assert_eq!(
                    batched.find_many(queries, usize::MAX),
                    queries.map(|key| individual.find(key).map(|(_, value)| value))
                );
                for key in keys {
                    assert_eq!(batched.find(key), individual.find(key));
                }
            }
            batched.destroy(&pages);
            individual.destroy(&pages);
            pages.assert_all_released();
        }
    }

    #[test]
    fn insert_batches_with_duplicates_splits_and_raises_match_node_for_node() {
        let count = if cfg!(miri) { 240 } else { 4000 };
        let mut numbers: Vec<u64> = (0..count).chain(0..count).collect();
        TestRng::new(54).shuffle(&mut numbers);
        InsertBatches::check::<_, Auto, 8>(numbers.clone());
        InsertBatches::check::<_, Auto, 32>(numbers.clone());
        // Wide plain keys make raises frequent, inside a batch too.
        InsertBatches::check::<_, Auto, 16>(numbers.into_iter().map(|n| [n; 8]).collect());
    }

    #[test]
    fn insert_batches_shrinking_prefixes_or_ascending_match_node_for_node() {
        let count = if cfg!(miri) { 160 } else { 1600 };
        let keys: Vec<String> = (0..count)
            .map(|n| {
                if n % 41 == 40 {
                    format!("{n:06}")
                } else {
                    format!("shared/path/{n:06}")
                }
            })
            .collect();
        InsertBatches::check::<_, Auto, 8>(keys.clone());
        InsertBatches::check::<_, WithInlineKey, 4>(keys);
        InsertBatches::check::<_, Auto, 8>((0..count as u64).collect());
    }

    /// A lane that checks the executor: that it never moves a lane once
    /// polled, never polls a finished one, and drops each once, even when a
    /// lane panics.
    struct PinnedLane<'a> {
        pending_polls: Cell<usize>,
        address: Cell<usize>,
        finished: &'a Cell<usize>,
        dropped: &'a Cell<usize>,
        panics: bool,
        _pinned: PhantomPinned,
    }

    impl Future for PinnedLane<'_> {
        type Output = ();

        fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
            let address = core::ptr::from_ref(self.as_ref().get_ref()).addr();
            if self.address.get() == 0 {
                self.address.set(address);
            }
            assert_eq!(self.address.get(), address, "a polled lane moved");
            assert!(!self.panics, "a lane panics");
            let pending_polls = self.pending_polls.get();
            assert_ne!(pending_polls, usize::MAX, "a finished lane was polled");
            if pending_polls == 0 {
                self.pending_polls.set(usize::MAX);
                self.finished.set(self.finished.get() + 1);
                Poll::Ready(())
            } else {
                self.pending_polls.set(pending_polls - 1);
                Poll::Pending
            }
        }
    }

    impl Drop for PinnedLane<'_> {
        fn drop(&mut self) {
            if self.address.get() != 0 {
                assert_eq!(self.address.get(), core::ptr::from_ref(self).addr());
            }
            self.dropped.set(self.dropped.get() + 1);
        }
    }

    impl PinnedLane<'_> {
        fn check<const N: usize>(panics: bool) {
            let finished = Cell::new(0);
            let dropped = Cell::new(0);
            let lanes = core::array::from_fn::<_, N, _>(|index| PinnedLane {
                pending_polls: Cell::new(index % 4),
                address: Cell::new(0),
                finished: &finished,
                dropped: &dropped,
                panics: panics && index == N / 2,
                _pinned: PhantomPinned,
            });
            let result = catch_unwind(AssertUnwindSafe(|| Executor::run(lanes)));
            assert_eq!(result.is_err(), panics && N != 0);
            assert_eq!(dropped.get(), N);
            if !panics {
                assert_eq!(finished.get(), N);
            }
        }
    }

    #[test]
    fn executor_keeps_lanes_pinned_and_drops_each_once() {
        PinnedLane::check::<0>(false);
        PinnedLane::check::<1>(false);
        PinnedLane::check::<32>(false);
        PinnedLane::check::<64>(false);
        PinnedLane::check::<65>(false);
        if !cfg!(miri) {
            PinnedLane::check::<257>(false);
        }
        PinnedLane::check::<4>(true);
        PinnedLane::check::<65>(true);
    }
}
