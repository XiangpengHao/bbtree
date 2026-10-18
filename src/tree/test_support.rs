//! Test-only helpers: a deterministic PRNG, which also walks two iterators
//! alike from random ends and makes random range bounds; steps from either
//! end of an iterator; hashes; keys and values made from numbers; byte-string
//! keys that length tags tell apart; drop tracking; padded and over-aligned
//! test types; nodes of a known kind, trees built through the node API, and
//! a check of inline keys. The maps' and the set's tests use the first
//! eight.

#![forbid(unsafe_code)]

use core::cell::{Cell, RefCell};
use core::cmp::Ordering;
use core::fmt::Debug;
use core::hash::{Hash, Hasher};
use core::mem;
use core::ops::Bound;
use std::format;
use std::hash::DefaultHasher;
use std::string::String;
use std::vec;
use std::vec::Vec;

use super::invariants::NodeChecks;
use super::node::Node;
use super::node::inline_keys::Separator;
use super::node::internal::Internal;
use super::node::leaf::Leaf;
use super::node::page::{NodePtr, TestPages};
use super::node_stack::FirstEdge;
use super::root::Tree;
use crate::inline_key::{Auto, InlineKey, InlineKeyStrategy, InlineWord};

/// A deterministic xorshift64* generator; shared by reference.
pub(crate) struct TestRng(Cell<u64>);

impl TestRng {
    pub(crate) fn new(seed: u64) -> Self {
        Self(Cell::new(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1))
    }

    /// A uniformly distributed index in `0..bound` (up to modulo bias).
    pub(crate) fn below(&self, bound: usize) -> usize {
        self.below_u64(bound as u64) as usize
    }

    /// A uniformly distributed number in `0..bound` (up to modulo bias),
    /// for bounds past `usize` on 32-bit targets.
    pub(crate) fn below_u64(&self, bound: u64) -> u64 {
        assert!(bound > 0);
        let mut state = self.0.get();
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        self.0.set(state);
        state.wrapping_mul(0x2545_F491_4F6C_DD1D) % bound
    }

    /// Puts `items` in a random order.
    pub(crate) fn shuffle<T>(&self, items: &mut [T]) {
        for index in (1..items.len()).rev() {
            items.swap(index, self.below(index + 1));
        }
    }

    /// `len` distinct numbers of `0..universe`, in random order.
    pub(crate) fn distinct_numbers(&self, len: usize, universe: usize) -> Vec<usize> {
        let mut numbers: Vec<usize> = (0..universe).collect();
        self.shuffle(&mut numbers);
        numbers.truncate(len);
        numbers
    }

    /// Random bounds in order, of every kind, at keys that `key` makes from
    /// numbers of `0..key_space`: half of them narrow, so that a walk
    /// between them meets its other end.
    pub(crate) fn range_bounds<K: Ord + Clone>(
        &self,
        key_space: usize,
        mut key: impl FnMut(usize) -> K,
    ) -> (Bound<K>, Bound<K>) {
        let low_number = self.below(key_space);
        let high_number = if self.below(2) == 0 {
            low_number + self.below(48)
        } else {
            self.below(key_space)
        };
        let (mut low, mut high) = (key(low_number), key(high_number));
        if low > high {
            mem::swap(&mut low, &mut high);
        }
        let mut start = self.bound(low);
        let end = self.bound(high);
        if let (Bound::Excluded(start_key), Bound::Excluded(end_key)) = (&start, &end)
            && start_key == end_key
        {
            start = Bound::Included(start_key.clone());
        }
        (start, end)
    }

    /// A bound at `key` of a random kind, or none.
    fn bound<K>(&self, key: K) -> Bound<K> {
        match self.below(5) {
            0 => Bound::Unbounded,
            1 | 2 => Bound::Included(key),
            _ => Bound::Excluded(key),
        }
    }

    /// Takes up to `steps` items from `ours` and `theirs` alike, each from
    /// the front or the back at random, checking that they match, and so do
    /// the size hints before each; once both are done, that they stay done.
    pub(crate) fn assert_same_items<T: PartialEq + Debug>(
        &self,
        mut ours: impl DoubleEndedIterator<Item = T>,
        mut theirs: impl DoubleEndedIterator<Item = T>,
        steps: usize,
    ) {
        for _ in 0..steps {
            assert_eq!(ours.size_hint(), theirs.size_hint());
            let back = self.below(2) != 0;
            let (ours_item, theirs_item) = (ours.step(back), theirs.step(back));
            let done = theirs_item.is_none();
            assert_eq!(ours_item, theirs_item);
            if done {
                assert_eq!(ours.next(), None);
                assert_eq!(ours.next_back(), None);
                return;
            }
        }
    }
}

/// Steps a double-ended iterator from either end.
pub(crate) trait StepEitherEnd: DoubleEndedIterator {
    /// The next item from the back if `back`, otherwise from the front.
    fn step(&mut self, back: bool) -> Option<Self::Item> {
        if back { self.next_back() } else { self.next() }
    }
}

impl<I: DoubleEndedIterator> StepEitherEnd for I {}

/// `value`'s hash, by std's default hasher.
pub(crate) fn hash_of(value: &impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// A key or value made from a number. Different numbers make different
/// values as far as the type allows, and keys from ascending numbers ascend.
pub(crate) trait FromNumber {
    fn from_number(number: usize) -> Self;
}

impl FromNumber for u64 {
    fn from_number(number: usize) -> Self {
        number as u64
    }
}

impl FromNumber for u8 {
    fn from_number(number: usize) -> Self {
        number as u8
    }
}

impl FromNumber for () {
    fn from_number(_: usize) -> Self {}
}

impl FromNumber for String {
    fn from_number(number: usize) -> Self {
        format!("{number:010}")
    }
}

/// Byte-string keys whose lookups the leaves' length tags filter (D36), for
/// the tests of both maps: keys that tie on their inline keys and differ in
/// length, by a few bytes or past where tags saturate, and keys the map
/// lacks that tie with them.
pub(crate) struct LengthTagCase {
    /// What the map holds; each key's value is its index.
    keys: Vec<String>,
    /// Keys the map lacks, to look up besides its own.
    absent: Vec<String>,
}

impl LengthTagCase {
    /// A key that shares no byte with the others of its case: the tree then
    /// keeps no shared prefix (D50), and its root leaf takes its inline keys
    /// from their first byte, where the others tie.
    const OUTSIDE_KEY: &str = "z";

    /// The length from which keys' tags saturate.
    const SATURATED_LEN: usize = 65_535;

    /// Every case.
    pub(crate) fn all() -> Vec<Self> {
        let mut cases = Vec::from(Self::two_candidates_per_absent_key());
        cases.extend([
            Self::lone_candidates_longer_by_64_kib(),
            Self::lone_candidate_in_leaves_with_a_prefix(),
            Self::saturated_key_within_its_inline_key(),
            Self::zero_keys(),
        ]);
        cases
    }

    /// Cases where two keys, with another key between them, have each
    /// absent key's tag, so that its lookup compares the upper, then maybe
    /// the lower: keys of its length, and keys whose saturated tags match
    /// though their lengths don't.
    pub(crate) fn two_candidates_per_absent_key() -> [Self; 2] {
        let saturated =
            |stem: &str, extra| format!("{stem}{}", "\0".repeat(Self::SATURATED_LEN + extra));
        [
            Self::outside_and(
                [
                    "shared-prefix/a",
                    "shared-prefix/c0",
                    "shared-prefix/g11",
                    "shared-prefix/t0",
                    "shared-prefix/x111",
                ]
                .map(String::from),
                ["shared-prefix/b0", "shared-prefix/m0", "shared-prefix/z0"].map(String::from),
            ),
            Self::outside_and(
                [
                    saturated("shared-prefix/c0", 0),
                    String::from("shared-prefix/m11"),
                    saturated("shared-prefix/t0", 65),
                ],
                [
                    saturated("shared-prefix/c0", 5),
                    saturated("shared-prefix/m0", 0),
                    saturated("shared-prefix/t0", 0),
                    saturated("shared-prefix/z", 4_465),
                ],
            ),
        ]
    }

    /// Absent keys that end within their inline keys, each with one
    /// candidate, 65,536 bytes longer: a tag that wrapped would have the
    /// absent key's length, and an unlatched copy would take it for it.
    fn lone_candidates_longer_by_64_kib() -> Self {
        Self::outside_and(
            [
                format!("user:042{}", "x".repeat(65_536)),
                format!("a{}", "\0".repeat(65_536)),
            ],
            [String::from("user:042"), String::from("a")],
        )
    }

    /// As [`Self::lone_candidates_longer_by_64_kib`], among enough keys
    /// that leaves keep a prefix: the absent key's inline key ends in zeros
    /// past its end, where the long key has them too.
    fn lone_candidate_in_leaves_with_a_prefix() -> Self {
        let name = |number: usize| format!("k{number:07}");
        let mut keys: Vec<String> = (0..3_000)
            .filter(|&number| number != 1_500)
            .map(name)
            .collect();
        keys.push(format!(
            "{}{}{}",
            name(1_500),
            "\0".repeat(8),
            "y".repeat(65_528)
        ));
        Self {
            keys,
            absent: vec![name(1_500)],
        }
    }

    /// Absent keys of 65,535 bytes or more, whose tags saturate, that end
    /// within their inline keys, each with one candidate whose tag
    /// saturates too: equal saturated tags tell nothing of lengths. A key
    /// that long ends within its inline key only in a leaf whose fences
    /// share 65,527 bytes or more: one between two others, as the tree's
    /// shared prefix stops at 64 bytes (D50).
    fn saturated_key_within_its_inline_key() -> Self {
        let shared = "p".repeat(Self::SATURATED_LEN - 5);
        let name = |number: usize| format!("{shared}a{number:04}");
        let mut keys: Vec<String> = (0..80).filter(|&number| number != 40).map(name).collect();
        keys.push(format!("{}{}x", name(40), "\0".repeat(6)));
        Self {
            keys,
            absent: vec![name(40), format!("{}\0", name(40))],
        }
    }

    /// Keys of zeros alone, whose zero-padded inline keys are all equal, of
    /// lengths whose tags are equal or saturate.
    fn zero_keys() -> Self {
        let zeros = |len| "\0".repeat(len);
        Self {
            keys: [0, 1, 2, 65_536].map(zeros).into(),
            absent: [3, 65_535, 65_537].map(zeros).into(),
        }
    }

    /// A case of `keys` and the outside key, which lacks `absent`.
    fn outside_and(keys: impl Into<Vec<String>>, absent: impl Into<Vec<String>>) -> Self {
        let mut keys = keys.into();
        keys.push(String::from(Self::OUTSIDE_KEY));
        Self {
            keys,
            absent: absent.into(),
        }
    }

    pub(crate) fn keys(&self) -> &[String] {
        &self.keys
    }

    pub(crate) fn absent(&self) -> &[String] {
        &self.absent
    }

    /// What to look up: every key, and the keys the map lacks.
    pub(crate) fn probes(&self) -> impl Iterator<Item = &String> {
        self.keys.iter().chain(&self.absent)
    }

    /// The value a map built from the case's keys holds for `probe`: its
    /// index among the keys.
    pub(crate) fn value_of(&self, probe: &str) -> Option<usize> {
        self.keys.iter().position(|key| key == probe)
    }

    /// Whether the leaves of a map of `String` keys and `V` values, under
    /// strategy `S`, keep length tags, which the cases need to test them.
    pub(crate) fn leaves_keep_tags<V, S: InlineKeyStrategy<String>>() -> bool {
        Leaf::<String, V, S>::plan().has_length_tags()
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

/// The id as a big-endian `u64`, which orders like the value: maps with
/// `WithInlineKey` test the node operations with inline keys on tracked keys.
impl InlineKey for Tracked<'_> {
    fn inline_key_at(&self, offset: usize) -> InlineWord {
        u64::from(self.id).to_be_bytes().inline_key_at(offset)
    }
}

/// A tracked key or value padded to 152 bytes. As keys, with [`Tracked`]
/// values, they fill a 2 KB node with exactly 12 entries in both node kinds
/// (D13, D15, D17, D21): tall trees from few entries, which Miri can afford,
/// and the node shapes the balancing tests spell out. No key size gives
/// exactly 12 in both kinds of a 1 KB node. As values, they test large values
/// in leaves. Ordered by id.
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

impl<'l, K: FromTracked<'l>, V: FromTracked<'l>, S: InlineKeyStrategy<K>> Leaf<K, V, S> {
    /// The `(key id, value id)` of each entry, in order.
    pub(super) fn tracked_ids(self) -> Vec<(usize, usize)> {
        (0..self.len())
            .map(|index| {
                let (key, value) = self.key_value(index);
                (key.tracked_id(), value.tracked_id())
            })
            .collect()
    }

    /// The id of the head, if the leaf has one.
    pub(super) fn head_id(self) -> Option<usize> {
        self.has_head().then(|| self.head_value().tracked_id())
    }

    /// The id of the head's key, if the leaf has a head and separator keys
    /// are heads (D49).
    pub(super) fn head_key_id(self) -> Option<usize> {
        (self.has_head() && NodePtr::<K, V, S>::has_separator_keys_in_heads())
            .then(|| self.head_key().tracked_id())
    }
}

impl<'l, K: FromTracked<'l>, V: FromTracked<'l>, S: InlineKeyStrategy<K>> Internal<K, V, S> {
    /// The `(key id, value id)` of each separator, in order, its value read
    /// from the head that holds it (D17).
    pub(super) fn tracked_ids(self) -> Vec<(usize, usize)> {
        (0..self.len())
            .map(|index| {
                let value = self.head_leaf(index).head_value();
                (self.separator_key(index).tracked_id(), value.tracked_id())
            })
            .collect()
    }
}

impl<'l, K: FromTracked<'l>, V: FromTracked<'l>, S: InlineKeyStrategy<K>> Node<K, V, S> {
    /// The `(key id, value id)` of every entry under this node, in order;
    /// see [`Node::for_each_entry`].
    pub(super) fn tracked_ids_under(self) -> Vec<(usize, usize)> {
        let mut ids = Vec::new();
        self.for_each_entry(&mut |key, value| ids.push((key.tracked_id(), value.tracked_id())));
        ids
    }

    /// The id of the head of the leftmost leaf under this node, if it has
    /// one: the value of the separator just left of the node (D17).
    pub(super) fn leftmost_head_id(self) -> Option<usize> {
        self.border_leaf::<FirstEdge>().head_id()
    }

    /// The id of the key of the separator just left of this node, given
    /// `separator`, as it came out of its node: its own, where internal
    /// nodes keep keys, or the leftmost leaf's head's under this node (D49).
    pub(super) fn separator_key_id(self, separator: &Separator<K, S>) -> usize {
        match separator.key() {
            Some(key) => key.tracked_id(),
            None => (self.border_leaf::<FirstEdge>().head_key_id())
                .expect("the leaf right of a separator has a head with its key"),
        }
    }
}

impl<K, V, S> Node<K, V, S> {
    /// The leaf this node is.
    ///
    /// # Panics
    ///
    /// If the node is internal.
    pub(crate) fn expect_leaf(self) -> Leaf<K, V, S> {
        match self {
            Node::Leaf(leaf) => leaf,
            Node::Internal(_) => panic!("the node is internal, not a leaf"),
        }
    }

    /// The internal node this node is.
    ///
    /// # Panics
    ///
    /// If the node is a leaf.
    pub(crate) fn expect_internal(self) -> Internal<K, V, S> {
        match self {
            Node::Internal(internal) => internal,
            Node::Leaf(_) => panic!("the node is a leaf, not internal"),
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Node<K, V, S> {
    /// Panics unless every node under and at this one has its keys'
    /// windows at its prefix length as its inline keys, or, without inline
    /// keys, none and a prefix length of 0 (D18, 05-fence-key.md); see
    /// [`NodeChecks::assert_inline_keys_match`].
    pub(in crate::tree) fn assert_inline_keys_match_under(self) {
        match self {
            Node::Leaf(leaf) => leaf.assert_inline_keys_match(),
            Node::Internal(internal) => {
                internal.assert_inline_keys_match();
                for index in 0..=internal.len() {
                    internal.child(index).assert_inline_keys_match_under();
                }
            }
        }
    }

    /// Calls `on_entry` on every entry under this node in key order: each
    /// leaf's keys and values, and each separator's key with its value from
    /// the head that holds it (D17). A head in the leftmost leaf belongs to
    /// a separator outside, and is skipped.
    pub(super) fn for_each_entry<'a>(self, on_entry: &mut impl FnMut(&'a K, &'a V))
    where
        K: 'a,
        V: 'a,
    {
        match self {
            Node::Leaf(leaf) => {
                for index in 0..leaf.len() {
                    let (key, value) = leaf.key_value(index);
                    on_entry(key, value);
                }
            }
            Node::Internal(internal) => {
                for index in 0..=internal.len() {
                    internal.child(index).for_each_entry(on_entry);
                    if index < internal.len() {
                        let value = internal.head_leaf(index).head_value();
                        on_entry(internal.separator_key(index), value);
                    }
                }
            }
        }
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

    pub(in crate::tree) fn get(self) -> u64 {
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
    tree: Tree<K, V, S>,
    pages: &'p TestPages<K, V, S>,
}

impl<'p, K, V, S: InlineKeyStrategy<K>> TestTree<'p, K, V, S> {
    /// A tree with an empty leaf as its root.
    pub(super) fn new(pages: &'p TestPages<K, V, S>) -> Self {
        Self {
            tree: Tree::new(pages),
            pages,
        }
    }

    pub(super) fn root(&self) -> Node<K, V, S> {
        self.tree.root()
    }

    pub(super) fn root_mut(&mut self) -> Node<K, V, S> {
        self.tree.root_mut()
    }

    pub(super) fn leaf_depth(&self) -> usize {
        self.tree.leaf_depth()
    }

    /// One `on_entry(key, value)` per entry, in order.
    pub(super) fn entries<T>(&self, mut on_entry: impl FnMut(&K, &V) -> T) -> Vec<T> {
        let mut entries = Vec::new();
        self.root().for_each_entry(&mut |key, value| {
            entries.push(on_entry(key, value));
        });
        entries
    }

    /// Lowers the tree while its root is internal without keys, as a cut
    /// does after merges empty the root.
    pub(super) fn lower_while_root_empty(&mut self) {
        self.tree.lower_while_root_empty(self.pages);
    }

    /// Takes the tree apart, giving every page back to the pool.
    pub(super) fn destroy(self) {
        let Self { tree, pages } = self;
        tree.destroy(pages);
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Tree<K, V, S> {
    /// Raises the tree a level ([`Tree::raise`]), the root node moving to a
    /// page from `pages`, and returns the root, internal now.
    pub(super) fn raise_in(&mut self, pages: &TestPages<K, V, S>) -> Internal<K, V, S> {
        self.raise(pages);
        self.root().expect_internal()
    }
}

impl<K: Ord, V, S: InlineKeyStrategy<K>> TestTree<'_, K, V, S> {
    /// Inserts `key`/`value`, or replaces the value of an equal key and
    /// returns the old one, as the map does.
    pub(super) fn insert(&mut self, key: K, value: V) -> Option<V> {
        let mut length = 0;
        self.tree.insert(key, value, self.pages, &mut length)
    }
}

mod tests {
    use super::{Bulky, CacheLineAligned, PageAligned, Tracked};
    use crate::tree::node::layout::NodeLayout;

    #[test]
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
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
