//! Compile-time byte layout of the node pages of each kind.

#![forbid(unsafe_code)]

use core::alloc::Layout;
use core::marker::PhantomData;
use core::mem::{align_of, size_of};

use super::page::{NodeHeader, NodePtr};

/// Byte layouts of the node pages for keys `K` and values `V`: one for
/// leaves and one for internal nodes (D9, D12, D17, D21).
///
/// ```text
/// leaf      | header | k0 k1 … →           free           ← … v1 v0 | head |
///           0        KEYS_OFFSET                           HEAD_OFFSET  LEAF_SIZE
///
/// internal  | header | k0 k1 … →       free       | e0 e1 … e_cap |
///           0        KEYS_OFFSET                  EDGES_OFFSET    INTERNAL_SIZE
/// ```
///
/// - Both kinds have one shape: a [`NodeHeader`] at offset 0 (the number of
///   keys, and whether a leaf has a head), keys growing up from
///   `KEYS_OFFSET`, and one more trailing slot than keys at the end of the
///   node: a leaf's head and values, or an internal node's edges. One formula
///   sizes both kinds, from the size of their trailing slots.
/// - Key `i` is at `KEYS_OFFSET + i * size_of::<K>()` in both kinds, so one
///   search routine serves both.
/// - Each kind is the smallest multiple of its alignment that holds
///   `MIN_CAPACITY` keys, and is aligned to 1 KB or more (D13, D15). A
///   leaf's size depends on `K` and `V`; an internal node's on `K` alone, as
///   it holds no values (D17, D21).
/// - A leaf's head is the last `V` of the node, at `HEAD_OFFSET`, and its
///   values grow down from there: value `i` is at
///   `HEAD_OFFSET - (i + 1) * size_of::<V>()`. Stored in reverse, the head
///   and values `[0, b)` are one contiguous `[V]` in reverse order, head
///   last.
/// - An internal node's `INTERNAL_CAPACITY + 1` edges fill its end.
///
/// Every invariant the node code relies on for a kind is asserted at
/// compile time when that kind's layout (`LEAF_LAYOUT` or `INTERNAL_LAYOUT`)
/// is evaluated, which happens before any page of that kind exists.
pub(crate) struct NodeLayout<K, V>(PhantomData<fn() -> (K, V)>);

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

    /// Keys start right after the header, rounded up to `K`'s alignment.
    pub(crate) const KEYS_OFFSET: usize = size_of::<NodeHeader>().next_multiple_of(align_of::<K>());

    pub(crate) const LEAF_ALIGN: usize = Self::node_align(align_of::<V>());

    /// The smallest multiple of `LEAF_ALIGN` that holds `MIN_CAPACITY` keys
    /// and values, and the head.
    pub(crate) const LEAF_SIZE: usize = Self::smallest_node_size(size_of::<V>(), Self::LEAF_ALIGN);

    pub(crate) const LEAF_CAPACITY: usize =
        Self::capacity_with_trailing_slots(Self::LEAF_SIZE, size_of::<V>());

    /// Where a leaf's head starts, and its values end.
    pub(crate) const HEAD_OFFSET: usize = Self::LEAF_SIZE - size_of::<V>();

    /// The allocation layout of every leaf's page.
    pub(crate) const LEAF_LAYOUT: Layout = {
        Self::assert_leaf_invariants();
        Self::page_layout(Self::LEAF_SIZE, Self::LEAF_ALIGN)
    };

    // Internal nodes depend on `K` and the edge, a pointer, alone: nothing
    // below uses `V`.

    const EDGE_SIZE: usize = size_of::<NodePtr<K, V>>();

    const EDGE_ALIGN: usize = align_of::<NodePtr<K, V>>();

    pub(crate) const INTERNAL_ALIGN: usize = Self::node_align(Self::EDGE_ALIGN);

    /// The smallest multiple of `INTERNAL_ALIGN` that holds `MIN_CAPACITY`
    /// keys and one more edge.
    pub(crate) const INTERNAL_SIZE: usize =
        Self::smallest_node_size(Self::EDGE_SIZE, Self::INTERNAL_ALIGN);

    pub(crate) const INTERNAL_CAPACITY: usize =
        Self::capacity_with_trailing_slots(Self::INTERNAL_SIZE, Self::EDGE_SIZE);

    pub(crate) const EDGES_OFFSET: usize =
        Self::INTERNAL_SIZE - (Self::INTERNAL_CAPACITY + 1) * Self::EDGE_SIZE;

    /// The allocation layout of every internal node's page.
    pub(crate) const INTERNAL_LAYOUT: Layout = {
        Self::assert_internal_invariants();
        Self::page_layout(Self::INTERNAL_SIZE, Self::INTERNAL_ALIGN)
    };

    const fn assert_leaf_invariants() {
        let key_size = size_of::<K>();
        let value_size = size_of::<V>();
        Self::assert_kind_invariants(Self::LEAF_SIZE, Self::LEAF_ALIGN, Self::LEAF_CAPACITY);

        // The head and values are aligned, as the node end and every
        // `size_of::<V>()` step below it are, and they never overlap the keys
        // at full capacity.
        assert!(Self::LEAF_SIZE.is_multiple_of(align_of::<V>()));
        assert!(Self::HEAD_OFFSET + value_size == Self::LEAF_SIZE);
        assert!(
            Self::KEYS_OFFSET + Self::LEAF_CAPACITY * key_size
                <= Self::HEAD_OFFSET - Self::LEAF_CAPACITY * value_size
        );
    }

    const fn assert_internal_invariants() {
        Self::assert_kind_invariants(
            Self::INTERNAL_SIZE,
            Self::INTERNAL_ALIGN,
            Self::INTERNAL_CAPACITY,
        );

        // Keys never overlap the edges at full capacity, and the edges are
        // aligned and end exactly at the node end.
        assert!(Self::KEYS_OFFSET + Self::INTERNAL_CAPACITY * size_of::<K>() <= Self::EDGES_OFFSET);
        assert!(Self::EDGES_OFFSET.is_multiple_of(Self::EDGE_ALIGN));
        assert!(
            Self::EDGES_OFFSET + (Self::INTERNAL_CAPACITY + 1) * Self::EDGE_SIZE
                == Self::INTERNAL_SIZE
        );
    }

    /// What every node kind needs: whole 1 KB steps, a capacity in bounds,
    /// and the header and keys in place.
    const fn assert_kind_invariants(node_size: usize, node_align: usize, capacity: usize) {
        assert!(node_align.is_power_of_two());
        assert!(node_size.is_multiple_of(Self::BASE_NODE_SIZE));
        assert!(node_size.is_multiple_of(node_align));

        assert!(capacity >= Self::MIN_CAPACITY);
        assert!(capacity <= Self::MAX_CAPACITY);
        assert!(Self::MAX_CAPACITY <= u16::MAX as usize);

        // The header is aligned and never overlaps the keys.
        assert!(node_align.is_multiple_of(align_of::<NodeHeader>()));
        assert!(Self::KEYS_OFFSET >= size_of::<NodeHeader>());
        assert!(Self::KEYS_OFFSET.is_multiple_of(align_of::<K>()));
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

    /// The smallest multiple of `node_align` that holds `MIN_CAPACITY` keys
    /// and one more trailing slot of `trailing_slot_size` bytes than keys.
    const fn smallest_node_size(trailing_slot_size: usize, node_align: usize) -> usize {
        let lower_bound = Self::KEYS_OFFSET
            + Self::MIN_CAPACITY * size_of::<K>()
            + (Self::MIN_CAPACITY + 1) * trailing_slot_size;
        lower_bound.next_multiple_of(node_align)
    }

    /// The most keys that fit in `node_size` when the end of the node holds
    /// one more slot of `slot_size` bytes than there are keys: the largest
    /// `n` with `KEYS_OFFSET + n·size_of K ≤ node_size − (n + 1)·slot_size`.
    const fn capacity_with_trailing_slots(node_size: usize, slot_size: usize) -> usize {
        let size_per_key = size_of::<K>() + slot_size;
        if size_per_key == 0 {
            return Self::MAX_CAPACITY;
        }
        let room = node_size.saturating_sub(Self::KEYS_OFFSET + slot_size);
        Self::min(Self::MAX_CAPACITY, room / size_per_key)
    }
}

#[cfg(test)]
mod tests {
    //! Every expected number is worked out by hand from the rules of D13,
    //! D15 and D21, with the header 4 bytes and an edge 8:
    //!
    //! - `keys_offset` is 4 rounded up to `align_of K`;
    //! - a leaf needs `keys_offset + 12·size_of K + 13·size_of V` bytes, an
    //!   internal node `keys_offset + 12·size_of K + 13·8`;
    //! - the node size is that rounded up to the node's alignment,
    //!   max(1024, align_of K, align_of V) for a leaf and
    //!   max(1024, align_of K, 8) for an internal node;
    //! - the capacity is ⌊(size − keys_offset − slot) / (size_of K + slot)⌋,
    //!   at most 1023, where the slot is `size_of V` for a leaf and 8 for an
    //!   internal node.

    use core::alloc::Layout;
    use std::string::String;
    use std::vec::Vec;

    use super::NodeLayout;
    use crate::node::test_support::{CacheLineAligned, PageAligned};

    /// The expected page of one node kind.
    struct ExpectedNode {
        size: usize,
        align: usize,
        capacity: usize,
    }

    impl ExpectedNode {
        /// A 1 KB, 1 KB-aligned node.
        fn one_page(capacity: usize) -> Self {
            Self {
                size: 1024,
                align: 1024,
                capacity,
            }
        }

        fn assert_matches(&self, layout: Layout, capacity: usize) {
            assert_eq!(
                layout,
                Layout::from_size_align(self.size, self.align).unwrap()
            );
            assert_eq!(capacity, self.capacity);
            assert!(layout.size().is_multiple_of(1024));
            assert!(layout.align() >= 1024);
        }
    }

    struct ExpectedLayout {
        keys_offset: usize,
        leaf: ExpectedNode,
        internal: ExpectedNode,
    }

    impl ExpectedLayout {
        fn assert_matches<K, V>(&self) {
            assert_eq!(NodeLayout::<K, V>::KEYS_OFFSET, self.keys_offset);
            self.leaf.assert_matches(
                NodeLayout::<K, V>::LEAF_LAYOUT,
                NodeLayout::<K, V>::LEAF_CAPACITY,
            );
            assert_eq!(
                NodeLayout::<K, V>::LEAF_SIZE,
                NodeLayout::<K, V>::LEAF_LAYOUT.size()
            );
            self.internal.assert_matches(
                NodeLayout::<K, V>::INTERNAL_LAYOUT,
                NodeLayout::<K, V>::INTERNAL_CAPACITY,
            );
            assert_eq!(
                NodeLayout::<K, V>::INTERNAL_SIZE,
                NodeLayout::<K, V>::INTERNAL_LAYOUT.size()
            );
        }
    }

    /// Values and edges are both 8 bytes, so both kinds are alike.
    #[test]
    fn u64_to_u64_fills_one_page() {
        ExpectedLayout {
            keys_offset: 8,
            // Needs 8 + 12·8 + 13·8 = 208; ⌊(1024 − 8 − 8) / 16⌋ = 63.
            leaf: ExpectedNode::one_page(63),
            // The same numbers.
            internal: ExpectedNode::one_page(63),
        }
        .assert_matches::<u64, u64>();
    }

    #[test]
    fn string_to_u64_fills_one_page() {
        ExpectedLayout {
            keys_offset: 8,
            // Needs 8 + 12·24 + 13·8 = 400; ⌊(1024 − 8 − 8) / 32⌋ = 31.
            leaf: ExpectedNode::one_page(31),
            // The same numbers.
            internal: ExpectedNode::one_page(31),
        }
        .assert_matches::<String, u64>();
    }

    /// The case D21 is about: the leaf grows to fit 12 values, and the
    /// internal node stays the one of any other `u64`-keyed map.
    #[test]
    fn large_values_grow_leaves_only() {
        ExpectedLayout {
            keys_offset: 8,
            // Needs 8 + 12·8 + 13·1000 = 13 104, over 12 KB: 13 KB.
            // ⌊(13 312 − 8 − 1000) / 1008⌋ = ⌊12 304 / 1008⌋ = 12.
            leaf: ExpectedNode {
                size: 13 * 1024,
                align: 1024,
                capacity: 12,
            },
            // Needs 8 + 12·8 + 13·8 = 208; ⌊(1024 − 8 − 8) / 16⌋ = 63.
            internal: ExpectedNode::one_page(63),
        }
        .assert_matches::<u64, [u8; 1000]>();
    }

    /// The internal layout is a function of the key type alone.
    #[test]
    fn internal_nodes_ignore_the_value_type() {
        fn assert_internal_matches_u64_to_u64<V>() {
            assert_eq!(
                NodeLayout::<u64, V>::INTERNAL_LAYOUT,
                NodeLayout::<u64, u64>::INTERNAL_LAYOUT
            );
            assert_eq!(
                NodeLayout::<u64, V>::INTERNAL_CAPACITY,
                NodeLayout::<u64, u64>::INTERNAL_CAPACITY
            );
            assert_eq!(
                NodeLayout::<u64, V>::EDGES_OFFSET,
                NodeLayout::<u64, u64>::EDGES_OFFSET
            );
        }
        assert_internal_matches_u64_to_u64::<()>();
        assert_internal_matches_u64_to_u64::<u8>();
        assert_internal_matches_u64_to_u64::<[u8; 71]>();
        assert_internal_matches_u64_to_u64::<[u8; 1000]>();
        assert_internal_matches_u64_to_u64::<CacheLineAligned>();
        assert_internal_matches_u64_to_u64::<PageAligned>();
    }

    #[test]
    fn small_values_pack_more_leaf_entries() {
        ExpectedLayout {
            keys_offset: 8,
            // Needs 8 + 12·8 + 13·1 = 117; ⌊(1024 − 8 − 1) / 9⌋ = 112.
            leaf: ExpectedNode::one_page(112),
            // Needs 8 + 12·8 + 13·8 = 208; ⌊(1024 − 8 − 8) / 16⌋ = 63.
            internal: ExpectedNode::one_page(63),
        }
        .assert_matches::<u64, u8>();
    }

    #[test]
    fn set_of_bytes_packs_tightly() {
        ExpectedLayout {
            keys_offset: 4,
            // Needs 4 + 12·1 = 16; ⌊(1024 − 4 − 0) / 1⌋ = 1020.
            leaf: ExpectedNode::one_page(1020),
            // Needs 4 + 12·1 + 13·8 = 120; ⌊(1024 − 4 − 8) / 9⌋ = 112.
            internal: ExpectedNode::one_page(112),
        }
        .assert_matches::<u8, ()>();
    }

    #[test]
    fn zero_sized_keys_and_values_hit_the_capacity_cap() {
        ExpectedLayout {
            keys_offset: 4,
            // Needs 4; entries take no room, so the cap of 1023 binds.
            leaf: ExpectedNode::one_page(1023),
            // Needs 4 + 13·8 = 108; ⌊(1024 − 4 − 8) / 8⌋ = 126.
            internal: ExpectedNode::one_page(126),
        }
        .assert_matches::<(), ()>();
    }

    #[test]
    fn over_aligned_keys_shift_the_key_array() {
        ExpectedLayout {
            keys_offset: 64,
            // Needs 64 + 12·64 + 13·8 = 936; ⌊(1024 − 64 − 8) / 72⌋ = 13.
            leaf: ExpectedNode::one_page(13),
            // The same numbers.
            internal: ExpectedNode::one_page(13),
        }
        .assert_matches::<CacheLineAligned, u64>();
    }

    /// Over-aligned keys and values: the leaf grows for the values, and the
    /// internal node is that of `CacheLineAligned → u64`.
    #[test]
    fn over_aligned_values_grow_leaves_only() {
        ExpectedLayout {
            keys_offset: 64,
            // Needs 64 + 12·64 + 13·64 = 1664: 2 KB.
            // ⌊(2048 − 64 − 64) / 128⌋ = 15.
            leaf: ExpectedNode {
                size: 2048,
                align: 1024,
                capacity: 15,
            },
            // Needs 64 + 12·64 + 13·8 = 936; ⌊(1024 − 64 − 8) / 72⌋ = 13.
            internal: ExpectedNode::one_page(13),
        }
        .assert_matches::<CacheLineAligned, CacheLineAligned>();
    }

    #[test]
    fn page_aligned_values_raise_leaf_alignment_only() {
        ExpectedLayout {
            keys_offset: 8,
            // Aligned to 8 KB. Needs 8 + 12·8 + 13·8192 = 106 600, over
            // 13·8192: 14·8192 = 114 688.
            // ⌊(114 688 − 8 − 8192) / 8200⌋ = ⌊106 488 / 8200⌋ = 12.
            leaf: ExpectedNode {
                size: 14 * 8192,
                align: 8192,
                capacity: 12,
            },
            // Needs 8 + 12·8 + 13·8 = 208; ⌊(1024 − 8 − 8) / 16⌋ = 63.
            internal: ExpectedNode::one_page(63),
        }
        .assert_matches::<u64, PageAligned>();
    }

    #[test]
    fn heap_owning_keys_and_values() {
        ExpectedLayout {
            keys_offset: 8,
            // Needs 8 + 12·24 + 13·24 = 608; ⌊(1024 − 8 − 24) / 48⌋ = 20.
            leaf: ExpectedNode::one_page(20),
            // Needs 8 + 12·24 + 13·8 = 400; ⌊(1024 − 8 − 8) / 32⌋ = 31.
            internal: ExpectedNode::one_page(31),
        }
        .assert_matches::<String, Vec<u8>>();
    }

    /// With `K = u64`, 12 entries and a head fit in a 1 KB leaf up to
    /// `size_of V = 70`.
    #[test]
    fn largest_value_that_fits_one_page() {
        ExpectedLayout {
            keys_offset: 8,
            // Needs 8 + 12·8 + 13·70 = 1014; ⌊(1024 − 8 − 70) / 78⌋ = 12.
            leaf: ExpectedNode::one_page(12),
            internal: ExpectedNode::one_page(63),
        }
        .assert_matches::<u64, [u8; 70]>();
    }

    #[test]
    fn one_value_byte_more_needs_a_two_page_leaf() {
        ExpectedLayout {
            keys_offset: 8,
            // Needs 8 + 12·8 + 13·71 = 1027: 2 KB.
            // ⌊(2048 − 8 − 71) / 79⌋ = ⌊1969 / 79⌋ = 24.
            leaf: ExpectedNode {
                size: 2048,
                align: 1024,
                capacity: 24,
            },
            internal: ExpectedNode::one_page(63),
        }
        .assert_matches::<u64, [u8; 71]>();
    }

    /// 12 keys and 13 edges fit in a 1 KB internal node up to
    /// `size_of K = 76` (with `align_of K = 1`).
    #[test]
    fn largest_key_that_fits_a_one_page_internal_node() {
        ExpectedLayout {
            keys_offset: 4,
            // Needs 4 + 12·76 = 916; ⌊(1024 − 4 − 0) / 76⌋ = 13.
            leaf: ExpectedNode::one_page(13),
            // Needs 4 + 12·76 + 13·8 = 1020; ⌊(1024 − 4 − 8) / 84⌋ = 12.
            internal: ExpectedNode::one_page(12),
        }
        .assert_matches::<[u8; 76], ()>();
    }

    /// An internal node can be the larger kind: one key byte more and it
    /// needs 2 KB, while the leaf of a set still fits in 1 KB.
    #[test]
    fn one_key_byte_more_needs_a_two_page_internal_node() {
        ExpectedLayout {
            keys_offset: 4,
            // Needs 4 + 12·77 = 928; ⌊(1024 − 4 − 0) / 77⌋ = 13.
            leaf: ExpectedNode::one_page(13),
            // Needs 4 + 12·77 + 13·8 = 1032: 2 KB.
            // ⌊(2048 − 4 − 8) / 85⌋ = ⌊2036 / 85⌋ = 23.
            internal: ExpectedNode {
                size: 2048,
                align: 1024,
                capacity: 23,
            },
        }
        .assert_matches::<[u8; 77], ()>();
    }
}
