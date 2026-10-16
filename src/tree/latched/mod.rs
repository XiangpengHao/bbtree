//! A tree that threads share (`doc/09-concurrency.md`): the node code of
//! every tree, with a latch word in each node (`node/latch.rs`), behind an
//! atomic pointer to the root page.
//!
//! - **Descent** ([`LatchedTree::descend`]): optimistic through the
//!   internal nodes, each read racing with its writers and checked by its
//!   version after its child's is read (`node/optimistic.rs`); the leaf is
//!   latched, shared or exclusive, and then its parent's version checked
//!   again, so the leaf is still where the key belongs. A failed check
//!   starts over from the root.
//! - **Operations** run the caller's closure on an entry while its leaf is
//!   latched, and let go before returning: no reference or latch outlives
//!   the call.
//! - **Splits** ([`LatchedTree::split_leaf`]): an insert into a full leaf,
//!   held exclusively, takes the internal nodes the split changes, from its
//!   parent up to the first with room, at the versions its descent read,
//!   and starts over if any changed. It changes them in place, as bbtree
//!   does, and lets go of them only once all are done: a reader that read
//!   one meanwhile fails its version check.
//!
//! The tree only grows: nothing removes an entry, merges nodes or frees a
//! page while threads share it. So a pointer a racing reader loads is to a
//! live page, and a leaf's head key, which internal nodes route by (D49),
//! never changes while threads read it. The keys all share nothing, as far
//! as the tree knows: its shared prefix (D50) stays empty, so no insert
//! changes the nodes along its borders.

use core::alloc::Layout;
use core::borrow::Borrow;
use core::marker::PhantomData;
use core::mem;
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicPtr, Ordering};

use super::node::inline_keys::{KeyInliner, SearchKey, Separator, WindowedKey};
use super::node::internal::Internal;
use super::node::latch::{Latched, NodeLatch, Version};
use super::node::leaf::Leaf;
use super::node::optimistic::Routed;
use super::node::page::{FreshPage, NodePtr};
use super::node::root_page::RootPage;
use super::node::{Child, Insertion, Node};
use super::node_stack::{FirstEdge, MAX_DEPTH};
use super::root::Tree;
use crate::allocator::{AllocError, Allocator};
use crate::inline_key::InlineKeyStrategy;

/// A tree that threads share, by an atomic pointer to its root page, tagged
/// with the root's kind; null until the first insert makes one. Its nodes'
/// strategy is `Latched<S>`.
///
/// It owns its nodes, keys and values, as a [`Tree`] does, and has no
/// `Drop`: [`Self::destroy`] takes it apart.
pub(crate) struct LatchedTree<K, V, S> {
    root: AtomicPtr<u8>,
    /// Owns keys and values that threads share: see the `Send` and `Sync`
    /// impls.
    _entries: PhantomData<*const (K, V)>,
    _strategy: PhantomData<fn() -> S>,
}

// SAFETY: a tree owns its pages, keys and values, as a `Box` owns its
// contents: sending it sends them.
unsafe impl<K: Send, V: Send, S> Send for LatchedTree<K, V, S> {}

// SAFETY: through `&self`, a thread moves keys and values into the tree,
// which other threads then read, change through `&mut V`, or drop: they
// must be `Send` and `Sync`. Every change to a node through `&self` holds
// its latch, and every read either holds it or is checked by its version
// (`node/latch.rs`, `node/optimistic.rs`).
unsafe impl<K: Send + Sync, V: Send + Sync, S> Sync for LatchedTree<K, V, S> {}

/// The allocator of a latched tree's pages: the map's, each page zeroed,
/// so that every word of an internal node a racing reader loads, slots past
/// its length included, is initialized, and every latch word starts unheld
/// (`node/optimistic.rs`, `node/latch.rs`).
struct ZeroedPages<'a, A>(&'a A);

// SAFETY: every block is the map's allocator's, given back to it as it was
// handed out: zeroing the bytes of a block changes nothing of its contract.
unsafe impl<A: Allocator> Allocator for ZeroedPages<'_, A> {
    fn allocate(&self, layout: Layout) -> Result<NonNull<u8>, AllocError> {
        let page = self.0.allocate(layout)?;
        // SAFETY: the block was just allocated for `layout`.
        unsafe { page.write_bytes(0, layout.size()) };
        Ok(page)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        // SAFETY: forwarded.
        unsafe { self.0.deallocate(ptr, layout) }
    }
}

/// A leaf of a latched tree, latched in mode `M` until dropped.
struct HeldLeaf<K, V, S: InlineKeyStrategy<K>, M: LatchMode> {
    leaf: Leaf<K, V, Latched<S>>,
    held: M::Held,
}

/// How a leaf is latched: [`Shared`] or [`Exclusive`].
trait LatchMode {
    /// What letting go needs.
    type Held: Copy;

    fn lock(latch: &NodeLatch) -> Self::Held;

    fn unlock(latch: &NodeLatch, held: Self::Held);
}

/// Latched for reading.
enum Shared {}

/// Latched for changing.
enum Exclusive {}

impl LatchMode for Shared {
    type Held = ();

    #[inline(always)]
    fn lock(latch: &NodeLatch) {
        latch.lock_shared();
    }

    #[inline(always)]
    fn unlock(latch: &NodeLatch, (): ()) {
        latch.unlock_shared();
    }
}

impl LatchMode for Exclusive {
    type Held = Version;

    #[inline(always)]
    fn lock(latch: &NodeLatch) -> Version {
        latch.lock_exclusive()
    }

    #[inline(always)]
    fn unlock(latch: &NodeLatch, version: Version) {
        latch.unlock_exclusive(version);
    }
}

impl<K, V, S: InlineKeyStrategy<K>, M: LatchMode> HeldLeaf<K, V, S, M> {
    #[inline(always)]
    fn lock(leaf: Leaf<K, V, Latched<S>>) -> Self {
        Self {
            leaf,
            held: M::lock(leaf.latch()),
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>, M: LatchMode> Drop for HeldLeaf<K, V, S, M> {
    #[inline(always)]
    fn drop(&mut self) {
        M::unlock(self.leaf.latch(), self.held);
    }
}

/// What a descent keeps of the internal nodes it goes through: nothing, for
/// most operations, or each node, for a split ([`SplitPath`]).
trait DescentPath<K, V, S> {
    /// Forgets every node: the descent starts over.
    fn clear(&mut self);

    /// The next node down, read at `version`, and where the key went from it.
    fn record(&mut self, node: Internal<K, V, Latched<S>>, version: Version, routed: Routed);
}

impl<K, V, S> DescentPath<K, V, S> for () {
    #[inline(always)]
    fn clear(&mut self) {}

    #[inline(always)]
    fn record(&mut self, _: Internal<K, V, Latched<S>>, _: Version, _: Routed) {}
}

/// An internal node a descent went through, at the version it read, and
/// where the key went from it.
struct PathLevel<K, V, S> {
    node: Internal<K, V, Latched<S>>,
    version: Version,
    routed: Routed,
}

impl<K, V, S> Clone for PathLevel<K, V, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K, V, S> Copy for PathLevel<K, V, S> {}

/// The internal nodes from the root down to a leaf's parent, for a split:
/// level 0 is the root.
struct SplitPath<K, V, S> {
    levels: [Option<PathLevel<K, V, S>>; MAX_DEPTH],
    depth: usize,
}

impl<K, V, S> SplitPath<K, V, S> {
    fn new() -> Self {
        Self {
            levels: [None; MAX_DEPTH],
            depth: 0,
        }
    }

    /// Level `depth`, which the descent went through.
    ///
    /// # Panics
    ///
    /// If it didn't.
    fn level(&self, depth: usize) -> PathLevel<K, V, S> {
        self.levels[depth].expect("a level the descent went through")
    }
}

impl<K, V, S> DescentPath<K, V, S> for SplitPath<K, V, S> {
    fn clear(&mut self) {
        self.depth = 0;
    }

    fn record(&mut self, node: Internal<K, V, Latched<S>>, version: Version, routed: Routed) {
        self.levels[self.depth] = Some(PathLevel {
            node,
            version,
            routed,
        });
        self.depth += 1;
    }
}

impl<K, V, S: InlineKeyStrategy<K>> LatchedTree<K, V, S> {
    /// No tree.
    ///
    /// # Panics
    ///
    /// If the keys have no inline keys under `S`: a reader that races with
    /// writers reads internal nodes, which must then keep no keys (D49).
    pub(crate) fn new() -> Self {
        assert!(
            NodePtr::<K, V, Latched<S>>::has_inline_keys(),
            "a BCTreeMap's keys need inline keys"
        );
        Self {
            root: AtomicPtr::new(ptr::null_mut()),
            _entries: PhantomData,
            _strategy: PhantomData,
        }
    }

    /// The root page, as the tree's pointer tags it; `None` without a tree.
    #[inline(always)]
    fn root_page(&self) -> Option<RootPage<K, V, Latched<S>>> {
        let tagged = NonNull::new(self.root.load(Ordering::Acquire))?;
        // SAFETY: a non-null root pointer is a root page of this tree's,
        // tagged with its root's kind, stored by `root_page_or_new` or a
        // split that raised the tree.
        Some(RootPage::of_node(Node::at(unsafe {
            NodePtr::from_tagged(tagged)
        })))
    }

    /// The root page, or that of a new tree, from `alloc`, if there is none:
    /// where an insert starts.
    #[inline(always)]
    fn root_page_or_new(&self, alloc: &impl Allocator) -> RootPage<K, V, Latched<S>> {
        match self.root_page() {
            Some(root_page) => root_page,
            None => self.start_tree(alloc),
        }
    }

    #[cold]
    #[inline(never)]
    fn start_tree(&self, alloc: &impl Allocator) -> RootPage<K, V, Latched<S>> {
        let root_page = RootPage::new(FreshPage::allocate(alloc));
        let tagged = root_page.node().node_ptr().tagged();
        match self.root.compare_exchange(
            ptr::null_mut(),
            tagged.as_ptr(),
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => root_page,
            Err(_) => {
                // SAFETY: the page is a new, empty root page that no other
                // thread ever reached.
                unsafe { root_page.free() }.deallocate(alloc);
                self.root_page().expect("another thread made the tree")
            }
        }
    }

    /// The leaf where `key` belongs, latched in mode `M`, and whether `key`
    /// equals the separator left of it, whose entry is its head (D17); with
    /// `path` keeping the internal nodes it went through.
    ///
    /// # Panics
    ///
    /// Without a tree.
    #[inline(always)]
    fn descend<Q, M: LatchMode, P: DescentPath<K, V, S>>(
        &self,
        key: &SearchKey<'_, K, Latched<S>, Q>,
        path: &mut P,
    ) -> (HeldLeaf<K, V, S, M>, bool)
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        'restart: loop {
            path.clear();
            let root_page = self.root_page().expect("a tree to descend");
            let mut node = match root_page.node() {
                Node::Leaf(leaf) => {
                    let held = HeldLeaf::lock(leaf);
                    // A split that raised the tree changed the tag first.
                    if self.root.load(Ordering::Acquire) != leaf.node_ptr().tagged().as_ptr() {
                        continue 'restart;
                    }
                    return (held, false);
                }
                Node::Internal(internal) => internal,
            };
            let mut version = node.latch().read_version();
            let mut at_separator = false;
            loop {
                let Some(routed) = node.route_racing(key, version) else {
                    continue 'restart;
                };
                let Some(child) = node.racing_child(routed.edge) else {
                    continue 'restart;
                };
                at_separator |= routed.at_separator;
                path.record(node, version, routed);
                match child {
                    Node::Leaf(leaf) => {
                        let held = HeldLeaf::lock(leaf);
                        if !node.latch().is_unchanged(version) {
                            continue 'restart;
                        }
                        return (held, at_separator);
                    }
                    Node::Internal(child) => {
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

    /// Runs `f` on the entry whose key equals `key`, while its leaf is
    /// latched shared, and returns what `f` returned; `None` without one.
    #[inline]
    pub(crate) fn read<Q, R>(&self, key: &Q, f: impl FnOnce(&K, &V) -> R) -> Option<R>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.root_page()?;
        let key = SearchKey::new(key);
        let (held, at_separator) = self.descend::<Q, Shared, ()>(&key, &mut ());
        let leaf = held.leaf;
        let (key, value) = if at_separator {
            (leaf.head_key(), leaf.head_value())
        } else {
            let index = leaf.find(&key, ())?;
            // SAFETY: a search returns the index of a key of the leaf.
            unsafe { leaf.key_value_unchecked(index) }
        };
        let result = f(key, value);
        drop(held);
        Some(result)
    }

    /// Runs `f` on the entry whose key equals `key`, its value exclusive,
    /// while its leaf is latched exclusively, and returns what `f` returned;
    /// `None` without one.
    #[inline]
    pub(crate) fn update<Q, R>(&self, key: &Q, f: impl FnOnce(&K, &mut V) -> R) -> Option<R>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.root_page()?;
        let key = SearchKey::new(key);
        let (held, at_separator) = self.descend::<Q, Exclusive, ()>(&key, &mut ());
        let leaf = held.leaf;
        let (key, value) = if at_separator {
            (leaf.head_key(), leaf.head_value_mut())
        } else {
            let index = leaf.find(&key, ())?;
            // SAFETY: a search returns the index of a key of the leaf, whose
            // key and value are apart.
            unsafe { (leaf.key_unchecked(index), leaf.value_mut_unchecked(index)) }
        };
        let result = f(key, value);
        drop(held);
        Some(result)
    }

    /// Inserts `key` and `value` and returns `None`; or, if an equal key is
    /// there, keeps it, drops `key`, replaces its value and returns the old
    /// one, as std's `BTreeMap::insert` does. Pages come from `alloc`.
    #[inline]
    pub(crate) fn insert(&self, key: K, value: V, alloc: &impl Allocator) -> Option<V>
    where
        K: Ord,
    {
        let alloc = &ZeroedPages(alloc);
        self.root_page_or_new(alloc);
        let (held, at_separator) = self.descend::<K, Exclusive, ()>(&SearchKey::new(&key), &mut ());
        match Self::insert_into(&held, at_separator, key, value) {
            Ok(replaced) => replaced,
            Err((key, value, _)) => {
                drop(held);
                self.insert_splitting(key, value, alloc)
            }
        }
    }

    /// Inserts into `held`, the leaf where `key` belongs, as [`Self::insert`]
    /// does, if it holds `key` or has room; or gives both back, with the
    /// edge where `key` goes in the full leaf.
    #[inline(always)]
    fn insert_into(
        held: &HeldLeaf<K, V, S, Exclusive>,
        at_separator: bool,
        key: K,
        value: V,
    ) -> Result<Option<V>, (K, V, usize)>
    where
        K: Ord,
    {
        let leaf = held.leaf;
        if at_separator {
            // As std, the new key is dropped before the value changes.
            drop(key);
            return Ok(Some(mem::replace(leaf.head_value_mut(), value)));
        }
        match leaf.search_always_inlined(&SearchKey::new(&key)) {
            Ok(index) => {
                drop(key);
                Ok(Some(mem::replace(leaf.value_mut(index), value)))
            }
            Err(edge) if leaf.len() < Leaf::<K, V, Latched<S>>::capacity() => {
                leaf.insert_fit(edge, WindowedKey::new(key, leaf.prefix_len()), value);
                Ok(None)
            }
            Err(edge) => Err((key, value, edge)),
        }
    }

    /// [`Self::insert`] where the leaf was full: searches again, keeping
    /// the internal nodes it goes through, and splits, until it inserts.
    #[inline(never)]
    fn insert_splitting(&self, mut key: K, mut value: V, alloc: &impl Allocator) -> Option<V>
    where
        K: Ord,
    {
        loop {
            let mut path = SplitPath::new();
            let (held, at_separator) =
                self.descend::<K, Exclusive, _>(&SearchKey::new(&key), &mut path);
            let edge;
            (key, value, edge) = match Self::insert_into(&held, at_separator, key, value) {
                Ok(replaced) => return replaced,
                Err(full) => full,
            };
            match self.split_leaf(&path, held, edge, key, value, alloc) {
                Ok(()) => return None,
                Err(entry) => (key, value) = entry,
            }
        }
    }

    /// Splits `held`, the full leaf `path` leads to, and inserts `key` and
    /// `value` at `edge` of it, carrying the split up as far as it goes; or,
    /// if a node it changes changed since the descent read it, changes
    /// nothing and gives both back.
    ///
    /// Every node it changes it holds, and changes in place, as bbtree's
    /// insert does; readers that read one meanwhile fail their version check
    /// (`node/optimistic.rs`). The halves of each split fit their prefixes
    /// to their fences right after it (D26), while the nodes above, where
    /// the fences are, are still as the descent read them.
    fn split_leaf(
        &self,
        path: &SplitPath<K, V, S>,
        held: HeldLeaf<K, V, S, Exclusive>,
        edge: usize,
        key: K,
        value: V,
        alloc: &impl Allocator,
    ) -> Result<(), (K, V)>
    where
        K: Ord,
    {
        let depth = path.depth;
        // The lowest node with room takes the split's separator; every node
        // below it splits. Without one, the root splits and the tree grows.
        let room = (0..depth)
            .rev()
            .find(|&level| path.level(level).routed.len < Internal::<K, V, Latched<S>>::capacity());
        let first_changed = room.unwrap_or(0);
        // The fences of the nodes that split are separators in the nodes
        // above them: in the nodes the split changes, or, where the top one
        // that splits is the first or last child of the node with room, in
        // the nearest node above with a separator on that side, which the
        // split holds too, unchanged, to read it.
        let fence_holders = room.map_or([None, None], |top| {
            let routed = path.level(top).routed;
            let nearest_above = |holds: fn(Routed) -> bool| {
                (0..top)
                    .rev()
                    .find(|&level| holds(path.level(level).routed))
            };
            let low = (routed.edge == 0)
                .then(|| nearest_above(|routed| routed.edge > 0))
                .flatten();
            let high = (routed.edge == routed.len)
                .then(|| nearest_above(|routed| routed.edge < routed.len))
                .flatten();
            [low, high.filter(|&high| Some(high) != low)]
        });
        let mut held_levels = [0; MAX_DEPTH + 2];
        let mut held_count = 0;
        for level in fence_holders
            .into_iter()
            .flatten()
            .chain(first_changed..depth)
        {
            held_levels[held_count] = level;
            held_count += 1;
        }
        if !Self::try_lock_levels(path, &held_levels[..held_count]) {
            return Err((key, value));
        }

        // From the leaf up: split, fit the halves, carry the separator.
        let leaf = held.leaf;
        let split = leaf.split_insert(
            edge,
            WindowedKey::new(key, leaf.prefix_len()),
            value,
            FreshPage::allocate(alloc),
        );
        let separator_key = split.right.head_key();
        Self::refit_halves(leaf, split.right, separator_key, path, depth);
        let mut carried = (split.separator, Node::Leaf(split.right));
        let mut level = depth;
        let raised_leaf_root = loop {
            let (separator, right) = carried;
            let Some(parent_level) = level.checked_sub(1) else {
                break self.raise(depth == 0, separator, right, alloc);
            };
            let parent = path.level(parent_level).node;
            match parent.insert(
                path.level(parent_level).routed.edge,
                separator,
                right,
                alloc,
            ) {
                Insertion::Fit(_) => break None,
                Insertion::Split(split) => {
                    let separator_key = Node::Internal(split.right)
                        .border_leaf::<FirstEdge>()
                        .head_key();
                    Self::refit_halves(parent, split.right, separator_key, path, parent_level);
                    carried = (split.separator, Node::Internal(split.right));
                    level = parent_level;
                }
            }
        };

        // A root that was a leaf is internal now: retag its pointer while it
        // is still held; then let go of everything.
        if let Some(root_page) = raised_leaf_root {
            let tagged = root_page.node().node_ptr().tagged();
            self.root.store(tagged.as_ptr(), Ordering::Release);
        }
        for level in first_changed..depth {
            let PathLevel { node, version, .. } = path.level(level);
            node.latch().unlock_exclusive(version);
        }
        for level in fence_holders.into_iter().flatten() {
            let PathLevel { node, version, .. } = path.level(level);
            node.latch().unlock_unchanged(version);
        }
        drop(held);
        Ok(())
    }

    /// Takes the nodes at `levels` of `path` exclusively, at the versions
    /// the descent read them at; or, if one changed since, none of them.
    fn try_lock_levels(path: &SplitPath<K, V, S>, levels: &[usize]) -> bool {
        for (count, &level) in levels.iter().enumerate() {
            let PathLevel { node, version, .. } = path.level(level);
            if !node.latch().try_lock_at(version) {
                for &locked in &levels[..count] {
                    let PathLevel { node, version, .. } = path.level(locked);
                    node.latch().unlock_unchanged(version);
                }
                return false;
            }
        }
        true
    }

    /// Makes the tree a level taller, its root having split in place, with
    /// `separator` between its node, the left half, and `right`: the root's
    /// node moves to a fresh page, unlatched, and the root page's node
    /// becomes an internal one above both halves, as `RootPage::raise`
    /// does; for a root leaf, the caller holds it exclusively, and the root
    /// page starts its internal node on zeroed bytes
    /// (`RootPage::raise_latched_leaf`). Returns the root page if the root
    /// was a leaf, whose pointer the caller tags anew.
    fn raise(
        &self,
        root_was_leaf: bool,
        separator: Separator<K, Latched<S>>,
        right: Node<K, V, Latched<S>>,
        alloc: &impl Allocator,
    ) -> Option<RootPage<K, V, Latched<S>>> {
        let root_page = self.root_page().expect("a tree to raise");
        let raised = if root_was_leaf {
            root_page.raise_latched_leaf(alloc)
        } else {
            root_page.raise(alloc)
        };
        let Node::Internal(root) = raised.node() else {
            unreachable!("a raised root is internal");
        };
        if let Node::Internal(moved) = root.child(0) {
            // It moved with the root's latch word, which the split holds.
            moved.latch().reset();
        }
        root.insert_fit(0, separator, right);
        root_was_leaf.then_some(raised)
    }

    /// Fits the prefixes of `left` and `right`, the halves of the node at
    /// `level` of `path` (the leaf's at `path.depth`), with `separator_key`
    /// between them, to their fences: the node's old fences outside, read
    /// from the nodes above, which are still as the descent read them, and
    /// `separator_key` between, no shorter than the parent's prefix (D44).
    /// A half whose keys differ past its prefix reads no fence
    /// (`Child::keys_differ_past_prefix`).
    fn refit_halves<C: Child<K, V, Latched<S>>>(
        left: C,
        right: C,
        separator_key: &K,
        path: &SplitPath<K, V, S>,
        level: usize,
    ) {
        let fences = OldFences { path, level };
        let parent_prefix_len = level
            .checked_sub(1)
            .map_or(0, |parent| path.level(parent).node.prefix_len());
        if !left.keys_differ_past_prefix() {
            Self::refit(left, fences.low(), Some(separator_key), parent_prefix_len);
        }
        if !right.keys_differ_past_prefix() {
            Self::refit(right, Some(separator_key), fences.high(), parent_prefix_len);
        }
    }

    /// Fits `node`'s prefix to its fences `low` and `high`, no shorter than
    /// its parent's (D44).
    fn refit<C: Child<K, V, Latched<S>>>(
        node: C,
        low: Option<&K>,
        high: Option<&K>,
        parent_prefix_len: usize,
    ) {
        let prefix_len =
            KeyInliner::fence_prefix_len::<Latched<S>>(low, high, 0).max(parent_prefix_len);
        node.refit_prefix(prefix_len);
    }

    /// Panics unless the tree is a well-formed B+-tree of `length` entries,
    /// with every node's prefix what its fences share
    /// ([`Tree::check_invariants`]); once no other thread changes it.
    #[cfg(test)]
    pub(crate) fn check_invariants(&mut self, length: usize)
    where
        K: Ord,
    {
        match self.root_page() {
            Some(root_page) => Tree::of_root_page(root_page).check_invariants(length),
            None => assert_eq!(length, 0, "a map without a tree has entries"),
        }
    }

    /// Takes the tree apart, with `alloc`, the allocator its pages came
    /// from, once no other thread reaches it.
    pub(crate) fn destroy(&mut self, alloc: &impl Allocator) {
        if let Some(root_page) = self.root_page() {
            *self.root.get_mut() = ptr::null_mut();
            Tree::of_root_page(root_page).destroy(alloc);
        }
    }
}

/// The fences a node on a split's path had before the split: the
/// separators beside it in the nodes above, read from the nodes themselves,
/// which the split holds, unchanged yet (`LatchedTree::split_leaf`).
struct OldFences<'p, K, V, S> {
    path: &'p SplitPath<K, V, S>,
    /// The node's level: `path.depth` for the leaf.
    level: usize,
}

impl<'p, K, V, S: InlineKeyStrategy<K>> OldFences<'p, K, V, S> {
    /// The low fence; `None` on the tree's left border.
    fn low(&self) -> Option<&'p K> {
        self.nearest(|routed| routed.edge.checked_sub(1))
    }

    /// The high fence; `None` on the tree's right border.
    fn high(&self) -> Option<&'p K> {
        self.nearest(|routed| (routed.edge < routed.len).then_some(routed.edge))
    }

    /// The separator `beside` picks in the nearest node above that has one.
    fn nearest(&self, beside: impl Fn(Routed) -> Option<usize>) -> Option<&'p K> {
        (0..self.level).rev().find_map(|level| {
            let PathLevel { node, routed, .. } = self.path.level(level);
            beside(routed).map(|index| node.separator_key(index))
        })
    }
}
