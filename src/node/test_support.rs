//! Test-only helpers: a deterministic PRNG, drop tracking, padded and
//! over-aligned test types, and trees built through the node API. The map's
//! tests use the first three too.

#![forbid(unsafe_code)]

use core::cell::{Cell, RefCell};
use core::cmp::Ordering;
use std::vec::Vec;

use super::handle::{ForceResult, Immut, LeafOrInternal, NodeKind, NodeRef, Subtree};
use super::page::TestPages;

/// A deterministic xorshift64* generator; shared by reference.
pub(crate) struct TestRng(Cell<u64>);

impl TestRng {
    pub(crate) fn new(seed: u64) -> Self {
        Self(Cell::new(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1))
    }

    /// A uniformly distributed index in `0..bound` (up to modulo bias).
    pub(crate) fn below(&self, bound: usize) -> usize {
        assert!(bound > 0);
        let mut state = self.0.get();
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        self.0.set(state);
        (state.wrapping_mul(0x2545_F491_4F6C_DD1D) % bound as u64) as usize
    }

    /// Puts `items` in a random order.
    pub(crate) fn shuffle<T>(&self, items: &mut [T]) {
        for index in (1..items.len()).rev() {
            items.swap(index, self.below(index + 1));
        }
    }
}

/// Records which [`Tracked`] values are alive, to catch leaks and double
/// drops, and the order they were dropped in.
pub(crate) struct DropLedger {
    alive: RefCell<Vec<bool>>,
    drop_order: RefCell<Vec<usize>>,
}

impl DropLedger {
    pub(crate) fn new() -> Self {
        Self {
            alive: RefCell::new(Vec::new()),
            drop_order: RefCell::new(Vec::new()),
        }
    }

    /// The ids of the dropped values, in drop order.
    pub(crate) fn drop_order(&self) -> Vec<usize> {
        self.drop_order.borrow().clone()
    }

    pub(crate) fn track(&self) -> Tracked<'_> {
        self.track_with(false)
    }

    pub(crate) fn track_panicking_on_drop(&self) -> Tracked<'_> {
        self.track_with(true)
    }

    /// `count` tracked values with ascending ids.
    pub(super) fn track_many(&self, count: usize) -> Vec<Tracked<'_>> {
        (0..count).map(|_| self.track()).collect()
    }

    pub(crate) fn assert_all_dropped_once(&self) {
        let alive = self.alive.borrow();
        let leaked: Vec<usize> = (0..alive.len()).filter(|&id| alive[id]).collect();
        assert!(leaked.is_empty(), "leaked tracked values: {leaked:?}");
    }

    fn track_with(&self, panics_on_drop: bool) -> Tracked<'_> {
        let mut alive = self.alive.borrow_mut();
        let id = alive.len();
        alive.push(true);
        Tracked {
            id,
            ledger: self,
            panics_on_drop,
        }
    }

    fn record_drop(&self, id: usize) {
        let mut alive = self.alive.borrow_mut();
        assert!(alive[id], "tracked value {id} dropped twice");
        alive[id] = false;
        self.drop_order.borrow_mut().push(id);
    }
}

/// A value whose drop is recorded in a [`DropLedger`]. Ordered by id.
pub(crate) struct Tracked<'l> {
    id: usize,
    ledger: &'l DropLedger,
    panics_on_drop: bool,
}

impl Tracked<'_> {
    pub(crate) fn id(&self) -> usize {
        self.id
    }
}

impl Drop for Tracked<'_> {
    fn drop(&mut self) {
        self.ledger.record_drop(self.id);
        if self.panics_on_drop {
            panic!("tracked value {} panics on drop, as asked", self.id);
        }
    }
}

impl PartialEq for Tracked<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for Tracked<'_> {}

impl PartialOrd for Tracked<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Tracked<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.id.cmp(&other.id)
    }
}

/// A tracked value padded so that, with `Tracked` keys, 160-byte entries fill
/// a 2 KB node with exactly 12 entries in both node kinds (D13, D15): tall
/// trees from few entries, which Miri can afford, and the node shapes the
/// balancing tests spell out. No entry size gives exactly 12 in both kinds
/// of a 1 KB node.
pub(super) struct Bulky<'l> {
    tracked: Tracked<'l>,
    _padding: [u8; 112],
}

/// A value type that tests make from a [`Tracked`] and tell apart by its id.
pub(crate) trait TrackedValue<'l> {
    fn from_tracked(tracked: Tracked<'l>) -> Self;
    fn tracked_id(&self) -> usize;
}

impl<'l> TrackedValue<'l> for Tracked<'l> {
    fn from_tracked(tracked: Tracked<'l>) -> Self {
        tracked
    }

    fn tracked_id(&self) -> usize {
        self.id()
    }
}

impl<'l> TrackedValue<'l> for Bulky<'l> {
    fn from_tracked(tracked: Tracked<'l>) -> Self {
        Self {
            tracked,
            _padding: [0; 112],
        }
    }

    fn tracked_id(&self) -> usize {
        self.tracked.id()
    }
}

impl<'a, 'l, V: TrackedValue<'l>, Kind: NodeKind> NodeRef<Immut<'a>, Tracked<'l>, V, Kind> {
    /// The `(key id, value id)` of each entry, in order.
    pub(super) fn tracked_ids(self) -> Vec<(usize, usize)> {
        (0..self.len())
            .map(|index| {
                let (key, value) = self.kv_at(index).into_key_value();
                (key.id(), value.tracked_id())
            })
            .collect()
    }
}

/// A `u64` aligned to a cache line.
#[repr(align(64))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct CacheLineAligned(u64);

impl CacheLineAligned {
    pub(crate) fn new(value: u64) -> Self {
        Self(value)
    }

    pub(crate) fn get(self) -> u64 {
        self.0
    }
}

/// A `u64` aligned to two pages, which makes nodes 8 KB-aligned and 112 KB
/// long (D13).
#[repr(align(8192))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PageAligned(u64);

impl PageAligned {
    pub(crate) fn new(value: u64) -> Self {
        Self(value)
    }

    pub(crate) fn get(&self) -> u64 {
        self.0
    }
}

/// A tree built through the node API with the map's insert (search down,
/// insert into the leaf, and split full nodes on the way back up), over
/// pages from a [`TestPages`] pool.
pub(super) struct TestTree<'p, K, V> {
    /// `None` only while the root is being replaced.
    root: Option<Subtree<K, V>>,
    pages: &'p TestPages<K, V>,
}

impl<'p, K, V> TestTree<'p, K, V> {
    /// A tree with an empty leaf as its root.
    pub(super) fn new(pages: &'p TestPages<K, V>) -> Self {
        Self {
            root: Some(Subtree::new_leaf(pages.fresh()).forget_type()),
            pages,
        }
    }

    pub(super) fn root(&self) -> &Subtree<K, V> {
        self.root
            .as_ref()
            .expect("the root is always there between calls")
    }

    pub(super) fn root_mut(&mut self) -> &mut Subtree<K, V> {
        self.root
            .as_mut()
            .expect("the root is always there between calls")
    }

    pub(super) fn height(&self) -> usize {
        self.root().height()
    }

    /// One `entry(key, value)` per entry, in order.
    pub(super) fn entries<T>(&self, mut entry: impl FnMut(&K, &V) -> T) -> Vec<T> {
        let mut entries = Vec::new();
        Self::visit(self.root().borrow(), &mut |key, value| {
            entries.push(entry(key, value));
        });
        entries
    }

    /// Replaces an internal root without keys by its only child, as the tree
    /// layer does after a merge empties the root.
    pub(super) fn pop_empty_roots(&mut self) {
        loop {
            let root = self.root.take().expect("the root is always there");
            match root.force() {
                ForceResult::Internal(internal) if internal.borrow().len() == 0 => {
                    let (only_child, page) = internal.pop_internal_level();
                    self.pages.release(page);
                    self.root = Some(only_child);
                }
                ForceResult::Internal(internal) => {
                    self.root = Some(internal.forget_type());
                    return;
                }
                ForceResult::Leaf(leaf) => {
                    self.root = Some(leaf.forget_type());
                    return;
                }
            }
        }
    }

    /// Takes the tree apart, giving every page back to the pool.
    pub(super) fn destroy(mut self) {
        let pages = self.pages;
        let root = self.root.take().expect("the root is always there");
        root.destroy(|page| pages.release(page));
    }

    fn visit<'a>(
        node: NodeRef<Immut<'a>, K, V, LeafOrInternal>,
        entry: &mut impl FnMut(&'a K, &'a V),
    ) where
        K: 'a,
        V: 'a,
    {
        match node.force() {
            ForceResult::Leaf(leaf) => {
                for index in 0..leaf.len() {
                    let (key, value) = leaf.kv_at(index).into_key_value();
                    entry(key, value);
                }
            }
            ForceResult::Internal(internal) => {
                for index in 0..=internal.len() {
                    Self::visit(internal.edge_at(index).descend(), entry);
                    if index < internal.len() {
                        let (key, value) = internal.kv_at(index).into_key_value();
                        entry(key, value);
                    }
                }
            }
        }
    }
}

impl<K: Ord, V> TestTree<'_, K, V> {
    /// Inserts `key`/`value`, or replaces the value of an equal key and
    /// returns the old one, as the map does.
    pub(super) fn insert(&mut self, key: K, value: V) -> Option<V> {
        let pages = self.pages;
        self.root_mut().insert(key, value, || pages.fresh())
    }
}

#[cfg(test)]
mod tests {
    use super::{Bulky, CacheLineAligned, PageAligned, Tracked};
    use crate::node::layout::NodeLayout;

    #[test]
    fn bulky_nodes_hold_twelve_entries() {
        assert_eq!(NodeLayout::<Tracked<'_>, Bulky<'_>>::LEAF_CAPACITY, 12);
        assert_eq!(NodeLayout::<Tracked<'_>, Bulky<'_>>::INTERNAL_CAPACITY, 12);
    }

    #[test]
    fn over_aligned_types_keep_their_value() {
        assert_eq!(CacheLineAligned::new(7).get(), 7);
        assert_eq!(PageAligned::new(9).get(), 9);
        assert_eq!(NodeLayout::<u64, PageAligned>::NODE_ALIGN, 8192);
    }
}
