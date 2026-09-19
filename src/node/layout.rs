//! Compile-time byte layout of a node page.

#![forbid(unsafe_code)]

use core::alloc::Layout;
use core::marker::PhantomData;
use core::mem::{align_of, size_of};

use super::page::NodePtr;

/// Byte layout of a node page for keys `K` and values `V`.
///
/// ```text
/// leaf      | len | k0 k1 … →              free              ← … v1 v0 |
/// internal  | len | k0 k1 … →        free        ← … v1 v0 | e0 … e_cap |
///           0     KEYS_OFFSET                   values end  EDGES_OFFSET  NODE_SIZE
/// ```
///
/// - The header is `len: u16` at offset 0 and nothing else.
/// - Key `i` is at `KEYS_OFFSET + i * size_of::<K>()`.
/// - Value `i` is at `values_end - (i + 1) * size_of::<V>()`, where values end
///   at `LEAF_VALUES_END` in a leaf and `INTERNAL_VALUES_END` in an internal
///   node. Values are stored in reverse, so values `[a, b)` are one
///   contiguous `[V]` in reverse order.
/// - Internal nodes keep `INTERNAL_CAPACITY + 1` edges at the end of the node.
/// - Leaves and internal nodes share one `NODE_SIZE` and one `LAYOUT`, so
///   deallocation never needs the node kind.
///
/// Every invariant the node code relies on is asserted at compile time when
/// `LAYOUT` is evaluated, which happens before any page of this layout exists.
pub(crate) struct NodeLayout<K, V>(PhantomData<fn() -> (K, V)>);

impl<K, V> NodeLayout<K, V> {
    /// Every node is a multiple of this size and at least this aligned (D6, D7).
    const PAGE_SIZE: usize = 4096;

    /// Every node kind holds at least this many entries (D13); nodes grow by
    /// whole `NODE_ALIGN` steps until it fits.
    const MIN_CAPACITY: usize = 12;

    /// Upper bound on any node's capacity, so that an index (`0..=capacity`)
    /// fits in the 12 low bits that page alignment leaves free. Only binds
    /// when keys and values are zero-sized.
    const MAX_CAPACITY: usize = 4095;

    const EDGE_SIZE: usize = size_of::<NodePtr<K, V>>();

    /// Keys start right after the `len: u16` header, rounded up to `K`'s alignment.
    pub(crate) const KEYS_OFFSET: usize = size_of::<u16>().next_multiple_of(align_of::<K>());

    pub(crate) const NODE_ALIGN: usize =
        Self::max(Self::PAGE_SIZE, Self::max(align_of::<K>(), align_of::<V>()));

    /// The smallest multiple of `NODE_ALIGN` in which both node kinds hold at
    /// least `MIN_CAPACITY` keys.
    pub(crate) const NODE_SIZE: usize = Self::smallest_node_size();

    pub(crate) const LEAF_CAPACITY: usize = Self::leaf_capacity(Self::NODE_SIZE);

    pub(crate) const INTERNAL_CAPACITY: usize = Self::internal_capacity(Self::NODE_SIZE);

    pub(crate) const LEAF_VALUES_END: usize = Self::NODE_SIZE;

    pub(crate) const EDGES_OFFSET: usize =
        Self::NODE_SIZE - (Self::INTERNAL_CAPACITY + 1) * Self::EDGE_SIZE;

    pub(crate) const INTERNAL_VALUES_END: usize =
        Self::round_down(Self::EDGES_OFFSET, align_of::<V>());

    /// The allocation layout of every node page.
    pub(crate) const LAYOUT: Layout = {
        Self::assert_invariants();
        match Layout::from_size_align(Self::NODE_SIZE, Self::NODE_ALIGN) {
            Ok(layout) => layout,
            Err(_) => panic!("node size overflows isize"),
        }
    };

    const fn assert_invariants() {
        let key_size = size_of::<K>();
        let value_size = size_of::<V>();

        assert!(Self::NODE_ALIGN.is_power_of_two());
        assert!(Self::NODE_SIZE.is_multiple_of(Self::PAGE_SIZE));
        assert!(Self::NODE_SIZE.is_multiple_of(Self::NODE_ALIGN));

        assert!(Self::LEAF_CAPACITY >= Self::MIN_CAPACITY);
        assert!(Self::INTERNAL_CAPACITY >= Self::MIN_CAPACITY);
        assert!(Self::LEAF_CAPACITY <= Self::MAX_CAPACITY);
        assert!(Self::INTERNAL_CAPACITY <= Self::MAX_CAPACITY);
        assert!(Self::MAX_CAPACITY <= u16::MAX as usize);

        // The `len` header is aligned and never overlaps the keys.
        assert!(Self::NODE_ALIGN.is_multiple_of(align_of::<u16>()));
        assert!(Self::KEYS_OFFSET >= size_of::<u16>());
        assert!(Self::KEYS_OFFSET.is_multiple_of(align_of::<K>()));

        // Values are aligned: each values end is a multiple of `align_of::<V>()`,
        // and so is `size_of::<V>()`.
        assert!(Self::LEAF_VALUES_END.is_multiple_of(align_of::<V>()));
        assert!(Self::INTERNAL_VALUES_END.is_multiple_of(align_of::<V>()));

        // Leaf: keys and values never overlap at full capacity.
        assert!(
            Self::KEYS_OFFSET + Self::LEAF_CAPACITY * key_size
                <= Self::LEAF_VALUES_END - Self::LEAF_CAPACITY * value_size
        );

        // Internal: keys, values and edges never overlap at full capacity.
        assert!(
            Self::KEYS_OFFSET + Self::INTERNAL_CAPACITY * key_size
                <= Self::INTERNAL_VALUES_END - Self::INTERNAL_CAPACITY * value_size
        );
        assert!(Self::INTERNAL_VALUES_END <= Self::EDGES_OFFSET);
        assert!(Self::EDGES_OFFSET.is_multiple_of(align_of::<NodePtr<K, V>>()));
        assert!(
            Self::EDGES_OFFSET + (Self::INTERNAL_CAPACITY + 1) * Self::EDGE_SIZE == Self::NODE_SIZE
        );
    }

    const fn max(a: usize, b: usize) -> usize {
        if a > b { a } else { b }
    }

    const fn min(a: usize, b: usize) -> usize {
        if a < b { a } else { b }
    }

    /// `offset` rounded down to a multiple of `align` (a power of two).
    const fn round_down(offset: usize, align: usize) -> usize {
        offset & !(align - 1)
    }

    const fn smallest_node_size() -> usize {
        let entry_size = size_of::<K>() + size_of::<V>();
        // Both kinds need `MIN_CAPACITY` entries; an internal node also needs
        // `MIN_CAPACITY + 1` edges. Alignment slack is resolved by the loop.
        let lower_bound = Self::KEYS_OFFSET
            + Self::MIN_CAPACITY * entry_size
            + (Self::MIN_CAPACITY + 1) * Self::EDGE_SIZE;
        let mut node_size = lower_bound.next_multiple_of(Self::NODE_ALIGN);
        while Self::leaf_capacity(node_size) < Self::MIN_CAPACITY
            || Self::internal_capacity(node_size) < Self::MIN_CAPACITY
        {
            node_size += Self::NODE_ALIGN;
        }
        node_size
    }

    /// The largest `n` with `KEYS_OFFSET + n·size_of K ≤ node_size − n·size_of V`.
    const fn leaf_capacity(node_size: usize) -> usize {
        let entry_size = size_of::<K>() + size_of::<V>();
        if entry_size == 0 {
            return Self::MAX_CAPACITY;
        }
        Self::min(
            Self::MAX_CAPACITY,
            node_size.saturating_sub(Self::KEYS_OFFSET) / entry_size,
        )
    }

    /// The largest `n` whose keys, values and `n + 1` edges fit in `node_size`.
    const fn internal_capacity(node_size: usize) -> usize {
        let entry_size = size_of::<K>() + size_of::<V>() + Self::EDGE_SIZE;
        // Upper bound ignoring the extra edge and the values-end rounding;
        // `internal_fits` is monotone in `n`, so count down to the first fit.
        let mut capacity = Self::min(
            Self::MAX_CAPACITY,
            node_size.saturating_sub(Self::KEYS_OFFSET) / entry_size,
        );
        while capacity > 0 && !Self::internal_fits(node_size, capacity) {
            capacity -= 1;
        }
        capacity
    }

    const fn internal_fits(node_size: usize, capacity: usize) -> bool {
        let edges_size = (capacity + 1) * Self::EDGE_SIZE;
        if edges_size > node_size {
            return false;
        }
        let values_end = Self::round_down(node_size - edges_size, align_of::<V>());
        let values_size = capacity * size_of::<V>();
        if values_size > values_end {
            return false;
        }
        Self::KEYS_OFFSET + capacity * size_of::<K>() <= values_end - values_size
    }
}

#[cfg(test)]
mod tests {
    use core::alloc::Layout;
    use std::string::String;
    use std::vec::Vec;

    use super::NodeLayout;

    #[repr(align(64))]
    struct CacheLineAligned(#[expect(dead_code, reason = "only its layout matters")] u8);

    struct ExpectedLayout {
        keys_offset: usize,
        leaf_capacity: usize,
        internal_capacity: usize,
        node_size: usize,
    }

    impl ExpectedLayout {
        fn assert_matches<K, V>(&self) {
            assert_eq!(NodeLayout::<K, V>::KEYS_OFFSET, self.keys_offset);
            assert_eq!(NodeLayout::<K, V>::LEAF_CAPACITY, self.leaf_capacity);
            assert_eq!(
                NodeLayout::<K, V>::INTERNAL_CAPACITY,
                self.internal_capacity
            );
            assert_eq!(NodeLayout::<K, V>::NODE_SIZE, self.node_size);
            assert_eq!(
                NodeLayout::<K, V>::LAYOUT,
                Layout::from_size_align(self.node_size, NodeLayout::<K, V>::NODE_ALIGN).unwrap()
            );
            assert!(NodeLayout::<K, V>::NODE_SIZE.is_multiple_of(4096));
            assert!(NodeLayout::<K, V>::NODE_ALIGN >= 4096);
        }
    }

    #[test]
    fn u64_to_u64_fills_one_page() {
        ExpectedLayout {
            keys_offset: 8,
            leaf_capacity: 255,
            internal_capacity: 170,
            node_size: 4096,
        }
        .assert_matches::<u64, u64>();
    }

    #[test]
    fn set_of_bytes_packs_tightly() {
        ExpectedLayout {
            keys_offset: 2,
            leaf_capacity: 4094,
            internal_capacity: 454,
            node_size: 4096,
        }
        .assert_matches::<u8, ()>();
    }

    #[test]
    fn zero_sized_keys_and_values_hit_the_capacity_cap() {
        ExpectedLayout {
            keys_offset: 2,
            leaf_capacity: 4095,
            internal_capacity: 510,
            node_size: 4096,
        }
        .assert_matches::<(), ()>();
    }

    #[test]
    fn large_values_get_multi_page_nodes() {
        ExpectedLayout {
            keys_offset: 8,
            leaf_capacity: 12,
            internal_capacity: 12,
            node_size: 12288,
        }
        .assert_matches::<u64, [u8; 1000]>();
    }

    #[test]
    fn over_aligned_keys_shift_the_key_array() {
        ExpectedLayout {
            keys_offset: 64,
            leaf_capacity: 56,
            internal_capacity: 50,
            node_size: 4096,
        }
        .assert_matches::<CacheLineAligned, u64>();
    }

    #[test]
    fn heap_owning_keys_and_values() {
        ExpectedLayout {
            keys_offset: 8,
            leaf_capacity: 85,
            internal_capacity: 72,
            node_size: 4096,
        }
        .assert_matches::<String, Vec<u8>>();
    }

    /// With `K = u64`, the internal node is the binding kind: 12 entries plus
    /// 13 edges fit in 4 KB up to `size_of K + size_of V = 332`.
    #[test]
    fn largest_entry_that_fits_one_page() {
        ExpectedLayout {
            keys_offset: 8,
            leaf_capacity: 12,
            internal_capacity: 12,
            node_size: 4096,
        }
        .assert_matches::<u64, [u8; 324]>();
    }

    #[test]
    fn one_byte_more_needs_two_pages() {
        ExpectedLayout {
            keys_offset: 8,
            leaf_capacity: 24,
            internal_capacity: 23,
            node_size: 8192,
        }
        .assert_matches::<u64, [u8; 325]>();
    }

    #[test]
    fn page_aligned_values_raise_node_alignment() {
        #[repr(align(8192))]
        struct PageAligned(#[expect(dead_code, reason = "only its layout matters")] u8);

        assert_eq!(NodeLayout::<u64, PageAligned>::NODE_ALIGN, 8192);
        ExpectedLayout {
            keys_offset: 8,
            leaf_capacity: 13,
            internal_capacity: 12,
            node_size: 114_688,
        }
        .assert_matches::<u64, PageAligned>();
    }
}
