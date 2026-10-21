//! Compile-time byte layout of the node pages of each kind, without and with
//! inline keys of each width.

#![forbid(unsafe_code)]

use core::alloc::Layout;
use core::marker::PhantomData;
use core::mem::{align_of, size_of};

use super::page::{InlineKeyNodeHeader, LatchedNodeHeader, NodeHeader, NodePtr};
use crate::inline_key::{Auto, InlineWidth, InlineWord};
use crate::node_config::NodeConfig;

/// Byte layouts of the node pages for keys `K` and values `V`, with the page
/// sizes of node config `C` (D59) and leaf annex `L` (`leaf_annex.rs`): for
/// leaves and for internal nodes (D9, D12, D17, D21, D29), each without an
/// array of inline keys, and with one of each [`InlineWidth`] (D18).
///
/// ```text
/// leaf      | header | k0 k1 … k_n | head v0 v1 … v_n |
/// internal  | header | k0 k1 … k_n | e0 e1 … e_n+1     |
///
/// with inline keys:
/// leaf      | header | w0 … w_n | (hk head) (k0 v0) (k1 v1) … (k_n v_n) |
/// internal  | header | w0 … w_n | e0 e1 … e_n+1                         |
///           0        inline_keys_offset
/// ```
///
/// - Every layout has a header at offset 0, and one more trailing slot than
///   keys: a leaf's head and values, or an internal node's edges. Trailing
///   slot 0 is the head, or edge 0, and slot `i + 1` goes with key `i`, the
///   one left of it: value `i`, or the edge right of key `i`. A common
///   formula sizes each layout from slot sizes, alignment and inline keys.
/// - Without inline keys, the header is a [`NodeHeader`]; the keys are an
///   array of their own, from `keys_offset`, at the same offset in both
///   kinds, and the trailing slots an array right after the keys' capacity.
/// - With inline keys, the header is an [`InlineKeyNodeHeader`]. An array of
///   `capacity` inline keys of the kind's width follows it, from offset 8 at
///   every width (16 in a latched tree, [`LatchedNodeHeader`]). Leaves pair
///   their keys and values, each pair a `#[repr(C)]` entry. Entry 0 holds
///   the head, the value of the separator left of the leaf, and that
///   separator's key (`hk`, D49); entry `i + 1` holds key `i` and value `i`.
///   A leaf search reads the dense inline keys first, then the key it ends
///   at, whose value is in the same entry (D29). Power-of-two entries no
///   larger than a cache line start at a multiple of their size, so each
///   pair stays within one cache line. Internal nodes keep no keys, only
///   inline keys and edges. Their search reads an edge after the inline
///   keys, and a separator's key, a head below, only on a tie (D49).
/// - A leaf whose plan keeps hash tags has a byte per key right after its
///   inline keys, where its layout has room for them (D36).
/// - Key `i` is at `keys_offset + i * key_stride`, and trailing slot `i` at
///   `trailing_offset + i * trailing_stride`, with strides of the slots'
///   own sizes, or the entries' with inline keys.
/// - Each kind is the smallest multiple of its alignment that holds
///   `MIN_CAPACITY` keys, and is aligned to it: a multiple of its page size
///   in `C`, by default 1 KiB, or 512 B for an internal node that keeps no
///   keys (D13, D15, D49). A leaf's size depends on `K` and `V`; an
///   internal node's on `K` alone, as it holds no values (D17, D21).
///
/// The layouts are constants. Every invariant the node code relies on
/// for one is asserted at compile time when that constant is evaluated,
/// which happens before any page of that layout exists. A map picks the
/// constants its node plans call for (`node_plan.rs`), by a choice that
/// folds after monomorphization.
pub(in crate::tree) struct NodeLayout<K, V, C, L = ()> {
    _types: PhantomData<fn() -> (K, V)>,
    _config: PhantomData<fn() -> C>,
    _annex: PhantomData<fn() -> L>,
}

/// Where the keys of one kind of node sit, and how many fit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tree) struct KeyArea {
    /// The most keys the node holds.
    pub(in crate::tree) capacity: usize,
    /// Where `capacity` inline keys start, right after the header, in a
    /// layout with inline keys.
    pub(in crate::tree) inline_keys_offset: Option<usize>,
    /// Where key 0 is; in an internal node that keeps no keys (D49), where
    /// they would start, which nothing reads.
    pub(in crate::tree) keys_offset: usize,
    /// Bytes from one key to the next: a key's size, or with inline keys an
    /// entry's (D29); 0 in an internal node that keeps no keys.
    pub(in crate::tree) key_stride: usize,
}

/// The layout of a leaf's page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tree) struct LeafLayout {
    pub(in crate::tree) page: Layout,
    pub(in crate::tree) keys: KeyArea,
    /// Where the head is, trailing slot 0; value `i` is trailing slot
    /// `i + 1`.
    pub(in crate::tree) head_offset: usize,
    /// Bytes from the head to value 0, and from one value to the next.
    pub(in crate::tree) value_stride: usize,
    /// Where a dense array of `u8` hash tags is, for a leaf whose plan
    /// keeps them and whose layout has room: in the padding before entry 0,
    /// or past it at the cost of an entry (D36); see
    /// [`NodeLayout::compute_leaf`].
    pub(in crate::tree) hash_tags_offset: Option<usize>,
    /// Where the head's key is, in entry 0 before the head, in a leaf with
    /// inline keys: the key of the separator left of the leaf (D49).
    pub(in crate::tree) head_key_offset: Option<usize>,
}

impl LeafLayout {
    /// Where value 0 is: a stride past the head.
    pub(in crate::tree) const fn values_offset(self) -> usize {
        self.head_offset + self.value_stride
    }
}

/// The layout of an internal node's page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tree) struct InternalLayout {
    pub(in crate::tree) page: Layout,
    pub(in crate::tree) keys: KeyArea,
    /// Where edge 0 is.
    pub(in crate::tree) edges_offset: usize,
    /// Bytes from one edge to the next.
    pub(in crate::tree) edge_stride: usize,
}

/// What a node holds after each of its keys, and once more: a leaf's values
/// and head, or an internal node's edges.
#[derive(Clone, Copy)]
struct TrailingSlot {
    size: usize,
    align: usize,
    /// Inline-key leaves pair keys with values; internal edges stay dense.
    pair_with_key: bool,
}

impl TrailingSlot {
    const fn paired(self, inline_key_size: usize) -> bool {
        self.pair_with_key && inline_key_size > 0
    }
}

/// What a node's header holds besides its plain fields (`page.rs`): a
/// latch word, in a tree that threads share ([`LatchedNodeHeader`]); and,
/// right after the node header, a leaf's annex, in a map that keeps one
/// (`leaf_annex.rs`), of layout `annex`: of size 0 in every other node.
#[derive(Clone, Copy)]
pub(super) struct Header {
    latched: bool,
    annex: Layout,
}

impl Header {
    /// An internal node's: no annex.
    const fn internal(latched: bool) -> Self {
        Self {
            latched,
            annex: Layout::new::<()>(),
        }
    }

    /// A leaf's, with an annex of layout `annex`.
    pub(super) const fn leaf(latched: bool, annex: Layout) -> Self {
        Self { latched, annex }
    }

    /// The node header's size.
    const fn node_header_size(self) -> usize {
        if self.latched {
            size_of::<LatchedNodeHeader>()
        } else {
            size_of::<InlineKeyNodeHeader>()
        }
    }

    /// Where the annex starts: after the node header, aligned.
    pub(super) const fn annex_offset(self) -> usize {
        self.node_header_size().next_multiple_of(self.annex.align())
    }

    /// The whole header's size, its annex's included.
    const fn size(self) -> usize {
        if self.annex.size() == 0 {
            self.node_header_size()
        } else {
            self.annex_offset() + self.annex.size()
        }
    }
}

/// One kind's layout, before it is named a leaf's or an internal node's.
#[derive(Clone, Copy)]
struct KindLayout {
    page: Layout,
    keys: KeyArea,
    trailing_offset: usize,
    trailing_stride: usize,
}

impl<K, V, C: NodeConfig, L> NodeLayout<K, V, C, L> {
    /// Every leaf is a multiple of this size, and aligned to it or more
    /// (D15). Leaves of at least 1 KiB bound how deep a tree grows
    /// (`MAX_DEPTH`).
    const LEAF_PAGE_SIZE: usize = Self::page_size(C::LEAF_PAGE_SIZE, 1024);

    /// Every internal node that keeps no keys (D49) is a multiple of this
    /// size, and aligned to it. By default 512 B: half the bytes hold as
    /// many children as 1 KiB holds with keys, or more, and a search scans
    /// their inline keys whole (06, lesson 1).
    const INTERNAL_PAGE_SIZE: usize = Self::page_size(C::INTERNAL_PAGE_SIZE, Self::CACHE_LINE_SIZE);

    /// Every internal node that keeps its keys (D17) is a multiple of this
    /// size, and aligned to it or more.
    const PLAIN_INTERNAL_PAGE_SIZE: usize =
        Self::page_size(C::PLAIN_INTERNAL_PAGE_SIZE, Self::CACHE_LINE_SIZE);

    /// Entries up to this size start at a multiple of it, so that none
    /// crosses a cache line (D34).
    const CACHE_LINE_SIZE: usize = 64;

    /// Every node kind holds at least this many keys (D13); nodes grow by
    /// whole allocation-size steps until it fits.
    const MIN_CAPACITY: usize = 12;

    /// Upper bound on any node's capacity. Indexes use `u16`; this tighter
    /// bound keeps the search work bounded in nodes of nearly zero-sized
    /// slots.
    const MAX_CAPACITY: usize = 1023;

    /// `size`, a page size of the config, once checked: a power of two, and
    /// at least `min`.
    const fn page_size(size: usize, min: usize) -> usize {
        assert!(
            size.is_power_of_two() && size >= min,
            "a NodeConfig page size is a power of two, at least its minimum"
        );
        size
    }

    /// Where the inline keys start in a node that has them: after its
    /// header, which in a latched tree holds a latch word too
    /// ([`LatchedNodeHeader`]), and in a leaf its annex.
    const fn inline_keys_offset(header: Header) -> usize {
        header.size().next_multiple_of(align_of::<InlineWord>())
    }

    /// A leaf's trailing slot: a value.
    const VALUE: TrailingSlot = TrailingSlot {
        size: size_of::<V>(),
        align: align_of::<V>(),
        pair_with_key: true,
    };

    /// An internal node's trailing slot: an edge, a pointer to a child node,
    /// the same for every strategy, which is only a marker in it.
    const EDGE: TrailingSlot = TrailingSlot {
        size: size_of::<NodePtr<K, V, Auto>>(),
        align: align_of::<NodePtr<K, V, Auto>>(),
        pair_with_key: false,
    };

    pub(in crate::tree) const PLAIN_LEAF: LeafLayout =
        Self::compute_leaf(None, false, Self::leaf_header(false));

    pub(super) const TWO_BYTE_INLINE_LEAF: LeafLayout =
        Self::compute_leaf(Some(InlineWidth::Two), false, Self::leaf_header(false));

    pub(super) const FOUR_BYTE_INLINE_LEAF: LeafLayout =
        Self::compute_leaf(Some(InlineWidth::Four), false, Self::leaf_header(false));

    pub(super) const EIGHT_BYTE_INLINE_LEAF: LeafLayout =
        Self::compute_leaf(Some(InlineWidth::Eight), false, Self::leaf_header(false));

    pub(super) const TWO_BYTE_INLINE_HASH_TAGGED_LEAF: LeafLayout =
        Self::compute_leaf(Some(InlineWidth::Two), true, Self::leaf_header(false));

    pub(super) const FOUR_BYTE_INLINE_HASH_TAGGED_LEAF: LeafLayout =
        Self::compute_leaf(Some(InlineWidth::Four), true, Self::leaf_header(false));

    pub(super) const EIGHT_BYTE_INLINE_HASH_TAGGED_LEAF: LeafLayout =
        Self::compute_leaf(Some(InlineWidth::Eight), true, Self::leaf_header(false));

    pub(in crate::tree) const PLAIN_INTERNAL: InternalLayout =
        Self::compute_internal(None, Header::internal(false));

    pub(super) const TWO_BYTE_INLINE_INTERNAL: InternalLayout =
        Self::compute_internal(Some(InlineWidth::Two), Header::internal(false));

    pub(super) const FOUR_BYTE_INLINE_INTERNAL: InternalLayout =
        Self::compute_internal(Some(InlineWidth::Four), Header::internal(false));

    pub(super) const EIGHT_BYTE_INLINE_INTERNAL: InternalLayout =
        Self::compute_internal(Some(InlineWidth::Eight), Header::internal(false));

    // A latched tree's nodes, whose keys have inline keys: internal nodes
    // read optimistically must not keep keys (`tree/node/latch.rs`).

    pub(super) const LATCHED_TWO_BYTE_INLINE_LEAF: LeafLayout =
        Self::compute_leaf(Some(InlineWidth::Two), false, Self::leaf_header(true));

    pub(super) const LATCHED_FOUR_BYTE_INLINE_LEAF: LeafLayout =
        Self::compute_leaf(Some(InlineWidth::Four), false, Self::leaf_header(true));

    pub(super) const LATCHED_EIGHT_BYTE_INLINE_LEAF: LeafLayout =
        Self::compute_leaf(Some(InlineWidth::Eight), false, Self::leaf_header(true));

    pub(super) const LATCHED_TWO_BYTE_INLINE_HASH_TAGGED_LEAF: LeafLayout =
        Self::compute_leaf(Some(InlineWidth::Two), true, Self::leaf_header(true));

    pub(super) const LATCHED_FOUR_BYTE_INLINE_HASH_TAGGED_LEAF: LeafLayout =
        Self::compute_leaf(Some(InlineWidth::Four), true, Self::leaf_header(true));

    pub(super) const LATCHED_EIGHT_BYTE_INLINE_HASH_TAGGED_LEAF: LeafLayout =
        Self::compute_leaf(Some(InlineWidth::Eight), true, Self::leaf_header(true));

    pub(super) const LATCHED_TWO_BYTE_INLINE_INTERNAL: InternalLayout =
        Self::compute_internal(Some(InlineWidth::Two), Header::internal(true));

    pub(super) const LATCHED_FOUR_BYTE_INLINE_INTERNAL: InternalLayout =
        Self::compute_internal(Some(InlineWidth::Four), Header::internal(true));

    pub(super) const LATCHED_EIGHT_BYTE_INLINE_INTERNAL: InternalLayout =
        Self::compute_internal(Some(InlineWidth::Eight), Header::internal(true));

    /// A leaf's header: a latch word and the annex if `latched`, as only a
    /// latched tree's leaves keep an annex (`page.rs`).
    const fn leaf_header(latched: bool) -> Header {
        let annex = if latched {
            Layout::new::<L>()
        } else {
            Layout::new::<()>()
        };
        Header::leaf(latched, annex)
    }

    /// The leaf layout constant for these choices, which the node plan and
    /// the strategy pick after monomorphization:
    /// - without inline keys, or with inline keys of `inline_width`;
    /// - with room for hash tags if `hash_tags` and there is room (D36);
    /// - with a latch word in its header if `latched`.
    ///
    /// Only leaves with inline keys keep hash tags (`node_plan.rs`), and
    /// only nodes with inline keys are latched.
    #[inline(always)]
    pub(super) fn leaf(
        inline_width: Option<InlineWidth>,
        hash_tags: bool,
        latched: bool,
    ) -> LeafLayout {
        match (inline_width, hash_tags, latched) {
            (None, _, false) => Self::PLAIN_LEAF,
            (Some(InlineWidth::Two), false, false) => Self::TWO_BYTE_INLINE_LEAF,
            (Some(InlineWidth::Four), false, false) => Self::FOUR_BYTE_INLINE_LEAF,
            (Some(InlineWidth::Eight), false, false) => Self::EIGHT_BYTE_INLINE_LEAF,
            (Some(InlineWidth::Two), true, false) => Self::TWO_BYTE_INLINE_HASH_TAGGED_LEAF,
            (Some(InlineWidth::Four), true, false) => Self::FOUR_BYTE_INLINE_HASH_TAGGED_LEAF,
            (Some(InlineWidth::Eight), true, false) => Self::EIGHT_BYTE_INLINE_HASH_TAGGED_LEAF,
            (Some(InlineWidth::Two), false, true) => Self::LATCHED_TWO_BYTE_INLINE_LEAF,
            (Some(InlineWidth::Four), false, true) => Self::LATCHED_FOUR_BYTE_INLINE_LEAF,
            (Some(InlineWidth::Eight), false, true) => Self::LATCHED_EIGHT_BYTE_INLINE_LEAF,
            (Some(InlineWidth::Two), true, true) => Self::LATCHED_TWO_BYTE_INLINE_HASH_TAGGED_LEAF,
            (Some(InlineWidth::Four), true, true) => {
                Self::LATCHED_FOUR_BYTE_INLINE_HASH_TAGGED_LEAF
            }
            (Some(InlineWidth::Eight), true, true) => {
                Self::LATCHED_EIGHT_BYTE_INLINE_HASH_TAGGED_LEAF
            }
            (None, _, true) => unreachable!("a latched tree's keys have inline keys"),
        }
    }

    /// The internal layout constant without inline keys, or with inline keys
    /// of `inline_width` and no keys (D49), with a latch word in its header
    /// if `latched`. The node plan and the strategy pick it after
    /// monomorphization.
    #[inline(always)]
    pub(super) fn internal(inline_width: Option<InlineWidth>, latched: bool) -> InternalLayout {
        match (inline_width, latched) {
            (None, false) => Self::PLAIN_INTERNAL,
            (Some(InlineWidth::Two), false) => Self::TWO_BYTE_INLINE_INTERNAL,
            (Some(InlineWidth::Four), false) => Self::FOUR_BYTE_INLINE_INTERNAL,
            (Some(InlineWidth::Eight), false) => Self::EIGHT_BYTE_INLINE_INTERNAL,
            (Some(InlineWidth::Two), true) => Self::LATCHED_TWO_BYTE_INLINE_INTERNAL,
            (Some(InlineWidth::Four), true) => Self::LATCHED_FOUR_BYTE_INLINE_INTERNAL,
            (Some(InlineWidth::Eight), true) => Self::LATCHED_EIGHT_BYTE_INLINE_INTERNAL,
            (None, true) => unreachable!("a latched tree's keys have inline keys"),
        }
    }

    /// Leaves are sized by their keys and values, and the head (D17).
    ///
    /// With `hash_tags`, a leaf keeps a tag per key where the tags fit in
    /// the padding before its entries, as in many leaves (D36). Where they
    /// fit only by also taking the head key's place (D49), between the
    /// padding and the head's value, the leaf gives up an entry to keep
    /// them. Its capacity is then sized with the tags beside the inline keys.
    /// Otherwise it keeps none.
    const fn compute_leaf(
        inline_width: Option<InlineWidth>,
        hash_tags: bool,
        header: Header,
    ) -> LeafLayout {
        let inline_key_size = Self::inline_key_size(inline_width);
        let layout = Self::kind_layout(
            Self::LEAF_PAGE_SIZE,
            header,
            inline_key_size,
            0,
            Self::VALUE,
            true,
        );
        if inline_key_size == 0 || !hash_tags {
            return Self::leaf_of(layout, None);
        }
        let tags_start = Self::columns_end(header, inline_key_size, layout.keys.capacity);
        let tags_end = tags_start + layout.keys.capacity * size_of::<u8>();
        if tags_end <= Self::entries_offset(layout) {
            Self::leaf_of(layout, Some(tags_start))
        } else if tags_end <= layout.trailing_offset {
            let hash_tagged = Self::kind_layout(
                Self::LEAF_PAGE_SIZE,
                header,
                inline_key_size,
                size_of::<u8>(),
                Self::VALUE,
                true,
            );
            Self::leaf_of(
                hash_tagged,
                Some(Self::columns_end(
                    header,
                    inline_key_size,
                    hash_tagged.keys.capacity,
                )),
            )
        } else {
            Self::leaf_of(layout, None)
        }
    }

    /// A leaf of `layout`, with its hash tags at `hash_tags_offset`.
    const fn leaf_of(layout: KindLayout, hash_tags_offset: Option<usize>) -> LeafLayout {
        let head_key_offset = if layout.keys.inline_keys_offset.is_some() {
            Some(Self::entries_offset(layout))
        } else {
            None
        };
        LeafLayout {
            page: layout.page,
            keys: layout.keys,
            head_offset: layout.trailing_offset,
            value_stride: layout.trailing_stride,
            hash_tags_offset,
            head_key_offset,
        }
    }

    /// Where entry 0 is in a leaf of paired entries: its key's place, which
    /// the head's key takes (D49), then the head.
    const fn entries_offset(layout: KindLayout) -> usize {
        layout.trailing_offset - Self::entry_trailing_offset(Self::VALUE)
    }

    // Internal nodes depend on `K` and the edge, a pointer, alone: nothing
    // below uses `V`.

    /// Internal nodes are sized by their keys, if they keep them, and their
    /// edges, one more than keys. With inline keys they keep no keys (D49).
    const fn compute_internal(inline_width: Option<InlineWidth>, header: Header) -> InternalLayout {
        let inline_key_size = Self::inline_key_size(inline_width);
        let keeps_keys = inline_key_size == 0;
        let page_size = if keeps_keys {
            Self::PLAIN_INTERNAL_PAGE_SIZE
        } else {
            Self::INTERNAL_PAGE_SIZE
        };
        let layout = Self::kind_layout(
            page_size,
            header,
            inline_key_size,
            0,
            Self::EDGE,
            keeps_keys,
        );
        InternalLayout {
            page: layout.page,
            keys: layout.keys,
            edges_offset: layout.trailing_offset,
            edge_stride: layout.trailing_stride,
        }
    }

    /// The smallest node, in steps of `page_size`, that holds
    /// `MIN_CAPACITY` keys, with as many keys as fit in it, for a kind whose
    /// trailing slot is `trailing`. It has inline keys of `inline_key_size`
    /// bytes (none if 0), `hash_tag_size` more bytes of hash tags per key
    /// beside them, its keys if `keeps_keys`, and a latch word in its header
    /// if `latched`.
    const fn kind_layout(
        page_size: usize,
        header: Header,
        inline_key_size: usize,
        hash_tag_size: usize,
        trailing: TrailingSlot,
        keeps_keys: bool,
    ) -> KindLayout {
        let column_size = inline_key_size + hash_tag_size;
        let align = Self::node_align(page_size, trailing.align, keeps_keys);
        let size = Self::bytes_needed(
            header,
            column_size,
            trailing,
            keeps_keys,
            Self::MIN_CAPACITY,
        )
        .next_multiple_of(align);
        let capacity = Self::capacity_in(
            header,
            size,
            Self::max_capacity(page_size, inline_key_size),
            column_size,
            trailing,
            keeps_keys,
        );
        let keys_start = Self::columns_end(header, column_size, capacity);
        let (keys_offset, key_stride, trailing_offset, trailing_stride) =
            if trailing.paired(inline_key_size) {
                let entry_size = Self::entry_size(trailing);
                // The page and the complete entries both occupy multiples
                // of this alignment, so rounding their start up still fits
                // the same capacity. It only uses the unused page tail.
                let entries_align =
                    if entry_size.is_power_of_two() && entry_size <= Self::CACHE_LINE_SIZE {
                        entry_size
                    } else {
                        Self::entry_align(trailing)
                    };
                let entries_offset = keys_start.next_multiple_of(entries_align);
                (
                    entries_offset + entry_size,
                    entry_size,
                    entries_offset + Self::entry_trailing_offset(trailing),
                    entry_size,
                )
            } else if keeps_keys {
                let keys_offset = keys_start.next_multiple_of(align_of::<K>());
                let trailing_offset =
                    (keys_offset + capacity * size_of::<K>()).next_multiple_of(trailing.align);
                (keys_offset, size_of::<K>(), trailing_offset, trailing.size)
            } else {
                // No keys (D49): the trailing slots follow the inline keys.
                (
                    keys_start,
                    0,
                    keys_start.next_multiple_of(trailing.align),
                    trailing.size,
                )
            };
        let layout = KindLayout {
            page: Self::page_layout(size, align),
            keys: KeyArea {
                capacity,
                inline_keys_offset: if inline_key_size > 0 {
                    Some(Self::inline_keys_offset(header))
                } else {
                    None
                },
                keys_offset,
                key_stride,
            },
            trailing_offset,
            trailing_stride,
        };
        Self::assert_invariants(
            layout,
            page_size,
            header,
            inline_key_size,
            column_size,
            trailing,
            keeps_keys,
        );
        layout
    }

    /// What every layout needs: whole pages of `page_size`, a capacity in
    /// bounds, the header, inline keys and hash tags, keys and trailing slots
    /// in place, in that order, aligned, in the page and clear of each other.
    const fn assert_invariants(
        layout: KindLayout,
        page_size: usize,
        header: Header,
        inline_key_size: usize,
        column_size: usize,
        trailing: TrailingSlot,
        keeps_keys: bool,
    ) {
        let (page, keys) = (layout.page, layout.keys);
        let (size, align, capacity) = (page.size(), page.align(), keys.capacity);
        assert!(align.is_power_of_two());
        assert!(size.is_multiple_of(page_size));
        assert!(size.is_multiple_of(align));

        assert!(capacity >= Self::MIN_CAPACITY);
        assert!(capacity <= Self::MAX_CAPACITY);
        assert!(Self::MAX_CAPACITY <= u16::MAX as usize);

        // The header is aligned and never overlaps what follows it.
        assert!(align.is_multiple_of(align_of::<NodeHeader>()));
        let keys_start = Self::columns_end(header, column_size, capacity);
        match keys.inline_keys_offset {
            None => assert!(
                keys_start == size_of::<NodeHeader>()
                    && !header.latched
                    && header.annex.size() == 0
            ),
            Some(inline_keys_offset) => {
                assert!(align.is_multiple_of(align_of::<InlineKeyNodeHeader>()));
                assert!(inline_keys_offset >= size_of::<InlineKeyNodeHeader>());
                if header.latched {
                    assert!(align.is_multiple_of(align_of::<LatchedNodeHeader>()));
                    assert!(inline_keys_offset >= size_of::<LatchedNodeHeader>());
                }
                if header.annex.size() > 0 {
                    // The annex after the node header, aligned.
                    let annex = header.annex_offset();
                    assert!(align.is_multiple_of(header.annex.align()));
                    assert!(annex.is_multiple_of(header.annex.align()));
                    assert!(annex >= header.node_header_size());
                    assert!(inline_keys_offset >= header.size());
                }
                // Aligned for inline keys of every width.
                assert!(align.is_multiple_of(align_of::<InlineWord>()));
                assert!(inline_keys_offset.is_multiple_of(align_of::<InlineWord>()));
                assert!(inline_key_size <= size_of::<InlineWord>());
                assert!(capacity * inline_key_size <= Self::max_inline_keys_size(page_size));
                assert!(inline_keys_offset + capacity * column_size == keys_start);
            }
        }

        // Every key and trailing slot is aligned, as the node and the steps
        // between them are.
        let (key_stride, trailing_stride) = (keys.key_stride, layout.trailing_stride);
        assert!(align.is_multiple_of(trailing.align));
        assert!(layout.trailing_offset.is_multiple_of(trailing.align));
        assert!(trailing_stride >= trailing.size && trailing_stride.is_multiple_of(trailing.align));
        if keeps_keys {
            assert!(align.is_multiple_of(align_of::<K>()));
            assert!(keys.keys_offset.is_multiple_of(align_of::<K>()));
            assert!(key_stride >= size_of::<K>() && key_stride.is_multiple_of(align_of::<K>()));
        } else {
            assert!(key_stride == 0 && keys.inline_keys_offset.is_some());
        }

        // The last key's stride, and the last trailing slot, end in the page.
        assert!(keys.keys_offset + capacity * key_stride <= size);
        assert!(layout.trailing_offset + capacity * trailing_stride + trailing.size <= size);

        if trailing.paired(inline_key_size) {
            // Entries after the inline keys: trailing slot 0 in entry 0,
            // after its key's place, which a leaf's head key takes (D49),
            // then key `i` and trailing slot `i + 1` in entry `i + 1`, the
            // key first, the slot after it.
            let slot_offset = Self::entry_trailing_offset(trailing);
            let entries_offset = layout.trailing_offset - slot_offset;
            assert!(keys_start <= entries_offset);
            assert!(key_stride == Self::entry_size(trailing) && trailing_stride == key_stride);
            assert!(keys.keys_offset == entries_offset + key_stride);
            assert!(slot_offset >= size_of::<K>());
            assert!(slot_offset + trailing.size <= key_stride);
            if key_stride.is_power_of_two() && key_stride <= Self::CACHE_LINE_SIZE {
                assert!(keys.keys_offset.is_multiple_of(key_stride));
            }
        } else if keeps_keys {
            // The keys, then the trailing slots, each an array of its own.
            assert!(key_stride == size_of::<K>() && trailing_stride == trailing.size);
            assert!(keys_start <= keys.keys_offset);
            assert!(keys.keys_offset + capacity * size_of::<K>() <= layout.trailing_offset);
        } else {
            // No keys: the trailing slots after the inline keys.
            assert!(trailing_stride == trailing.size);
            assert!(keys_start <= layout.trailing_offset);
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

    /// A node is aligned to its page size (D15, D49), and to its keys, if it
    /// keeps them, and trailing slots.
    const fn node_align(page_size: usize, trailing_slot_align: usize, keeps_keys: bool) -> usize {
        let key_align = if keeps_keys { align_of::<K>() } else { 1 };
        Self::max(page_size, Self::max(key_align, trailing_slot_align))
    }

    /// The bytes of each inline key that a node with inline keys of
    /// `inline_width` stores; 0 for a node without.
    const fn inline_key_size(inline_width: Option<InlineWidth>) -> usize {
        match inline_width {
            Some(width) => width.byte_len(),
            None => 0,
        }
    }

    /// Where the keys, or the entries, could start, before rounding up to
    /// their alignment: after the header and the arrays beside the keys, if
    /// any (the inline keys and hash tags, `column_size` bytes per key).
    const fn columns_end(header: Header, column_size: usize, capacity: usize) -> usize {
        if column_size > 0 {
            Self::inline_keys_offset(header) + capacity * column_size
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

    /// The minimum bytes a node of `capacity` keys needs, before using tail
    /// slack to align whole pairs. That is the header, the arrays beside the
    /// keys (`column_size` bytes per key), its keys if `keeps_keys`, and one
    /// more trailing slot than keys. With inline keys, a leaf pairs keys and
    /// trailing slots in entries, and the first entry's key is the head's
    /// (D49).
    const fn bytes_needed(
        header: Header,
        column_size: usize,
        trailing: TrailingSlot,
        keeps_keys: bool,
        capacity: usize,
    ) -> usize {
        let keys_start = Self::columns_end(header, column_size, capacity);
        if trailing.paired(column_size) {
            keys_start.next_multiple_of(Self::entry_align(trailing))
                + (capacity + 1) * Self::entry_size(trailing)
        } else if keeps_keys {
            (keys_start.next_multiple_of(align_of::<K>()) + capacity * size_of::<K>())
                .next_multiple_of(trailing.align)
                + (capacity + 1) * trailing.size
        } else {
            keys_start.next_multiple_of(trailing.align) + (capacity + 1) * trailing.size
        }
    }

    /// The most bytes the inline keys of a node of pages of `page_size` may
    /// take: as many as one page, or a 1 KiB node, holds, whichever is more.
    /// Only nodes whose keys and trailing slots take no room, and which are
    /// aligned past that, have their capacity capped by it.
    const fn max_inline_keys_size(page_size: usize) -> usize {
        Self::max(page_size, 1024)
    }

    /// The most keys a node of pages of `page_size` may hold: `MAX_CAPACITY`,
    /// and with inline keys of `inline_key_size` bytes, as many as
    /// [`Self::max_inline_keys_size`] holds.
    const fn max_capacity(page_size: usize, inline_key_size: usize) -> usize {
        match Self::max_inline_keys_size(page_size).checked_div(inline_key_size) {
            Some(inline_keys_bound) => Self::min(Self::MAX_CAPACITY, inline_keys_bound),
            None => Self::MAX_CAPACITY,
        }
    }

    /// The largest capacity, up to `max_capacity`, whose `bytes_needed` fit
    /// in `node_size` bytes.
    const fn capacity_in(
        header: Header,
        node_size: usize,
        max_capacity: usize,
        column_size: usize,
        trailing: TrailingSlot,
        keeps_keys: bool,
    ) -> usize {
        // An upper bound: every key takes at least its columns, and its key,
        // if kept, and trailing slot, or its entry. Keys that take no room
        // fit up to the cap.
        let size_per_key = column_size
            + if trailing.paired(column_size) {
                Self::entry_size(trailing)
            } else if keeps_keys {
                size_of::<K>() + trailing.size
            } else {
                trailing.size
            };
        let mut capacity = match node_size.checked_div(size_per_key) {
            Some(bound) => Self::min(max_capacity, bound),
            None => max_capacity,
        };
        while Self::bytes_needed(header, column_size, trailing, keeps_keys, capacity) > node_size {
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
    //! - with inline keys, the header is 6 bytes and the inline keys start
    //!   at 8; an internal node keeps no keys (D49), its edges at `8 + w·n`,
    //!   rounded up to 8, and its keys' offset that unrounded end, with a
    //!   stride of 0, in a 512-byte page: 31, 41 or 49 keys at 8, 4 or 2
    //!   bytes. Leaf entries start at the end of the inline keys, rounded up
    //!   to their alignment, the larger of the key's and the slot's: an entry
    //!   is the key, then the slot aligned to its alignment, rounded up to the
    //!   entry's alignment, and a node needs `n + 1` of them, entry 0 holding
    //!   trailing slot 0 after the head's key (D49), so key 0 is in entry 1.
    //!   Power-of-two entry sizes at most 64 also align the start to the entry
    //!   size, consuming tail slack without changing the page size or capacity;
    //! - the node size is what 12 keys need, rounded up to the node's
    //!   alignment. Alignment is max(1024, align_of K, align_of V) for a
    //!   leaf, max(1024, align_of K, 8) for an internal node that keeps keys,
    //!   and 512 for one that keeps none (D49);
    //! - the capacity is the largest `n` that fits, at most 1023, and with
    //!   inline keys at most `1024 / w`.

    use core::alloc::Layout;
    use std::string::String;
    use std::vec::Vec;

    use super::{InternalLayout, KeyArea, LeafLayout, NodeLayout};
    use crate::inline_key::{Auto, InlineWidth, WithInlineKey};
    use crate::node_config::{Configured, DefaultNodeConfig};
    #[cfg(feature = "file")]
    use crate::tree::file::InFile;
    use crate::tree::node::latch::Latched;
    use crate::tree::node::page::NodePtr;
    use crate::tree::test_support::{
        CacheLineAligned, LargePages, PageAligned, SmallInternalPages,
    };

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
        /// A 1 KiB, 1 KiB-aligned node.
        fn one_page(capacity: usize, keys_offset: usize, trailing_offset: usize) -> Self {
            Self {
                size: 1024,
                align: 1024,
                capacity,
                keys_offset,
                trailing_offset,
            }
        }

        /// A 512-byte, 512-byte-aligned internal node that keeps no keys
        /// (D49).
        fn keyless(capacity: usize, keys_offset: usize, trailing_offset: usize) -> Self {
            Self {
                size: 512,
                align: 512,
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
                NodeLayout::<K, V, DefaultNodeConfig>::PLAIN_LEAF,
                strides::<K, V>(false),
                false,
            );
            self.internal.assert_internal(
                NodeLayout::<K, V, DefaultNodeConfig>::PLAIN_INTERNAL,
                strides::<K, Edge>(false),
                false,
            );
            assert_eq!(
                NodeLayout::<K, V, DefaultNodeConfig>::leaf(None, false, false),
                NodeLayout::<K, V, DefaultNodeConfig>::PLAIN_LEAF
            );
            assert_eq!(
                NodeLayout::<K, V, DefaultNodeConfig>::leaf(None, true, false),
                NodeLayout::<K, V, DefaultNodeConfig>::PLAIN_LEAF
            );
            assert_eq!(
                NodeLayout::<K, V, DefaultNodeConfig>::internal(None, false),
                NodeLayout::<K, V, DefaultNodeConfig>::PLAIN_INTERNAL
            );
        }

        /// Checks the layouts with inline keys of `width`: an internal node
        /// keeps no keys (D49), so its keys' stride is 0.
        fn assert_inline<K, V>(&self, width: InlineWidth) {
            let leaf = NodeLayout::<K, V, DefaultNodeConfig>::leaf(Some(width), false, false);
            self.leaf.assert_leaf(leaf, strides::<K, V>(true), true);
            assert_eq!(
                leaf.head_key_offset,
                Some(leaf.keys.keys_offset - leaf.keys.key_stride)
            );
            let internal = NodeLayout::<K, V, DefaultNodeConfig>::internal(Some(width), false);
            self.internal
                .assert_internal(internal, (0, size_of::<Edge>()), true);
        }
    }

    /// Values and edges are both 8 bytes, so both kinds are alike.
    #[test]
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
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
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
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
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
    fn string_to_u64_with_inline_keys() {
        ExpectedLayout {
            // 8 + 8n + 32(n + 1) ≤ 1024: 40n ≤ 984, n = 24; entries at
            // 8 + 24·8 = 200, rounded to 224 for cache-line-contained
            // pairs; the head is 24 bytes into entry 0, key 0 at 256.
            leaf: ExpectedNode::one_page(24, 256, 248),
            // 8 + 8n + 8(n + 1) ≤ 512: n = 31; edges at 8 + 31·8 = 256.
            internal: ExpectedNode::keyless(31, 256, 256),
        }
        .assert_inline::<String, u64>(InlineWidth::Eight);
    }

    /// `Auto`'s heap byte strings keep 4-byte inline keys in internal nodes,
    /// whatever the leaves' 8-byte ones and the value type, and no keys
    /// (D49): `(8 + 4n → 8) + 8(n + 1) ≤ 512`, n = 41, edges at 176. So do
    /// keys whose impl has `cmp_from`, as `String`'s does, under
    /// `WithInlineKey`; those whose impl hasn't keep 8, as `[u8; 32]`.
    #[test]
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
    fn auto_heap_byte_internal_nodes_keep_four_bytes() {
        ExpectedNode::keyless(41, 172, 176).assert_internal(
            NodePtr::<String, u64, Auto>::internal_layout(),
            (0, 8),
            true,
        );
        assert_eq!(
            NodePtr::<String, u64, Auto>::internal_layout(),
            NodePtr::<String, String, Auto>::internal_layout(),
        );
        assert_eq!(
            NodePtr::<&str, u64, Auto>::internal_layout(),
            NodePtr::<String, u64, Auto>::internal_layout(),
        );
        assert_eq!(
            NodePtr::<String, u64, WithInlineKey>::internal_layout(),
            NodePtr::<String, u64, Auto>::internal_layout(),
        );
        assert_eq!(
            NodePtr::<[u8; 32], u64, WithInlineKey>::internal_layout(),
            NodeLayout::<[u8; 32], u64, DefaultNodeConfig>::EIGHT_BYTE_INLINE_INTERNAL,
        );
    }

    /// `Vec<u8>` is 24 bytes and 8-aligned, like `String`.
    #[test]
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
    fn byte_vec_to_u64_with_inline_keys() {
        ExpectedLayout {
            leaf: ExpectedNode::one_page(24, 256, 248),
            internal: ExpectedNode::keyless(31, 256, 256),
        }
        .assert_inline::<Vec<u8>, u64>(InlineWidth::Eight);
    }

    /// Large values cut the leaf's capacity, not the internal node's.
    #[test]
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
    fn string_to_string_with_inline_keys() {
        ExpectedLayout {
            // Entries of 24 + 24 = 48: 8 + 8n + 48(n + 1) ≤ 1024: 56n ≤ 968,
            // n = 17; entries at 8 + 17·8 = 144, the head at 168, key 0 at
            // 192.
            leaf: ExpectedNode::one_page(17, 192, 168),
            // As for `String` → `u64`.
            internal: ExpectedNode::keyless(31, 256, 256),
        }
        .assert_inline::<String, String>(InlineWidth::Eight);
    }

    /// Unaligned keys: the slot after each pads to its own alignment.
    #[test]
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
    fn byte_array_keys_with_inline_keys() {
        ExpectedLayout {
            // Entries of 32 + 8 = 40: 8 + 8n + 40(n + 1) ≤ 1024: 48n ≤ 976,
            // n = 20; entries at 8 + 20·8 = 168, the head at 200, key 0 at
            // 208.
            leaf: ExpectedNode::one_page(20, 208, 200),
            // As for `String` → `u64`: no keys.
            internal: ExpectedNode::keyless(31, 256, 256),
        }
        .assert_inline::<[u8; 32], u64>(InlineWidth::Eight);
    }

    /// `Auto`'s byte arrays keep two bytes of each inline key, and no keys,
    /// in internal nodes: `(8 + 2n → 8) + 8(n + 1) ≤ 512`, n = 49, edges
    /// at 112, whatever the array's length.
    #[test]
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
    fn recognized_arrays_keep_no_keys_in_internal_nodes() {
        fn check<const N: usize>() {
            let internal = NodePtr::<[u8; N], u64, Auto>::internal_layout();
            assert_eq!(
                internal,
                NodeLayout::<[u8; N], u64, DefaultNodeConfig>::TWO_BYTE_INLINE_INTERNAL
            );
            ExpectedNode::keyless(49, 106, 112).assert_internal(internal, (0, 8), true);
            // The same Rust array under an explicit strategy keeps that
            // strategy's eight-byte inline keys.
            assert_eq!(
                NodePtr::<[u8; N], u64, WithInlineKey>::internal_layout(),
                NodeLayout::<[u8; N], u64, DefaultNodeConfig>::EIGHT_BYTE_INLINE_INTERNAL
            );
        }
        check::<16>();
        check::<20>();
        check::<32>();
    }

    /// Hash tags (D36) fit in the padding before a `String` → `u64`
    /// leaf's entries, at no entry's cost: tags at 8 + 24·8 = 200, up to the
    /// head's key at 224; the head at 248, key 0 at 256.
    #[test]
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
    fn hash_tags_fit_in_the_padding() {
        let tagged = NodePtr::<String, u64, Auto>::leaf_layout();
        assert_eq!(
            tagged,
            NodeLayout::<String, u64, DefaultNodeConfig>::EIGHT_BYTE_INLINE_HASH_TAGGED_LEAF
        );
        ExpectedNode::one_page(24, 256, 248).assert_leaf(tagged, (32, 32), true);
        assert_eq!(tagged.hash_tags_offset, Some(200));
        assert_eq!(tagged.head_key_offset, Some(224));
        let untagged = NodeLayout::<String, u64, DefaultNodeConfig>::EIGHT_BYTE_INLINE_LEAF;
        assert_eq!(untagged.keys.capacity, 24);
    }

    /// Hash tags fit in the padding before a `String` → `String` leaf's
    /// entries only where its head's key is (D49): it gives up an entry to
    /// keep them, 17 → 16, sized with 8 + 1 bytes beside each key:
    /// `(8 + 9n → 8) + 48(n + 1) ≤ 1024`, n = 16; tags at 8 + 16·8 = 136,
    /// entries at 152, the head at 176, key 0 at 200. Leaves without room
    /// for tags keep none, and give up no entry.
    #[test]
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
    fn hash_tags_give_up_an_entry_for_the_head_key() {
        let tagged = NodePtr::<String, String, Auto>::leaf_layout();
        assert_eq!(
            tagged,
            NodeLayout::<String, String, DefaultNodeConfig>::EIGHT_BYTE_INLINE_HASH_TAGGED_LEAF
        );
        ExpectedNode::one_page(16, 200, 176).assert_leaf(tagged, (48, 48), true);
        assert_eq!(tagged.hash_tags_offset, Some(136));
        assert_eq!(tagged.head_key_offset, Some(152));
        let untagged = NodeLayout::<String, String, DefaultNodeConfig>::EIGHT_BYTE_INLINE_LEAF;
        assert_eq!(untagged.keys.capacity, 17);
        let without_room = NodePtr::<String, (), Auto>::leaf_layout();
        assert_eq!(without_room.hash_tags_offset, None);
        assert_eq!(
            without_room,
            NodeLayout::<String, (), DefaultNodeConfig>::EIGHT_BYTE_INLINE_LEAF
        );
    }

    /// Over-aligned keys pad each entry to their alignment.
    #[test]
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
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
            // No keys: the edges after the inline keys, 512 B aligned.
            internal: ExpectedNode::keyless(31, 256, 256),
        }
        .assert_inline::<CacheLineAligned, u64>(InlineWidth::Eight);
    }

    /// Small keys: the inline keys outweigh them.
    #[test]
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
    fn set_of_bytes_with_inline_keys() {
        ExpectedLayout {
            // Entries of 1 byte, the slot taking none: 8 + 8n + (n + 1) ≤
            // 1024: 9n ≤ 1015, n = 112; entries at 8 + 896 = 904, the head
            // and key 0 at 905.
            leaf: ExpectedNode::one_page(112, 905, 905),
            // As for `String` → `u64`: no keys.
            internal: ExpectedNode::keyless(31, 256, 256),
        }
        .assert_inline::<u8, ()>(InlineWidth::Eight);
    }

    /// The layout `u64` keys get with their inline keys: entries of 16
    /// bytes after 2 bytes each of inline keys, so 55 keys instead of 63.
    #[test]
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
    fn u64_to_u64_with_two_byte_inline_keys() {
        ExpectedLayout {
            // (8 + 2n → 8) + 16(n + 1) ≤ 1024: 1016 at n = 55, 1032 at 56;
            // entries at 120 rounded to 128 so complete pairs stay within
            // cache lines; the head is at 136, key 0 at 144.
            leaf: ExpectedNode::one_page(55, 144, 136),
            // (8 + 2n → 8) + 8(n + 1) ≤ 512: n = 49, edges at 106 → 112.
            internal: ExpectedNode::keyless(49, 106, 112),
        }
        .assert_inline::<u64, u64>(InlineWidth::Two);
    }

    #[test]
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
    fn cache_line_sized_pairs_use_tail_slack_without_losing_capacity() {
        // 8 + 2·14 → 8 = 40, followed by 15·64 = 960 bytes of entries.
        // Moving entry 0 to byte 64 uses the 24-byte tail; key 0 is at 128.
        ExpectedNode::one_page(14, 128, 72).assert_leaf(
            NodeLayout::<u64, [u64; 7], DefaultNodeConfig>::TWO_BYTE_INLINE_LEAF,
            (64, 64),
            true,
        );
        // 8 + 8·13 = 112, followed by 14·64 = 896 bytes. Moving entry 0
        // to byte 128 uses the 16-byte tail; the head is at 128 + 24.
        ExpectedNode::one_page(13, 192, 152).assert_leaf(
            NodeLayout::<String, [u64; 5], DefaultNodeConfig>::EIGHT_BYTE_INLINE_LEAF,
            (64, 64),
            true,
        );
    }

    /// 16-aligned keys pad each entry to 32 bytes.
    #[test]
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
    fn u128_to_u64_with_two_byte_inline_keys() {
        ExpectedLayout {
            // Entries of 16 + 8 → 32: (8 + 2n → 16) + 32(n + 1) ≤ 1024: 992
            // at n = 28, 1040 at 29; entries at 64, the head at 80, key 0 at
            // 96.
            leaf: ExpectedNode::one_page(28, 96, 80),
            // As for `u64` → `u64`: no keys.
            internal: ExpectedNode::keyless(49, 106, 112),
        }
        .assert_inline::<u128, u64>(InlineWidth::Two);
    }

    /// A set: the leaf has no values, and its entries are its keys.
    #[test]
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
    fn set_of_i64_with_two_byte_inline_keys() {
        ExpectedLayout {
            // Entries of 8: (8 + 2n → 8) + 8(n + 1) ≤ 1024: 1016 at n = 100,
            // 1032 at 101; entries at 208, the head and key 0 at 216.
            leaf: ExpectedNode::one_page(100, 216, 216),
            // As for `u64` → `u64`.
            internal: ExpectedNode::keyless(49, 106, 112),
        }
        .assert_inline::<i64, ()>(InlineWidth::Two);
    }

    #[test]
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
    fn u32_to_u32_with_four_byte_inline_keys() {
        ExpectedLayout {
            // Entries of 4 + 4 = 8: 8 + 4n + 8(n + 1) ≤ 1024: 12n ≤ 1008,
            // n = 84; entries at 8 + 336 = 344, the head at 348, key 0 at
            // 352.
            leaf: ExpectedNode::one_page(84, 352, 348),
            // (8 + 4n → 8) + 8(n + 1) ≤ 512: n = 41, edges at 172 → 176.
            internal: ExpectedNode::keyless(41, 172, 176),
        }
        .assert_inline::<u32, u32>(InlineWidth::Four);
    }

    /// No room past 1 KB aligned.
    #[repr(align(2048))]
    struct ZeroSizedPastOneKb;

    /// Inline keys take at most 1 KB, which only binds for keys that take
    /// no room and are aligned past 1 KB, whose nodes are larger than 1 KB.
    #[test]
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
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
            // No keys, so no key alignment: as for `u64` → `u64`.
            internal: ExpectedNode::keyless(49, 106, 112),
        }
        .assert_inline::<ZeroSizedPastOneKb, ()>(InlineWidth::Two);
    }

    /// The internal layout is a function of the key type alone.
    #[test]
    fn internal_nodes_ignore_the_value_type() {
        fn assert_internal_matches_u64_to_u64<V>() {
            assert_eq!(
                NodeLayout::<u64, V, DefaultNodeConfig>::PLAIN_INTERNAL,
                NodeLayout::<u64, u64, DefaultNodeConfig>::PLAIN_INTERNAL
            );
            assert_eq!(
                NodeLayout::<u64, V, DefaultNodeConfig>::TWO_BYTE_INLINE_INTERNAL,
                NodeLayout::<u64, u64, DefaultNodeConfig>::TWO_BYTE_INLINE_INTERNAL
            );
            assert_eq!(
                NodeLayout::<u64, V, DefaultNodeConfig>::EIGHT_BYTE_INLINE_INTERNAL,
                NodeLayout::<u64, u64, DefaultNodeConfig>::EIGHT_BYTE_INLINE_INTERNAL
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
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
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
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
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
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
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
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
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
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
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
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
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
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
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
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
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
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
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
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
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
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
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
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
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

    /// A node config's page sizes (D59), here 4 KiB leaves and 2 KiB
    /// internal nodes, size the nodes by the same rules.
    #[test]
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
    fn large_pages_hold_more_keys() {
        type Layouts = NodeLayout<u64, u64, LargePages>;
        let node = |size, capacity, keys_offset, trailing_offset| ExpectedNode {
            size,
            align: size,
            capacity,
            keys_offset,
            trailing_offset,
        };
        // 8 + 8n + 8(n + 1) ≤ 4096: n = 255; the head at 8 + 255·8 = 2048.
        node(4096, 255, 8, 2048).assert_leaf(Layouts::PLAIN_LEAF, (8, 8), false);
        // ≤ 2048: n = 127; edges at 8 + 127·8 = 1024.
        node(2048, 127, 8, 1024).assert_internal(Layouts::PLAIN_INTERNAL, (8, 8), false);
        // (8 + 2n → 8) + 16(n + 1) ≤ 4096: n = 226; entries at 8 + 226·2 =
        // 460, rounded to 464; the head 8 bytes into entry 0, key 0 at 480.
        node(4096, 226, 480, 472).assert_leaf(Layouts::TWO_BYTE_INLINE_LEAF, (16, 16), true);
        // (8 + 2n → 8) + 8(n + 1) ≤ 2048: n = 203; edges at 8 + 203·2 =
        // 414, rounded to 416.
        node(2048, 203, 414, 416).assert_internal(Layouts::TWO_BYTE_INLINE_INTERNAL, (0, 8), true);

        // The strategy hands its config to the nodes, and so do the
        // strategies that wrap it.
        type Strategy = Configured<Auto, LargePages>;
        assert_eq!(
            NodePtr::<u64, u64, Strategy>::leaf_layout(),
            Layouts::TWO_BYTE_INLINE_LEAF
        );
        assert_eq!(
            NodePtr::<u64, u64, Strategy>::internal_layout(),
            Layouts::TWO_BYTE_INLINE_INTERNAL
        );
        let latched = (
            NodePtr::<u64, u64, Latched<Strategy>>::leaf_layout(),
            NodePtr::<u64, u64, Latched<Strategy>>::internal_layout(),
        );
        assert_eq!(latched.0, Layouts::LATCHED_TWO_BYTE_INLINE_LEAF);
        assert_eq!(latched.1, Layouts::LATCHED_TWO_BYTE_INLINE_INTERNAL);
        assert_eq!((latched.0.page.size(), latched.1.page.size()), (4096, 2048));
        #[cfg(feature = "file")]
        assert_eq!(
            NodePtr::<u64, u64, InFile<Strategy>>::leaf_layout(),
            Layouts::TWO_BYTE_INLINE_LEAF
        );
    }

    /// Internal pages of 64 B (D59) are too small for 12 keys: an internal
    /// node takes as many as it needs, aligned to 64 B. Leaves keep their
    /// default.
    #[test]
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
    fn small_internal_pages_grow_to_hold_twelve_keys() {
        type Layouts = NodeLayout<u64, u64, SmallInternalPages>;
        let node = |size, capacity, keys_offset, trailing_offset| ExpectedNode {
            size,
            align: 64,
            capacity,
            keys_offset,
            trailing_offset,
        };
        // Needs 8 + 12·8 + 13·8 = 208, so 256: 8 + 8n + 8(n + 1) ≤ 256,
        // n = 15; edges at 8 + 15·8 = 128.
        node(256, 15, 8, 128).assert_internal(Layouts::PLAIN_INTERNAL, (8, 8), false);
        // Needs 8 + 12·2 + 13·8 = 136, so 192: (8 + 2n → 8) + 8(n + 1) ≤
        // 192, n = 17; edges at 8 + 17·2 = 42, rounded to 48.
        node(192, 17, 42, 48).assert_internal(Layouts::TWO_BYTE_INLINE_INTERNAL, (0, 8), true);
        assert_eq!(
            Layouts::TWO_BYTE_INLINE_LEAF,
            NodeLayout::<u64, u64, DefaultNodeConfig>::TWO_BYTE_INLINE_LEAF
        );
    }
}
