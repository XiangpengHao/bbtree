//! Batched lookups in a tree that threads share (doc 12,
//! `doc/09-concurrency.md`): bbtree's lanes and executor (`tree/batch.rs`)
//! on the optimistic descent (`descent.rs`). Each key descends in its own
//! lane, which prefetches the line it reads next and yields, so that one
//! lane's wait for memory overlaps the others' searches.
//!
//! A suspended lane holds no latch. It reads the internal nodes and its
//! leaf's inline keys optimistically, at versions it checks, and latches the
//! leaf only in the poll that searches it, runs the closure and lets go. So
//! no lane waits for a latch another lane of its batch holds, and a lane that
//! another thread's change sends back starts over from the root alone.
//!
//! - **Internal nodes**, as the descent reads them: route the key, fetch the
//!   chosen edge, read it, check the node's version; fetch the child's
//!   header, read its version, check the node's again.
//! - **The leaf**: its inline keys, read at the version the lane read, rule
//!   the key out, which that version, unchanged, then confirms without a
//!   latch; or name its one candidate, whose entry the lane fetches before
//!   it latches the leaf. A heap key's bytes the lane fetches too, their
//!   address read with the leaf latched a moment. Latched shared, the leaf
//!   still at that version, the lane compares the candidate and runs the
//!   closure. Ties go to the leaf's search, latched.
//! - **Separators**: a key equal to one is its leaf's head, which the lane
//!   checks with the leaf latched, as the descent does.

use core::borrow::Borrow;
use core::cell::RefCell;
use core::future::Future;

use super::super::batch::{BatchSearch, Executor, Prefetcher};
use super::super::node::Node;
use super::super::node::inline_keys::SearchKey;
use super::super::node::latch::{Latched, Version};
use super::super::node::leaf::{CandidateLine, Leaf};
use super::LatchedTree;
use super::descent::{HeldLeaf, RootRead, Shared};
use crate::inline_key::InlineKeyStrategy;

/// The leaf changed after the lane read it, or the separator its key
/// matched was removed: the lane starts over from the root.
struct Restart;

/// What a batch's lanes share: the tree, and the closure each runs on its
/// entry, one lane at a time. Each lane keeps one pointer for both.
struct ReadBatch<'t, K, V, S, F> {
    tree: &'t LatchedTree<K, V, S>,
    f: RefCell<F>,
}

/// One key's lookup in a batch: the batch, and the key, whose search key is
/// made once (doc 12). With the few locals of [`Self::read`], all a
/// suspended lane holds: 88 bytes for `u64` keys.
struct Lane<'b, 'q, K, V, S, Q: ?Sized, F> {
    batch: &'b ReadBatch<'b, K, V, S, F>,
    key: SearchKey<'q, K, Latched<S>, Q>,
}

impl<K, V, S: InlineKeyStrategy<K>> LatchedTree<K, V, S> {
    /// Runs `f` on each key's entry, while its leaf is latched shared, and
    /// returns what `f` returned, in input order, as separate [`Self::read`]s
    /// do; `f` runs in the order the lanes reach their entries.
    pub(crate) fn read_many<Q, R, const N: usize>(
        &self,
        keys: [&Q; N],
        f: impl FnMut(&K, &V) -> R,
    ) -> [Option<R>; N]
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if N < 2 || !self.batches() {
            let mut f = f;
            return keys.map(|key| self.read(key, &mut f));
        }
        let batch = ReadBatch {
            tree: self,
            f: RefCell::new(f),
        };
        let mut results = core::array::from_fn(|_| None);
        let mut outputs = results.iter_mut();
        Executor::run(keys.map(|key| {
            let lane = Lane {
                batch: &batch,
                key: SearchKey::new(key),
            };
            lane.read(outputs.next().expect("one result per key"))
        }));
        results
    }

    /// Whether the tree is large enough for batches to pay for their lanes:
    /// about an L2 of leaf pages, as bbtree's cutoff (doc 12). Below, the
    /// lanes find their lines in cache and cost more than they save. The
    /// tree keeps no length, so its height tells instead: a tree that splits
    /// grew has three internal levels only past 2·m² leaves, m half an
    /// internal node's capacity, about 1,150 leaf pages for `u64` keys,
    /// just over L2's 1 MiB. Read optimistically, a hint. Tests batch on
    /// every tree with an internal root, to reach the lanes on small trees.
    fn batches(&self) -> bool {
        let Some(root_page) = self.root_page() else {
            return false;
        };
        let Node::Internal(root) = root_page.node() else {
            return false;
        };
        if cfg!(test) {
            return true;
        }
        let Some(version) = self.root_version(root) else {
            return false;
        };
        let child = root.racing_child(0);
        if !root.latch().is_unchanged(version) {
            return false;
        }
        // Pages below the root keep their kind (`pages.rs`): an edge read
        // from one is null or a node of the kind its tag says.
        matches!(child, Some(Node::Internal(child))
            if matches!(child.racing_child(0), Some(Node::Internal(_))))
    }
}

impl<'b, K, V, S, Q, F> Lane<'b, '_, K, V, S, Q, F>
where
    K: Borrow<Q>,
    S: InlineKeyStrategy<K>,
    Q: Ord + ?Sized,
{
    /// Looks the key up, writing what the batch's closure returned on its
    /// entry to `output`, if the tree has one. As bbtree's lane, one flat
    /// async block that returns unit (doc 12).
    #[expect(
        clippy::manual_async_fn,
        reason = "the block's attribute reaches its poll"
    )]
    fn read<R>(self, output: &'b mut Option<R>) -> impl Future<Output = ()>
    where
        F: FnMut(&K, &V) -> R,
    {
        #[inline(always)]
        async move {
            'restart: loop {
                let tree = self.batch.tree;
                let root_page = tree.root_page().expect("lanes run on a tree");
                let Node::Internal(root) = root_page.node() else {
                    // Lowered to a leaf since the batch started.
                    *output = tree.read(self.key.key(), |key, value| {
                        (self.batch.f.borrow_mut())(key, value)
                    });
                    return;
                };
                let (mut node, mut version) = match tree.read_root(root_page, root, &self.key) {
                    RootRead::At(version) => (root, version),
                    RootRead::Outside(_) => return,
                    RootRead::Changed => continue 'restart,
                };
                if let Some(line) = self.key.window_line(node.racing_prefix_len()) {
                    Prefetcher::fetch(line).await;
                }
                let mut at_separator = false;
                let leaf = loop {
                    let Some(routed) = node.route_racing(&self.key, version) else {
                        continue 'restart;
                    };
                    at_separator |= routed.at_separator;
                    let edge = routed.edge as u16;
                    Prefetcher::fetch(node.edge_line(usize::from(edge))).await;
                    let Some(child) = node.racing_child(usize::from(edge)) else {
                        continue 'restart;
                    };
                    // Followed only once the node's version vouches for it.
                    if !node.latch().is_unchanged(version) {
                        continue 'restart;
                    }
                    match child {
                        Node::Leaf(leaf) => {
                            Prefetcher::fetch(leaf.search_line()).await;
                            let leaf_version = leaf.latch().read_version();
                            if !node.latch().is_unchanged(version) {
                                continue 'restart;
                            }
                            version = leaf_version;
                            break leaf;
                        }
                        Node::Internal(child) => {
                            Prefetcher::fetch(child.search_line()).await;
                            let child_version = child.latch().read_version();
                            if !node.latch().is_unchanged(version) {
                                continue 'restart;
                            }
                            (node, version) = (child, child_version);
                        }
                    }
                };
                let found = if at_separator {
                    LatchedTree::read_head(leaf, &self.key, &self.batch.f).map(Some)
                } else {
                    let (candidate, prefix_len) = leaf.candidate_line_racing(&self.key);
                    let index = match candidate {
                        CandidateLine::Absent if leaf.latch().is_unchanged(version) => return,
                        CandidateLine::Absent => continue 'restart,
                        CandidateLine::One(index, line) => {
                            let (index, prefix_len) = (index as u16, prefix_len as u16);
                            Prefetcher::fetch(line).await;
                            if Leaf::<K, V, Latched<S>>::plan().has_length_tags() {
                                let line = LatchedTree::candidate_bytes_line(
                                    leaf, version, &self.key, index, prefix_len,
                                );
                                match line {
                                    Ok(Some(line)) => Prefetcher::fetch(line).await,
                                    Ok(None) => {}
                                    Err(Restart) => continue 'restart,
                                }
                            }
                            Some((index, prefix_len))
                        }
                        CandidateLine::Unknown => None,
                    };
                    LatchedTree::read_in_leaf(leaf, version, &self.key, index, &self.batch.f)
                };
                match found {
                    Ok(result) => *output = result,
                    Err(Restart) => continue 'restart,
                }
                return;
            }
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> LatchedTree<K, V, S> {
    /// What `f` returns on the entry of `key` in `leaf`, latched shared, if
    /// the leaf is still at `version`, the version it was read at to find
    /// `candidate`: the one key whose inline key, at the prefix length with
    /// it, equals `key`'s. `None` if that key isn't `key`. Without a
    /// candidate, the leaf's own search. Synchronous: the latch is never
    /// held across a lane's yield.
    #[inline(always)]
    fn read_in_leaf<Q, R>(
        leaf: Leaf<K, V, Latched<S>>,
        version: Version,
        key: &SearchKey<'_, K, Latched<S>, Q>,
        candidate: Option<(u16, u16)>,
        f: &RefCell<impl FnMut(&K, &V) -> R>,
    ) -> Result<Option<R>, Restart>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let held = HeldLeaf::<K, V, S, Shared>::lock(leaf);
        if !leaf.latch().is_unchanged(version) {
            return Err(Restart);
        }
        let found = match candidate {
            // SAFETY: the leaf is at the version it was read at to find the
            // candidate and its prefix length, and latched: it hasn't changed
            // since, and doesn't while held.
            Some((index, prefix_len)) => unsafe {
                leaf.find_candidate(key, usize::from(index), usize::from(prefix_len))
            },
            None => leaf.find(key, BatchSearch),
        };
        let result = found.map(|index| {
            // SAFETY: a search returns the index of a key of the leaf.
            let (key, value) = unsafe { leaf.key_value_unchecked(index) };
            (f.borrow_mut())(key, value)
        });
        drop(held);
        Ok(result)
    }

    /// The first line of `candidate`'s heap bytes that comparing it with
    /// `key` reads, as [`Self::read_in_leaf`]'s candidate, read with the leaf
    /// latched shared if it is still at `version`; `None` if the comparison
    /// reads none. The lane lets go before it fetches the line: a pointer
    /// into a key is read only under the latch, and a prefetch of a stale
    /// one is harmless.
    fn candidate_bytes_line<Q>(
        leaf: Leaf<K, V, Latched<S>>,
        version: Version,
        key: &SearchKey<'_, K, Latched<S>, Q>,
        candidate: u16,
        prefix_len: u16,
    ) -> Result<Option<*const u8>, Restart>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let held = HeldLeaf::<K, V, S, Shared>::lock(leaf);
        if !leaf.latch().is_unchanged(version) {
            return Err(Restart);
        }
        // SAFETY: the leaf is at the version it was read at to find the
        // candidate and its prefix length, and latched.
        let line = unsafe {
            leaf.candidate_bytes_line(key, usize::from(candidate), usize::from(prefix_len))
        };
        drop(held);
        Ok(line)
    }

    /// What `f` returns on `leaf`'s head, latched shared, if its key is
    /// `key`, which matched the separator left of the leaf; that separator
    /// may have been removed since, its successor the head now. Synchronous,
    /// as [`Self::read_in_leaf`].
    fn read_head<Q, R>(
        leaf: Leaf<K, V, Latched<S>>,
        key: &SearchKey<'_, K, Latched<S>, Q>,
        f: &RefCell<impl FnMut(&K, &V) -> R>,
    ) -> Result<R, Restart>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let held = HeldLeaf::<K, V, S, Shared>::lock(leaf);
        if !Self::is_head(leaf, key) {
            return Err(Restart);
        }
        let result = (f.borrow_mut())(leaf.head_key(), leaf.head_value());
        drop(held);
        Ok(result)
    }
}
