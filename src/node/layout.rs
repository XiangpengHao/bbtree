//! Compile-time byte layout of the node pages of each kind, without and with
//! inline keys of each width.

#![forbid(unsafe_code)]

use core::alloc::Layout;
use core::marker::PhantomData;
use core::mem::{align_of, size_of};

use super::page::{InlineKeyNodeHeader, NodeHeader, NodePtr};
use crate::inline_key::{Auto, InlineWidth, InlineWord};

/// Byte layouts of the node pages for keys `K` and values `V`: for leaves
/// and for internal nodes (D9, D12, D17, D21, D29), each without an array
/// of inline keys, and with one of each [`InlineWidth`] (D18).
///
/// ```text
/// leaf      | header | k0 k1 … k_n | head v0 v1 … v_n |
/// internal  | header | k0 k1 … k_n | e0 e1 … e_n+1     |
///
/// with inline keys:
/// leaf      | header | w0 … w_n | (_ head) (k0 v0) (k1 v1) … (k_n v_n)   |
/// internal  | header | w0 … w_n | (_ e0) (k0 e1) (k1 e2) … (k_n e_n+1)  |
///           0        inline_keys_offset
/// ```
///
/// - Every layout has one shape: a header at offset 0, keys growing up, and
///   one more trailing slot than keys: a leaf's head and values, or an
///   internal node's edges. Trailing slot 0 is the head, or edge 0, and slot
///   `i + 1` goes with key `i`, the one left of it: value `i`, or the edge
///   right of key `i`. One formula sizes every layout, from the trailing
///   slot's size and alignment and whether the node has inline keys.
/// - Without inline keys, the header is a [`NodeHeader`]; the keys are an
///   array of their own, from `keys_offset`, at the same offset in both
///   kinds, and the trailing slots an array right after the keys' capacity.
/// - With inline keys, the header is an [`InlineKeyNodeHeader`], an array of
///   `capacity` inline keys of the map's width follows it, from offset 8 at
///   every width, and then the keys and trailing slots, paired in entries
///   growing up: entry 0 holds trailing slot 0 alone, its key's place
///   unused, and entry `i + 1` key `i` and trailing slot `i + 1`, each a
///   `#[repr(C)]` pair. A search reads the dense inline keys first, then the
///   key it ends at, whose value or edge is in the same entry (D29).
/// - Key `i` is at `keys_offset + i * key_stride`, and trailing slot `i` at
///   `trailing_offset + i * trailing_stride`, with strides of the slots'
///   own sizes, or the entries' with inline keys.
/// - Each kind is the smallest multiple of its alignment that holds
///   `MIN_CAPACITY` keys, and is aligned to 1 KB or more (D13, D15). A
///   leaf's size depends on `K` and `V`; an internal node's on `K` alone, as
///   it holds no values (D17, D21).
///
/// The eight layouts are constants. Every invariant the node code relies on
/// for one is asserted at compile time when that constant is evaluated,
/// which happens before any page of that layout exists. A map picks the
/// constants of its key type (`NodePtr::inline_width`, in `page.rs`), by a
/// choice that folds after monomorphization.
pub(super) struct NodeLayout<K, V>(PhantomData<fn() -> (K, V)>);

/// Where the keys of one kind of node sit, and how many fit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct KeyArea {
    /// The most keys the node holds.
    pub(super) capacity: usize,
    /// Where `capacity` inline keys start, right after the header, in a
    /// layout with inline keys.
    pub(super) inline_keys_offset: Option<usize>,
    /// Where key 0 is.
    pub(super) keys_offset: usize,
    /// Bytes from one key to the next: a key's size, or with inline keys an
    /// entry's (D29).
    pub(super) key_stride: usize,
}

impl KeyArea {
    /// The most bytes a node's inline keys take: as many as a 1 KB node has,
    /// so that only nodes whose keys and trailing slots take no room, and
    /// which are aligned past 1 KB, have their capacity capped by it. A
    /// node's inline keys fit in a buffer of this size (`prefix.rs`).
    pub(super) const MAX_INLINE_KEYS_SIZE: usize = 1024;
}

/// The layout of a leaf's page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct LeafLayout {
    pub(super) page: Layout,
    pub(super) keys: KeyArea,
    /// Where the head is, trailing slot 0; value `i` is trailing slot
    /// `i + 1`.
    pub(super) head_offset: usize,
    /// Bytes from the head to value 0, and from one value to the next.
    pub(super) value_stride: usize,
}

/// The layout of an internal node's page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct InternalLayout {
    pub(super) page: Layout,
    pub(super) keys: KeyArea,
    /// Where edge 0 is.
    pub(super) edges_offset: usize,
    /// Bytes from one edge to the next.
    pub(super) edge_stride: usize,
}

/// What a node holds after each of its keys, and once more: a leaf's values
/// and head, or an internal node's edges.
#[derive(Clone, Copy)]
struct TrailingSlot {
    size: usize,
    align: usize,
}

/// One kind's layout, before it is named a leaf's or an internal node's.
#[derive(Clone, Copy)]
struct KindLayout {
    page: Layout,
    keys: KeyArea,
    trailing_offset: usize,
    trailing_stride: usize,
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

    /// A leaf's trailing slot: a value.
    const VALUE: TrailingSlot = TrailingSlot {
        size: size_of::<V>(),
        align: align_of::<V>(),
    };

    /// An internal node's trailing slot: an edge, a pointer to a child node,
    /// the same for every strategy, which is only a marker in it.
    const EDGE: TrailingSlot = TrailingSlot {
        size: size_of::<NodePtr<K, V, Auto>>(),
        align: align_of::<NodePtr<K, V, Auto>>(),
    };

    pub(super) const PLAIN_LEAF: LeafLayout = Self::leaf_layout(None);

    pub(super) const TWO_BYTE_INLINE_LEAF: LeafLayout = Self::leaf_layout(Some(InlineWidth::Two));

    pub(super) const FOUR_BYTE_INLINE_LEAF: LeafLayout = Self::leaf_layout(Some(InlineWidth::Four));

    pub(super) const EIGHT_BYTE_INLINE_LEAF: LeafLayout =
        Self::leaf_layout(Some(InlineWidth::Eight));

    pub(super) const PLAIN_INTERNAL: InternalLayout = Self::internal_layout(None);

    pub(super) const TWO_BYTE_INLINE_INTERNAL: InternalLayout =
        Self::internal_layout(Some(InlineWidth::Two));

    pub(super) const FOUR_BYTE_INLINE_INTERNAL: InternalLayout =
        Self::internal_layout(Some(InlineWidth::Four));

    pub(super) const EIGHT_BYTE_INLINE_INTERNAL: InternalLayout =
        Self::internal_layout(Some(InlineWidth::Eight));

    /// The leaf layout without inline keys, or with inline keys of
    /// `inline_width`: one of the four constants, which `inline_width` picks
    /// after monomorphization.
    pub(super) fn leaf(inline_width: Option<InlineWidth>) -> LeafLayout {
        match inline_width {
            None => Self::PLAIN_LEAF,
            Some(InlineWidth::Two) => Self::TWO_BYTE_INLINE_LEAF,
            Some(InlineWidth::Four) => Self::FOUR_BYTE_INLINE_LEAF,
            Some(InlineWidth::Eight) => Self::EIGHT_BYTE_INLINE_LEAF,
        }
    }

    /// The internal-node layout without inline keys, or with inline keys of
    /// `inline_width`; see [`Self::leaf`].
    pub(super) fn internal(inline_width: Option<InlineWidth>) -> InternalLayout {
        match inline_width {
            None => Self::PLAIN_INTERNAL,
            Some(InlineWidth::Two) => Self::TWO_BYTE_INLINE_INTERNAL,
            Some(InlineWidth::Four) => Self::FOUR_BYTE_INLINE_INTERNAL,
            Some(InlineWidth::Eight) => Self::EIGHT_BYTE_INLINE_INTERNAL,
        }
    }

    /// Leaves are sized by their keys and values, and the head (D17).
    const fn leaf_layout(inline_width: Option<InlineWidth>) -> LeafLayout {
        let layout = Self::kind_layout(Self::inline_key_size(inline_width), Self::VALUE);
        LeafLayout {
            page: layout.page,
            keys: layout.keys,
            head_offset: layout.trailing_offset,
            value_stride: layout.trailing_stride,
        }
    }

    // Internal nodes depend on `K` and the edge, a pointer, alone: nothing
    // below uses `V`.

    /// Internal nodes are sized by their keys and one more edge.
    const fn internal_layout(inline_width: Option<InlineWidth>) -> InternalLayout {
        let layout = Self::kind_layout(Self::inline_key_size(inline_width), Self::EDGE);
        InternalLayout {
            page: layout.page,
            keys: layout.keys,
            edges_offset: layout.trailing_offset,
            edge_stride: layout.trailing_stride,
        }
    }

    /// The layout of a kind whose trailing slot is `trailing`, with inline
    /// keys of `inline_key_size` bytes, none if 0: the smallest node that
    /// holds `MIN_CAPACITY` keys, and as many keys as fit in it.
    const fn kind_layout(inline_key_size: usize, trailing: TrailingSlot) -> KindLayout {
        let align = Self::node_align(trailing.align);
        let size = Self::bytes_needed(inline_key_size, trailing, Self::MIN_CAPACITY)
            .next_multiple_of(align);
        let capacity = Self::capacity_in(size, inline_key_size, trailing);
        let keys_start = Self::keys_start(inline_key_size, capacity);
        let (keys_offset, key_stride, trailing_offset, trailing_stride) = if inline_key_size > 0 {
            let entry_size = Self::entry_size(trailing);
            let entries_offset = keys_start.next_multiple_of(Self::entry_align(trailing));
            (
                entries_offset + entry_size,
                entry_size,
                entries_offset + Self::entry_trailing_offset(trailing),
                entry_size,
            )
        } else {
            let keys_offset = keys_start.next_multiple_of(align_of::<K>());
            let trailing_offset =
                (keys_offset + capacity * size_of::<K>()).next_multiple_of(trailing.align);
            (keys_offset, size_of::<K>(), trailing_offset, trailing.size)
        };
        let layout = KindLayout {
            page: Self::page_layout(size, align),
            keys: KeyArea {
                capacity,
                inline_keys_offset: if inline_key_size > 0 {
                    Some(Self::INLINE_KEYS_OFFSET)
                } else {
                    None
                },
                keys_offset,
                key_stride,
            },
            trailing_offset,
            trailing_stride,
        };
        Self::assert_invariants(layout, inline_key_size, trailing);
        layout
    }

    /// What every layout needs: whole 1 KB steps, a capacity in bounds, the
    /// header, inline keys, keys and trailing slots in place, in that
    /// order, aligned, in the page and clear of each other.
    const fn assert_invariants(layout: KindLayout, inline_key_size: usize, trailing: TrailingSlot) {
        let (page, keys) = (layout.page, layout.keys);
        let (size, align, capacity) = (page.size(), page.align(), keys.capacity);
        assert!(align.is_power_of_two());
        assert!(size.is_multiple_of(Self::BASE_NODE_SIZE));
        assert!(size.is_multiple_of(align));

        assert!(capacity >= Self::MIN_CAPACITY);
        assert!(capacity <= Self::MAX_CAPACITY);
        assert!(Self::MAX_CAPACITY <= u16::MAX as usize);

        // The header is aligned and never overlaps what follows it.
        assert!(align.is_multiple_of(align_of::<NodeHeader>()));
        let keys_start = Self::keys_start(inline_key_size, capacity);
        match keys.inline_keys_offset {
            None => assert!(keys_start == size_of::<NodeHeader>()),
            Some(inline_keys_offset) => {
                assert!(align.is_multiple_of(align_of::<InlineKeyNodeHeader>()));
                assert!(inline_keys_offset >= size_of::<InlineKeyNodeHeader>());
                // Aligned for inline keys of every width.
                assert!(align.is_multiple_of(align_of::<InlineWord>()));
                assert!(inline_keys_offset.is_multiple_of(align_of::<InlineWord>()));
                assert!(inline_key_size <= size_of::<InlineWord>());
                assert!(capacity * inline_key_size <= KeyArea::MAX_INLINE_KEYS_SIZE);
                assert!(inline_keys_offset + capacity * inline_key_size == keys_start);
            }
        }

        // Every key and trailing slot is aligned, as the node and the steps
        // between them are.
        let (key_stride, trailing_stride) = (keys.key_stride, layout.trailing_stride);
        assert!(align.is_multiple_of(align_of::<K>()));
        assert!(align.is_multiple_of(trailing.align));
        assert!(keys.keys_offset.is_multiple_of(align_of::<K>()));
        assert!(key_stride >= size_of::<K>() && key_stride.is_multiple_of(align_of::<K>()));
        assert!(layout.trailing_offset.is_multiple_of(trailing.align));
        assert!(trailing_stride >= trailing.size && trailing_stride.is_multiple_of(trailing.align));

        // The last key's stride, and the last trailing slot, end in the page.
        assert!(keys.keys_offset + capacity * key_stride <= size);
        assert!(layout.trailing_offset + capacity * trailing_stride + trailing.size <= size);

        if keys.inline_keys_offset.is_some() {
            // Entries after the inline keys: trailing slot 0 alone in entry
            // 0, then key `i` and trailing slot `i + 1` in entry `i + 1`, the
            // key first, the slot after it.
            let slot_offset = Self::entry_trailing_offset(trailing);
            let entries_offset = layout.trailing_offset - slot_offset;
            assert!(keys_start <= entries_offset);
            assert!(key_stride == Self::entry_size(trailing) && trailing_stride == key_stride);
            assert!(keys.keys_offset == entries_offset + key_stride);
            assert!(slot_offset >= size_of::<K>());
            assert!(slot_offset + trailing.size <= key_stride);
        } else {
            // The keys, then the trailing slots, each an array of its own.
            assert!(key_stride == size_of::<K>() && trailing_stride == trailing.size);
            assert!(keys_start <= keys.keys_offset);
            assert!(keys.keys_offset + capacity * size_of::<K>() <= layout.trailing_offset);
        }
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

    /// The bytes of each inline key that a node with inline keys of
    /// `inline_width` stores; 0 for a node without.
    const fn inline_key_size(inline_width: Option<InlineWidth>) -> usize {
        match inline_width {
            Some(width) => width.bytes(),
            None => 0,
        }
    }

    /// Where the keys, or the entries, could start, before rounding up to
    /// their alignment: after the header, and the inline keys if any.
    const fn keys_start(inline_key_size: usize, capacity: usize) -> usize {
        if inline_key_size > 0 {
            Self::INLINE_KEYS_OFFSET + capacity * inline_key_size
        } else {
            size_of::<NodeHeader>()
        }
    }

    /// Where an entry's trailing slot is: after its key, aligned.
    const fn entry_trailing_offset(trailing: TrailingSlot) -> usize {
        size_of::<K>().next_multiple_of(trailing.align)
    }

    /// An entry's alignment: its key's and its trailing slot's.
    const fn entry_align(trailing: TrailingSlot) -> usize {
        Self::max(align_of::<K>(), trailing.align)
    }

    /// An entry's size: its key, then its trailing slot, as `#[repr(C)]`
    /// lays out a pair of them.
    const fn entry_size(trailing: TrailingSlot) -> usize {
        (Self::entry_trailing_offset(trailing) + trailing.size)
            .next_multiple_of(Self::entry_align(trailing))
    }

    /// The bytes a node of `capacity` keys needs: the header, the inline
    /// keys if any, the keys, and one more trailing slot than keys; with
    /// inline keys, paired in entries, the first entry's key unused.
    const fn bytes_needed(
        inline_key_size: usize,
        trailing: TrailingSlot,
        capacity: usize,
    ) -> usize {
        let keys_start = Self::keys_start(inline_key_size, capacity);
        if inline_key_size > 0 {
            keys_start.next_multiple_of(Self::entry_align(trailing))
                + (capacity + 1) * Self::entry_size(trailing)
        } else {
            (keys_start.next_multiple_of(align_of::<K>()) + capacity * size_of::<K>())
                .next_multiple_of(trailing.align)
                + (capacity + 1) * trailing.size
        }
    }

    /// The most keys a node may hold: `MAX_CAPACITY`, and with inline keys,
    /// as many as `MAX_INLINE_KEYS_SIZE` holds.
    const fn max_capacity(inline_key_size: usize) -> usize {
        match KeyArea::MAX_INLINE_KEYS_SIZE.checked_div(inline_key_size) {
            Some(inline_keys_bound) => Self::min(Self::MAX_CAPACITY, inline_keys_bound),
            None => Self::MAX_CAPACITY,
        }
    }

    /// The largest capacity, up to [`Self::max_capacity`], whose
    /// `bytes_needed` fit in `node_size` bytes.
    const fn capacity_in(
        node_size: usize,
        inline_key_size: usize,
        trailing: TrailingSlot,
    ) -> usize {
        let max_capacity = Self::max_capacity(inline_key_size);
        // An upper bound: every key takes at least its inline key, and its
        // key and trailing slot, or its entry. Keys that take no room fit up
        // to the cap.
        let size_per_key = inline_key_size
            + if inline_key_size > 0 {
                Self::entry_size(trailing)
            } else {
                size_of::<K>() + trailing.size
            };
        let mut capacity = match node_size.checked_div(size_per_key) {
            Some(bound) => Self::min(max_capacity, bound),
            None => max_capacity,
        };
        while Self::bytes_needed(inline_key_size, trailing, capacity) > node_size {
            capacity -= 1;
        }
        capacity
    }
}

#[cfg(test)]
mod tests {
    //! Every expected number is worked out by hand from the rules of D13,
    //! D15, D18, D21 and D29, with an edge 8 bytes and an inline key `w`, 2,
    //! 4 or 8 bytes; a node's trailing slot is a value in a leaf and an edge
    //! in an internal node:
    //!
    //! - without inline keys, the header is 4 bytes, the keys start at 4
    //!   rounded up to `align_of K`, and the trailing slots right after `n`
    //!   keys, rounded up to their alignment: a node needs
    //!   `keys_offset + n·size_of K`, so rounded, plus `(n + 1)·slot`;
    //! - with inline keys, the header is 6 bytes, the inline keys start at 8,
    //!   and the entries at `8 + w·n` rounded up to their alignment, the
    //!   larger of the key's and the slot's: an entry is the key, then the
    //!   slot aligned to its alignment, rounded up to the entry's alignment,
    //!   and a node needs `n + 1` of them, entry 0 holding trailing slot 0
    //!   after its unused key, so key 0 is in entry 1;
    //! - the node size is what 12 keys need, rounded up to the node's
    //!   alignment, max(1024, align_of K, align_of V) for a leaf and
    //!   max(1024, align_of K, 8) for an internal node;
    //! - the capacity is the largest `n` that fits, at most 1023, and with
    //!   inline keys at most `1024 / w`.

    use core::alloc::Layout;
    use std::string::String;
    use std::vec::Vec;

    use super::{InternalLayout, KeyArea, LeafLayout, NodeLayout};
    use crate::inline_key::InlineWidth;
    use crate::node::test_support::{CacheLineAligned, PageAligned};

    /// The expected page of one node kind.
    struct ExpectedNode {
        size: usize,
        align: usize,
        capacity: usize,
        keys_offset: usize,
        /// Where trailing slot 0 is: the head, or edge 0.
        trailing_offset: usize,
    }

    impl ExpectedNode {
        /// A 1 KB, 1 KB-aligned node.
        fn one_page(capacity: usize, keys_offset: usize, trailing_offset: usize) -> Self {
            Self {
                size: 1024,
                align: 1024,
                capacity,
                keys_offset,
                trailing_offset,
            }
        }

        /// Checks one kind's layout, with its trailing slots at
        /// `trailing.0`, `trailing.1` bytes apart, against these numbers and
        /// `strides`, the keys' and the trailing slots'.
        fn assert_matches(
            &self,
            page: Layout,
            keys: KeyArea,
            trailing: (usize, usize),
            strides: (usize, usize),
            inline_keys: bool,
        ) {
            assert_eq!(
                page,
                Layout::from_size_align(self.size, self.align).unwrap()
            );
            assert_eq!(keys.capacity, self.capacity);
            assert_eq!(keys.keys_offset, self.keys_offset);
            assert_eq!(trailing.0, self.trailing_offset);
            assert_eq!((keys.key_stride, trailing.1), strides);
            assert_eq!(keys.inline_keys_offset, inline_keys.then_some(8));
            assert!(page.size().is_multiple_of(1024));
            assert!(page.align() >= 1024);
        }

        fn assert_leaf(&self, layout: LeafLayout, strides: (usize, usize), inline_keys: bool) {
            let trailing = (layout.head_offset, layout.value_stride);
            self.assert_matches(layout.page, layout.keys, trailing, strides, inline_keys);
        }

        fn assert_internal(
            &self,
            layout: InternalLayout,
            strides: (usize, usize),
            inline_keys: bool,
        ) {
            let trailing = (layout.edges_offset, layout.edge_stride);
            self.assert_matches(layout.page, layout.keys, trailing, strides, inline_keys);
        }
    }

    /// The strides the rules give for keys `K` and trailing slots `Slot`:
    /// their own sizes, or with inline keys an entry's, for both.
    fn strides<K, Slot>(inline_keys: bool) -> (usize, usize) {
        if inline_keys {
            let slot_offset = size_of::<K>().next_multiple_of(align_of::<Slot>());
            let entry_align = align_of::<K>().max(align_of::<Slot>());
            let entry = (slot_offset + size_of::<Slot>()).next_multiple_of(entry_align);
            (entry, entry)
        } else {
            (size_of::<K>(), size_of::<Slot>())
        }
    }

    /// An edge's size and alignment, for [`strides`].
    type Edge = usize;

    struct ExpectedLayout {
        leaf: ExpectedNode,
        internal: ExpectedNode,
    }

    impl ExpectedLayout {
        /// Checks the layouts without inline keys.
        fn assert_plain<K, V>(&self) {
            assert_eq!(self.leaf.keys_offset, self.internal.keys_offset);
            self.leaf.assert_leaf(
                NodeLayout::<K, V>::PLAIN_LEAF,
                strides::<K, V>(false),
                false,
            );
            self.internal.assert_internal(
                NodeLayout::<K, V>::PLAIN_INTERNAL,
                strides::<K, Edge>(false),
                false,
            );
            assert_eq!(
                NodeLayout::<K, V>::leaf(None),
                NodeLayout::<K, V>::PLAIN_LEAF
            );
            assert_eq!(
                NodeLayout::<K, V>::internal(None),
                NodeLayout::<K, V>::PLAIN_INTERNAL
            );
        }

        /// Checks the layouts with inline keys of `width`.
        fn assert_inline<K, V>(&self, width: InlineWidth) {
            self.leaf.assert_leaf(
                NodeLayout::<K, V>::leaf(Some(width)),
                strides::<K, V>(true),
                true,
            );
            self.internal.assert_internal(
                NodeLayout::<K, V>::internal(Some(width)),
                strides::<K, Edge>(true),
                true,
            );
        }
    }

    /// Values and edges are both 8 bytes, so both kinds are alike.
    #[test]
    fn u64_to_u64_fills_one_page() {
        ExpectedLayout {
            // Needs 8 + 12·8 + 13·8 = 208; ⌊(1024 − 8 − 8) / 16⌋ = 63; the
            // head after the keys, at 8 + 63·8 = 512.
            leaf: ExpectedNode::one_page(63, 8, 512),
            // The same numbers.
            internal: ExpectedNode::one_page(63, 8, 512),
        }
        .assert_plain::<u64, u64>();
    }

    #[test]
    fn string_to_u64_fills_one_page() {
        ExpectedLayout {
            // Needs 8 + 12·24 + 13·8 = 400; ⌊(1024 − 8 − 8) / 32⌋ = 31; the
            // head at 8 + 31·24 = 752.
            leaf: ExpectedNode::one_page(31, 8, 752),
            // The same numbers.
            internal: ExpectedNode::one_page(31, 8, 752),
        }
        .assert_plain::<String, u64>();
    }

    /// The layout `String` keys get by default (D18, D29): entries of
    /// 24 + 8 = 32 bytes, so 24 keys instead of 31.
    #[test]
    fn string_to_u64_with_inline_keys() {
        ExpectedLayout {
            // 8 + 8n + 32(n + 1) ≤ 1024: 40n ≤ 984, n = 24; entries at
            // 8 + 24·8 = 200, the head 24 bytes into entry 0, key 0 at 232.
            leaf: ExpectedNode::one_page(24, 232, 224),
            // The same numbers, an edge in place of a value.
            internal: ExpectedNode::one_page(24, 232, 224),
        }
        .assert_inline::<String, u64>(InlineWidth::Eight);
    }

    /// `Vec<u8>` is 24 bytes and 8-aligned, like `String`.
    #[test]
    fn byte_vec_to_u64_with_inline_keys() {
        ExpectedLayout {
            leaf: ExpectedNode::one_page(24, 232, 224),
            internal: ExpectedNode::one_page(24, 232, 224),
        }
        .assert_inline::<Vec<u8>, u64>(InlineWidth::Eight);
    }

    /// Large values cut the leaf's capacity, not the internal node's.
    #[test]
    fn string_to_string_with_inline_keys() {
        ExpectedLayout {
            // Entries of 24 + 24 = 48: 8 + 8n + 48(n + 1) ≤ 1024: 56n ≤ 968,
            // n = 17; entries at 8 + 17·8 = 144, the head at 168, key 0 at
            // 192.
            leaf: ExpectedNode::one_page(17, 192, 168),
            // As for `String` → `u64`.
            internal: ExpectedNode::one_page(24, 232, 224),
        }
        .assert_inline::<String, String>(InlineWidth::Eight);
    }

    /// Unaligned keys: the slot after each pads to its own alignment.
    #[test]
    fn byte_array_keys_with_inline_keys() {
        ExpectedLayout {
            // Entries of 32 + 8 = 40: 8 + 8n + 40(n + 1) ≤ 1024: 48n ≤ 976,
            // n = 20; entries at 8 + 20·8 = 168, the head at 200, key 0 at
            // 208.
            leaf: ExpectedNode::one_page(20, 208, 200),
            internal: ExpectedNode::one_page(20, 208, 200),
        }
        .assert_inline::<[u8; 32], u64>(InlineWidth::Eight);
    }

    /// Over-aligned keys pad each entry to their alignment.
    #[test]
    fn over_aligned_keys_with_inline_keys() {
        // Entries of 64 + 8 → 128 bytes: 12 keys need (8 + 96 → 128) +
        // 13·128 = 1792, so 2 KB. (8 + 8n → 128) + 128(n + 1) ≤ 2048 at
        // n = 14; entries at 128, the head at 192, key 0 at 256.
        let expected = || ExpectedNode {
            size: 2048,
            align: 1024,
            capacity: 14,
            keys_offset: 256,
            trailing_offset: 192,
        };
        ExpectedLayout {
            leaf: expected(),
            internal: expected(),
        }
        .assert_inline::<CacheLineAligned, u64>(InlineWidth::Eight);
    }

    /// Small keys: the inline keys outweigh them, and in an internal node
    /// each entry pads its key to the edge's alignment.
    #[test]
    fn set_of_bytes_with_inline_keys() {
        ExpectedLayout {
            // Entries of 1 byte, the slot taking none: 8 + 8n + (n + 1) ≤
            // 1024: 9n ≤ 1015, n = 112; entries at 8 + 896 = 904, the head
            // and key 0 at 905.
            leaf: ExpectedNode::one_page(112, 905, 905),
            // Entries of 1 → 8 + 8 = 16: 8 + 8n + 16(n + 1) ≤ 1024:
            // 24n ≤ 1000, n = 41; entries at 8 + 328 = 336, edge 0 at 344,
            // key 0 at 352.
            internal: ExpectedNode::one_page(41, 352, 344),
        }
        .assert_inline::<u8, ()>(InlineWidth::Eight);
    }

    /// The layout `u64` keys get with their inline keys: entries of 16
    /// bytes after 2 bytes each of inline keys, so 55 keys instead of 63.
    #[test]
    fn u64_to_u64_with_two_byte_inline_keys() {
        ExpectedLayout {
            // (8 + 2n → 8) + 16(n + 1) ≤ 1024: 1016 at n = 55, 1032 at 56;
            // entries at 120, the head at 128, key 0 at 136.
            leaf: ExpectedNode::one_page(55, 136, 128),
            internal: ExpectedNode::one_page(55, 136, 128),
        }
        .assert_inline::<u64, u64>(InlineWidth::Two);
    }

    /// 16-aligned keys pad each entry to 32 bytes.
    #[test]
    fn u128_to_u64_with_two_byte_inline_keys() {
        ExpectedLayout {
            // Entries of 16 + 8 → 32: (8 + 2n → 16) + 32(n + 1) ≤ 1024: 992
            // at n = 28, 1040 at 29; entries at 64, the head at 80, key 0 at
            // 96.
            leaf: ExpectedNode::one_page(28, 96, 80),
            internal: ExpectedNode::one_page(28, 96, 80),
        }
        .assert_inline::<u128, u64>(InlineWidth::Two);
    }

    /// A set: the leaf has no values, and its entries are its keys.
    #[test]
    fn set_of_i64_with_two_byte_inline_keys() {
        ExpectedLayout {
            // Entries of 8: (8 + 2n → 8) + 8(n + 1) ≤ 1024: 1016 at n = 100,
            // 1032 at 101; entries at 208, the head and key 0 at 216.
            leaf: ExpectedNode::one_page(100, 216, 216),
            // As for `u64` → `u64`.
            internal: ExpectedNode::one_page(55, 136, 128),
        }
        .assert_inline::<i64, ()>(InlineWidth::Two);
    }

    #[test]
    fn u32_to_u32_with_four_byte_inline_keys() {
        ExpectedLayout {
            // Entries of 4 + 4 = 8: 8 + 4n + 8(n + 1) ≤ 1024: 12n ≤ 1008,
            // n = 84; entries at 8 + 336 = 344, the head at 348, key 0 at
            // 352.
            leaf: ExpectedNode::one_page(84, 352, 348),
            // Entries of 4 → 8 + 8 = 16: (8 + 4n → 8) + 16(n + 1) ≤ 1024:
            // 1024 at n = 50, 1048 at 51; entries at 208, edge 0 at 216,
            // key 0 at 224.
            internal: ExpectedNode::one_page(50, 224, 216),
        }
        .assert_inline::<u32, u32>(InlineWidth::Four);
    }

    /// No room past 1 KB aligned.
    #[repr(align(2048))]
    struct ZeroSizedPastOneKb;

    /// Inline keys take at most 1 KB, which only binds for keys that take
    /// no room and are aligned past 1 KB, whose nodes are larger than 1 KB.
    #[test]
    fn inline_keys_take_at_most_one_kb() {
        ExpectedLayout {
            // A 2 KB leaf of entries of no size: 1020 keys' inline keys fit,
            // but 1024 / 2 = 512 is the cap; the entries, the head and the
            // keys start at 8 + 1024 → 2048.
            leaf: ExpectedNode {
                size: 2048,
                align: 2048,
                capacity: 512,
                keys_offset: 2048,
                trailing_offset: 2048,
            },
            // Entries of an edge each, padded to the key's 2 KB alignment:
            // 12 keys need 2048 + 13·2048 bytes, so 28 KB; entries at 2048,
            // edge 0 there, key 0 at 4096.
            internal: ExpectedNode {
                size: 28 * 1024,
                align: 2048,
                capacity: 12,
                keys_offset: 4096,
                trailing_offset: 2048,
            },
        }
        .assert_inline::<ZeroSizedPastOneKb, ()>(InlineWidth::Two);
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
                NodeLayout::<u64, V>::TWO_BYTE_INLINE_INTERNAL,
                NodeLayout::<u64, u64>::TWO_BYTE_INLINE_INTERNAL
            );
            assert_eq!(
                NodeLayout::<u64, V>::EIGHT_BYTE_INLINE_INTERNAL,
                NodeLayout::<u64, u64>::EIGHT_BYTE_INLINE_INTERNAL
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
            // ⌊(13 312 − 8 − 1000) / 1008⌋ = ⌊12 304 / 1008⌋ = 12; the head
            // at 8 + 12·8 = 104.
            leaf: ExpectedNode {
                size: 13 * 1024,
                align: 1024,
                capacity: 12,
                keys_offset: 8,
                trailing_offset: 104,
            },
            // Needs 8 + 12·8 + 13·8 = 208; ⌊(1024 − 8 − 8) / 16⌋ = 63.
            internal: ExpectedNode::one_page(63, 8, 512),
        }
        .assert_plain::<u64, [u8; 1000]>();
    }

    #[test]
    fn small_values_pack_more_leaf_entries() {
        ExpectedLayout {
            // Needs 8 + 12·8 + 13·1 = 117; ⌊(1024 − 8 − 1) / 9⌋ = 112; the
            // head at 8 + 112·8 = 904.
            leaf: ExpectedNode::one_page(112, 8, 904),
            // Needs 8 + 12·8 + 13·8 = 208; ⌊(1024 − 8 − 8) / 16⌋ = 63.
            internal: ExpectedNode::one_page(63, 8, 512),
        }
        .assert_plain::<u64, u8>();
    }

    #[test]
    fn set_of_bytes_packs_tightly() {
        ExpectedLayout {
            // Needs 4 + 12·1 = 16; ⌊(1024 − 4 − 0) / 1⌋ = 1020; the head, of
            // no size, at 1024.
            leaf: ExpectedNode::one_page(1020, 4, 1024),
            // (4 + n → 8) + 8(n + 1) ≤ 1024: 1024 at n = 112, 1032 at 113;
            // edge 0 at 116 → 120.
            internal: ExpectedNode::one_page(112, 4, 120),
        }
        .assert_plain::<u8, ()>();
    }

    #[test]
    fn zero_sized_keys_and_values_hit_the_capacity_cap() {
        ExpectedLayout {
            // Needs 4; entries take no room, so the cap of 1023 binds.
            leaf: ExpectedNode::one_page(1023, 4, 4),
            // Edges from 4 → 8: 8 + 8(n + 1) ≤ 1024, n = 126.
            internal: ExpectedNode::one_page(126, 4, 8),
        }
        .assert_plain::<(), ()>();
    }

    #[test]
    fn over_aligned_keys_shift_the_key_array() {
        ExpectedLayout {
            // Needs 64 + 12·64 + 13·8 = 936; ⌊(1024 − 64 − 8) / 72⌋ = 13;
            // the head at 64 + 13·64 = 896.
            leaf: ExpectedNode::one_page(13, 64, 896),
            // The same numbers.
            internal: ExpectedNode::one_page(13, 64, 896),
        }
        .assert_plain::<CacheLineAligned, u64>();
    }

    /// Over-aligned keys and values: the leaf grows for the values, and the
    /// internal node is that of `CacheLineAligned → u64`.
    #[test]
    fn over_aligned_values_grow_leaves_only() {
        ExpectedLayout {
            // Needs 64 + 12·64 + 13·64 = 1664: 2 KB.
            // ⌊(2048 − 64 − 64) / 128⌋ = 15; the head at 64 + 15·64 = 1024.
            leaf: ExpectedNode {
                size: 2048,
                align: 1024,
                capacity: 15,
                keys_offset: 64,
                trailing_offset: 1024,
            },
            // Needs 64 + 12·64 + 13·8 = 936; ⌊(1024 − 64 − 8) / 72⌋ = 13.
            internal: ExpectedNode::one_page(13, 64, 896),
        }
        .assert_plain::<CacheLineAligned, CacheLineAligned>();
    }

    #[test]
    fn page_aligned_values_raise_leaf_alignment_only() {
        ExpectedLayout {
            // Aligned to 8 KB. Needs (8 + 12·8 → 8192) + 13·8192 = 114 688:
            // 14·8192, which holds 12 keys and no more; the head at 8192.
            leaf: ExpectedNode {
                size: 14 * 8192,
                align: 8192,
                capacity: 12,
                keys_offset: 8,
                trailing_offset: 8192,
            },
            // Needs 8 + 12·8 + 13·8 = 208; ⌊(1024 − 8 − 8) / 16⌋ = 63.
            internal: ExpectedNode::one_page(63, 8, 512),
        }
        .assert_plain::<u64, PageAligned>();
    }

    #[test]
    fn heap_owning_keys_and_values() {
        ExpectedLayout {
            // Needs 8 + 12·24 + 13·24 = 608; ⌊(1024 − 8 − 24) / 48⌋ = 20;
            // the head at 8 + 20·24 = 488.
            leaf: ExpectedNode::one_page(20, 8, 488),
            // Needs 8 + 12·24 + 13·8 = 400; ⌊(1024 − 8 − 8) / 32⌋ = 31.
            internal: ExpectedNode::one_page(31, 8, 752),
        }
        .assert_plain::<String, Vec<u8>>();
    }

    /// With `K = u64`, 12 entries and a head fit in a 1 KB leaf up to
    /// `size_of V = 70`.
    #[test]
    fn largest_value_that_fits_one_page() {
        ExpectedLayout {
            // Needs 8 + 12·8 + 13·70 = 1014; ⌊(1024 − 8 − 70) / 78⌋ = 12;
            // the head at 8 + 12·8 = 104.
            leaf: ExpectedNode::one_page(12, 8, 104),
            internal: ExpectedNode::one_page(63, 8, 512),
        }
        .assert_plain::<u64, [u8; 70]>();
    }

    #[test]
    fn one_value_byte_more_needs_a_two_page_leaf() {
        ExpectedLayout {
            // Needs 8 + 12·8 + 13·71 = 1027: 2 KB.
            // ⌊(2048 − 8 − 71) / 79⌋ = ⌊1969 / 79⌋ = 24; the head at
            // 8 + 24·8 = 200.
            leaf: ExpectedNode {
                size: 2048,
                align: 1024,
                capacity: 24,
                keys_offset: 8,
                trailing_offset: 200,
            },
            internal: ExpectedNode::one_page(63, 8, 512),
        }
        .assert_plain::<u64, [u8; 71]>();
    }

    /// 12 keys and 13 edges fit in a 1 KB internal node up to
    /// `size_of K = 76` (with `align_of K = 1`).
    #[test]
    fn largest_key_that_fits_a_one_page_internal_node() {
        ExpectedLayout {
            // Needs 4 + 12·76 = 916; ⌊(1024 − 4 − 0) / 76⌋ = 13; the head,
            // of no size, at 4 + 13·76 = 992.
            leaf: ExpectedNode::one_page(13, 4, 992),
            // Needs (4 + 12·76 → 920) + 13·8 = 1024, and 13 keys more: 12;
            // edge 0 at 920.
            internal: ExpectedNode::one_page(12, 4, 920),
        }
        .assert_plain::<[u8; 76], ()>();
    }

    /// An internal node can be the larger kind: one key byte more and it
    /// needs 2 KB, while the leaf of a set still fits in 1 KB.
    #[test]
    fn one_key_byte_more_needs_a_two_page_internal_node() {
        ExpectedLayout {
            // Needs 4 + 12·77 = 928; ⌊(1024 − 4 − 0) / 77⌋ = 13; the head at
            // 4 + 13·77 = 1005.
            leaf: ExpectedNode::one_page(13, 4, 1005),
            // Needs (4 + 12·77 → 928) + 13·8 = 1032: 2 KB. (4 + 77n → 8) +
            // 8(n + 1) ≤ 2048: 1968 at n = 23, 2056 at 24; edge 0 at
            // 4 + 23·77 = 1775 → 1776.
            internal: ExpectedNode {
                size: 2048,
                align: 1024,
                capacity: 23,
                keys_offset: 4,
                trailing_offset: 1776,
            },
        }
        .assert_plain::<[u8; 77], ()>();
    }
}
