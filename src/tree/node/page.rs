//! Node pages: the allocation boundary ([`FreshPage`]), raw access to an
//! initialized node's header and slot arrays ([`NodePtr`]), and the type
//! identities that decide whether a page holds inline keys, and let the
//! search read them ([`ByteString`], [`SameType`]). The root's page, a node
//! of either kind and the root area after it, is `root_page.rs`'s.

use alloc::alloc::{alloc, dealloc, handle_alloc_error};
use core::alloc::Layout;
use core::fmt;
use core::marker::PhantomData;
use core::ptr::NonNull;

use super::internal::Internal;
use super::layout::{InternalLayout, KeyArea, LeafLayout, NodeLayout};
use super::leaf::Leaf;
use super::node_plan::NodePlan;
use super::root_page::RootPage;
use super::slots::{InlineKeySlots, KeyColumns, KeySlots, SeparatorSlots, Slots};
use crate::inline_key::{InlineKeyStrategy, InlineWidth};

pub(super) use type_identity::{ByteString, SameType, StdInteger};

/// What pages are allocated for: a leaf, an internal node ([`NodeKind`]s), or
/// the root ([`RootPage`]), each with a page layout of its own (D21). A page
/// is only ever used, and deallocated, with the layout it was allocated for.
///
/// # Safety
///
/// `page_layout` is the layout the code that uses pages of this type
/// accesses them with.
pub(crate) unsafe trait PageType: Sized {
    /// The allocation layout of every page of this type (D21) and strategy
    /// (D18): one of the layout constants, picked after monomorphization.
    /// Using it also runs those constants' compile-time assertions.
    fn page_layout() -> Layout;
}

/// A kind of node: [`Leaf`] or [`Internal`]. A page holds nodes of the kind
/// it was allocated for only, but for the root's, which holds either
/// (`root_page.rs`). What depends on a node's layout, its capacity and where
/// its slots are, takes the kind as a type, so that it folds to the kind's
/// constants.
///
/// # Safety
///
/// `IS_LEAF` is whether nodes of this kind are leaves, and `page_layout` the
/// layout of their pages: the node code accesses a node of this kind with
/// that kind's offsets and capacity, from the start of its page.
pub(crate) unsafe trait NodeKind: PageType {
    const IS_LEAF: bool;
}

/// An allocated page of type `P` ([`Leaf`], [`Internal`] or [`RootPage`]),
/// that holds no node.
///
/// A `FreshPage` owns its page, like a `Box` that has nothing in it: this is
/// the only way to create a node, and what a node turns back into once it is
/// freed, ready to be reused or deallocated. The kinds have different
/// layouts (D21), and so do maps whose keys have inline keys and those whose
/// keys have none (D18). The type carries all that decides the layout, so a
/// page only ever holds a node of the layout it was allocated for, and is
/// deallocated with the layout it was allocated with.
///
/// Invariant: the page comes from the global allocator, with
/// [`Self::layout`], and nothing else uses it.
pub(crate) struct FreshPage<P> {
    page: NonNull<u8>,
    /// The type of page it is.
    _page_type: PhantomData<fn() -> P>,
}

/// A fresh page sorted by its type: what releasing a node whose kind is
/// known only at run time gives back.
pub(crate) enum PageByType<K, V, S> {
    Leaf(FreshPage<Leaf<K, V, S>>),
    Internal(FreshPage<Internal<K, V, S>>),
    Root(FreshPage<RootPage<K, V, S>>),
}

impl<P: PageType> FreshPage<P> {
    /// The allocation layout of every page of this type; see
    /// [`PageType::page_layout`].
    pub(super) fn layout() -> Layout {
        P::page_layout()
    }

    /// Takes ownership of a page. Its contents are treated as uninitialized.
    ///
    /// Panics if `page` is not aligned to [`Self::layout`].
    ///
    /// # Safety
    ///
    /// - `page` was allocated by the global allocator with [`Self::layout`].
    /// - Until the page is handed back by [`Self::into_raw`], it stays
    ///   allocated and is accessed only through the returned value and the
    ///   nodes built from it.
    pub(crate) unsafe fn from_raw(page: NonNull<u8>) -> Self {
        let layout = Self::layout();
        assert!(
            page.addr().get().is_multiple_of(layout.align()),
            "node page is not aligned to {}",
            layout.align()
        );
        Self {
            page,
            _page_type: PhantomData,
        }
    }
}

impl<P> FreshPage<P> {
    /// Hands the page back, e.g. for deallocation by the global allocator
    /// with [`Self::layout`].
    pub(crate) fn into_raw(self) -> NonNull<u8> {
        self.page
    }
}

/// Temporary allocation boundary, until the allocator trait (D8) replaces it:
/// one global-allocator call per page. A node's page is a multiple of 1 KiB,
/// or of 512 bytes for internal nodes that keep no keys (D49), and aligned
/// to that step (D15), or more if its keys or values need it: so no node
/// crosses a page, and the lowest bit of every node pointer is free for its
/// kind's tag ([`NodePtr`]). A root page is aligned as the larger of
/// the two kinds' (`root_page.rs`).
impl<P: PageType> FreshPage<P> {
    /// A page from the global allocator.
    ///
    /// Calls [`handle_alloc_error`] if the allocation fails.
    pub(crate) fn allocate() -> Self {
        let layout = Self::layout();
        // SAFETY: `layout` has a non-zero size: nodes are at least 512 B.
        let page = unsafe { alloc(layout) };
        let Some(page) = NonNull::new(page) else {
            handle_alloc_error(layout)
        };
        // SAFETY: the page was just allocated by the global allocator with
        // `layout()`, and nothing else has it.
        unsafe { Self::from_raw(page) }
    }

    /// Gives the page back to the global allocator.
    pub(crate) fn deallocate(self) {
        let page = self.into_raw();
        // SAFETY: the page came from the global allocator with `layout()`,
        // the same every time for this type, and is used by nothing else
        // (type invariant); consuming `self` gives up the last use of it.
        unsafe { dealloc(page.as_ptr(), Self::layout()) }
    }
}

impl<P> fmt::Debug for FreshPage<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("FreshPage").field(&self.page).finish()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> PageByType<K, V, S> {
    /// Gives the page back to the global allocator, with its kind's layout.
    pub(crate) fn deallocate(self) {
        match self {
            Self::Leaf(page) => page.deallocate(),
            Self::Internal(page) => page.deallocate(),
            Self::Root(page) => page.deallocate(),
        }
    }
}

impl<K, V, S> From<FreshPage<Leaf<K, V, S>>> for PageByType<K, V, S> {
    fn from(page: FreshPage<Leaf<K, V, S>>) -> Self {
        Self::Leaf(page)
    }
}

impl<K, V, S> From<FreshPage<Internal<K, V, S>>> for PageByType<K, V, S> {
    fn from(page: FreshPage<Internal<K, V, S>>) -> Self {
        Self::Internal(page)
    }
}

impl<K, V, S> From<FreshPage<RootPage<K, V, S>>> for PageByType<K, V, S> {
    fn from(page: FreshPage<RootPage<K, V, S>>) -> Self {
        Self::Root(page)
    }
}

/// The header at the start of every node page.
#[repr(C)]
pub(in crate::tree) struct NodeHeader {
    /// The number of keys.
    len: u16,
    /// Whether a leaf holds a head (D17); its head slot is initialized
    /// exactly when this is set, and so is its head key slot in a map whose
    /// separator keys are heads (D49). Always `false` in an internal node.
    has_head: bool,
}

/// The header of a node with inline keys (D18): the plain node header, then
/// what only such nodes need. It fits in the bytes before the inline keys,
/// which start 8-aligned, so it costs no room.
#[repr(C)]
pub(in crate::tree) struct InlineKeyNodeHeader {
    header: NodeHeader,
    /// How many leading bytes the node's fences share, and so every key in
    /// it, which its inline keys skip (05-fence-key.md); for a node on the
    /// tree's border, which lacks a fence, what every key of the tree shares
    /// (D50).
    prefix_len: u16,
}

impl InlineKeyNodeHeader {
    /// The longest prefix a node records: what `prefix_len` holds.
    pub(super) const MAX_PREFIX_LEN: usize = u16::MAX as usize;
}

/// A pointer to an initialized node of `NodeLayout<K, V>`, for a map with
/// strategy `S`, that tells the node's kind in its lowest bit, which every
/// page leaves 0, being aligned to 512 B at least (D15, D49). Internal nodes
/// store it as edges, paths and walks as the nodes on them (`tree/path.rs`),
/// the tree as its root (`root_page.rs`), and the node handles, [`Leaf`] and
/// [`Internal`], wrap it: following one decides the kind of the node it leads
/// to before reading the node. A leaf's pointer carries the tag, so that a
/// descent uses an internal node's as it is.
///
/// The tag is the only record of a node's kind: the node does not keep it.
/// A pointer gets it where its page becomes a node ([`Self::initialize`],
/// [`Self::move_node_to`]), from the kind the page is made into, and keeps it
/// as it is copied. The kinds have different layouts (D21), so every slot
/// array it hands out needs the caller to know the kind. Everything it
/// offers is raw: the node handles are its users.
#[repr(transparent)]
pub(crate) struct NodePtr<K, V, S> {
    tagged: NonNull<u8>,
    _marker: PhantomData<*const (K, V)>,
    _strategy: PhantomData<fn() -> S>,
}

impl<K, V, S> Clone for NodePtr<K, V, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K, V, S> Copy for NodePtr<K, V, S> {}

impl<K, V, S> NodePtr<K, V, S> {
    /// The bit that marks a leaf's pointer.
    const LEAF_TAG: usize = 1;

    /// The node of kind `Kind` in the page at `page`, tagged with its kind.
    #[inline(always)]
    fn tagged_as<Kind: NodeKind>(page: NonNull<u8>) -> Self {
        debug_assert_eq!(page.addr().get() & Self::LEAF_TAG, 0);
        Self {
            tagged: page.map_addr(|addr| addr | (usize::from(Kind::IS_LEAF) * Self::LEAF_TAG)),
            _marker: PhantomData,
            _strategy: PhantomData,
        }
    }

    /// Whether the node is a leaf, as the tag says.
    #[inline(always)]
    pub(in crate::tree) fn is_leaf(self) -> bool {
        self.tagged.addr().get() & Self::LEAF_TAG != 0
    }

    /// The page's address: the pointer without its tag, which every access
    /// to the node starts from.
    #[inline(always)]
    pub(super) fn page(self) -> NonNull<u8> {
        // SAFETY: clearing the tag gives back the page's address, which is
        // not null.
        unsafe {
            NonNull::new_unchecked(self.tagged.as_ptr().map_addr(|addr| addr & !Self::LEAF_TAG))
        }
    }

    /// The page's address, which tells nodes apart (`tree/inspect.rs`).
    #[cfg(any(test, feature = "inspect"))]
    pub(in crate::tree) fn addr(self) -> usize {
        self.page().addr().get()
    }

    /// Whether `self` and `other` point to the same node.
    pub(in crate::tree) fn is(self, other: Self) -> bool {
        self.tagged == other.tagged
    }

    /// Gives the page back, as a fresh page of its type.
    ///
    /// # Safety
    ///
    /// - The page was allocated as a page of type `P`.
    /// - The node's keys, values, head and edges have all been moved out,
    ///   dropped or deliberately leaked, and neither this pointer nor any
    ///   copy of it (an edge, a handle) is used afterwards.
    pub(super) unsafe fn into_fresh_page<P: PageType>(self) -> FreshPage<P> {
        // The page came from a `FreshPage` of type `P` (caller), for the
        // same `K`, `V` and `S`, so it is allocated with
        // `FreshPage::<P>::layout()` and aligned, and the caller hands
        // over every use of it: it is a `FreshPage` again.
        FreshPage {
            page: self.page(),
            _page_type: PhantomData,
        }
    }

    /// The header, written by `from_fresh_page`. In a node with inline keys,
    /// it starts an [`InlineKeyNodeHeader`].
    fn header(self) -> *mut NodeHeader {
        self.page().cast::<NodeHeader>().as_ptr()
    }

    /// The `len` header field.
    ///
    /// # Safety
    ///
    /// The node has not been freed, and nothing writes its header
    /// concurrently.
    pub(super) unsafe fn len(self) -> usize {
        // SAFETY: the header was written by `from_fresh_page` and is aligned;
        // the page is allocated (caller).
        usize::from(unsafe { (*self.header()).len })
    }

    /// Whether a leaf's head slot, and head key slot where separator keys
    /// are heads, are initialized (D17, D49).
    ///
    /// # Safety
    ///
    /// The node has not been freed, and nothing writes its header
    /// concurrently.
    pub(super) unsafe fn has_head(self) -> bool {
        // SAFETY: the header was written by `from_fresh_page` and is aligned;
        // the page is allocated (caller).
        unsafe { (*self.header()).has_head }
    }

    /// # Safety
    ///
    /// The node is a leaf that has not been freed, its handle has
    /// exclusive access, and its head slot, and head key slot where separator
    /// keys are heads, are initialized exactly if `has_head` by the time the
    /// node is used again.
    pub(super) unsafe fn set_has_head(self, has_head: bool) {
        // SAFETY: the header is in bounds and aligned; the caller has
        // exclusive access.
        unsafe { (*self.header()).has_head = has_head }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> NodePtr<K, V, S> {
    /// The plan of the map's nodes of kind `Kind` (`node_plan.rs`): what they
    /// store beside their keys, which every layout decision below follows.
    ///
    /// A pure function of `K`, `S` and `Kind` that folds to a constant: every
    /// page of a map and kind gets the same answer, so accesses match its
    /// allocation layout.
    #[inline(always)]
    pub(super) fn plan<Kind: NodeKind>() -> NodePlan<K> {
        NodePlan::of::<S, Kind>()
    }

    /// The width of the inline keys nodes of `Kind` hold, or `None` if they
    /// hold none (D18).
    #[inline]
    pub(in crate::tree) fn inline_width<Kind: NodeKind>() -> Option<InlineWidth> {
        Self::plan::<Kind>().inline_width()
    }

    /// Whether the map's nodes hold inline keys (D18): those of both kinds
    /// do, or neither does.
    pub(in crate::tree) fn has_inline_keys() -> bool {
        Self::plan::<Leaf<K, V, S>>().inline_width().is_some()
    }

    /// Whether each separator key is the head of the leftmost leaf right of
    /// it rather than a key of its internal node (D49): in a map whose keys
    /// have inline keys.
    #[inline(always)]
    pub(in crate::tree) fn has_separator_keys_in_heads() -> bool {
        Self::plan::<Internal<K, V, S>>().has_separator_keys_in_heads()
    }

    pub(in crate::tree) fn leaf_layout() -> LeafLayout {
        let plan = Self::plan::<Leaf<K, V, S>>();
        NodeLayout::<K, V>::leaf(plan.inline_width(), plan.has_length_tags())
    }

    pub(in crate::tree) fn internal_layout() -> InternalLayout {
        NodeLayout::<K, V>::internal(Self::inline_width::<Internal<K, V, S>>())
    }

    /// Where the keys and inline keys of a node of kind `Kind` sit.
    pub(in crate::tree) fn key_area<Kind: NodeKind>() -> KeyArea {
        if Kind::IS_LEAF {
            Self::leaf_layout().keys
        } else {
            Self::internal_layout().keys
        }
    }

    /// The maximum number of keys of a node of kind `Kind`.
    pub(in crate::tree) fn capacity<Kind: NodeKind>() -> usize {
        Self::key_area::<Kind>().capacity
    }

    /// Whether a leaf pairs its keys with its values in entries (D29), which
    /// then move whole: with inline keys, unless the values take no room,
    /// which leaves the keys side by side. Internal nodes never pair their
    /// edges with keys (D32).
    pub(super) fn has_paired_leaf_entries() -> bool {
        Self::leaf_layout().keys.key_stride != size_of::<K>()
    }

    /// Turns a fresh page into an empty node of the page's kind: no keys
    /// and, for a leaf, no head. An internal node still needs its first edge
    /// written.
    ///
    /// It also initializes the columns beside the keys
    /// ([`KeyColumns::initialize`]): every inline-key slot up to the capacity
    /// gets the greatest inline key, so that searches may count the slots
    /// past `len` (`tree/node/mod.rs`, invariant 4).
    pub(super) fn from_fresh_page<Kind: NodeKind>(page: FreshPage<Kind>) -> Self {
        // SAFETY: the page is a fresh one of kind `Kind`, which nothing else
        // uses.
        unsafe { Self::initialize::<Kind>(page.into_raw()) }
    }

    /// Makes the page at `page` an empty node of kind `Kind`, as
    /// `from_fresh_page` says, whatever it held, and returns its pointer,
    /// tagged with `Kind`: as a fresh page becomes one, and as the root's
    /// page turns from one kind into the other (`root_page.rs`).
    ///
    /// # Safety
    ///
    /// The page is allocated, aligned to `Kind`'s layout and at least its
    /// size, the access is exclusive, and nothing in the page needs dropping
    /// or freeing anymore: it is overwritten.
    pub(super) unsafe fn initialize<Kind: NodeKind>(page: NonNull<u8>) -> Self {
        let node = Self::tagged_as::<Kind>(page);
        let header = NodeHeader {
            len: 0,
            has_head: false,
        };
        // SAFETY: the page is aligned to its layout (the caller), whose
        // alignment either header's divides, and the header of its layout
        // fits before its keys and columns, whose `capacity` slots lie in the
        // page, aligned (layout invariants); the access is exclusive.
        unsafe {
            if Self::has_inline_keys() {
                page.cast::<InlineKeyNodeHeader>()
                    .write(InlineKeyNodeHeader {
                        header,
                        prefix_len: 0,
                    });
            } else {
                page.cast::<NodeHeader>().write(header);
            }
            node.key_columns::<Kind>().initialize();
        }
        node
    }

    /// Moves the node of kind `Kind` here into the page at `dest`, a page
    /// for a node of that kind: every byte of its layout, as raw bytes,
    /// keys, values and edges moving without being read. Returns the node's
    /// pointer there, tagged with `Kind`.
    ///
    /// # Safety
    ///
    /// The node is of kind `Kind`, and `dest` is another page at least that
    /// kind's size, aligned to it, which the access to is exclusive and which
    /// holds nothing that needs dropping. Afterwards, this node's contents
    /// belong to `dest`: nothing reads them here again but as raw bytes.
    pub(super) unsafe fn move_node_to<Kind: NodeKind>(self, dest: NonNull<u8>) -> Self {
        debug_assert_eq!(self.is_leaf(), Kind::IS_LEAF);
        let size = Kind::page_layout().size();
        // SAFETY: both pages are allocated with at least `size` bytes, and
        // distinct (the caller); a byte copy carries uninitialized bytes as
        // they are.
        unsafe {
            self.page()
                .as_ptr()
                .copy_to_nonoverlapping(dest.as_ptr(), size);
        }
        Self::tagged_as::<Kind>(dest)
    }

    /// The `prefix_len` of a node with inline keys (05-fence-key.md), or
    /// `None` for a node without.
    ///
    /// # Safety
    ///
    /// The node has not been freed, and nothing writes its header
    /// concurrently.
    pub(super) unsafe fn prefix_len(self) -> Option<usize> {
        Self::has_inline_keys().then(|| {
            let header = self.page().cast::<InlineKeyNodeHeader>().as_ptr();
            // SAFETY: a node with inline keys starts with an
            // `InlineKeyNodeHeader`, written by `from_fresh_page` and
            // aligned; the page is allocated (caller).
            usize::from(unsafe { (*header).prefix_len })
        })
    }

    /// Sets the `prefix_len` of a node with inline keys; does nothing to a
    /// node without. Keeping the inline keys at that prefix length is the
    /// caller's business: it decides answers, not memory safety.
    ///
    /// # Safety
    ///
    /// The node has not been freed, and its handle has exclusive access.
    pub(super) unsafe fn set_prefix_len(self, prefix_len: usize) {
        if Self::has_inline_keys() {
            debug_assert!(prefix_len <= InlineKeyNodeHeader::MAX_PREFIX_LEN);
            let prefix_len = prefix_len.min(InlineKeyNodeHeader::MAX_PREFIX_LEN) as u16;
            let header = self.page().cast::<InlineKeyNodeHeader>().as_ptr();
            // SAFETY: a node with inline keys starts with an aligned
            // `InlineKeyNodeHeader`; the caller has exclusive access.
            unsafe { (*header).prefix_len = prefix_len };
        }
    }

    /// # Safety
    ///
    /// The node has not been freed, its handle has exclusive access, and
    /// `len` is at most the capacity of the node's kind.
    pub(super) unsafe fn set_len(self, len: usize) {
        // `capacity <= MAX_CAPACITY <= u16::MAX` (layout invariants).
        let len = len as u16;
        // SAFETY: the header is in bounds and aligned; the caller has
        // exclusive access.
        unsafe { (*self.header()).len = len }
    }

    /// The key slots, `capacity` of them from the keys' offset, a key or an
    /// entry apart (D29), and in a node with inline keys, as many inline-key
    /// slots from theirs (D18).
    ///
    /// Only for a node that keeps keys: a leaf, or an internal node without
    /// inline keys (D49).
    ///
    /// # Safety
    ///
    /// The node has not been freed, and is of kind `Kind`; nothing writes
    /// its header concurrently.
    pub(super) unsafe fn key_slots<Kind: NodeKind>(self) -> KeySlots<K, S, Kind> {
        debug_assert!(Kind::IS_LEAF || Self::internal_layout().keeps_keys());
        let area = Self::key_area::<Kind>();
        // SAFETY: forwarded.
        let prefix_len = unsafe { self.prefix_len() }.unwrap_or(0);
        // SAFETY: the keys' offset is aligned for `K`, and so is their
        // stride, and `capacity` keys from there stay within a page of the
        // node's kind, `Kind`, clear of its other arrays (layout
        // invariants); the columns are this kind's; the page is allocated
        // (caller).
        unsafe {
            let keys = Slots::with_stride(
                self.page().byte_add(area.keys_offset).cast(),
                area.capacity,
                area.key_stride,
            );
            KeySlots::new(keys, self.key_columns::<Kind>(), prefix_len)
        }
    }

    /// An internal node's separators (D17): its keys, in a map without inline
    /// keys, or their windows, in a map with them, whose separator keys are
    /// heads (D49).
    ///
    /// # Safety
    ///
    /// The node is an internal node that has not been freed; nothing
    /// writes its header concurrently.
    #[inline(always)]
    pub(super) unsafe fn separator_slots(self) -> SeparatorSlots<K, V, S> {
        // SAFETY: forwarded; the node is internal (caller).
        unsafe {
            if Self::has_separator_keys_in_heads() {
                SeparatorSlots::Windows {
                    columns: self.key_columns::<Internal<K, V, S>>(),
                    prefix_len: self.prefix_len().unwrap_or(0),
                }
            } else {
                SeparatorSlots::Keys(self.key_slots::<Internal<K, V, S>>())
            }
        }
    }

    /// The arrays a node of kind `Kind` keeps beside its keys
    /// ([`KeyColumns`]): its inline keys, at its plan's width, and its
    /// length tags (D36), if its plan keeps them and its layout has room for
    /// them in the padding before its head and first key. Reads nothing
    /// from the page.
    ///
    /// # Safety
    ///
    /// The page is allocated, for a node of kind `Kind`.
    #[inline(always)]
    pub(super) unsafe fn key_columns<Kind: NodeKind>(self) -> KeyColumns {
        let plan = Self::plan::<Kind>();
        let area = Self::key_area::<Kind>();
        let length_tags_offset = if plan.has_length_tags() {
            Self::leaf_layout().length_tags_offset
        } else {
            None
        };
        // SAFETY: the inline keys' offset is aligned for inline keys of any
        // width, and `capacity` of them from there stay within a page of the
        // node's kind, `Kind`, clear of its other arrays, as do the length
        // tags, which only a leaf's layout has room for (layout
        // invariants); the inline keys are of the width the layout was made
        // for; the page is allocated (caller).
        unsafe {
            let inline_keys =
                Option::zip(plan.inline_width(), area.inline_keys_offset).map(|(width, offset)| {
                    InlineKeySlots::new(width, self.page().byte_add(offset), area.capacity)
                });
            let length_tags = length_tags_offset
                .map(|offset| Slots::new(self.page().byte_add(offset).cast(), area.capacity));
            KeyColumns::new(inline_keys, length_tags, area.capacity)
        }
    }

    /// A leaf's head key slot: the key's place of entry 0, before the head,
    /// where the key of the separator left of the leaf is (D49).
    ///
    /// # Safety
    ///
    /// The node is a leaf that has not been freed, in a map whose
    /// separator keys are heads.
    pub(super) unsafe fn head_key_slot(self) -> NonNull<K> {
        let offset = Self::leaf_layout()
            .head_key_offset
            .expect("a leaf with inline keys has a head key slot");
        // SAFETY: the page is a leaf's (caller), of this layout, whose entry
        // 0 starts with a key's place, aligned for `K` and in the page, clear
        // of every other array (layout invariants); the page is allocated
        // (caller).
        unsafe { self.page().byte_add(offset).cast() }
    }

    /// A leaf's value slots: `capacity` of them, value `i` beside key `i`
    /// (D29). They are [`Self::head_and_value_slots`] without the head:
    /// value slot `i` is head-and-value slot `i + 1`.
    ///
    /// # Safety
    ///
    /// The node is a leaf that has not been freed.
    pub(super) unsafe fn value_slots(self) -> Slots<V> {
        let layout = Self::leaf_layout();
        // SAFETY: the page is a leaf's (caller), of this layout. Value 0 is
        // a stride past the head, both aligned for `V`, and so is the
        // stride, and `capacity` values from there stay in the page and
        // clear of the keys (layout invariants); the page is allocated
        // (caller).
        unsafe {
            Slots::with_stride(
                self.page()
                    .byte_add(layout.head_offset + layout.value_stride)
                    .cast(),
                layout.keys.capacity,
                layout.value_stride,
            )
        }
    }

    /// A leaf's head and value slots as one array, its trailing slots:
    /// `capacity + 1` of them, slot 0 the head and slot `i + 1` value `i`
    /// (D17). Moves that carry a head along with values use it.
    ///
    /// # Safety
    ///
    /// The node is a leaf that has not been freed.
    pub(super) unsafe fn head_and_value_slots(self) -> Slots<V> {
        let layout = Self::leaf_layout();
        // SAFETY: the page is a leaf's (caller), of this layout: the head
        // and `capacity` values after it, a stride apart, are aligned for
        // `V`, in the page and clear of the keys (layout invariants); the
        // page is allocated (caller).
        unsafe {
            Slots::with_stride(
                self.page().byte_add(layout.head_offset).cast(),
                layout.keys.capacity + 1,
                layout.value_stride,
            )
        }
    }

    /// The `capacity + 1` edge slots of an internal node, its trailing
    /// slots in a dense array: edge `i + 1` leads right of key `i` (D32).
    ///
    /// # Safety
    ///
    /// The node is an internal node that has not been freed.
    pub(super) unsafe fn edge_slots(self) -> Slots<NodePtr<K, V, S>> {
        let layout = Self::internal_layout();
        // SAFETY: the page is an internal node's (caller), of this layout.
        // The edges' offset and stride are aligned for an edge, a
        // `NodePtr` whatever the strategy, and the `capacity + 1` edges stay in the
        // page and clear of the keys (layout invariants); the page is
        // allocated (caller).
        unsafe {
            Slots::with_stride(
                self.page().byte_add(layout.edges_offset).cast(),
                layout.keys.capacity + 1,
                layout.edge_stride,
            )
        }
    }
}

/// Lifetime-erased type identity, and the views of a value that it proves
/// sound: how [`Auto`] recognizes std byte strings and reads their bytes,
/// how it recognizes std integers and uses their [`InlineKey`] impls, and
/// how [`WithInlineKey`] uses a lookup key as a key (D18, D31).
///
/// [`InlineKey`]: crate::InlineKey
///
/// [`Auto`]: crate::Auto
/// [`WithInlineKey`]: crate::WithInlineKey
mod type_identity {
    use alloc::boxed::Box;
    use alloc::string::String;
    use alloc::vec::Vec;
    use core::any::TypeId;
    use core::borrow::Borrow;
    use core::marker::PhantomData;
    use core::{mem, ptr, slice};

    use crate::inline_key::{InlineKey, InlineWidth, InlineWord};

    /// A type's identity with its lifetimes erased: `&'a str` and
    /// `&'static str` have the same one. `TypeId::of` needs `'static` types,
    /// and keys need not be.
    ///
    /// dtolnay's `typeid` trick, vendored (D2; MIT OR Apache-2.0). Comparing
    /// two of them folds to a constant after monomorphization.
    #[derive(Clone, Copy, PartialEq, Eq)]
    struct ErasedTypeId(TypeId);

    impl ErasedTypeId {
        #[inline(always)]
        fn of<T: ?Sized>() -> Self {
            /// `Any`, with the `'static` bound on the method instead of the
            /// trait, so that `PhantomData<T>` implements it for any `T`.
            trait NonStaticAny {
                fn type_id(&self) -> TypeId
                where
                    Self: 'static;
            }

            impl<T: ?Sized> NonStaticAny for PhantomData<T> {
                #[inline(always)]
                fn type_id(&self) -> TypeId
                where
                    Self: 'static,
                {
                    TypeId::of::<T>()
                }
            }

            let phantom = PhantomData::<T>;
            // SAFETY: the two references differ only in the trait object's
            // lifetime bound, not in layout or vtable. The `'static` bound
            // they claim only lets `type_id` call `TypeId::of::<T>()`, which
            // reads nothing and returns the identity of `T` as compiled,
            // where lifetimes no longer exist. Nothing borrowed is used
            // beyond this call.
            let object = unsafe {
                mem::transmute::<&dyn NonStaticAny, &(dyn NonStaticAny + 'static)>(&phantom)
            };
            Self(object.type_id())
        }

        /// Whether `T` is `U`, lifetimes aside.
        #[inline(always)]
        fn is<T: ?Sized, U: ?Sized>() -> bool {
            Self::of::<T>() == Self::of::<U>()
        }
    }

    /// Proof that `T` is one of the std byte-string types that [`Auto`]
    /// gives inline keys, lifetimes aside, and how it holds its bytes. Every
    /// one of them orders like its bytes.
    ///
    /// [`Auto`]: crate::Auto
    pub(in crate::tree) struct ByteString<T: ?Sized> {
        representation: ByteRepresentation,
        _type: PhantomData<fn(&T)>,
    }

    impl<T: ?Sized> Clone for ByteString<T> {
        fn clone(&self) -> Self {
            *self
        }
    }

    impl<T: ?Sized> Copy for ByteString<T> {}

    /// How a recognized byte string holds its bytes.
    #[derive(Clone, Copy)]
    enum ByteRepresentation {
        /// The value is its bytes: `str` and `[u8]`.
        Slice,
        /// The value is its bytes, as many as every value of the type has:
        /// `[u8; 16]`, `[u8; 20]` and `[u8; 32]`.
        Array,
        String,
        ByteVec,
        BoxedStr,
        BoxedBytes,
        StrRef,
        BytesRef,
    }

    impl<T: ?Sized> ByteString<T> {
        /// The proof, if `T` is `String`, `str`, `Vec<u8>`, `[u8]`,
        /// `Box<str>`, `Box<[u8]>`, `&str`, `&[u8]`, `[u8; 16]`, `[u8; 20]`
        /// or `[u8; 32]`, lifetimes aside. Folds to a constant after
        /// monomorphization.
        ///
        /// Unoptimized builds compare type identities at run time, on every
        /// call, so the candidates are first narrowed by what is known
        /// without comparing any: whether `T` is unsized, and whether it
        /// owns memory to drop.
        #[inline(always)]
        pub(in crate::tree) fn recognize() -> Option<Self> {
            let is_unsized = mem::size_of::<&T>() > mem::size_of::<&u8>();
            let representation = if is_unsized {
                if ErasedTypeId::is::<T, str>() || ErasedTypeId::is::<T, [u8]>() {
                    ByteRepresentation::Slice
                } else {
                    return None;
                }
            } else if mem::needs_drop::<T>() {
                if ErasedTypeId::is::<T, String>() {
                    ByteRepresentation::String
                } else if ErasedTypeId::is::<T, Vec<u8>>() {
                    ByteRepresentation::ByteVec
                } else if ErasedTypeId::is::<T, Box<str>>() {
                    ByteRepresentation::BoxedStr
                } else if ErasedTypeId::is::<T, Box<[u8]>>() {
                    ByteRepresentation::BoxedBytes
                } else {
                    return None;
                }
            } else if ErasedTypeId::is::<T, [u8; 16]>()
                || ErasedTypeId::is::<T, [u8; 20]>()
                || ErasedTypeId::is::<T, [u8; 32]>()
            {
                ByteRepresentation::Array
            } else if ErasedTypeId::is::<T, &str>() {
                ByteRepresentation::StrRef
            } else if ErasedTypeId::is::<T, &[u8]>() {
                ByteRepresentation::BytesRef
            } else {
                return None;
            };
            Some(Self {
                representation,
                _type: PhantomData,
            })
        }

        /// Whether a value is its bytes, as an array is, rather than a
        /// handle to bytes elsewhere: two keys' bytes then compare without
        /// reading past the node that holds them.
        #[inline(always)]
        pub(in crate::tree) fn holds_its_bytes(self) -> bool {
            match self.representation {
                ByteRepresentation::Slice | ByteRepresentation::Array => true,
                ByteRepresentation::String
                | ByteRepresentation::ByteVec
                | ByteRepresentation::BoxedStr
                | ByteRepresentation::BoxedBytes
                | ByteRepresentation::StrRef
                | ByteRepresentation::BytesRef => false,
            }
        }

        /// The bytes of `value`. Their length comes from `value` itself: for
        /// the types that hold their bytes elsewhere, from its length field,
        /// without reading what it points to.
        #[inline(always)]
        pub(in crate::tree) fn bytes<'v>(self, value: &'v T) -> &'v [u8] {
            let size = mem::size_of_val(value);
            let value = ptr::from_ref(value);
            // SAFETY: `recognize` found `T` to be the type `representation`
            // names, lifetimes aside, so `value` points to a valid one, and
            // each arm reads it as that type for `'v`, the borrow it came
            // with. In place, its `size` bytes are initialized `u8`s. A
            // borrowed string `&'b str` or `&'b [u8]` is read as `&'v str`
            // or `&'v [u8]`, which it outlives: `&'v &'b str` is well-formed
            // only if `'b: 'v`.
            unsafe {
                match self.representation {
                    ByteRepresentation::Slice | ByteRepresentation::Array => {
                        slice::from_raw_parts(value.cast::<u8>(), size)
                    }
                    ByteRepresentation::String => (&*value.cast::<String>()).as_bytes(),
                    ByteRepresentation::ByteVec => (&*value.cast::<Vec<u8>>()).as_slice(),
                    ByteRepresentation::BoxedStr => (&*value.cast::<Box<str>>()).as_bytes(),
                    ByteRepresentation::BoxedBytes => &*value.cast::<Box<[u8]>>(),
                    ByteRepresentation::StrRef => (*value.cast::<&'v str>()).as_bytes(),
                    ByteRepresentation::BytesRef => *value.cast::<&'v [u8]>(),
                }
            }
        }
    }

    /// Proof that `T` is one of the std integer types that [`Auto`] gives
    /// inline keys, `u64`, `i64`, `u128` or `i128`, with that type's
    /// [`InlineKey`] impl as one for `T`.
    ///
    /// [`Auto`]: crate::Auto
    pub(in crate::tree) struct StdInteger<T> {
        inline_key_at: fn(&T, usize) -> InlineWord,
        width: InlineWidth,
    }

    impl<T> Clone for StdInteger<T> {
        fn clone(&self) -> Self {
            *self
        }
    }

    impl<T> Copy for StdInteger<T> {}

    impl<T> StdInteger<T> {
        /// The proof, if `T` is `u64`, `i64`, `u128` or `i128`. Folds to a
        /// constant after monomorphization.
        ///
        /// Unoptimized builds compare type identities at run time, so the
        /// candidates are first narrowed by `T`'s size.
        #[inline(always)]
        pub(in crate::tree) fn recognize() -> Option<Self> {
            // SAFETY: each arm passes the type that `T` was just found to be;
            // integers have no lifetimes to erase.
            unsafe {
                match mem::size_of::<T>() {
                    8 if ErasedTypeId::is::<T, u64>() => Some(Self::of::<u64>()),
                    8 if ErasedTypeId::is::<T, i64>() => Some(Self::of::<i64>()),
                    16 if ErasedTypeId::is::<T, u128>() => Some(Self::of::<u128>()),
                    16 if ErasedTypeId::is::<T, i128>() => Some(Self::of::<i128>()),
                    _ => None,
                }
            }
        }

        /// `Integer`'s impl, as one for `T`.
        ///
        /// # Safety
        ///
        /// `T` is `Integer`.
        #[inline(always)]
        unsafe fn of<Integer: InlineKey>() -> Self {
            let inline_key_at: fn(&Integer, usize) -> InlineWord = Integer::inline_key_at;
            // SAFETY: `T` is `Integer` (the caller's promise), so the two
            // function pointer types are one type.
            let inline_key_at = unsafe {
                mem::transmute::<fn(&Integer, usize) -> InlineWord, fn(&T, usize) -> InlineWord>(
                    inline_key_at,
                )
            };
            Self {
                inline_key_at,
                width: Integer::WIDTH,
            }
        }

        /// The integer's [`InlineKey::inline_key_at`].
        #[inline(always)]
        pub(in crate::tree) fn inline_key_at(self) -> fn(&T, usize) -> InlineWord {
            self.inline_key_at
        }

        /// The integer's [`InlineKey::WIDTH`].
        #[inline(always)]
        pub(in crate::tree) fn width(self) -> InlineWidth {
            self.width
        }
    }

    /// Proof that a lookup key type `Q` is the key type `K` itself,
    /// lifetimes included.
    pub(in crate::tree) struct SameType<K, Q: ?Sized>(PhantomData<fn(&Q) -> &K>);

    impl<K: Borrow<Q>, Q: ?Sized> SameType<K, Q> {
        /// The proof, if `Q` is `K` lifetimes aside. Then they are the same
        /// type, lifetimes included, as `K: Borrow<Q>` holds: the only impl
        /// that can prove `Borrow<Q>` for a type that is `Q` lifetimes aside
        /// is std's `impl<T: ?Sized> Borrow<T> for T`, because any other
        /// would overlap with it (coherence ignores lifetimes) and is
        /// rejected; and that impl makes them one type. Folds to a constant
        /// after monomorphization.
        #[inline(always)]
        pub(in crate::tree) fn recognize() -> Option<Self> {
            ErasedTypeId::is::<K, Q>().then_some(Self(PhantomData))
        }

        /// `key` as the key type it is.
        #[inline(always)]
        pub(in crate::tree) fn cast(self, key: &Q) -> &K {
            // SAFETY: `Q` is `K`, lifetimes included (`check`), so `key`
            // points to a valid `K`, borrowed for as long as it says.
            unsafe { &*ptr::from_ref(key).cast::<K>() }
        }
    }
}

#[cfg(test)]
pub(in crate::tree) use test_pages::TestPages;

#[cfg(test)]
mod test_pages {
    use core::alloc::Layout;
    use core::cell::RefCell;
    use core::marker::PhantomData;
    use core::ptr::NonNull;
    use std::alloc::{alloc, dealloc, handle_alloc_error};
    use std::vec::Vec;

    use super::{FreshPage, PageByType, PageType};
    use crate::inline_key::{Auto, InlineKeyStrategy};
    use crate::tree::node::internal::Internal;
    use crate::tree::node::leaf::Leaf;
    use crate::tree::node::root_page::RootPage;

    /// A page allocator for tests: hands out pages of every type, for a
    /// map with keys `K`, values `V` and strategy `S`, from the global
    /// allocator, takes released pages back for reuse, and frees them when
    /// dropped.
    ///
    /// It keeps each page's layout: a released page is handed out again only
    /// for a kind of the same layout, and a page released under a kind of
    /// another layout than it was allocated with panics.
    ///
    /// Pages still out when it is dropped (owned by a leaked node) are never
    /// freed, so nothing can dangle; tests check
    /// [`TestPages::assert_all_released`] to catch such leaks.
    pub(in crate::tree) struct TestPages<K, V, S = Auto> {
        /// Every page this pool allocated, with its layout.
        allocated: RefCell<Vec<(NonNull<u8>, Layout)>>,
        /// Released pages, with their layouts, ready for reuse.
        released: RefCell<Vec<(NonNull<u8>, Layout)>>,
        _marker: PhantomData<fn() -> (K, V)>,
        _strategy: PhantomData<fn() -> S>,
    }

    impl<K, V, S: InlineKeyStrategy<K>> TestPages<K, V, S> {
        pub(in crate::tree) fn new() -> Self {
            Self {
                allocated: RefCell::new(Vec::new()),
                released: RefCell::new(Vec::new()),
                _marker: PhantomData,
                _strategy: PhantomData,
            }
        }

        /// A page of type `P`, reusing the most recently released page of
        /// that type's layout if any.
        pub(in crate::tree) fn fresh<P: PageType>(&self) -> FreshPage<P> {
            let layout = FreshPage::<P>::layout();
            let mut released = self.released.borrow_mut();
            let reusable = (released.iter()).rposition(|&(_, page_layout)| page_layout == layout);
            let page = match reusable {
                Some(index) => released.remove(index).0,
                None => {
                    // SAFETY: `layout` has a non-zero size (at least 512 B).
                    let raw = unsafe { alloc(layout) };
                    let page = NonNull::new(raw).unwrap_or_else(|| handle_alloc_error(layout));
                    self.allocated.borrow_mut().push((page, layout));
                    page
                }
            };
            // SAFETY: allocated by this pool from the global allocator with
            // `layout`, `P`'s, and either fresh or released, so nothing
            // else owns it. It is freed only in `drop`, and only if it was
            // released again by then.
            unsafe { FreshPage::from_raw(page) }
        }

        /// [`Self::fresh`], for a leaf.
        pub(in crate::tree) fn fresh_leaf(&self) -> FreshPage<Leaf<K, V, S>> {
            self.fresh()
        }

        /// [`Self::fresh`], for an internal node.
        pub(in crate::tree) fn fresh_internal(&self) -> FreshPage<Internal<K, V, S>> {
            self.fresh()
        }

        /// Takes a page of any type back, and scribbles over it so that
        /// nothing can rely on what it held.
        ///
        /// # Panics
        ///
        /// If the page is not one of this pool's, was allocated with another
        /// layout than its kind's, or is released twice.
        pub(in crate::tree) fn release(&self, page: impl Into<PageByType<K, V, S>>) {
            let (page, layout) = match page.into() {
                PageByType::Leaf(page) => (page.into_raw(), FreshPage::<Leaf<K, V, S>>::layout()),
                PageByType::Internal(page) => {
                    (page.into_raw(), FreshPage::<Internal<K, V, S>>::layout())
                }
                PageByType::Root(page) => {
                    (page.into_raw(), FreshPage::<RootPage<K, V, S>>::layout())
                }
            };
            assert!(
                self.allocated.borrow().contains(&(page, layout)),
                "released a page from elsewhere, or with another layout"
            );
            assert!(
                !(self.released.borrow().iter()).any(|&(released, _)| released == page),
                "released a page twice"
            );
            // SAFETY: the page was allocated by this pool with `layout`, and
            // as a `FreshPage` it is used by nothing else.
            unsafe { page.write_bytes(0xA5, layout.size()) };
            self.released.borrow_mut().push((page, layout));
        }

        /// The number of pages allocated so far, in use or not.
        pub(in crate::tree) fn allocated_count(&self) -> usize {
            self.allocated.borrow().len()
        }

        /// The number of pages handed out and not released.
        pub(in crate::tree) fn in_use_count(&self) -> usize {
            self.allocated.borrow().len() - self.released.borrow().len()
        }

        pub(in crate::tree) fn assert_all_released(&self) {
            assert_eq!(
                self.released.borrow().len(),
                self.allocated.borrow().len(),
                "pages were not released"
            );
        }
    }

    impl<K, V, S> Drop for TestPages<K, V, S> {
        fn drop(&mut self) {
            for (page, layout) in self.released.get_mut().drain(..) {
                // SAFETY: allocated by `fresh` with `layout`; a released page
                // has no owner left, and each is freed once.
                unsafe { dealloc(page.as_ptr(), layout) };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use core::mem::{align_of, size_of};
    use core::ops::Range;
    use std::borrow::ToOwned;
    use std::boxed::Box;
    use std::string::String;
    use std::vec::Vec;

    use super::{ByteString, FreshPage, NodeHeader, NodeKind, NodePtr, SameType, TestPages};
    use crate::inline_key::{Auto, InlineKeyStrategy, InlineWidth, InlineWord, WithInlineKey};
    use crate::tree::node::internal::Internal;
    use crate::tree::node::layout::NodeLayout;
    use crate::tree::node::leaf::Leaf;
    use crate::tree::node::slots::{InlineKeySlots, Slots};
    use crate::tree::test_support::{CacheLineAligned, PageAligned, Tracked};

    /// The bytes `[start, end)` that an array of slots spans, and those of
    /// each of its slots, which may lie a stride apart (D29).
    struct SlotRange(Range<usize>, Vec<Range<usize>>);

    impl SlotRange {
        /// The bytes of the page of `node`, a node of kind `Kind`, checked to
        /// be aligned to the kind's layout.
        fn page_bytes<K, V, S: InlineKeyStrategy<K>, Kind: NodeKind>(
            node: NodePtr<K, V, S>,
        ) -> Range<usize> {
            let layout = FreshPage::<Kind>::layout();
            let start = node.page().addr().get();
            assert!(start.is_multiple_of(layout.align()));
            start..start + layout.size()
        }

        /// The bytes of all `capacity` slots, each checked to be aligned.
        fn of<T>(slots: Slots<T>, capacity: usize) -> Self {
            let each: Vec<Range<usize>> = (0..capacity)
                .map(|index| {
                    // SAFETY: `index < capacity`; nothing is dereferenced.
                    let start = unsafe { slots.slot(index) }.addr().get();
                    assert!(start.is_multiple_of(align_of::<T>()));
                    start..start + size_of::<T>()
                })
                .collect();
            let start = each.iter().map(|slot| slot.start).min().unwrap();
            let end = each.iter().map(|slot| slot.end).max().unwrap();
            Self(start..end, each)
        }

        /// The bytes of all `capacity` inline-key slots, at their width.
        fn of_inline_keys(slots: InlineKeySlots, capacity: usize) -> Self {
            match slots {
                InlineKeySlots::Two(slots) => Self::of(slots, capacity),
                InlineKeySlots::Four(slots) => Self::of(slots, capacity),
                InlineKeySlots::Eight(slots) => Self::of(slots, capacity),
            }
        }

        fn assert_within(&self, page: &Range<usize>) {
            assert!(page.start <= self.0.start && self.0.end <= page.end);
        }

        /// Checks that no slot of `self` overlaps one of `other`.
        fn assert_disjoint(&self, other: &Self) {
            for a in &self.1 {
                for b in &other.1 {
                    assert!(a.end <= b.start || b.end <= a.start);
                }
            }
        }
    }

    /// Checks the slot arrays of both node kinds, each in a page of its own
    /// kind: inside the page, aligned, after the header, and no slot over
    /// another, with a leaf's values its head-and-value slots past the head
    /// (D17), and without inline keys, the keys at the same offset in both
    /// kinds. With inline keys, an internal node keeps no keys, and a leaf's
    /// head key slot is entry 0's key (D49).
    fn assert_slots_are_aligned_and_disjoint<K, V, S: InlineKeyStrategy<K>>() {
        let pages = TestPages::<K, V, S>::new();
        let leaf = NodePtr::from_fresh_page(pages.fresh::<Leaf<K, V, S>>());
        let internal = NodePtr::from_fresh_page(pages.fresh::<Internal<K, V, S>>());
        let leaf_page = SlotRange::page_bytes::<K, V, S, Leaf<K, V, S>>(leaf);
        let internal_page = SlotRange::page_bytes::<K, V, S, Internal<K, V, S>>(internal);
        let leaf_capacity = NodePtr::<K, V, S>::capacity::<Leaf<K, V, S>>();
        let internal_capacity = NodePtr::<K, V, S>::capacity::<Internal<K, V, S>>();

        let keeps_keys = NodePtr::<K, V, S>::internal_layout().keeps_keys();
        assert_eq!(
            keeps_keys,
            !NodePtr::<K, V, S>::has_separator_keys_in_heads()
        );
        // SAFETY: both pages are allocated, and each is asked for the slots
        // of its own kind, the internal node for keys only if it keeps them;
        // nothing is dereferenced.
        let (leaf_keys, values, head_and_values, internal_keys, internal_columns, edges) = unsafe {
            (
                leaf.key_slots::<Leaf<K, V, S>>(),
                SlotRange::of(leaf.value_slots(), leaf_capacity),
                SlotRange::of(leaf.head_and_value_slots(), leaf_capacity + 1),
                keeps_keys.then(|| internal.key_slots::<Internal<K, V, S>>()),
                internal.key_columns::<Internal<K, V, S>>(),
                SlotRange::of(internal.edge_slots(), internal_capacity + 1),
            )
        };
        assert_eq!(
            leaf_keys.inline_keys().map(InlineKeySlots::width),
            NodePtr::<K, V, S>::inline_width::<Leaf<K, V, S>>()
        );
        assert_eq!(
            internal_columns.inline_keys().map(InlineKeySlots::width),
            NodePtr::<K, V, S>::inline_width::<Internal<K, V, S>>()
        );
        let leaf_inline_keys =
            (leaf_keys.inline_keys()).map(|slots| SlotRange::of_inline_keys(slots, leaf_capacity));
        let internal_inline_keys = (internal_columns.inline_keys())
            .map(|slots| SlotRange::of_inline_keys(slots, internal_capacity));
        // SAFETY: as above; the leaf's head key slot only where separator
        // keys are heads.
        let head_key = NodePtr::<K, V, S>::has_separator_keys_in_heads().then(|| unsafe {
            let slot = leaf.head_key_slot();
            assert!(slot.addr().get().is_multiple_of(align_of::<K>()));
            let start = slot.addr().get();
            SlotRange(
                start..start + size_of::<K>(),
                std::vec![start..start + size_of::<K>()],
            )
        });
        let leaf_keys = SlotRange::of(leaf_keys.keys(), leaf_capacity);
        let internal_keys = internal_keys.map(|keys| SlotRange::of(keys.keys(), internal_capacity));

        // SAFETY: allocated nodes of the requested kind; only addresses
        // and the allocation-initialized tag slots are inspected.
        unsafe {
            assert!(
                internal
                    .key_columns::<Internal<K, V, S>>()
                    .length_tags()
                    .is_none()
            );
            if let Some(tags) = leaf.key_columns::<Leaf<K, V, S>>().length_tags() {
                assert!(
                    tags.run(0, leaf_capacity)
                        .as_ref()
                        .iter()
                        .all(|&tag| tag == 0)
                );
                let tags = SlotRange::of(tags, leaf_capacity);
                tags.assert_within(&leaf_page);
                tags.assert_disjoint(&leaf_keys);
                tags.assert_disjoint(&head_and_values);
                tags.assert_disjoint(leaf_inline_keys.as_ref().unwrap());
                if let Some(head_key) = &head_key {
                    tags.assert_disjoint(head_key);
                }
            }
        }

        for slots in [&leaf_keys, &values, &head_and_values] {
            slots.assert_within(&leaf_page);
        }
        if let Some(head_key) = &head_key {
            head_key.assert_within(&leaf_page);
            head_key.assert_disjoint(&leaf_keys);
            head_key.assert_disjoint(&head_and_values);
            head_key.assert_disjoint(leaf_inline_keys.as_ref().unwrap());
        }
        edges.assert_within(&internal_page);
        if let Some(internal_keys) = &internal_keys {
            internal_keys.assert_within(&internal_page);
            internal_keys.assert_disjoint(&edges);
        }
        let keys_offset = leaf_keys.0.start - leaf_page.start;
        assert!(size_of::<NodeHeader>() <= keys_offset);
        assert_eq!(values.1, head_and_values.1[1..]);
        leaf_keys.assert_disjoint(&head_and_values);
        match (leaf_inline_keys, internal_inline_keys) {
            (None, None) => {
                let internal_keys = internal_keys.expect("a node without inline keys keeps keys");
                assert_eq!(internal_keys.0.start - internal_page.start, keys_offset);
            }
            (Some(leaf_inline_keys), Some(internal_inline_keys)) => {
                leaf_inline_keys.assert_within(&leaf_page);
                internal_inline_keys.assert_within(&internal_page);
                // Right after the header, and before the keys.
                assert_eq!(leaf_inline_keys.0.start - leaf_page.start, 8);
                assert_eq!(internal_inline_keys.0.start - internal_page.start, 8);
                assert!(leaf_inline_keys.0.end <= leaf_keys.0.start);
                assert!(
                    internal_keys.is_none(),
                    "a node with inline keys keeps no keys"
                );
                leaf_inline_keys.assert_disjoint(&head_and_values);
                internal_inline_keys.assert_disjoint(&edges);
            }
            _ => unreachable!("both kinds have inline keys or neither"),
        }

        // SAFETY: the nodes hold nothing, each is of the kind it is released
        // as, and nothing uses them afterwards.
        unsafe {
            pages.release(leaf.into_fresh_page::<Leaf<K, V, S>>());
            pages.release(internal.into_fresh_page::<Internal<K, V, S>>());
        }
        pages.assert_all_released();
    }

    #[test]
    fn slots_are_aligned_and_disjoint() {
        assert_slots_are_aligned_and_disjoint::<u64, u64, Auto>();
        assert_slots_are_aligned_and_disjoint::<u8, (), Auto>();
        assert_slots_are_aligned_and_disjoint::<(), (), Auto>();
        assert_slots_are_aligned_and_disjoint::<u64, [u8; 1000], Auto>();
        assert_slots_are_aligned_and_disjoint::<[u8; 77], (), Auto>();
        assert_slots_are_aligned_and_disjoint::<CacheLineAligned, u64, Auto>();
        assert_slots_are_aligned_and_disjoint::<u8, CacheLineAligned, Auto>();
        assert_slots_are_aligned_and_disjoint::<u64, PageAligned, Auto>();
        assert_slots_are_aligned_and_disjoint::<String, Vec<u8>, Auto>();
        assert_slots_are_aligned_and_disjoint::<String, u64, Auto>();
        assert_slots_are_aligned_and_disjoint::<String, (), Auto>();
        assert_slots_are_aligned_and_disjoint::<String, [u64; 5], Auto>();
        assert_slots_are_aligned_and_disjoint::<Vec<u8>, u64, Auto>();
        assert_slots_are_aligned_and_disjoint::<String, u64, WithInlineKey>();
        assert_slots_are_aligned_and_disjoint::<&str, PageAligned, Auto>();
        assert_slots_are_aligned_and_disjoint::<[u8; 20], (), Auto>();
        assert_slots_are_aligned_and_disjoint::<[u8; 16], u64, Auto>();
        assert_slots_are_aligned_and_disjoint::<[u8; 32], u64, Auto>();
        assert_slots_are_aligned_and_disjoint::<Tracked<'_>, Tracked<'_>, WithInlineKey>();
        assert_slots_are_aligned_and_disjoint::<Tracked<'_>, (), WithInlineKey>();
        // Two-byte inline keys.
        assert_slots_are_aligned_and_disjoint::<u64, u64, WithInlineKey>();
        assert_slots_are_aligned_and_disjoint::<i64, (), WithInlineKey>();
        assert_slots_are_aligned_and_disjoint::<u128, [u8; 1000], WithInlineKey>();
        assert_slots_are_aligned_and_disjoint::<i128, CacheLineAligned, WithInlineKey>();
    }

    /// Zero overhead without inline keys (D18): an 8-byte key that `Auto`
    /// doesn't recognize, `[u64; 1]`, keeps the layout `u64` → `u64` had
    /// before inline keys; std byte strings and integers get the layout
    /// with them.
    #[test]
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
    fn only_inline_keys_change_the_layout() {
        assert!(!NodePtr::<[u64; 1], u64, Auto>::has_inline_keys());
        assert_eq!(
            NodePtr::<[u64; 1], u64, Auto>::leaf_layout(),
            NodeLayout::<[u64; 1], u64>::PLAIN_LEAF
        );
        assert_eq!(
            NodePtr::<[u64; 1], u64, Auto>::internal_layout(),
            NodeLayout::<[u64; 1], u64>::PLAIN_INTERNAL
        );
        // 1 KB, 63 keys at offset 8, as before inline keys, and the head, or
        // the edges, right after them, at 8 + 63·8 = 512.
        let leaf = NodePtr::<[u64; 1], u64, Auto>::leaf_layout();
        let internal = NodePtr::<[u64; 1], u64, Auto>::internal_layout();
        assert_eq!((leaf.page.size(), leaf.page.align()), (1024, 1024));
        assert_eq!((internal.page.size(), internal.page.align()), (1024, 1024));
        assert_eq!(leaf.keys, internal.keys);
        assert_eq!(
            (
                leaf.keys.capacity,
                leaf.keys.keys_offset,
                leaf.keys.inline_keys_offset
            ),
            (63, 8, None)
        );
        assert_eq!((leaf.head_offset, internal.edges_offset), (512, 512));
        assert_eq!(size_of::<NodeHeader>(), 4);

        assert!(NodePtr::<String, u64, Auto>::has_inline_keys());
        assert_eq!(
            NodePtr::<String, u64, Auto>::leaf_layout(),
            NodeLayout::<String, u64>::EIGHT_BYTE_INLINE_LENGTH_TAGGED_LEAF
        );
        assert_eq!(
            NodePtr::<Vec<u8>, u64, Auto>::internal_layout(),
            NodeLayout::<Vec<u8>, u64>::FOUR_BYTE_INLINE_INTERNAL
        );
        assert_eq!(
            NodePtr::<String, u64, Auto>::inline_width::<Leaf<String, u64, Auto>>(),
            Some(InlineWidth::Eight)
        );
        assert_eq!(
            NodePtr::<String, u64, Auto>::inline_width::<Internal<String, u64, Auto>>(),
            Some(InlineWidth::Four)
        );
        assert_eq!(
            NodePtr::<String, u64, WithInlineKey>::internal_layout(),
            NodeLayout::<String, u64>::EIGHT_BYTE_INLINE_INTERNAL
        );
        assert!(NodePtr::<Tracked<'_>, u64, WithInlineKey>::has_inline_keys());
        assert!(!NodePtr::<Tracked<'_>, u64, Auto>::has_inline_keys());

        // Integers have two-byte inline keys, under `Auto` as under
        // `WithInlineKey`.
        assert!(NodePtr::<i64, u64, Auto>::has_inline_keys());
        assert_eq!(
            NodePtr::<u64, u64, Auto>::leaf_layout(),
            NodeLayout::<u64, u64>::TWO_BYTE_INLINE_LEAF
        );
        assert_eq!(
            NodePtr::<u64, u64, WithInlineKey>::leaf_layout(),
            NodeLayout::<u64, u64>::TWO_BYTE_INLINE_LEAF
        );
        assert_eq!(
            NodePtr::<i128, u64, WithInlineKey>::internal_layout(),
            NodeLayout::<i128, u64>::TWO_BYTE_INLINE_INTERNAL
        );
    }

    #[test]
    fn a_new_node_is_empty() {
        let pages = TestPages::<u32, u64>::new();
        let node = NodePtr::<u32, u64, Auto>::from_fresh_page(pages.fresh_leaf());
        // SAFETY: the page is allocated and holds a node.
        assert_eq!(unsafe { (node.len(), node.has_head()) }, (0, false));
        // SAFETY: as above.
        assert_eq!(unsafe { node.prefix_len() }, None);
        // SAFETY: the node is a leaf that holds nothing, and nothing uses it
        // afterwards.
        pages.release(unsafe { node.into_fresh_page::<Leaf<u32, u64, Auto>>() });
        pages.assert_all_released();
    }

    /// The header of a node with inline keys reserves the prefix length of
    /// 05-fence-key.md, at 0.
    #[test]
    fn a_new_node_with_inline_keys_has_no_prefix() {
        let pages = TestPages::<String, u64>::new();
        // A page full of stale bytes, which the header must overwrite.
        pages.release(pages.fresh_internal());
        let node = NodePtr::<String, u64, Auto>::from_fresh_page(pages.fresh_internal());
        // SAFETY: the page is allocated and holds a node.
        assert_eq!(unsafe { (node.len(), node.prefix_len()) }, (0, Some(0)));
        // SAFETY: the node is an internal node that holds nothing, and
        // nothing uses it afterwards.
        pages.release(unsafe { node.into_fresh_page::<Internal<String, u64, Auto>>() });
        pages.assert_all_released();
    }

    #[test]
    fn released_pages_are_handed_out_again() {
        let pages = TestPages::<u64, u64>::new();
        let first = pages.fresh_leaf().into_raw();
        let second = pages.fresh_leaf().into_raw();
        assert_ne!(first, second);
        // SAFETY: both came from `pages` as leaf pages, hold nothing, and
        // nothing else uses them.
        unsafe {
            pages.release(FreshPage::<Leaf<u64, u64, Auto>>::from_raw(first));
            pages.release(FreshPage::<Leaf<u64, u64, Auto>>::from_raw(second));
        }
        assert_eq!(pages.in_use_count(), 0);
        let reused = pages.fresh_leaf();
        assert_eq!(pages.in_use_count(), 1);
        assert_eq!(reused.into_raw(), second);
        // SAFETY: as above.
        pages.release(unsafe { FreshPage::<Leaf<u64, u64, Auto>>::from_raw(second) });
        pages.assert_all_released();
    }

    /// A released page serves a node of either kind whose layout it has, and
    /// no other.
    #[test]
    fn released_pages_are_handed_out_for_their_layout_only() {
        // Without inline keys, both kinds are 1 KB pages: a leaf's page can
        // hold an internal node.
        let pages = TestPages::<u32, u64>::new();
        pages.release(pages.fresh_leaf());
        pages.release(pages.fresh_internal());
        assert_eq!(pages.allocated_count(), 1);
        pages.assert_all_released();

        // A 13 KB leaf and a 1 KB internal node: neither reuses the other's.
        let pages = TestPages::<u64, [u8; 1000]>::new();
        pages.release(pages.fresh_leaf());
        let internal = pages.fresh_internal();
        assert_eq!((pages.allocated_count(), pages.in_use_count()), (2, 1));
        pages.release(internal);
        pages.release(pages.fresh_leaf());
        assert_eq!(pages.allocated_count(), 2);
        pages.assert_all_released();
    }

    /// The std byte strings `Auto` recognizes, read in place or through
    /// their pointer, lifetimes aside; and nothing else.
    #[test]
    fn byte_strings_are_recognized_by_type() {
        fn bytes_of<T: ?Sized>(value: &T) -> Option<&[u8]> {
            ByteString::<T>::recognize().map(|byte_string| byte_string.bytes(value))
        }
        let text = "bytes".to_owned();
        let expected = Some(text.as_bytes());
        assert_eq!(bytes_of::<String>(&text), expected);
        assert_eq!(bytes_of::<str>(&text), expected);
        assert_eq!(bytes_of::<Vec<u8>>(&text.clone().into_bytes()), expected);
        assert_eq!(bytes_of::<[u8]>(text.as_bytes()), expected);
        assert_eq!(bytes_of::<Box<str>>(&Box::from(text.as_str())), expected);
        assert_eq!(bytes_of::<Box<[u8]>>(&Box::from(text.as_bytes())), expected);
        // A borrowed string whose lifetime is not `'static`.
        assert_eq!(bytes_of::<&str>(&text.as_str()), expected);
        assert_eq!(bytes_of::<&[u8]>(&text.as_bytes()), expected);
        assert_eq!(bytes_of::<[u8; 16]>(&[3; 16]), Some(&[3; 16][..]));
        assert_eq!(bytes_of::<[u8; 20]>(&[4; 20]), Some(&[4; 20][..]));
        assert_eq!(bytes_of::<[u8; 32]>(&[5; 32]), Some(&[5; 32][..]));

        assert!(ByteString::<u64>::recognize().is_none());
        assert!(ByteString::<[u8; 8]>::recognize().is_none());
        assert!(ByteString::<Vec<i8>>::recognize().is_none());
        assert!(ByteString::<&String>::recognize().is_none());
        assert!(ByteString::<&&str>::recognize().is_none());
        assert!(ByteString::<Box<String>>::recognize().is_none());
        assert!(ByteString::<InlineWord>::recognize().is_none());
    }

    /// A lookup type is the key type only if it is that type: `K: Borrow<Q>`
    /// makes a borrowed key type's lifetimes match too.
    #[test]
    fn same_type_is_the_key_type_itself() {
        let text = "same".to_owned();
        let borrowed: &str = &text;
        let same = SameType::<&str, &str>::recognize().expect("the same type");
        assert_eq!(*same.cast(&borrowed), "same");
        assert!(SameType::<String, String>::recognize().is_some());
        assert!(SameType::<String, str>::recognize().is_none());
        assert!(SameType::<Vec<u8>, [u8]>::recognize().is_none());
        assert!(SameType::<&str, str>::recognize().is_none());
    }
}
