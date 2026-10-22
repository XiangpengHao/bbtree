//! Node pages: the allocation boundary ([`FreshPage`]) and raw access to an
//! initialized node's header and slot arrays ([`NodePtr`]). The root's page,
//! a node of either kind and the root area after it, is `root_page.rs`'s.

use alloc::alloc::handle_alloc_error;
use core::alloc::Layout;
use core::fmt;
use core::marker::PhantomData;
use core::mem::offset_of;
#[cfg(feature = "file")]
use core::ops::Range;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, AtomicU16, Ordering};

use super::super::interleaving::Interleaving;
use super::internal::Internal;
use super::latch::NodeLatch;
use super::layout::{Header, InternalLayout, KeyArea, LeafLayout, NodeLayout};
use super::leaf::Leaf;
use super::node_plan::NodePlan;
use super::root_page::RootPage;
use super::slots::{InlineKeySlots, KeyColumns, KeySlots, SeparatorSlots, Slots};
use crate::allocator::{AllocError, Allocator, PageKind};
use crate::inline_key::sealed::Sealed;
use crate::inline_key::{InlineKeyStrategy, InlineWidth};

/// What pages are allocated for: a leaf, an internal node ([`NodeKind`]s), or
/// the root ([`RootPage`]), each with a page layout of its own (D21). A page
/// is only ever used, and deallocated, with the layout it was allocated for.
///
/// # Safety
///
/// `page_layout` is the layout the code that uses pages of this type
/// accesses them with.
pub(crate) unsafe trait PageType: Sized {
    /// What pages of this type are for: which of the allocator's methods
    /// hands them out and takes them back.
    const KIND: PageKind;

    /// The allocation layout of every page of this type (D21) and strategy
    /// (D18), from the layout constants: a node kind's own, or for the root
    /// page, both kinds' and the root area, folded after monomorphization.
    /// Using it also runs those constants' compile-time assertions.
    fn page_layout() -> Layout;
}

/// A kind of node: [`Leaf`] or [`Internal`]. A page holds only nodes of the
/// kind it was allocated for, except the root page, which holds either
/// (`root_page.rs`). What depends on a node's layout (its capacity and where
/// its slots are) takes the kind as a type, so that it folds to the kind's
/// constants.
///
/// # Safety
///
/// `IS_LEAF` is whether nodes of this kind are leaves, and `page_layout` the
/// layout of their pages: the node code accesses a node of this kind with
/// that kind's offsets and capacity, from the start of its page.
pub(in crate::tree) unsafe trait NodeKind: PageType {
    const IS_LEAF: bool;

    /// Whether the layout of nodes of this kind has room for hash tags
    /// beside inline keys of `inline_width` (D36): only some leaves' does.
    fn has_room_for_hash_tags(inline_width: InlineWidth) -> bool;
}

/// An allocated page of type `P` ([`Leaf`], [`Internal`] or [`RootPage`]),
/// that holds no node.
///
/// A `FreshPage` owns its page, like a `Box` that has nothing in it. It is
/// the only way to create a node, and what a node turns back into once it is
/// freed, ready to be reused or deallocated. The kinds have different
/// layouts (D21), and so do maps whose keys have inline keys and those whose
/// keys have none (D18). The type carries all that decides the layout, so a
/// page only ever holds a node of the layout it was allocated for, and is
/// deallocated with the layout it was allocated with.
///
/// The type does not carry which allocator the page came from. It is the
/// map's: every operation that allocates or frees a page is given it, and
/// gives back to it only pages from it.
///
/// Invariant: the page comes from the map's allocator, for
/// [`PageType::page_layout`], and nothing else uses it.
pub(crate) struct FreshPage<P> {
    page: NonNull<u8>,
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
    /// Takes ownership of a page. Its contents are treated as uninitialized.
    ///
    /// # Safety
    ///
    /// - `page` was allocated by the map's allocator for
    ///   [`PageType::page_layout`], which aligns it (debug-checked).
    /// - Until the page is handed back by [`Self::into_raw`], it stays
    ///   allocated and is accessed only through the returned value and the
    ///   nodes built from it.
    unsafe fn from_raw(page: NonNull<u8>) -> Self {
        debug_assert!(
            page.addr().get().is_multiple_of(P::page_layout().align()),
            "node page is not aligned to its layout"
        );
        Self {
            page,
            _page_type: PhantomData,
        }
    }
}

impl<P> FreshPage<P> {
    /// Hands the page back, e.g. for deallocation by the map's allocator
    /// with [`PageType::page_layout`].
    pub(in crate::tree) fn into_raw(self) -> NonNull<u8> {
        self.page
    }
}

/// Where the tree's operations take pages from and give them back to: the
/// map's [`Allocator`], or, in a latched tree, the pages of one operation
/// (`tree/latched/pages.rs`), which hands out pages it took beforehand.
/// Every call names the page's kind, which picks the allocator's method. An
/// operation has nothing to do about a failed allocation: the map's
/// allocator ends the program, as std's collections do, and an operation
/// of a latched tree that might fail takes its pages before it changes
/// anything. The operations a latched tree runs take it; teardown, copies
/// and bulk builds, which only the map's own allocator serves, take that.
///
/// # Safety
///
/// A page `allocate_page` returns is valid for reads and writes of
/// `layout.size()` bytes, aligned to `layout.align()`, and in no other use
/// until it is given back to `deallocate_page`, with its kind. Neither
/// method unwinds.
pub(crate) unsafe trait PageAllocator {
    /// A page of `kind` for `layout`, of a non-zero size, with any contents.
    fn allocate_page(&self, kind: PageKind, layout: Layout) -> NonNull<u8>;

    /// Takes back the page at `page`, of `kind`.
    ///
    /// # Safety
    ///
    /// `page` was handed out by `allocate_page` of this allocator for
    /// `kind` and `layout`, and not given back since.
    unsafe fn deallocate_page(&self, kind: PageKind, page: NonNull<u8>, layout: Layout);
}

// SAFETY: the allocator's pages are valid and apart until given back, and
// it doesn't unwind (its contract), kind by kind; a failed allocation
// doesn't return.
unsafe impl<A: Allocator + ?Sized> PageAllocator for A {
    fn allocate_page(&self, kind: PageKind, layout: Layout) -> NonNull<u8> {
        kind.allocate(self, layout)
            .unwrap_or_else(|AllocError| handle_alloc_error(layout))
    }

    unsafe fn deallocate_page(&self, kind: PageKind, page: NonNull<u8>, layout: Layout) {
        // SAFETY: the caller's contract, which is the allocator's.
        unsafe { kind.deallocate(self, page, layout) }
    }
}

/// The allocation boundary (D8): one allocator call per page, for the
/// page's layout. A node's page is a multiple of its kind's page size in
/// the map's node config (D59): by default 1 KiB, or 512 bytes for internal
/// nodes that keep no keys (D49). It is aligned to that page size (D15), or
/// more if its keys or values need it, so the lowest bit of every node
/// pointer is free for its kind's tag ([`NodePtr`]). A root page is
/// aligned as the larger of the two kinds' (`root_page.rs`).
impl<P: PageType> FreshPage<P> {
    /// A page from `alloc`.
    pub(crate) fn allocate(alloc: &impl PageAllocator) -> Self {
        // The layout has a non-zero size: nodes are at least 64 B.
        let page = alloc.allocate_page(P::KIND, P::page_layout());
        // SAFETY: the page was just allocated by the map's allocator for
        // `P::page_layout()`, and nothing else has it.
        unsafe { Self::from_raw(page) }
    }

    /// A page from `alloc`, the map's allocator; or, if it has none to
    /// give, the layout it was asked for.
    pub(crate) fn try_allocate(alloc: &impl Allocator) -> Result<Self, Layout> {
        let layout = P::page_layout();
        let page = P::KIND
            .allocate(alloc, layout)
            .map_err(|AllocError| layout)?;
        // SAFETY: as in `allocate`.
        Ok(unsafe { Self::from_raw(page) })
    }

    /// Gives the page back to `alloc`, which it came from.
    pub(in crate::tree) fn deallocate(self, alloc: &impl PageAllocator) {
        let page = self.into_raw();
        // SAFETY: the page came from the map's allocator, `alloc`, for
        // `P::page_layout()`, the same every time for this type, and is used
        // by nothing else (type invariant); consuming `self` gives up the last
        // use of it.
        unsafe { alloc.deallocate_page(P::KIND, page, P::page_layout()) }
    }
}

impl<P> fmt::Debug for FreshPage<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("FreshPage").field(&self.page).finish()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> PageByType<K, V, S> {
    /// Gives the page back to `alloc`, the map's allocator, with its kind's
    /// layout.
    pub(in crate::tree) fn deallocate(self, alloc: &impl PageAllocator) {
        match self {
            Self::Leaf(page) => page.deallocate(alloc),
            Self::Internal(page) => page.deallocate(alloc),
            Self::Root(page) => page.deallocate(alloc),
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
    /// Whether a leaf holds a head (D17); its head slot is initialized exactly
    /// when this is set, and so is its head key slot in a map with inline keys
    /// (D49). Always `false` in an internal node.
    has_head: bool,
}

/// The header of a node with inline keys (D18): the plain node header, then
/// what only such nodes need. It fits in the bytes before the inline keys,
/// which start 8-aligned, so it costs no room.
#[repr(C)]
pub(in crate::tree) struct InlineKeyNodeHeader {
    header: NodeHeader,
    /// How many leading bytes the node's fences share, and so every key in
    /// it; its inline keys skip them (05-fence-key.md). For a node on the
    /// tree's border, which lacks a fence, what every key of the tree shares
    /// (D50).
    prefix_len: u16,
}

#[cfg(feature = "file")]
impl NodeHeader {
    /// Where `len` is in a page: a `u16` at its start.
    pub(in crate::tree) const LEN_OFFSET: usize = offset_of!(Self, len);

    /// Where `has_head` is in a page: a byte, 0 or 1.
    pub(in crate::tree) const HAS_HEAD_OFFSET: usize = offset_of!(Self, has_head);

    /// The header's padding, which no write initializes.
    const PADDING: Range<usize> = Self::HAS_HEAD_OFFSET + size_of::<bool>()..size_of::<Self>();
}

impl InlineKeyNodeHeader {
    /// The longest prefix a node records: what `prefix_len` holds.
    pub(super) const MAX_PREFIX_LEN: usize = u16::MAX as usize;
}

/// The header of a node of a latched tree, one that threads share
/// (`latch.rs`): the inline-key node header, then the node's latch word,
/// which only atomics access. Its inline keys start after it, 16 bytes in.
#[repr(C)]
pub(in crate::tree) struct LatchedNodeHeader {
    header: InlineKeyNodeHeader,
    latch: NodeLatch,
}

impl LatchedNodeHeader {
    /// The latch word of the latched tree's page at `page`, a node's of
    /// either kind or the root's, for as long as the caller picks.
    ///
    /// # Safety
    ///
    /// `page` is a latched tree's page, allocated while the caller uses the
    /// latch, its latch word initialized: zeroed by the allocator, unheld at
    /// the first version, or kept since (`tree/latched/pages.rs`).
    #[inline(always)]
    pub(in crate::tree) unsafe fn latch_of<'a>(page: NonNull<u8>) -> &'a NodeLatch {
        // SAFETY: a latched page's header is a `LatchedNodeHeader`, aligned,
        // in the allocated page (caller), whose latch word every thread
        // accesses as an atomic only, through `NodeLatch`.
        unsafe {
            page.byte_add(offset_of!(Self, latch))
                .cast::<NodeLatch>()
                .as_ref()
        }
    }
}

/// A pointer to an initialized node of `NodeLayout<K, V, S::Config>`, for a
/// map with strategy `S`, whose lowest bit tells the node's kind. Every page
/// leaves that bit 0, being aligned to at least 64 B (D15, D49, D59). A leaf's pointer
/// carries the tag, so that a descent uses an internal node's as it is.
///
/// Internal nodes store it as edges, node stacks and walks as the nodes on
/// them (`tree/node_stack.rs`), and the tree as its root (`root_page.rs`).
/// The node handles, [`Leaf`] and [`Internal`], wrap it. Following one
/// decides the kind of the node it leads to before reading the node.
///
/// The tag is the only record of a node's kind: the node does not keep it.
/// A pointer gets it where its page becomes a node ([`Self::initialize`],
/// [`Self::move_node_to`]), from the kind the page is made into, and keeps it
/// as it is copied. The kinds have different layouts (D21), so every slot
/// array it hands out needs the caller to know the kind. Everything it
/// offers is raw: the node handles are its users.
#[repr(transparent)]
pub(in crate::tree) struct NodePtr<K, V, S> {
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
    /// The bit that marks a leaf's pointer, and a leaf's offset in a file map
    /// (`tree/file.rs`).
    pub(in crate::tree) const LEAF_TAG: usize = 1;

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

    /// The pointer as it is stored, tag and all: for a word that threads
    /// share, which holds one (`optimistic.rs`, `tree/latched`).
    #[inline(always)]
    pub(in crate::tree) fn tagged(self) -> NonNull<u8> {
        self.tagged
    }

    /// The pointer [`Self::tagged`] gave.
    ///
    /// # Safety
    ///
    /// `tagged` is what [`Self::tagged`] returned for a node of this map
    /// type.
    #[inline(always)]
    pub(in crate::tree) unsafe fn from_tagged(tagged: NonNull<u8>) -> Self {
        Self {
            tagged,
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
    pub(in crate::tree) fn page(self) -> NonNull<u8> {
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
        // same `K`, `V` and `S`, so it is allocated with `P::page_layout()`
        // and aligned. The caller hands over every use of it: it is a
        // `FreshPage` again.
        FreshPage {
            page: self.page(),
            _page_type: PhantomData,
        }
    }

    /// The header, written by `initialize`, or carried over by
    /// `move_node_to`. In a node with inline keys, it starts an
    /// [`InlineKeyNodeHeader`].
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
        // SAFETY: the header was written by `initialize`, or carried over by
        // `move_node_to`, and is aligned; the page is allocated (caller).
        usize::from(unsafe { (*self.header()).len })
    }

    /// Whether a leaf's head slot, and with inline keys its head key slot, are
    /// initialized (D17, D49).
    ///
    /// # Safety
    ///
    /// The node has not been freed, and nothing writes its header
    /// concurrently.
    pub(super) unsafe fn has_head(self) -> bool {
        // SAFETY: the header was written by `initialize`, or carried over by
        // `move_node_to`, and is aligned; the page is allocated (caller).
        unsafe { (*self.header()).has_head }
    }

    /// Sets the `has_head` header field.
    ///
    /// # Safety
    ///
    /// - The node is a leaf that has not been freed, and its handle has
    ///   exclusive access.
    /// - By the time the node is used again, its head slot, and with inline
    ///   keys its head key slot, are initialized exactly if `has_head`.
    pub(super) unsafe fn set_has_head(self, has_head: bool) {
        // SAFETY: the header is in bounds and aligned; the caller has
        // exclusive access.
        unsafe { (*self.header()).has_head = has_head }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> NodePtr<K, V, S> {
    /// Whether the map's nodes keep a latch word, in a tree that threads
    /// share ([`LatchedNodeHeader`]).
    #[inline(always)]
    pub(in crate::tree) fn is_latched() -> bool {
        <S as Sealed<K>>::LATCHED
    }

    /// Whether the map's nodes lie in a file map, whose edges are offsets
    /// from their node's page (`tree/file.rs`).
    #[inline(always)]
    pub(in crate::tree) fn is_in_file() -> bool {
        <S as Sealed<K>>::IN_FILE
    }

    /// The node's latch word, in a latched tree, for as long as the caller
    /// picks: the node's page is never freed while threads share the tree
    /// (`latch.rs`).
    ///
    /// # Safety
    ///
    /// The tree is latched, and the page allocated.
    #[inline(always)]
    pub(in crate::tree) unsafe fn latch<'a>(self) -> &'a NodeLatch {
        debug_assert!(Self::is_latched());
        // SAFETY: the page is a latched tree's, allocated (caller), its latch
        // word initialized with it.
        unsafe { LatchedNodeHeader::latch_of(self.page()) }
    }

    /// The `len` and `prefix_len` header fields of a latched tree's internal
    /// node, while a writer may be changing them, for an optimistic read
    /// (`optimistic.rs`). Each is read once, as a relaxed atomic, so that the
    /// compiler loads it once, and what the read checks is what it uses.
    ///
    /// # Safety
    ///
    /// The tree is latched, and the page allocated, its header initialized.
    #[inline(always)]
    pub(in crate::tree) unsafe fn load_racing_header(self) -> (usize, usize) {
        debug_assert!(Self::is_latched());
        Interleaving::switch_point();
        // SAFETY: both fields are aligned `u16`s of the allocated,
        // initialized header (caller), read through the raw pointer.
        unsafe {
            let header = self.page().cast::<InlineKeyNodeHeader>().as_ptr();
            let len = AtomicU16::from_ptr(&raw mut (*header).header.len);
            let prefix_len = AtomicU16::from_ptr(&raw mut (*header).prefix_len);
            (
                usize::from(len.load(Ordering::Relaxed)),
                usize::from(prefix_len.load(Ordering::Relaxed)),
            )
        }
    }

    /// The `has_head` header field of a latched tree's leaf, while a writer
    /// may be changing it, for an optimistic read (`optimistic.rs`): read
    /// once, as a relaxed atomic.
    ///
    /// # Safety
    ///
    /// The tree is latched, and the page allocated, its header initialized.
    #[inline(always)]
    pub(in crate::tree) unsafe fn load_racing_has_head(self) -> bool {
        debug_assert!(Self::is_latched());
        Interleaving::switch_point();
        // SAFETY: the field is a `bool` of the allocated, initialized header
        // (caller), which writers only ever set to a `bool`, read through
        // the raw pointer.
        unsafe { AtomicBool::from_ptr(&raw mut (*self.header()).has_head).load(Ordering::Relaxed) }
    }

    /// The width of the inline keys nodes of `Kind` hold, or `None` if they
    /// hold none (D18).
    #[inline]
    pub(in crate::tree) fn inline_width<Kind: NodeKind>() -> Option<InlineWidth> {
        NodePlan::of::<S, Kind>().inline_width()
    }

    /// Whether the map's nodes hold inline keys (D18): those of both kinds do,
    /// or neither does. With inline keys, each separator's key is the head key
    /// of the leftmost leaf right of it, and internal nodes keep no keys (D49).
    #[inline(always)]
    pub(in crate::tree) fn has_inline_keys() -> bool {
        NodePlan::of::<S, Leaf<K, V, S>>().inline_width().is_some()
    }

    /// Whether the map's leaves, with inline keys of `inline_width`, have
    /// room for hash tags (D36): a leaf's plan keeps them only where they
    /// fit.
    #[inline(always)]
    pub(super) fn leaf_has_room_for_hash_tags(inline_width: InlineWidth) -> bool {
        NodeLayout::<K, V, S::Config, S::LeafAnnex>::leaf(
            Some(inline_width),
            true,
            Self::is_latched(),
        )
        .hash_tags_offset
        .is_some()
    }

    pub(in crate::tree) fn leaf_layout() -> LeafLayout {
        const {
            assert!(
                size_of::<S::LeafAnnex>() == 0 || <S as Sealed<K>>::LATCHED,
                "only a BBTreeMap's leaves keep an annex"
            );
        }
        let plan = NodePlan::of::<S, Leaf<K, V, S>>();
        NodeLayout::<K, V, S::Config, S::LeafAnnex>::leaf(
            plan.inline_width(),
            plan.has_hash_tags(),
            Self::is_latched(),
        )
    }

    pub(in crate::tree) fn internal_layout() -> InternalLayout {
        NodeLayout::<K, V, S::Config>::internal(
            Self::inline_width::<Internal<K, V, S>>(),
            Self::is_latched(),
        )
    }

    /// Where the keys and inline keys of a node of kind `Kind` sit.
    pub(in crate::tree) fn key_area<Kind: NodeKind>() -> KeyArea {
        if Kind::IS_LEAF {
            Self::leaf_layout().keys
        } else {
            Self::internal_layout().keys
        }
    }

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

    /// Where a leaf's annex is (`leaf_annex.rs`): right after its node
    /// header (`layout.rs`). Nothing is dereferenced.
    #[inline(always)]
    pub(super) fn leaf_annex(self) -> NonNull<S::LeafAnnex> {
        let header = Header::leaf(Self::is_latched(), Layout::new::<S::LeafAnnex>());
        // SAFETY: a leaf's layout holds its annex at this offset, aligned,
        // in the page (layout invariants).
        unsafe { self.page().byte_add(header.annex_offset()).cast() }
    }

    /// Turns a fresh page into an empty node of the page's kind: no keys
    /// and, for a leaf, no head, and a new annex. An internal node still
    /// needs its first edge written.
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
    /// tagged with `Kind`. A fresh page becomes a node this way, and so does
    /// the root page as it turns from one kind into the other
    /// (`root_page.rs`).
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
        // alignment either header's divides. The header of its layout, a
        // leaf's annex included, fits before its keys and columns, whose
        // `capacity` slots lie in the page, aligned (layout invariants). The
        // access is exclusive, and what the page held is given up (caller).
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
            if Kind::IS_LEAF {
                node.leaf_annex().write(S::LeafAnnex::default());
            }
            node.key_columns::<Kind>().initialize();
        }
        node
    }

    /// Moves the node of kind `Kind` here into the page at `dest`, a page
    /// for a node of that kind. Every byte of its layout is copied raw, so
    /// keys, values and edges move without being read. Returns the node's
    /// pointer there, tagged with `Kind`.
    ///
    /// # Safety
    ///
    /// The node is of kind `Kind`. `dest` is another page, at least that
    /// kind's size and aligned to it, that the access to is exclusive and
    /// that holds nothing that needs dropping. Afterwards, this node's
    /// contents belong to `dest`: nothing reads them here again but as raw
    /// bytes.
    pub(super) unsafe fn move_node_to<Kind: NodeKind>(self, dest: NonNull<u8>) -> Self {
        debug_assert_eq!(self.is_leaf(), Kind::IS_LEAF);
        let size = Kind::page_layout().size();
        // SAFETY: both pages are allocated with at least `size` bytes, and
        // distinct (the caller); a byte copy carries uninitialized bytes as
        // they are. In a latched tree, the latch word stays the
        // destination's. A page keeps one latch word for as long as threads
        // share the tree, and its version only grows (`latch.rs`), so a
        // reader that read the page before can't find its version again.
        unsafe {
            let (source, dest) = (self.page().as_ptr(), dest.as_ptr());
            if Self::is_latched() {
                let latch = offset_of!(LatchedNodeHeader, latch);
                let latch_end = latch + size_of::<NodeLatch>();
                source.copy_to_nonoverlapping(dest, latch);
                source
                    .add(latch_end)
                    .copy_to_nonoverlapping(dest.add(latch_end), size - latch_end);
            } else {
                source.copy_to_nonoverlapping(dest, size);
            }
        }
        Self::tagged_as::<Kind>(dest)
    }

    /// Copies the first `size` bytes of the node's page to `dest`, as an
    /// file map keeps them (`tree/file.rs`): byte for byte, but for the
    /// header's padding, which no write initializes, as zeros.
    ///
    /// # Safety
    ///
    /// The page is allocated with at least `size` bytes, a node's header
    /// and more, and every byte of them but the header's padding is
    /// initialized: the page was zeroed when allocated, and its keys and
    /// values have no padding of their own. Nothing writes the page
    /// meanwhile.
    #[cfg(feature = "file")]
    pub(in crate::tree) unsafe fn copy_page_to(self, dest: &mut [u8], size: usize) {
        let padding = NodeHeader::PADDING;
        assert!(dest.len() >= size && size >= padding.end);
        let source = self.page().as_ptr();
        // SAFETY: both ranges lie within the first `size` bytes of the page
        // and of `dest`, which don't overlap, and are initialized (caller);
        // the padding between them is not read.
        unsafe {
            source.copy_to_nonoverlapping(dest.as_mut_ptr(), padding.start);
            source
                .add(padding.end)
                .copy_to_nonoverlapping(dest.as_mut_ptr().add(padding.end), size - padding.end);
        }
        dest[padding].fill(0);
    }

    /// The `prefix_len` of a node with inline keys (05-fence-key.md); 0 for
    /// a node without.
    ///
    /// # Safety
    ///
    /// The node has not been freed, and nothing writes its header
    /// concurrently.
    pub(super) unsafe fn prefix_len(self) -> usize {
        if !Self::has_inline_keys() {
            return 0;
        }
        let header = self.page().cast::<InlineKeyNodeHeader>().as_ptr();
        // SAFETY: a node with inline keys starts with an aligned
        // `InlineKeyNodeHeader`, written by `initialize` or carried over by
        // `move_node_to`; the page is allocated (caller).
        usize::from(unsafe { (*header).prefix_len })
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
            let prefix_len = prefix_len as u16;
            let header = self.page().cast::<InlineKeyNodeHeader>().as_ptr();
            // SAFETY: a node with inline keys starts with an aligned
            // `InlineKeyNodeHeader`; the caller has exclusive access.
            unsafe { (*header).prefix_len = prefix_len };
        }
    }

    /// Sets the `len` header field.
    ///
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

    /// Sets the length of a node of kind `Kind`; the inline-key slots a
    /// shorter length leaves get the greatest inline key, as all slots past
    /// the length hold (invariant 4). Called once the slots are where the
    /// length says.
    ///
    /// # Safety
    ///
    /// The page is allocated, for a node of kind `Kind`, its handle has
    /// exclusive access, `len` is at most the capacity, and the node's
    /// contents `0..len` are initialized and valid at this call.
    #[inline(always)]
    pub(super) unsafe fn set_len_refilling<Kind: NodeKind>(self, len: usize) {
        debug_assert!(len <= Self::capacity::<Kind>());
        // SAFETY: the caller's.
        unsafe {
            let old_len = self.len();
            self.key_columns::<Kind>().mark_vacant(len..old_len);
            self.set_len(len);
        }
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
        debug_assert!(Kind::IS_LEAF || !Self::has_inline_keys());
        let area = Self::key_area::<Kind>();
        // SAFETY: forwarded.
        let prefix_len = unsafe { self.prefix_len() };
        // SAFETY: the keys' offset and stride are aligned for `K`, and
        // `capacity` keys from there stay within a page of the node's kind,
        // `Kind`, clear of its other arrays (layout invariants). The columns
        // are this kind's. The page is allocated (caller).
        unsafe {
            let keys = Slots::with_stride(
                self.page().byte_add(area.keys_offset).cast(),
                area.capacity,
                area.key_stride,
            );
            KeySlots::new(keys, self.key_columns::<Kind>(), prefix_len)
        }
    }

    /// An internal node's separators (D17): its keys without inline keys, or
    /// their windows with inline keys (D49).
    ///
    /// # Safety
    ///
    /// The node is an internal node that has not been freed; nothing
    /// writes its header concurrently.
    #[inline(always)]
    pub(super) unsafe fn separator_slots(self) -> SeparatorSlots<K, V, S> {
        // SAFETY: forwarded; the node is internal (caller).
        unsafe {
            if Self::has_inline_keys() {
                SeparatorSlots::Windows {
                    columns: self.key_columns::<Internal<K, V, S>>(),
                    prefix_len: self.prefix_len(),
                }
            } else {
                SeparatorSlots::Keys(self.key_slots::<Internal<K, V, S>>())
            }
        }
    }

    /// The arrays a node of kind `Kind` keeps beside its keys
    /// ([`KeyColumns`]): its inline keys, at its plan's width, and its
    /// hash tags (D36), if its plan keeps them and its layout has room for
    /// them ([`LeafLayout::hash_tags_offset`]). Reads nothing from the
    /// page.
    ///
    /// # Safety
    ///
    /// The page is allocated, for a node of kind `Kind`.
    #[inline(always)]
    pub(super) unsafe fn key_columns<Kind: NodeKind>(self) -> KeyColumns {
        let plan = NodePlan::of::<S, Kind>();
        let area = Self::key_area::<Kind>();
        let hash_tags_offset = if plan.has_hash_tags() {
            Self::leaf_layout().hash_tags_offset
        } else {
            None
        };
        // SAFETY: the inline keys' offset is aligned for inline keys of any
        // width, and `capacity` of them from there stay within a page of the
        // node's kind, `Kind`, clear of its other arrays. So do the hash
        // tags, which only a leaf's layout has room for (layout invariants).
        // The inline keys are of the width the layout was made for. The
        // page is allocated (caller).
        unsafe {
            let inline_keys =
                Option::zip(plan.inline_width(), area.inline_keys_offset).map(|(width, offset)| {
                    InlineKeySlots::new(width, self.page().byte_add(offset), area.capacity)
                });
            let hash_tags = hash_tags_offset
                .map(|offset| Slots::new(self.page().byte_add(offset).cast(), area.capacity));
            KeyColumns::new(inline_keys, hash_tags, area.capacity)
        }
    }

    /// A leaf's head key slot: the key's place of entry 0, before the head,
    /// where the key of the separator left of the leaf is (D49).
    ///
    /// # Safety
    ///
    /// The node is a leaf that has not been freed, in a map with inline keys.
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
        // a stride past the head; both, and the stride, are aligned for `V`.
        // `capacity` values from there stay in the page and clear of the
        // keys (layout invariants). The page is allocated (caller).
        unsafe {
            Slots::with_stride(
                self.page().byte_add(layout.values_offset()).cast(),
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
        // The edges' offset and stride are aligned for an edge, a `NodePtr`
        // whatever the strategy, and the `capacity + 1` edges stay in the
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

    /// The edge slots of an internal node in a file map (`tree/file.rs`),
    /// [`Self::edge_slots`] read as what a file map keeps there: each child's
    /// offset from this node's page, tagged with its kind as a pointer to it
    /// would be.
    ///
    /// # Safety
    ///
    /// The node is an internal node in a file map.
    #[inline(always)]
    pub(super) unsafe fn file_edge_slots(self) -> Slots<usize> {
        debug_assert!(Self::is_in_file());
        let layout = Self::internal_layout();
        // SAFETY: as for `edge_slots`: an offset has an edge's size and
        // alignment, a pointer's (`layout.rs`).
        unsafe {
            Slots::with_stride(
                self.page().byte_add(layout.edges_offset).cast(),
                layout.keys.capacity + 1,
                layout.edge_stride,
            )
        }
    }

    /// The node whose tagged offset from this node's page is `offset`: an
    /// edge of a node in a file map, or the file map's root from its start.
    ///
    /// # Safety
    ///
    /// `offset` is the tagged offset of a node of this map type, in the
    /// same file map as this page, from this page.
    #[inline(always)]
    pub(in crate::tree) unsafe fn at_offset(page: NonNull<u8>, offset: usize) -> Self {
        // SAFETY: the node lies in the file map, as the page does (caller).
        unsafe { Self::from_tagged(page.byte_add(offset)) }
    }
}

#[cfg(test)]
pub(crate) use test_pages::TestPages;

#[cfg(test)]
mod test_pages {
    use core::alloc::Layout;
    use core::cell::{Cell, RefCell};
    use core::marker::PhantomData;
    use core::ptr::NonNull;
    use std::vec::Vec;

    use super::{FreshPage, PageByType, PageType};
    use crate::allocator::{AllocError, Allocator, Global, PageKind};
    use crate::inline_key::{Auto, InlineKeyStrategy};
    use crate::tree::node::internal::Internal;
    use crate::tree::node::leaf::Leaf;

    /// An allocator for tests, of pages for a map with keys `K`, values `V`
    /// and strategy `S`, whose types the helpers that hand out typed pages
    /// take: it takes blocks from the global allocator, takes them back for
    /// reuse, scribbled over until zeroed again, and frees them when dropped.
    ///
    /// It keeps each block's layout: a block given back is handed out again
    /// only for the same layout, and giving one back with another layout
    /// than it was allocated for panics, as does giving back a block twice,
    /// or one from elsewhere. It keeps each page's kind too: a page given
    /// back to another kind's method than it came from panics, as does a
    /// kind asked for with two layouts.
    ///
    /// Blocks still out when it is dropped (owned by a leaked node) are
    /// never freed, so nothing can dangle; tests check
    /// [`TestPages::assert_all_released`] to catch such leaks.
    ///
    /// A test may give it a budget ([`TestPages::set_budget`]), past which
    /// it has no block to give.
    pub(crate) struct TestPages<K, V, S = Auto> {
        /// Every block this pool allocated, with its layout.
        allocated: RefCell<Vec<(NonNull<u8>, Layout)>>,
        /// Blocks given back, with their layouts, ready for reuse.
        released: RefCell<Vec<(NonNull<u8>, Layout)>>,
        /// How many more blocks it hands out; `None` for as many as asked.
        budget: Cell<Option<usize>>,
        /// The pages out, each with its kind.
        kinds: RefCell<Vec<(NonNull<u8>, PageKind)>>,
        /// The layout each kind was asked for with, once it was.
        kind_layouts: RefCell<Vec<(PageKind, Layout)>>,
        _marker: PhantomData<fn() -> (K, V)>,
        _strategy: PhantomData<fn() -> S>,
    }

    impl<K, V, S: InlineKeyStrategy<K>> TestPages<K, V, S> {
        pub(crate) fn new() -> Self {
            Self {
                allocated: RefCell::new(Vec::new()),
                released: RefCell::new(Vec::new()),
                budget: Cell::new(None),
                kinds: RefCell::new(Vec::new()),
                kind_layouts: RefCell::new(Vec::new()),
                _marker: PhantomData,
                _strategy: PhantomData,
            }
        }

        /// Makes it hand out `budget` more blocks, and then refuse every
        /// one; with `None`, as many as asked, as when new.
        pub(crate) fn set_budget(&self, budget: Option<usize>) {
            self.budget.set(budget);
        }

        /// A page of type `P`, reusing the block given back last for that
        /// type's layout if any.
        pub(in crate::tree) fn fresh<P: PageType>(&self) -> FreshPage<P> {
            FreshPage::allocate(self)
        }

        /// [`Self::fresh`], for a leaf.
        pub(in crate::tree) fn fresh_leaf(&self) -> FreshPage<Leaf<K, V, S>> {
            self.fresh()
        }

        /// [`Self::fresh`], for an internal node.
        pub(in crate::tree) fn fresh_internal(&self) -> FreshPage<Internal<K, V, S>> {
            self.fresh()
        }

        /// Takes a page of any type back.
        pub(in crate::tree) fn release(&self, page: impl Into<PageByType<K, V, S>>) {
            page.into().deallocate(self);
        }

        /// The number of blocks allocated so far, in use or not.
        pub(in crate::tree) fn allocated_count(&self) -> usize {
            self.allocated.borrow().len()
        }

        /// The number of blocks handed out and not given back.
        pub(crate) fn in_use_count(&self) -> usize {
            self.allocated.borrow().len() - self.released.borrow().len()
        }

        pub(crate) fn assert_all_released(&self) {
            assert_eq!(
                self.released.borrow().len(),
                self.allocated.borrow().len(),
                "pages were not released"
            );
        }
    }

    // SAFETY: every block comes from the global allocator, for its layout,
    // and is handed out zeroed until given back, and then again only once
    // taken off the released list; it is freed only when the pool is
    // dropped, and only if given back by then. The pool has no clones, and
    // panics only on a block given back wrongly, which a map never does.
    unsafe impl<K, V, S> Allocator for TestPages<K, V, S> {
        fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<u8>, AllocError> {
            match self.budget.get() {
                Some(0) => return Err(AllocError),
                Some(left) => self.budget.set(Some(left - 1)),
                None => {}
            }
            let mut released = self.released.borrow_mut();
            let reusable = (released.iter()).rposition(|&(_, page_layout)| page_layout == layout);
            if let Some(index) = reusable {
                let block = released.remove(index).0;
                // SAFETY: the block was allocated by this pool for `layout`,
                // and nothing uses it since it was given back.
                unsafe { block.write_bytes(0, layout.size()) };
                return Ok(block);
            }
            let block = Global.allocate_zeroed(layout)?;
            self.allocated.borrow_mut().push((block, layout));
            Ok(block)
        }

        /// Scribbles over the block, so that nothing can rely on what it
        /// held.
        ///
        /// # Panics
        ///
        /// If the block is not one of this pool's, was allocated for another
        /// layout, or is given back twice.
        unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
            assert!(
                self.allocated.borrow().contains(&(ptr, layout)),
                "released a page from elsewhere, or with another layout"
            );
            assert!(
                !(self.released.borrow().iter()).any(|&(released, _)| released == ptr),
                "released a page twice"
            );
            // SAFETY: the block was allocated by this pool for `layout`, and
            // is given back by its only user.
            unsafe { ptr.write_bytes(0xA5, layout.size()) };
            self.released.borrow_mut().push((ptr, layout));
        }

        fn allocate_internal(&self, layout: Layout) -> Result<NonNull<u8>, AllocError> {
            self.allocate_page_of(PageKind::Internal, layout)
        }

        unsafe fn deallocate_internal(&self, page: NonNull<u8>, layout: Layout) {
            // SAFETY: forwarded.
            unsafe { self.deallocate_page_of(PageKind::Internal, page, layout) }
        }

        fn allocate_leaf(&self, layout: Layout) -> Result<NonNull<u8>, AllocError> {
            self.allocate_page_of(PageKind::Leaf, layout)
        }

        unsafe fn deallocate_leaf(&self, page: NonNull<u8>, layout: Layout) {
            // SAFETY: forwarded.
            unsafe { self.deallocate_page_of(PageKind::Leaf, page, layout) }
        }

        fn allocate_root(&self, layout: Layout) -> Result<NonNull<u8>, AllocError> {
            self.allocate_page_of(PageKind::Root, layout)
        }

        unsafe fn deallocate_root(&self, page: NonNull<u8>, layout: Layout) {
            // SAFETY: forwarded.
            unsafe { self.deallocate_page_of(PageKind::Root, page, layout) }
        }
    }

    impl<K, V, S> TestPages<K, V, S> {
        /// A page of `kind`, from `allocate_zeroed`, kept with its kind.
        ///
        /// # Panics
        ///
        /// If `kind` was asked for with another layout before.
        fn allocate_page_of(
            &self,
            kind: PageKind,
            layout: Layout,
        ) -> Result<NonNull<u8>, AllocError> {
            let mut kind_layouts = self.kind_layouts.borrow_mut();
            match kind_layouts.iter().find(|&&(asked, _)| asked == kind) {
                Some(&(_, first)) => assert_eq!(first, layout, "a kind's pages have one layout"),
                None => kind_layouts.push((kind, layout)),
            }
            let page = self.allocate_zeroed(layout)?;
            self.kinds.borrow_mut().push((page, kind));
            Ok(page)
        }

        /// Takes back `page`, of `kind`.
        ///
        /// # Panics
        ///
        /// If the page is out as another kind, or not out.
        ///
        /// # Safety
        ///
        /// As for `deallocate`.
        unsafe fn deallocate_page_of(&self, kind: PageKind, page: NonNull<u8>, layout: Layout) {
            let mut kinds = self.kinds.borrow_mut();
            let index = (kinds.iter())
                .position(|&(out, _)| out == page)
                .expect("a page given back is out");
            assert_eq!(
                kinds.swap_remove(index).1,
                kind,
                "a page goes back as its kind"
            );
            drop(kinds);
            // SAFETY: forwarded.
            unsafe { self.deallocate(page, layout) }
        }
    }

    impl<K, V, S> Drop for TestPages<K, V, S> {
        fn drop(&mut self) {
            for (block, layout) in self.released.get_mut().drain(..) {
                // SAFETY: allocated from `Global` for `layout`; a block given
                // back has no user left, and each is freed once.
                unsafe { Global.deallocate(block, layout) };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use core::mem::{align_of, size_of};
    use core::ops::Range;
    use std::string::String;
    use std::vec::Vec;

    use super::{FreshPage, NodeHeader, NodeKind, NodePtr, TestPages};
    use crate::inline_key::{Auto, InlineKeyStrategy, InlineWidth, WithInlineKey};
    use crate::node_config::DefaultNodeConfig;
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
            let layout = Kind::page_layout();
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

        let keeps_keys = !NodePtr::<K, V, S>::has_inline_keys();
        // SAFETY: both pages are allocated, and each is asked for the slots
        // of its own kind, the internal node for keys only if it keeps them;
        // nothing is dereferenced.
        let (
            leaf_keys,
            leaf_columns,
            values,
            head_and_values,
            internal_keys,
            internal_columns,
            edges,
        ) = unsafe {
            (
                leaf.key_slots::<Leaf<K, V, S>>(),
                leaf.key_columns::<Leaf<K, V, S>>(),
                SlotRange::of(leaf.value_slots(), leaf_capacity),
                SlotRange::of(leaf.head_and_value_slots(), leaf_capacity + 1),
                keeps_keys.then(|| internal.key_slots::<Internal<K, V, S>>()),
                internal.key_columns::<Internal<K, V, S>>(),
                SlotRange::of(internal.edge_slots(), internal_capacity + 1),
            )
        };
        assert_eq!(
            leaf_columns.inline_keys().map(InlineKeySlots::width),
            NodePtr::<K, V, S>::inline_width::<Leaf<K, V, S>>()
        );
        assert_eq!(
            internal_columns.inline_keys().map(InlineKeySlots::width),
            NodePtr::<K, V, S>::inline_width::<Internal<K, V, S>>()
        );
        let leaf_inline_keys = (leaf_columns.inline_keys())
            .map(|slots| SlotRange::of_inline_keys(slots, leaf_capacity));
        let internal_inline_keys = (internal_columns.inline_keys())
            .map(|slots| SlotRange::of_inline_keys(slots, internal_capacity));
        // SAFETY: as above; the leaf's head key slot only with inline keys.
        let head_key = NodePtr::<K, V, S>::has_inline_keys().then(|| unsafe {
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
                    .hash_tags()
                    .is_none()
            );
            if let Some(tags) = leaf.key_columns::<Leaf<K, V, S>>().hash_tags() {
                assert!(
                    tags.slice_ptr(leaf_capacity)
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
    /// doesn't recognize, `[u64; 1]`, keeps the plain layout; std byte
    /// strings and integers get the layout with inline keys.
    #[test]
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
    fn only_inline_keys_change_the_layout() {
        assert!(!NodePtr::<[u64; 1], u64, Auto>::has_inline_keys());
        assert_eq!(
            NodePtr::<[u64; 1], u64, Auto>::leaf_layout(),
            NodeLayout::<[u64; 1], u64, DefaultNodeConfig>::PLAIN_LEAF
        );
        assert_eq!(
            NodePtr::<[u64; 1], u64, Auto>::internal_layout(),
            NodeLayout::<[u64; 1], u64, DefaultNodeConfig>::PLAIN_INTERNAL
        );
        // 1 KB, 63 keys at offset 8, and the head, or the edges, right after
        // them, at 8 + 63·8 = 512.
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
            NodeLayout::<String, u64, DefaultNodeConfig>::EIGHT_BYTE_INLINE_HASH_TAGGED_LEAF
        );
        assert_eq!(
            NodePtr::<Vec<u8>, u64, Auto>::internal_layout(),
            NodeLayout::<Vec<u8>, u64, DefaultNodeConfig>::FOUR_BYTE_INLINE_INTERNAL
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
            NodePtr::<[u8; 32], u64, WithInlineKey>::internal_layout(),
            NodeLayout::<[u8; 32], u64, DefaultNodeConfig>::EIGHT_BYTE_INLINE_INTERNAL
        );
        assert!(NodePtr::<Tracked<'_>, u64, WithInlineKey>::has_inline_keys());
        assert!(!NodePtr::<Tracked<'_>, u64, Auto>::has_inline_keys());

        // Integers have two-byte inline keys, under `Auto` as under
        // `WithInlineKey`.
        assert!(NodePtr::<i64, u64, Auto>::has_inline_keys());
        assert_eq!(
            NodePtr::<u64, u64, Auto>::leaf_layout(),
            NodeLayout::<u64, u64, DefaultNodeConfig>::TWO_BYTE_INLINE_LEAF
        );
        assert_eq!(
            NodePtr::<u64, u64, WithInlineKey>::leaf_layout(),
            NodeLayout::<u64, u64, DefaultNodeConfig>::TWO_BYTE_INLINE_LEAF
        );
        assert_eq!(
            NodePtr::<i128, u64, WithInlineKey>::internal_layout(),
            NodeLayout::<i128, u64, DefaultNodeConfig>::TWO_BYTE_INLINE_INTERNAL
        );
    }

    #[test]
    fn a_new_node_is_empty() {
        let pages = TestPages::<u32, u64>::new();
        let node = NodePtr::<u32, u64, Auto>::from_fresh_page(pages.fresh_leaf());
        // SAFETY: the page is allocated and holds a node.
        assert_eq!(unsafe { (node.len(), node.has_head()) }, (0, false));
        // SAFETY: as above.
        assert_eq!(unsafe { node.prefix_len() }, 0);
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
        assert_eq!(unsafe { (node.len(), node.prefix_len()) }, (0, 0));
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
}
