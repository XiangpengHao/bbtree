//! Test-only helpers: a deterministic PRNG, drop tracking, padded and
//! over-aligned test types, trees built through the node API, and a check of
//! inline keys. The map's tests use the first three and the last too.

#![forbid(unsafe_code)]

use core::cell::{Cell, RefCell};
use core::cmp::Ordering;
use std::vec::Vec;

use super::handle::{
    ForceResult, Immut, Internal, Leaf, LeafOrInternal, NodeKind, NodeRef, Subtree,
};
use super::inline_keys::KeyInliner;
use super::page::TestPages;
use crate::inline_key::{Auto, InlineKey, InlineKeyStrategy, InlineWord};

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
        let id = u32::try_from(alive.len()).expect("fewer than 2^32 tracked values");
        alive.push(true);
        Tracked {
            ledger: self,
            id,
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

/// A value whose drop is recorded in a [`DropLedger`]. Ordered by id. 16
/// bytes, small enough to pad into node shapes that tests spell out.
pub(crate) struct Tracked<'l> {
    ledger: &'l DropLedger,
    id: u32,
    panics_on_drop: bool,
}

impl Tracked<'_> {
    pub(crate) fn id(&self) -> usize {
        self.id as usize
    }
}

impl Drop for Tracked<'_> {
    fn drop(&mut self) {
        self.ledger.record_drop(self.id());
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

/// The id, which orders like the value: maps with `WithInlineKey` test the
/// node operations with inline keys on tracked keys.
impl InlineKey for Tracked<'_> {
    fn inline_key(&self) -> InlineWord {
        InlineWord::from(self.id)
    }
}

/// A tracked key or value padded to 152 bytes. As keys, with [`Tracked`]
/// values, they fill a 2 KB node with exactly 12 entries in both node kinds
/// (D13, D15, D17, D21): tall trees from few entries, which Miri can afford,
/// and the node shapes the balancing tests spell out. No key size gives
/// exactly 12 in both kinds of a 1 KB node. As values, they test large values in
/// leaves. Ordered by id.
pub(super) struct Bulky<'l> {
    tracked: Tracked<'l>,
    _padding: [u8; 136],
}

impl PartialEq for Bulky<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.tracked == other.tracked
    }
}

impl Eq for Bulky<'_> {}

impl PartialOrd for Bulky<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Bulky<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.tracked.cmp(&other.tracked)
    }
}

/// A key or value type that tests make from a [`Tracked`] and tell apart by
/// its id.
pub(crate) trait FromTracked<'l> {
    fn from_tracked(tracked: Tracked<'l>) -> Self;
    fn tracked_id(&self) -> usize;
}

impl<'l> FromTracked<'l> for Tracked<'l> {
    fn from_tracked(tracked: Tracked<'l>) -> Self {
        tracked
    }

    fn tracked_id(&self) -> usize {
        self.id()
    }
}

impl<'l> FromTracked<'l> for Bulky<'l> {
    fn from_tracked(tracked: Tracked<'l>) -> Self {
        Self {
            tracked,
            _padding: [0; 136],
        }
    }

    fn tracked_id(&self) -> usize {
        self.tracked.id()
    }
}

impl<'a, 'l, K: FromTracked<'l>, V: FromTracked<'l>, S: InlineKeyStrategy<K>>
    NodeRef<Immut<'a>, K, V, S, Leaf>
{
    /// The `(key id, value id)` of each entry, in order.
    pub(super) fn tracked_ids(self) -> Vec<(usize, usize)> {
        (0..self.len())
            .map(|index| {
                let (key, value) = self.kv_at(index).into_key_value();
                (key.tracked_id(), value.tracked_id())
            })
            .collect()
    }

    /// The id of the head, if the leaf has one.
    pub(super) fn head_id(self) -> Option<usize> {
        self.head().map(|head| head.into_value().tracked_id())
    }
}

impl<'a, 'l, K: FromTracked<'l>, V: FromTracked<'l>, S: InlineKeyStrategy<K>>
    NodeRef<Immut<'a>, K, V, S, Internal>
{
    /// The `(key id, value id)` of each separator, in order, its value read
    /// from the head that holds it (D17).
    pub(super) fn tracked_ids(self) -> Vec<(usize, usize)> {
        (0..self.len())
            .map(|index| {
                let separator = self.kv_at(index);
                let value = separator.into_value_head().into_value();
                (separator.into_key().tracked_id(), value.tracked_id())
            })
            .collect()
    }
}

impl<'a, 'l, K: FromTracked<'l>, V: FromTracked<'l>, S: InlineKeyStrategy<K>>
    NodeRef<Immut<'a>, K, V, S, LeafOrInternal>
{
    /// The `(key id, value id)` of every entry of the subtree, in order; see
    /// [`NodeRef::for_each_entry`].
    pub(super) fn subtree_tracked_ids(self) -> Vec<(usize, usize)> {
        let mut ids = Vec::new();
        self.for_each_entry(&mut |key, value| ids.push((key.tracked_id(), value.tracked_id())));
        ids
    }

    /// The id of the head of the subtree's leftmost leaf, if it has one: the
    /// value of the separator just left of the subtree (D17).
    pub(super) fn leftmost_head_id(self) -> Option<usize> {
        let mut node = self;
        loop {
            match node.force() {
                ForceResult::Leaf(leaf) => return leaf.head_id(),
                ForceResult::Internal(internal) => node = internal.first_edge().descend(),
            }
        }
    }
}

impl<'a, K: 'a, V: 'a, S: InlineKeyStrategy<K>> NodeRef<Immut<'a>, K, V, S, LeafOrInternal> {
    /// Panics unless every node of the subtree has the inline keys of its
    /// keys, in order, and a prefix length of 0, or has neither (D18); see
    /// [`NodeRef::assert_inline_keys_match`].
    pub(crate) fn assert_subtree_inline_keys_match(self) {
        self.assert_inline_keys_match();
        if let ForceResult::Internal(internal) = self.force() {
            for index in 0..=internal.len() {
                (internal.edge_at(index).descend()).assert_subtree_inline_keys_match();
            }
        }
    }

    /// Calls `entry` on every entry of the subtree in key order: each leaf's
    /// keys and values, and each separator's key with its value from the head
    /// that holds it (D17). A head in the subtree's leftmost leaf belongs to
    /// a separator outside the subtree, and is skipped.
    pub(super) fn for_each_entry(self, entry: &mut impl FnMut(&'a K, &'a V)) {
        match self.force() {
            ForceResult::Leaf(leaf) => {
                for index in 0..leaf.len() {
                    let (key, value) = leaf.kv_at(index).into_key_value();
                    entry(key, value);
                }
            }
            ForceResult::Internal(internal) => {
                for index in 0..=internal.len() {
                    internal.edge_at(index).descend().for_each_entry(entry);
                    if index < internal.len() {
                        let separator = internal.kv_at(index);
                        let value = separator.into_value_head().into_value();
                        entry(separator.into_key(), value);
                    }
                }
            }
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>, Kind: NodeKind> NodeRef<Immut<'_>, K, V, S, Kind> {
    /// Panics unless the node has inline keys exactly if its map's strategy
    /// gives its keys some, and then has its keys' inline keys, in order, and
    /// a prefix length of 0 (D18, 05-fence-key.md).
    pub(crate) fn assert_inline_keys_match(self) {
        let expected: Option<Vec<InlineWord>> = KeyInliner::<K>::resolve::<S>().map(|inliner| {
            (self.keys().iter())
                .map(|key| inliner.inline_key(key))
                .collect()
        });
        assert_eq!(
            self.inline_keys(),
            expected.as_deref(),
            "the inline keys are not the keys'"
        );
        assert_eq!(self.prefix_len(), expected.map(|_| 0));
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

/// A `u64` aligned to two pages. As values, they make leaves 8 KB-aligned
/// and 112 KB long (D13), and leave internal nodes at 1 KB (D21).
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
pub(super) struct TestTree<'p, K, V, S: InlineKeyStrategy<K> = Auto> {
    /// `None` only while the root is being replaced.
    root: Option<Subtree<K, V, S>>,
    pages: &'p TestPages<K, V, S>,
}

impl<'p, K, V, S: InlineKeyStrategy<K>> TestTree<'p, K, V, S> {
    /// A tree with an empty leaf as its root.
    pub(super) fn new(pages: &'p TestPages<K, V, S>) -> Self {
        Self {
            root: Some(Subtree::new_leaf(pages.fresh()).forget_type()),
            pages,
        }
    }

    pub(super) fn root(&self) -> &Subtree<K, V, S> {
        self.root
            .as_ref()
            .expect("the root is always there between calls")
    }

    pub(super) fn root_mut(&mut self) -> &mut Subtree<K, V, S> {
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
        self.root().borrow().for_each_entry(&mut |key, value| {
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
}

impl<K: Ord, V, S: InlineKeyStrategy<K>> TestTree<'_, K, V, S> {
    /// Inserts `key`/`value`, or replaces the value of an equal key and
    /// returns the old one, as the map does.
    pub(super) fn insert(&mut self, key: K, value: V) -> Option<V> {
        let pages = self.pages;
        self.root_mut()
            .insert(key, value, || pages.fresh(), || pages.fresh())
    }
}

#[cfg(test)]
mod tests {
    use super::{Bulky, CacheLineAligned, PageAligned, Tracked};
    use crate::node::layout::NodeLayout;

    #[test]
    fn bulky_key_nodes_hold_twelve_entries() {
        assert_eq!(size_of::<Tracked<'_>>(), 16);
        assert_eq!(size_of::<Bulky<'_>>(), 152);
        let leaf = NodeLayout::<Bulky<'_>, Tracked<'_>>::PLAIN_LEAF;
        let internal = NodeLayout::<Bulky<'_>, Tracked<'_>>::PLAIN_INTERNAL;
        assert_eq!((leaf.keys.capacity, leaf.page.size()), (12, 2048));
        assert_eq!((internal.keys.capacity, internal.page.size()), (12, 2048));
    }

    #[test]
    fn over_aligned_types_keep_their_value() {
        assert_eq!(CacheLineAligned::new(7).get(), 7);
        assert_eq!(PageAligned::new(9).get(), 9);
        assert_eq!(
            NodeLayout::<u64, PageAligned>::PLAIN_LEAF.page.align(),
            8192
        );
        assert_eq!(
            NodeLayout::<u64, PageAligned>::PLAIN_INTERNAL.page.align(),
            1024
        );
    }
}
