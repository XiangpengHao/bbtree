//! Batched lookups and inserts in a tree that threads share (doc 12,
//! `doc/09-concurrency.md`, D55): the unsync tree's lanes and executor
//! (`tree/batch.rs`) on the optimistic descent (`descent.rs`). Each key
//! descends in its own lane, which prefetches the line it reads next and
//! yields, so that one lane's wait for memory overlaps the others' searches.
//!
//! A suspended lane holds no latch. It reads the internal nodes and its leaf's
//! inline keys optimistically, at versions it checks. It latches the leaf only
//! in the poll that searches it, runs the closure and lets go. So no lane waits
//! for a latch another lane of its batch holds, and a lane that another
//! thread's change sends back starts over from the root alone.
//!
//! - **Internal nodes**, as the descent reads them: route the key, fetch the
//!   chosen edge, read it, check the node's version; fetch the child's
//!   header, read its version, check the node's again.
//! - **The leaf**: its inline keys, read at the version the lane read,
//!   either rule the key out or name its one candidate. A key ruled out is
//!   absent once that version, unchanged, confirms it, without a latch. A
//!   candidate's entry the lane fetches before it latches the leaf. For a
//!   heap key it fetches the key's bytes too, their address read with the
//!   leaf latched a moment. Then, with the leaf latched shared and still at
//!   that version, the lane compares the candidate and runs the closure.
//!   Ties go to the leaf's search, latched.
//! - **Copies** ([`LatchedTree::get_many_copied`]): a lane that copies a
//!   `Copy` value copies the candidate's unlatched instead, then checks the
//!   leaf's version. That works where the leaf settles the key, as for a
//!   single [`LatchedTree::get_copied`] (`lookup.rs`); such a hit writes
//!   nothing either.
//! - **Separators**: a key equal to one is its leaf's head, which the lane
//!   checks with the leaf latched, as the descent does.

use core::borrow::Borrow;
use core::cell::RefCell;
use core::future::Future;
use core::mem::ManuallyDrop;

use super::super::batch::{BatchSearch, Executor, Prefetcher};
use super::super::node::Node;
use super::super::node::inline_keys::SearchKey;
use super::super::node::latch::{Latched, Version};
use super::super::node::leaf::{CandidateLine, Leaf};
use super::LatchedTree;
use super::descent::{HeldLeaf, RootRead};
use super::insert::Replace;
use super::lookup::{RacingRead, Unsettled};
use crate::allocator::Allocator;
use crate::inline_key::InlineKeyStrategy;

/// The leaf changed after the lane read it, or the separator its key
/// matched was removed: the lane starts over from the root.
struct Restart;

/// What a batch's lanes do with the entries they find: run the caller's
/// closure on each, latched ([`ReadWith`]), or copy its value out
/// ([`CopyValue`]).
trait EntryVisit<K, V, S: InlineKeyStrategy<K>> {
    type Output;

    /// Whether a lane copies its candidate's entry unlatched and checks the
    /// leaf's version after, for keys ordered by their own bytes, rather
    /// than latch the leaf (`lookup.rs`).
    const COPIES: bool;

    /// What the batch returns for the entry `key` and `value`.
    fn visit(&mut self, key: &K, value: &V) -> Self::Output;

    /// What the batch returns for `value`, copied out of the leaf unlatched:
    /// only where [`Self::COPIES`].
    fn copied(&mut self, value: ManuallyDrop<V>) -> Self::Output;

    /// One key's lookup, as a batch too small for lanes looks it up.
    fn look_up<Q>(&mut self, tree: &LatchedTree<K, V, S>, key: &Q) -> Option<Self::Output>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized;
}

/// Runs the caller's closure on each entry, latched shared.
struct ReadWith<F>(F);

/// Copies each entry's value out.
struct CopyValue;

impl<K, V, S: InlineKeyStrategy<K>, R, F: FnMut(&K, &V) -> R> EntryVisit<K, V, S> for ReadWith<F> {
    type Output = R;

    const COPIES: bool = false;

    #[inline(always)]
    fn visit(&mut self, key: &K, value: &V) -> R {
        (self.0)(key, value)
    }

    fn copied(&mut self, _: ManuallyDrop<V>) -> R {
        unreachable!("a batch that reads with a closure copies nothing")
    }

    #[inline(always)]
    fn look_up<Q>(&mut self, tree: &LatchedTree<K, V, S>, key: &Q) -> Option<R>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        tree.read(key, &mut self.0)
    }
}

impl<K, V: Copy, S: InlineKeyStrategy<K>> EntryVisit<K, V, S> for CopyValue {
    type Output = V;

    const COPIES: bool = true;

    #[inline(always)]
    fn visit(&mut self, _: &K, value: &V) -> V {
        *value
    }

    #[inline(always)]
    fn copied(&mut self, value: ManuallyDrop<V>) -> V {
        ManuallyDrop::into_inner(value)
    }

    #[inline(always)]
    fn look_up<Q>(&mut self, tree: &LatchedTree<K, V, S>, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        tree.get_copied(key)
    }
}

/// What a batch's lanes share: the tree, and what each does with its entry,
/// one lane at a time. Each lane keeps one pointer for both.
struct ReadBatch<'t, K, V, S, B> {
    tree: &'t LatchedTree<K, V, S>,
    visit: RefCell<B>,
}

/// One key's lookup in a batch: the batch, and the key, whose search key is
/// made once (doc 12). With the few locals of [`Self::read`], this is all a
/// suspended lane holds.
struct Lane<'b, 'q, K, V, S, Q: ?Sized, B> {
    batch: &'b ReadBatch<'b, K, V, S, B>,
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
        self.look_up_many(keys, ReadWith(f))
    }

    /// A copy of each key's value, in input order, as separate
    /// [`Self::get_copied`]s give them.
    pub(crate) fn get_many_copied<Q, const N: usize>(&self, keys: [&Q; N]) -> [Option<V>; N]
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
        V: Copy,
    {
        self.look_up_many(keys, CopyValue)
    }

    /// Looks each key up, in its own lane, and returns what `visit` made of
    /// its entry, in input order.
    #[inline(always)]
    fn look_up_many<Q, B: EntryVisit<K, V, S>, const N: usize>(
        &self,
        keys: [&Q; N],
        mut visit: B,
    ) -> [Option<B::Output>; N]
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if N < 2 || !self.batches() {
            return keys.map(|key| visit.look_up(self, key));
        }
        let batch = ReadBatch {
            tree: self,
            visit: RefCell::new(visit),
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
    /// about an L2 of leaf pages, as the unsync tree's cutoff (doc 12, D55).
    /// Below that, the lanes find their lines in cache and cost more than they
    /// save. The tree keeps no length, so its height tells instead. A tree
    /// grown by splits has three internal levels only past 2·m² leaves, m being
    /// half an internal node's capacity: about 1,150 leaf pages for `u64` keys,
    /// just over a 1 MiB L2. The height is read optimistically, so it is a
    /// hint. Tests batch on every tree with an internal root, to reach the
    /// lanes on small trees.
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

/// Where a batched insert's key goes, as its lane found it: the leaf, and the
/// version the lane read it at. Also whether the key matched a separator, and
/// so is the leaf's head (D17), which the insert checks.
struct Destination<K, V, S> {
    leaf: Leaf<K, V, Latched<S>>,
    version: Version,
    at_separator: bool,
}

/// A batched insert that its lane's leaf didn't take, and how it goes in
/// instead.
enum Fallback<K, V> {
    /// The leaf is full, and holds the key's place: an insert that splits.
    Full(K, V),
    /// The leaf changed, another thread holds it, or it lost the head the
    /// key matched: a single insert.
    Changed(K, V),
}

/// One key's descent in a batch of inserts, to the leaf where it goes: as a
/// lookup's lane descends, without reading the leaf's entries, which the
/// insert searches latched.
struct LeafLane<'t, 'q, K, V, S, Q: ?Sized> {
    tree: &'t LatchedTree<K, V, S>,
    key: SearchKey<'q, K, Latched<S>, Q>,
}

impl<K, V, S: InlineKeyStrategy<K>> LatchedTree<K, V, S> {
    /// Inserts `entries` in input order, and returns each replaced value, as
    /// separate [`Self::insert`]s with [`Replace`] do; pages come from
    /// `alloc`.
    pub(crate) fn insert_many<const N: usize>(
        &self,
        entries: [(K, V); N],
        alloc: &impl Allocator,
    ) -> [Option<V>; N]
    where
        K: Ord,
    {
        if N < 2 || !self.batches() {
            return entries.map(|(key, value)| self.insert(key, value, Replace, alloc));
        }
        let mut entries = entries.into_iter();
        let (key, value) = entries.next().expect("N >= 2");
        let first = match self.append(key, value, Replace) {
            Ok(first) => {
                // Ascending keys: the rest most likely append too, which the
                // single-key insert does without a descent.
                let mut first = Some(first);
                return core::array::from_fn(|_| match first.take() {
                    Some(first) => first,
                    None => {
                        let (key, value) = entries.next().expect("one entry per key");
                        self.insert(key, value, Replace, alloc)
                    }
                });
            }
            Err((key, value, Replace)) => (key, value),
        };
        let mut first = Some(first);
        let entries = core::array::from_fn(|_| {
            first
                .take()
                .unwrap_or_else(|| entries.next().expect("one entry per key"))
        });
        self.insert_batch(entries, alloc)
    }

    /// Finds every key's leaf first, in lanes, then inserts the entries in
    /// input order. Each goes into its leaf if the leaf is still at the version
    /// its lane read, as moved on by the batch's own inserts there. Otherwise,
    /// or where the leaf is full, it goes in as a single insert does. Outlined,
    /// to keep the lanes out of appends and small trees.
    #[inline(never)]
    fn insert_batch<const N: usize>(
        &self,
        entries: [(K, V); N],
        alloc: &impl Allocator,
    ) -> [Option<V>; N]
    where
        K: Ord,
    {
        let mut destinations = core::array::from_fn::<_, N, _>(|_| None);
        let mut outputs = destinations.iter_mut();
        Executor::run(entries.each_ref().map(|(key, _)| {
            let lane = LeafLane {
                tree: self,
                key: SearchKey::new(key),
            };
            lane.find_leaf(outputs.next().expect("one destination per key"))
        }));
        let mut next = 0;
        entries.map(|(key, value)| {
            let destination = destinations[next].take();
            next += 1;
            let Some(destination) = destination else {
                return self.insert(key, value, Replace, alloc);
            };
            let (leaf, version) = (destination.leaf, destination.version);
            let fallback = match self.insert_at(destination, key, value, alloc) {
                Ok(previous) => {
                    // Later keys whose lanes read the leaf as this insert
                    // found it trust it as it is now. A lane that read it
                    // before another thread changed it doesn't.
                    for later in destinations[next..].iter_mut().flatten() {
                        if later.leaf.node_ptr().is(leaf.node_ptr()) && later.version == version {
                            later.version = version.next();
                        }
                    }
                    return previous;
                }
                Err(fallback) => fallback,
            };
            // The leaf changed, or splits: no later key trusts it.
            for later in &mut destinations[next..] {
                if later
                    .as_ref()
                    .is_some_and(|later| later.leaf.node_ptr().is(leaf.node_ptr()))
                {
                    *later = None;
                }
            }
            match fallback {
                Fallback::Full(key, value) => self.insert_splitting(key, value, Replace, alloc),
                Fallback::Changed(key, value) => self.insert(key, value, Replace, alloc),
            }
        })
    }

    /// Inserts `key` and `value` into the leaf `destination` names, and returns
    /// the replaced value. That takes latching the leaf exclusively at the
    /// version its lane read it at, when it held the key's place, and a leaf
    /// with room or with the key. The leaf is at the next version after.
    /// Otherwise changes nothing and gives the entry back ([`Fallback`]).
    #[inline(always)]
    fn insert_at(
        &self,
        destination: Destination<K, V, S>,
        key: K,
        value: V,
        alloc: &impl Allocator,
    ) -> Result<Option<V>, Fallback<K, V>>
    where
        K: Ord,
    {
        let Destination {
            leaf,
            version,
            at_separator,
        } = destination;
        let Some(held) = HeldLeaf::try_lock_at(leaf, version) else {
            return Err(Fallback::Changed(key, value));
        };
        if at_separator && !Self::is_head(leaf, &SearchKey::new(&key)) {
            return Err(Fallback::Changed(key, value));
        }
        match self.insert_into(&held, at_separator, key, value, Replace, alloc) {
            Ok(previous) => Ok(previous),
            Err((key, value, Replace, _)) => Err(Fallback::Full(key, value)),
        }
    }
}

impl<'t, 'q, K, V, S, Q> LeafLane<'t, 'q, K, V, S, Q>
where
    K: Borrow<Q>,
    S: InlineKeyStrategy<K>,
    Q: Ord + ?Sized,
{
    /// Finds the leaf where the key goes, writing it to `output`, as
    /// [`Lane::read`] descends; `None` for a key that lacks what every key
    /// shares, which a single insert takes, shrinking that first.
    #[expect(
        clippy::manual_async_fn,
        reason = "the block's attribute reaches its poll"
    )]
    fn find_leaf(self, output: &'t mut Option<Destination<K, V, S>>) -> impl Future<Output = ()> {
        #[inline(always)]
        async move {
            'restart: loop {
                let tree = self.tree;
                let root_page = tree.root_page().expect("lanes run on a tree");
                let Node::Internal(root) = root_page.node() else {
                    // Lowered to a leaf since the batch started.
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
                let mut at_root = true;
                loop {
                    let Some(routed) = node.route_racing(&self.key, version) else {
                        continue 'restart;
                    };
                    at_separator |= routed.at_separator;
                    let edge = routed.edge as u16;
                    Prefetcher::fetch(node.edge_line(usize::from(edge))).await;
                    let Some(child) = node.racing_child(usize::from(edge)) else {
                        continue 'restart;
                    };
                    // An edge of the root page is followed only once the
                    // root's version vouches for it (`descent.rs`).
                    if at_root && !node.latch().is_unchanged(version) {
                        continue 'restart;
                    }
                    at_root = false;
                    match child {
                        Node::Leaf(leaf) => {
                            Prefetcher::fetch(leaf.search_line()).await;
                            let leaf_version = leaf.latch().read_version();
                            if !node.latch().is_unchanged(version) {
                                continue 'restart;
                            }
                            // The insertion shifts the entries from there: hint
                            // that line, as the unsync tree's batch does
                            // (`tree/batch.rs`).
                            if let Some(line) = leaf.racing_insertion_line(&self.key) {
                                Prefetcher::hint(line);
                            }
                            *output = Some(Destination {
                                leaf,
                                version: leaf_version,
                                at_separator,
                            });
                            return;
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
                }
            }
        }
    }
}

impl<'b, K, V, S, Q, B> Lane<'b, '_, K, V, S, Q, B>
where
    K: Borrow<Q>,
    S: InlineKeyStrategy<K>,
    Q: Ord + ?Sized,
    B: EntryVisit<K, V, S>,
{
    /// Looks the key up, writing what the batch's visit made of its entry to
    /// `output`, if the tree has one. As the unsync tree's lane, one flat async
    /// block that returns unit (doc 12).
    #[expect(
        clippy::manual_async_fn,
        reason = "the block's attribute reaches its poll"
    )]
    fn read(self, output: &'b mut Option<B::Output>) -> impl Future<Output = ()> {
        #[inline(always)]
        async move {
            'restart: loop {
                let tree = self.batch.tree;
                let root_page = tree.root_page().expect("lanes run on a tree");
                let Node::Internal(root) = root_page.node() else {
                    // Lowered to a leaf since the batch started.
                    *output = (self.batch.visit.borrow_mut()).look_up(tree, self.key.key());
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
                let mut at_root = true;
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
                    // An edge of the root page is followed only once the
                    // root's version vouches for it (`descent.rs`).
                    if at_root && !node.latch().is_unchanged(version) {
                        continue 'restart;
                    }
                    at_root = false;
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
                    if B::COPIES
                        && let Ok(read) = RacingRead::of_head(leaf)
                    {
                        let visit = &self.batch.visit;
                        LatchedTree::settle_copy(leaf, version, read, &self.key, true, visit)
                    } else {
                        let visit = &self.batch.visit;
                        LatchedTree::read_head(leaf, version, &self.key, visit).map(Some)
                    }
                } else {
                    let (candidate, prefix_len) = leaf.candidate_line_racing(&self.key);
                    let index = match candidate {
                        CandidateLine::Absent if leaf.latch().is_unchanged(version) => return,
                        CandidateLine::Absent => continue 'restart,
                        CandidateLine::One(index, line) => {
                            let (index, prefix_len) = (index as u16, prefix_len as u16);
                            Prefetcher::fetch(line).await;
                            if B::COPIES
                                && let Ok(read) = RacingRead::of_candidate(
                                    leaf,
                                    &self.key,
                                    usize::from(index),
                                    usize::from(prefix_len),
                                )
                            {
                                let settled = LatchedTree::settle_copy(
                                    leaf,
                                    version,
                                    read,
                                    &self.key,
                                    false,
                                    &self.batch.visit,
                                );
                                match settled {
                                    Ok(found) => *output = found,
                                    Err(Restart) => continue 'restart,
                                }
                                return;
                            }
                            if Leaf::<K, V, Latched<S>>::plan().is_for_heap_byte_strings() {
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
                    LatchedTree::read_in_leaf(leaf, version, &self.key, index, &self.batch.visit)
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
    /// What `visit` makes of the entry of `key` in `leaf`, latched shared, if
    /// the leaf is still at `version`, the version it was read at to find
    /// `candidate`. That is the one key whose inline key, at the prefix length
    /// given with it, equals `key`'s; `None` if it isn't `key`. Without a
    /// candidate, the leaf's own search. Synchronous: the latch is never held
    /// across a lane's yield (D55).
    #[inline(always)]
    fn read_in_leaf<Q, B: EntryVisit<K, V, S>>(
        leaf: Leaf<K, V, Latched<S>>,
        version: Version,
        key: &SearchKey<'_, K, Latched<S>, Q>,
        candidate: Option<(u16, u16)>,
        visit: &RefCell<B>,
    ) -> Result<Option<B::Output>, Restart>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let held = Self::latch_unchanged(leaf, version).ok_or(Restart)?;
        let candidate =
            candidate.map(|(index, prefix_len)| (usize::from(index), usize::from(prefix_len)));
        let result = Self::find_latched(leaf, key, candidate, BatchSearch).map(|index| {
            // SAFETY: a search returns the index of a key of the leaf.
            let (key, value) = unsafe { leaf.key_value_unchecked(index) };
            visit.borrow_mut().visit(key, value)
        });
        drop(held);
        Ok(result)
    }

    /// What `visit` makes of the value `read`, read unlatched from `leaf` (from
    /// its head if `at_separator`). That takes the leaf still at `version`,
    /// which it was read at, and the entry read being `key`'s (`lookup.rs`);
    /// `None` for a candidate that isn't. [`Restart`] if the leaf changed, or
    /// lost the head `key` matched.
    #[inline(always)]
    fn settle_copy<Q, B: EntryVisit<K, V, S>>(
        leaf: Leaf<K, V, Latched<S>>,
        version: Version,
        read: RacingRead<K, V>,
        key: &SearchKey<'_, K, Latched<S>, Q>,
        at_separator: bool,
        visit: &RefCell<B>,
    ) -> Result<Option<B::Output>, Restart>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if !leaf.latch().is_unchanged(version) {
            return Err(Restart);
        }
        // SAFETY: the leaf stayed at `version` from before the read to after
        // it.
        match unsafe { read.settle(key, at_separator) } {
            Ok(value) => Ok(value.map(|value| visit.borrow_mut().copied(value))),
            Err(Unsettled) => Err(Restart),
        }
    }

    /// The first line of `candidate`'s heap bytes that comparing it with `key`
    /// reads, for [`Self::read_in_leaf`]. It is read with the leaf latched
    /// shared, if the leaf is still at `version`; `None` if the comparison
    /// reads none. The lane lets go before it fetches the line: a pointer into
    /// a key is read only under the latch, and a prefetch of a stale one is
    /// harmless.
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
        let held = Self::latch_unchanged(leaf, version).ok_or(Restart)?;
        // SAFETY: the leaf is at the version it was read at to find the
        // candidate and its prefix length, and latched.
        let line = unsafe {
            leaf.candidate_bytes_line(key, usize::from(candidate), usize::from(prefix_len))
        };
        drop(held);
        Ok(line)
    }

    /// What `visit` makes of `leaf`'s head, latched shared. That takes the leaf
    /// still at `version`, the version it was read at under its parent, and its
    /// head's key being `key`, which matched the separator left of the leaf.
    /// That separator may have been removed since, and its successor be the
    /// head now. A leaf that changed since may have merged into its sibling and
    /// been freed, its bytes a stale copy of entries that live on there.
    /// Synchronous, as [`Self::read_in_leaf`].
    fn read_head<Q, B: EntryVisit<K, V, S>>(
        leaf: Leaf<K, V, Latched<S>>,
        version: Version,
        key: &SearchKey<'_, K, Latched<S>, Q>,
        visit: &RefCell<B>,
    ) -> Result<B::Output, Restart>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let held = Self::latch_unchanged(leaf, version).ok_or(Restart)?;
        if !Self::is_head(leaf, key) {
            return Err(Restart);
        }
        let result = visit.borrow_mut().visit(leaf.head_key(), leaf.head_value());
        drop(held);
        Ok(result)
    }
}
