//! Compile-time byte layout of the node pages of each kind, without and with
//! inline keys.

#![forbid(unsafe_code)]

use core::alloc::Layout;
use core::marker::PhantomData;
use core::mem::{align_of, size_of};

use super::page::{InlineKeyNodeHeader, NodeHeader, NodePtr};
use crate::inline_key::{Auto, InlineWord};

/// Byte layouts of the node pages for keys `K` and values `V`: for leaves
/// and for internal nodes (D9, D12, D17, D21), each without and with an
/// array of inline keys (D18).
///
/// ```text
/// leaf      | header | k0 k1 … →           free           ← … v1 v0 | head |
/// internal  | header | k0 k1 … →       free       | e0 e1 … e_cap |
///
/// with inline keys:
/// leaf      | header | w0 … w_cap | k0 k1 … →     free     ← … v1 v0 | head |
/// internal  | header | w0 … w_cap | k0 k1 … →   free   | e0 e1 … e_cap |
///           0        inline_keys_offset
///                                 keys_offset
/// ```
///
/// - Every layout has one shape: a header at offset 0, keys growing up from
///   `keys_offset`, and one more trailing slot than keys at the end of the
///   node: a leaf's head and values, or an internal node's edges. One formula
///   sizes them all, from the size of their trailing slots and whether they
///   have inline keys.
/// - Without inline keys, the header is a [`NodeHeader`], and the keys
///   follow it, at the same offset in both kinds.
/// - With inline keys, the header is an [`InlineKeyNodeHeader`], and an
///   array of `capacity` inline keys follows it, then the keys: a search
///   reads the dense inline keys first, from the front of the node. The keys'
///   offset depends on the capacity, so on the kind.
/// - Each kind is the smallest multiple of its alignment that holds
///   `MIN_CAPACITY` keys, and is aligned to 1 KB or more (D13, D15). A
///   leaf's size depends on `K` and `V`; an internal node's on `K` alone, as
///   it holds no values (D17, D21).
/// - A leaf's head is the last `V` of the node, at `head_offset`, and its
///   values grow down from there: value `i` is at
///   `head_offset - (i + 1) * size_of::<V>()`. Stored in reverse, the head
///   and values `[0, b)` are one contiguous `[V]` in reverse order, head
///   last.
/// - An internal node's `capacity + 1` edges fill its end.
///
/// The four layouts are constants. Every invariant the node code relies on
/// for one is asserted at compile time when that constant is evaluated,
/// which happens before any page of that layout exists. A map picks the
/// constants of its key type (`NodePtr::has_inline_keys`, in `page.rs`), by
/// a choice that folds after monomorphization.
pub(super) struct NodeLayout<K, V>(PhantomData<fn() -> (K, V)>);

/// Where the keys of one kind of node sit, and how many fit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct KeyArea {
    /// The most keys the node holds.
    pub(super) capacity: usize,
    /// Where `capacity` inline keys start, right after the header, in a
    /// layout with inline keys.
    pub(super) inline_keys_offset: Option<usize>,
    /// Where key 0 is; key `i` is at `keys_offset + i * size_of::<K>()`.
    pub(super) keys_offset: usize,
}

/// The layout of a leaf's page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LeafLayout {
    pub(super) page: Layout,
    pub(super) keys: KeyArea,
    /// Where the head starts and the values end: the last `V` of the page.
    pub(super) head_offset: usize,
}

/// The layout of an internal node's page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct InternalLayout {
    pub(super) page: Layout,
    pub(super) keys: KeyArea,
    /// Where the `capacity + 1` edges start; they end with the page.
    pub(super) edges_offset: usize,
}

impl<K, V> NodeLayout<K, V> {
    /// Every node is a multiple of this size and at least this aligned (D15).
    const BASE_NODE_SIZE: usize = 1024;

    /// Every node kind holds at least this many keys (D13); nodes grow by
    /// whole alignment steps until it fits.
    const MIN_CAPACITY: usize = 12;

    /// Upper bound on any node's capacity, so that an index (`0..=capacity`)
    /// fits in the 10 low bits that 1 KB alignment leaves free. Binds for
    /// leaves whose keys and values are (nearly) zero-sized.
    const MAX_CAPACITY: usize = 1023;

    /// Where the inline keys start in a node that has them.
    const INLINE_KEYS_OFFSET: usize =
        size_of::<InlineKeyNodeHeader>().next_multiple_of(align_of::<InlineWord>());

    /// An edge: a pointer to a child node, the same for every strategy,
    /// which is only a marker in it.
    const EDGE_SIZE: usize = size_of::<NodePtr<K, V, Auto>>();

    const EDGE_ALIGN: usize = align_of::<NodePtr<K, V, Auto>>();

    pub(super) const PLAIN_LEAF: LeafLayout = Self::leaf_layout(false);

    pub(super) const INLINE_LEAF: LeafLayout = Self::leaf_layout(true);

    pub(super) const PLAIN_INTERNAL: InternalLayout = Self::internal_layout(false);

    pub(super) const INLINE_INTERNAL: InternalLayout = Self::internal_layout(true);

    /// The leaf layout with or without inline keys: one of the two
    /// constants, which `inline_keys` picks after monomorphization.
    pub(super) fn leaf(inline_keys: bool) -> LeafLayout {
        if inline_keys {
            Self::INLINE_LEAF
        } else {
            Self::PLAIN_LEAF
        }
    }

    /// The internal-node layout with or without inline keys; see
    /// [`Self::leaf`].
    pub(super) fn internal(inline_keys: bool) -> InternalLayout {
        if inline_keys {
            Self::INLINE_INTERNAL
        } else {
            Self::PLAIN_INTERNAL
        }
    }

    /// Leaves are sized by their keys and values, and the head (D17).
    const fn leaf_layout(inline_keys: bool) -> LeafLayout {
        let value_size = size_of::<V>();
        let align = Self::node_align(align_of::<V>());
        let size = Self::smallest_node_size(inline_keys, value_size, align);
        let layout = LeafLayout {
            page: Self::page_layout(size, align),
            keys: Self::key_area(inline_keys, size, value_size),
            head_offset: size - value_size,
        };
        Self::assert_leaf_invariants(layout);
        layout
    }

    // Internal nodes depend on `K` and the edge, a pointer, alone: nothing
    // below uses `V`.

    /// Internal nodes are sized by their keys and one more edge.
    const fn internal_layout(inline_keys: bool) -> InternalLayout {
        let align = Self::node_align(Self::EDGE_ALIGN);
        let size = Self::smallest_node_size(inline_keys, Self::EDGE_SIZE, align);
        let keys = Self::key_area(inline_keys, size, Self::EDGE_SIZE);
        let layout = InternalLayout {
            page: Self::page_layout(size, align),
            keys,
            edges_offset: size - (keys.capacity + 1) * Self::EDGE_SIZE,
        };
        Self::assert_internal_invariants(layout);
        layout
    }

    const fn assert_leaf_invariants(layout: LeafLayout) {
        let (size, capacity) = (layout.page.size(), layout.keys.capacity);
        let value_size = size_of::<V>();
        Self::assert_kind_invariants(layout.page, layout.keys);

        // The head and values are aligned, as the node end and every
        // `size_of::<V>()` step below it are, and they never overlap the keys
        // at full capacity.
        assert!(size.is_multiple_of(align_of::<V>()));
        assert!(layout.head_offset + value_size == size);
        assert!(
            layout.keys.keys_offset + capacity * size_of::<K>()
                <= layout.head_offset - capacity * value_size
        );
    }

    const fn assert_internal_invariants(layout: InternalLayout) {
        let (size, capacity) = (layout.page.size(), layout.keys.capacity);
        Self::assert_kind_invariants(layout.page, layout.keys);

        // Keys never overlap the edges at full capacity, and the edges are
        // aligned and end exactly at the node end.
        assert!(layout.keys.keys_offset + capacity * size_of::<K>() <= layout.edges_offset);
        assert!(layout.edges_offset.is_multiple_of(Self::EDGE_ALIGN));
        assert!(layout.edges_offset + (capacity + 1) * Self::EDGE_SIZE == size);
    }

    /// What every layout needs: whole 1 KB steps, a capacity in bounds, and
    /// the header, inline keys and keys in place, in that order.
    const fn assert_kind_invariants(page: Layout, keys: KeyArea) {
        let (node_size, node_align) = (page.size(), page.align());
        assert!(node_align.is_power_of_two());
        assert!(node_size.is_multiple_of(Self::BASE_NODE_SIZE));
        assert!(node_size.is_multiple_of(node_align));

        assert!(keys.capacity >= Self::MIN_CAPACITY);
        assert!(keys.capacity <= Self::MAX_CAPACITY);
        assert!(Self::MAX_CAPACITY <= u16::MAX as usize);

        // The header is aligned and never overlaps what follows it.
        assert!(node_align.is_multiple_of(align_of::<NodeHeader>()));
        match keys.inline_keys_offset {
            None => assert!(keys.keys_offset >= size_of::<NodeHeader>()),
            Some(inline_keys_offset) => {
                assert!(node_align.is_multiple_of(align_of::<InlineKeyNodeHeader>()));
                assert!(inline_keys_offset >= size_of::<InlineKeyNodeHeader>());
                assert!(node_align.is_multiple_of(align_of::<InlineWord>()));
                assert!(inline_keys_offset.is_multiple_of(align_of::<InlineWord>()));
                assert!(
                    inline_keys_offset + keys.capacity * size_of::<InlineWord>()
                        <= keys.keys_offset
                );
            }
        }
        assert!(keys.keys_offset.is_multiple_of(align_of::<K>()));
    }

    const fn page_layout(node_size: usize, node_align: usize) -> Layout {
        match Layout::from_size_align(node_size, node_align) {
            Ok(layout) => layout,
            Err(_) => panic!("node size overflows isize"),
        }
    }

    const fn max(a: usize, b: usize) -> usize {
        if a > b { a } else { b }
    }

    const fn min(a: usize, b: usize) -> usize {
        if a < b { a } else { b }
    }

    /// A node is aligned to 1 KB (D15), and to its keys and trailing slots.
    const fn node_align(trailing_slot_align: usize) -> usize {
        Self::max(
            Self::BASE_NODE_SIZE,
            Self::max(align_of::<K>(), trailing_slot_align),
        )
    }

    /// The bytes per key of a node: the key, its trailing slot, and its
    /// inline key if the node has them.
    const fn size_per_key(inline_keys: bool, trailing_slot_size: usize) -> usize {
        let inline_key_size = if inline_keys {
            size_of::<InlineWord>()
        } else {
            0
        };
        inline_key_size + size_of::<K>() + trailing_slot_size
    }

    /// Where the keys could start, before rounding up to `K`'s alignment:
    /// after the header, and the inline keys if any.
    const fn keys_start(inline_keys: bool, capacity: usize) -> usize {
        if inline_keys {
            Self::INLINE_KEYS_OFFSET + capacity * size_of::<InlineWord>()
        } else {
            size_of::<NodeHeader>()
        }
    }

    /// The bytes a node of `capacity` keys needs: the header, the inline
    /// keys if any, the keys, and one more trailing slot of
    /// `trailing_slot_size` bytes than keys.
    const fn bytes_needed(inline_keys: bool, capacity: usize, trailing_slot_size: usize) -> usize {
        Self::keys_start(inline_keys, capacity).next_multiple_of(align_of::<K>())
            + capacity * size_of::<K>()
            + (capacity + 1) * trailing_slot_size
    }

    /// The smallest multiple of `node_align` that holds `MIN_CAPACITY` keys
    /// and one more trailing slot of `trailing_slot_size` bytes than keys.
    const fn smallest_node_size(
        inline_keys: bool,
        trailing_slot_size: usize,
        node_align: usize,
    ) -> usize {
        Self::bytes_needed(inline_keys, Self::MIN_CAPACITY, trailing_slot_size)
            .next_multiple_of(node_align)
    }

    /// The keys of a node of `node_size` bytes whose end holds one more slot
    /// of `trailing_slot_size` bytes than keys: the largest capacity, up to
    /// `MAX_CAPACITY`, whose `bytes_needed` fit, and where they go.
    const fn key_area(inline_keys: bool, node_size: usize, trailing_slot_size: usize) -> KeyArea {
        let size_per_key = Self::size_per_key(inline_keys, trailing_slot_size);
        // An upper bound: the keys start at least at `keys_start(0)`, plus
        // their inline keys. Without inline keys it is exact; with them,
        // rounding the keys' start up to `K`'s alignment can cost a few.
        // Entries that take no room fit up to the cap.
        let room = node_size.saturating_sub(Self::keys_start(inline_keys, 0) + trailing_slot_size);
        let mut capacity = match room.checked_div(size_per_key) {
            Some(bound) => Self::min(Self::MAX_CAPACITY, bound),
            None => Self::MAX_CAPACITY,
        };
        while Self::bytes_needed(inline_keys, capacity, trailing_slot_size) > node_size {
            capacity -= 1;
        }
        KeyArea {
            capacity,
            inline_keys_offset: if inline_keys {
                Some(Self::INLINE_KEYS_OFFSET)
            } else {
                None
            },
            keys_offset: Self::keys_start(inline_keys, capacity).next_multiple_of(align_of::<K>()),
        }
    }
}

#[cfg(test)]
mod tests {
    //! Every expected number is worked out by hand from the rules of D13,
    //! D15, D18 and D21, with an edge and an inline key 8 bytes each:
    //!
    //! - a node needs `keys_offset + n·size_of K + (n + 1)·slot` bytes for
    //!   `n` keys, where the slot is `size_of V` for a leaf and 8 for an
    //!   internal node;
    //! - without inline keys, the header is 4 bytes and `keys_offset` is 4
    //!   rounded up to `align_of K`;
    //! - with inline keys, the header is 6 bytes, the inline keys start at 8,
    //!   and `keys_offset` is `8 + 8·n` rounded up to `align_of K`;
    //! - the node size is what 12 keys need, rounded up to the node's
    //!   alignment, max(1024, align_of K, align_of V) for a leaf and
    //!   max(1024, align_of K, 8) for an internal node;
    //! - the capacity is the largest `n` that fits, at most 1023: without
    //!   inline keys, ⌊(size − keys_offset − slot) / (size_of K + slot)⌋.

    use core::alloc::Layout;
    use std::string::String;
    use std::vec::Vec;

    use super::{InternalLayout, KeyArea, LeafLayout, NodeLayout};
    use crate::node::test_support::{CacheLineAligned, PageAligned};

    /// The expected page of one node kind.
    struct ExpectedNode {
        size: usize,
        align: usize,
        capacity: usize,
        keys_offset: usize,
    }

    impl ExpectedNode {
        /// A 1 KB, 1 KB-aligned node.
        fn one_page(capacity: usize, keys_offset: usize) -> Self {
            Self {
                size: 1024,
                align: 1024,
                capacity,
                keys_offset,
            }
        }

        fn assert_matches(&self, page: Layout, keys: KeyArea, inline_keys: bool) {
            assert_eq!(
                page,
                Layout::from_size_align(self.size, self.align).unwrap()
            );
            assert_eq!(keys.capacity, self.capacity);
            assert_eq!(keys.keys_offset, self.keys_offset);
            assert_eq!(keys.inline_keys_offset, inline_keys.then_some(8));
            assert!(page.size().is_multiple_of(1024));
            assert!(page.align() >= 1024);
        }

        fn assert_leaf(&self, layout: LeafLayout, value_size: usize, inline_keys: bool) {
            self.assert_matches(layout.page, layout.keys, inline_keys);
            assert_eq!(layout.head_offset, self.size - value_size);
        }

        fn assert_internal(&self, layout: InternalLayout, inline_keys: bool) {
            self.assert_matches(layout.page, layout.keys, inline_keys);
            assert_eq!(layout.edges_offset, self.size - (self.capacity + 1) * 8);
        }
    }

    struct ExpectedLayout {
        leaf: ExpectedNode,
        internal: ExpectedNode,
    }

    impl ExpectedLayout {
        /// Checks the layouts without inline keys.
        fn assert_plain<K, V>(&self) {
            assert_eq!(self.leaf.keys_offset, self.internal.keys_offset);
            self.leaf
                .assert_leaf(NodeLayout::<K, V>::PLAIN_LEAF, size_of::<V>(), false);
            self.internal
                .assert_internal(NodeLayout::<K, V>::PLAIN_INTERNAL, false);
            assert_eq!(
                NodeLayout::<K, V>::leaf(false),
                NodeLayout::<K, V>::PLAIN_LEAF
            );
            assert_eq!(
                NodeLayout::<K, V>::internal(false),
                NodeLayout::<K, V>::PLAIN_INTERNAL
            );
        }

        /// Checks the layouts with inline keys.
        fn assert_inline<K, V>(&self) {
            self.leaf
                .assert_leaf(NodeLayout::<K, V>::INLINE_LEAF, size_of::<V>(), true);
            self.internal
                .assert_internal(NodeLayout::<K, V>::INLINE_INTERNAL, true);
            assert_eq!(
                NodeLayout::<K, V>::leaf(true),
                NodeLayout::<K, V>::INLINE_LEAF
            );
            assert_eq!(
                NodeLayout::<K, V>::internal(true),
                NodeLayout::<K, V>::INLINE_INTERNAL
            );
        }
    }

    /// Values and edges are both 8 bytes, so both kinds are alike.
    #[test]
    fn u64_to_u64_fills_one_page() {
        ExpectedLayout {
            // Needs 8 + 12·8 + 13·8 = 208; ⌊(1024 − 8 − 8) / 16⌋ = 63.
            leaf: ExpectedNode::one_page(63, 8),
            // The same numbers.
            internal: ExpectedNode::one_page(63, 8),
        }
        .assert_plain::<u64, u64>();
    }

    #[test]
    fn string_to_u64_fills_one_page() {
        ExpectedLayout {
            // Needs 8 + 12·24 + 13·8 = 400; ⌊(1024 − 8 − 8) / 32⌋ = 31.
            leaf: ExpectedNode::one_page(31, 8),
            // The same numbers.
            internal: ExpectedNode::one_page(31, 8),
        }
        .assert_plain::<String, u64>();
    }

    /// The layout `String` keys get by default (D18): 8 bytes more per key,
    /// so 25 keys instead of 31 (04-inline-key.md).
    #[test]
    fn string_to_u64_with_inline_keys() {
        ExpectedLayout {
            // Needs 8 + 12·8 + 12·24 + 13·8 = 496.
            // 8 + 8n + 24n + 8(n + 1) ≤ 1024: 40n ≤ 1008, n = 25, keys at
            // 8 + 25·8 = 208.
            leaf: ExpectedNode::one_page(25, 208),
            // The same numbers.
            internal: ExpectedNode::one_page(25, 208),
        }
        .assert_inline::<String, u64>();
    }

    /// `Vec<u8>` is 24 bytes and 8-aligned, like `String`.
    #[test]
    fn byte_vec_to_u64_with_inline_keys() {
        ExpectedLayout {
            leaf: ExpectedNode::one_page(25, 208),
            internal: ExpectedNode::one_page(25, 208),
        }
        .assert_inline::<Vec<u8>, u64>();
    }

    /// Large values cut the leaf's capacity, not the internal node's.
    #[test]
    fn string_to_string_with_inline_keys() {
        ExpectedLayout {
            // 8 + 8n + 24n + 24(n + 1) ≤ 1024: 56n ≤ 992, n = 17, keys at
            // 8 + 17·8 = 144.
            leaf: ExpectedNode::one_page(17, 144),
            // As for `String` → `u64`.
            internal: ExpectedNode::one_page(25, 208),
        }
        .assert_inline::<String, String>();
    }

    /// Inline keys on unaligned keys: the keys start right after them.
    #[test]
    fn byte_array_keys_with_inline_keys() {
        ExpectedLayout {
            // 8 + 8n + 32n + 8(n + 1) ≤ 1024: 48n ≤ 1008, n = 21, keys at
            // 8 + 21·8 = 176.
            leaf: ExpectedNode::one_page(21, 176),
            internal: ExpectedNode::one_page(21, 176),
        }
        .assert_inline::<[u8; 32], u64>();
    }

    /// Over-aligned keys round their start up past the inline keys.
    #[test]
    fn over_aligned_keys_with_inline_keys() {
        ExpectedLayout {
            // 12 keys need (8 + 96 → 128) + 12·64 + 13·8 = 1000.
            // An upper bound is ⌊(1024 − 8 − 8) / 80⌋ = 12, which fits; keys
            // at 8 + 12·8 = 104 → 128.
            leaf: ExpectedNode::one_page(12, 128),
            internal: ExpectedNode::one_page(12, 128),
        }
        .assert_inline::<CacheLineAligned, u64>();
    }

    /// Small keys: the inline keys outweigh them.
    #[test]
    fn set_of_bytes_with_inline_keys() {
        ExpectedLayout {
            // 8 + 8n + n ≤ 1024: 9n ≤ 1016, n = 112, keys at 8 + 896 = 904.
            leaf: ExpectedNode::one_page(112, 904),
            // 8 + 8n + n + 8(n + 1) ≤ 1024: 17n ≤ 1008, n = 59, keys at
            // 8 + 472 = 480.
            internal: ExpectedNode::one_page(59, 480),
        }
        .assert_inline::<u8, ()>();
    }

    /// The internal layout is a function of the key type alone.
    #[test]
    fn internal_nodes_ignore_the_value_type() {
        fn assert_internal_matches_u64_to_u64<V>() {
            assert_eq!(
                NodeLayout::<u64, V>::PLAIN_INTERNAL,
                NodeLayout::<u64, u64>::PLAIN_INTERNAL
            );
            assert_eq!(
                NodeLayout::<u64, V>::INLINE_INTERNAL,
                NodeLayout::<u64, u64>::INLINE_INTERNAL
            );
        }
        assert_internal_matches_u64_to_u64::<()>();
        assert_internal_matches_u64_to_u64::<u8>();
        assert_internal_matches_u64_to_u64::<[u8; 71]>();
        assert_internal_matches_u64_to_u64::<[u8; 1000]>();
        assert_internal_matches_u64_to_u64::<CacheLineAligned>();
        assert_internal_matches_u64_to_u64::<PageAligned>();
    }

    /// The case D21 is about: the leaf grows to fit 12 values, and the
    /// internal node stays the one of any other `u64`-keyed map.
    #[test]
    fn large_values_grow_leaves_only() {
        ExpectedLayout {
            // Needs 8 + 12·8 + 13·1000 = 13 104, over 12 KB: 13 KB.
            // ⌊(13 312 − 8 − 1000) / 1008⌋ = ⌊12 304 / 1008⌋ = 12.
            leaf: ExpectedNode {
                size: 13 * 1024,
                align: 1024,
                capacity: 12,
                keys_offset: 8,
            },
            // Needs 8 + 12·8 + 13·8 = 208; ⌊(1024 − 8 − 8) / 16⌋ = 63.
            internal: ExpectedNode::one_page(63, 8),
        }
        .assert_plain::<u64, [u8; 1000]>();
    }

    #[test]
    fn small_values_pack_more_leaf_entries() {
        ExpectedLayout {
            // Needs 8 + 12·8 + 13·1 = 117; ⌊(1024 − 8 − 1) / 9⌋ = 112.
            leaf: ExpectedNode::one_page(112, 8),
            // Needs 8 + 12·8 + 13·8 = 208; ⌊(1024 − 8 − 8) / 16⌋ = 63.
            internal: ExpectedNode::one_page(63, 8),
        }
        .assert_plain::<u64, u8>();
    }

    #[test]
    fn set_of_bytes_packs_tightly() {
        ExpectedLayout {
            // Needs 4 + 12·1 = 16; ⌊(1024 − 4 − 0) / 1⌋ = 1020.
            leaf: ExpectedNode::one_page(1020, 4),
            // Needs 4 + 12·1 + 13·8 = 120; ⌊(1024 − 4 − 8) / 9⌋ = 112.
            internal: ExpectedNode::one_page(112, 4),
        }
        .assert_plain::<u8, ()>();
    }

    #[test]
    fn zero_sized_keys_and_values_hit_the_capacity_cap() {
        ExpectedLayout {
            // Needs 4; entries take no room, so the cap of 1023 binds.
            leaf: ExpectedNode::one_page(1023, 4),
            // Needs 4 + 13·8 = 108; ⌊(1024 − 4 − 8) / 8⌋ = 126.
            internal: ExpectedNode::one_page(126, 4),
        }
        .assert_plain::<(), ()>();
    }

    #[test]
    fn over_aligned_keys_shift_the_key_array() {
        ExpectedLayout {
            // Needs 64 + 12·64 + 13·8 = 936; ⌊(1024 − 64 − 8) / 72⌋ = 13.
            leaf: ExpectedNode::one_page(13, 64),
            // The same numbers.
            internal: ExpectedNode::one_page(13, 64),
        }
        .assert_plain::<CacheLineAligned, u64>();
    }

    /// Over-aligned keys and values: the leaf grows for the values, and the
    /// internal node is that of `CacheLineAligned → u64`.
    #[test]
    fn over_aligned_values_grow_leaves_only() {
        ExpectedLayout {
            // Needs 64 + 12·64 + 13·64 = 1664: 2 KB.
            // ⌊(2048 − 64 − 64) / 128⌋ = 15.
            leaf: ExpectedNode {
                size: 2048,
                align: 1024,
                capacity: 15,
                keys_offset: 64,
            },
            // Needs 64 + 12·64 + 13·8 = 936; ⌊(1024 − 64 − 8) / 72⌋ = 13.
            internal: ExpectedNode::one_page(13, 64),
        }
        .assert_plain::<CacheLineAligned, CacheLineAligned>();
    }

    #[test]
    fn page_aligned_values_raise_leaf_alignment_only() {
        ExpectedLayout {
            // Aligned to 8 KB. Needs 8 + 12·8 + 13·8192 = 106 600, over
            // 13·8192: 14·8192 = 114 688.
            // ⌊(114 688 − 8 − 8192) / 8200⌋ = ⌊106 488 / 8200⌋ = 12.
            leaf: ExpectedNode {
                size: 14 * 8192,
                align: 8192,
                capacity: 12,
                keys_offset: 8,
            },
            // Needs 8 + 12·8 + 13·8 = 208; ⌊(1024 − 8 − 8) / 16⌋ = 63.
            internal: ExpectedNode::one_page(63, 8),
        }
        .assert_plain::<u64, PageAligned>();
    }

    #[test]
    fn heap_owning_keys_and_values() {
        ExpectedLayout {
            // Needs 8 + 12·24 + 13·24 = 608; ⌊(1024 − 8 − 24) / 48⌋ = 20.
            leaf: ExpectedNode::one_page(20, 8),
            // Needs 8 + 12·24 + 13·8 = 400; ⌊(1024 − 8 − 8) / 32⌋ = 31.
            internal: ExpectedNode::one_page(31, 8),
        }
        .assert_plain::<String, Vec<u8>>();
    }

    /// With `K = u64`, 12 entries and a head fit in a 1 KB leaf up to
    /// `size_of V = 70`.
    #[test]
    fn largest_value_that_fits_one_page() {
        ExpectedLayout {
            // Needs 8 + 12·8 + 13·70 = 1014; ⌊(1024 − 8 − 70) / 78⌋ = 12.
            leaf: ExpectedNode::one_page(12, 8),
            internal: ExpectedNode::one_page(63, 8),
        }
        .assert_plain::<u64, [u8; 70]>();
    }

    #[test]
    fn one_value_byte_more_needs_a_two_page_leaf() {
        ExpectedLayout {
            // Needs 8 + 12·8 + 13·71 = 1027: 2 KB.
            // ⌊(2048 − 8 − 71) / 79⌋ = ⌊1969 / 79⌋ = 24.
            leaf: ExpectedNode {
                size: 2048,
                align: 1024,
                capacity: 24,
                keys_offset: 8,
            },
            internal: ExpectedNode::one_page(63, 8),
        }
        .assert_plain::<u64, [u8; 71]>();
    }

    /// 12 keys and 13 edges fit in a 1 KB internal node up to
    /// `size_of K = 76` (with `align_of K = 1`).
    #[test]
    fn largest_key_that_fits_a_one_page_internal_node() {
        ExpectedLayout {
            // Needs 4 + 12·76 = 916; ⌊(1024 − 4 − 0) / 76⌋ = 13.
            leaf: ExpectedNode::one_page(13, 4),
            // Needs 4 + 12·76 + 13·8 = 1020; ⌊(1024 − 4 − 8) / 84⌋ = 12.
            internal: ExpectedNode::one_page(12, 4),
        }
        .assert_plain::<[u8; 76], ()>();
    }

    /// An internal node can be the larger kind: one key byte more and it
    /// needs 2 KB, while the leaf of a set still fits in 1 KB.
    #[test]
    fn one_key_byte_more_needs_a_two_page_internal_node() {
        ExpectedLayout {
            // Needs 4 + 12·77 = 928; ⌊(1024 − 4 − 0) / 77⌋ = 13.
            leaf: ExpectedNode::one_page(13, 4),
            // Needs 4 + 12·77 + 13·8 = 1032: 2 KB.
            // ⌊(2048 − 4 − 8) / 85⌋ = ⌊2036 / 85⌋ = 23.
            internal: ExpectedNode {
                size: 2048,
                align: 1024,
                capacity: 23,
                keys_offset: 4,
            },
        }
        .assert_plain::<[u8; 77], ()>();
    }
}
