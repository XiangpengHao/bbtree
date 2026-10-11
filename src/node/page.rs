//! Node pages: the allocation boundary ([`FreshPage`]), raw access to an
//! initialized node's header and slot arrays ([`NodePtr`]), and the type
//! identities that decide whether a page holds inline keys, and let the
//! search read them ([`ByteString`], [`SameType`]).

use alloc::alloc::{alloc, dealloc, handle_alloc_error};
use core::alloc::Layout;
use core::fmt;
use core::marker::PhantomData;
use core::ptr::NonNull;

use super::handle::{ForceResult, Internal, Leaf, NodeKind};
use super::inline_keys::KeyInliner;
use super::layout::{InternalLayout, KeyArea, LeafLayout, NodeLayout};
use super::slots::{InlineKeySlots, KeySlots, Slots};
use crate::inline_key::{InlineKeyStrategy, InlineWidth};

pub(super) use type_identity::{ByteString, SameType, StdInteger};

/// A node kind that pages are allocated for, known at compile time: [`Leaf`]
/// or [`Internal`]. Each kind has a page layout of its own (D21), and a page
/// holds nodes of the kind it was allocated for only. Everything that
/// depends on a node's layout, its capacity and where its slots are, is
/// for handles of these kinds only: a traversal decides a node's kind once,
/// with `NodeRef::force`, and its code knows it from then on.
///
/// # Safety
///
/// `IS_LEAF` is whether nodes of this kind are leaves, the nodes at height
/// 0: a page of this kind gets the layout of that kind of node, and the node
/// code accesses the node made from it with that kind's offsets and
/// capacity.
pub(crate) unsafe trait PageKind: NodeKind + Sized {
    const IS_LEAF: bool;

    /// `page`, sorted by its kind, this one.
    fn page_by_kind<K, V, S>(page: FreshPage<K, V, S, Self>) -> PageByKind<K, V, S>;
}

// SAFETY: leaves are the nodes at height 0.
unsafe impl PageKind for Leaf {
    const IS_LEAF: bool = true;

    fn page_by_kind<K, V, S>(page: FreshPage<K, V, S, Self>) -> PageByKind<K, V, S> {
        ForceResult::Leaf(page)
    }
}

// SAFETY: internal nodes are the nodes above height 0.
unsafe impl PageKind for Internal {
    const IS_LEAF: bool = false;

    fn page_by_kind<K, V, S>(page: FreshPage<K, V, S, Self>) -> PageByKind<K, V, S> {
        ForceResult::Internal(page)
    }
}

/// An allocated page for a node of kind `Kind` ([`Leaf`] or [`Internal`])
/// of a map with keys `K`, values `V` and inline-key strategy `S`, that
/// holds no node.
///
/// A `FreshPage` owns its page, like a `Box` that has nothing in it: this is
/// the only way to create a node, and what a node turns back into once it is
/// released, ready to be reused or deallocated. The kinds have different
/// layouts (D21), and so do maps whose keys have inline keys and those whose
/// keys have none (D18). The type carries all that decides the layout, so a
/// page only ever holds a node of the layout it was allocated for, and is
/// deallocated with the layout it was allocated with.
///
/// Invariant: the page comes from the global allocator, with
/// [`Self::layout`], and nothing else uses it.
pub(crate) struct FreshPage<K, V, S, Kind> {
    page: NonNull<u8>,
    _marker: PhantomData<fn() -> (K, V)>,
    _strategy: PhantomData<fn() -> S>,
    /// The kind of node the page is for.
    _kind: PhantomData<Kind>,
}

/// A fresh page sorted by the kind of node it is for: what releasing a node
/// whose kind is known only at run time gives back.
pub(crate) type PageByKind<K, V, S> =
    ForceResult<FreshPage<K, V, S, Leaf>, FreshPage<K, V, S, Internal>>;

impl<K, V, S: InlineKeyStrategy<K>, Kind: PageKind> FreshPage<K, V, S, Kind> {
    /// The allocation layout of every page of this kind (D21) and strategy
    /// (D18): one of the layout constants, picked after monomorphization.
    /// Using it also runs those constants' compile-time assertions.
    pub(super) fn layout() -> Layout {
        if Kind::IS_LEAF {
            NodePtr::<K, V, S>::leaf_layout().page
        } else {
            NodePtr::<K, V, S>::internal_layout().page
        }
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
    pub(crate) unsafe fn new(page: NonNull<u8>) -> Self {
        let layout = Self::layout();
        assert!(
            page.addr().get().is_multiple_of(layout.align()),
            "node page is not aligned to {}",
            layout.align()
        );
        Self {
            page,
            _marker: PhantomData,
            _strategy: PhantomData,
            _kind: PhantomData,
        }
    }
}

impl<K, V, S, Kind> FreshPage<K, V, S, Kind> {
    /// Hands the page back, e.g. for deallocation by the global allocator
    /// with [`Self::layout`].
    pub(crate) fn into_raw(self) -> NonNull<u8> {
        self.page
    }
}

/// Temporary allocation boundary, until the allocator trait (D8) replaces it:
/// one global-allocator call per node page. Nodes keep a 1 KiB size floor
/// but need only cache-line alignment, or larger alignment required by
/// their key/value types. The smaller alignment permits tighter packing
/// in allocators that keep metadata beside each allocation.
impl<K, V, S: InlineKeyStrategy<K>, Kind: PageKind> FreshPage<K, V, S, Kind> {
    /// A page from the global allocator.
    ///
    /// Calls [`handle_alloc_error`] if the allocation fails.
    pub(crate) fn allocate() -> Self {
        let layout = Self::layout();
        // SAFETY: `layout` has a non-zero size: nodes are at least 1 KB.
        let page = unsafe { alloc(layout) };
        let Some(page) = NonNull::new(page) else {
            handle_alloc_error(layout)
        };
        // SAFETY: the page was just allocated by the global allocator with
        // `layout()`, and nothing else has it.
        unsafe { Self::new(page) }
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

impl<K, V, S, Kind> fmt::Debug for FreshPage<K, V, S, Kind> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("FreshPage").field(&self.page).finish()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> PageByKind<K, V, S> {
    /// Gives the page back to the global allocator, with its kind's layout.
    pub(crate) fn deallocate(self) {
        match self {
            ForceResult::Leaf(page) => page.deallocate(),
            ForceResult::Internal(page) => page.deallocate(),
        }
    }
}

impl<K, V, S> From<FreshPage<K, V, S, Leaf>> for PageByKind<K, V, S> {
    fn from(page: FreshPage<K, V, S, Leaf>) -> Self {
        ForceResult::Leaf(page)
    }
}

impl<K, V, S> From<FreshPage<K, V, S, Internal>> for PageByKind<K, V, S> {
    fn from(page: FreshPage<K, V, S, Internal>) -> Self {
        ForceResult::Internal(page)
    }
}

/// The header at the start of every node page.
#[repr(C)]
pub(super) struct NodeHeader {
    /// The number of keys.
    len: u16,
    /// Whether a leaf holds a head value (D17); its head slot is initialized
    /// exactly when this is set. Always `false` in an internal node.
    has_head: bool,
}

/// The header of a node with inline keys (D18): the common header, then
/// what only such nodes need. It fits in the bytes before the inline keys,
/// which start 8-aligned, so it costs no room. The optional local search
/// offset fills what was padding; it never changes descendants' fences.
#[repr(C)]
pub(super) struct InlineKeyNodeHeader {
    common: NodeHeader,
    /// How many leading bytes the node's fences share, and so every key in
    /// it, which its inline keys skip (05-fence-key.md).
    prefix_len: u16,
    /// Optional offset used only for searching this node's own keys. Zero
    /// keeps fence windows; the field occupies the header's old padding.
    search_prefix_len: u16,
}

impl InlineKeyNodeHeader {
    /// The longest prefix a node records: what `prefix_len` holds.
    pub(super) const MAX_PREFIX_LEN: usize = u16::MAX as usize;
}

/// A pointer to an initialized node of `NodeLayout<K, V>`, for a map with
/// strategy `S`: the page of a former [`FreshPage`] whose header has been
/// written.
///
/// It does not know the node's kind (the height does, D11), and it is what
/// internal nodes store as edges, and paths as the nodes on them (`path.rs`).
/// The kinds have different layouts (D21), so every slot array it hands out
/// needs the caller to know the kind. Everything it offers is raw: the typed
/// handles in `handle.rs` are the only users.
#[repr(transparent)]
pub(crate) struct NodePtr<K, V, S> {
    page: NonNull<u8>,
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
    /// Whether `self` and `other` point to the same node.
    pub(super) fn is(self, other: Self) -> bool {
        self.page == other.page
    }

    /// Gives the page back, as a fresh page for the node's kind.
    ///
    /// # Safety
    ///
    /// - The node is of kind `Kind`.
    /// - The node's keys, values, head and edges have all been moved out,
    ///   dropped or deliberately leaked, and neither this pointer nor any
    ///   copy of it (an edge, a handle) is used afterwards.
    pub(super) unsafe fn into_fresh_page<Kind: PageKind>(self) -> FreshPage<K, V, S, Kind> {
        // The page came from a `FreshPage` (`from_fresh_page` is the only
        // constructor) for the node's kind, which is `Kind` (caller), and
        // the same `K`, `V` and `S`, so it is allocated with
        // `FreshPage::<K, V, S, Kind>::layout()` and aligned, and the caller
        // hands over every use of it: it is a `FreshPage` again.
        FreshPage {
            page: self.page,
            _marker: PhantomData,
            _strategy: PhantomData,
            _kind: PhantomData,
        }
    }

    /// The header, written by `from_fresh_page`. In a node with inline keys,
    /// it starts an [`InlineKeyNodeHeader`].
    fn header(self) -> *mut NodeHeader {
        self.page.cast::<NodeHeader>().as_ptr()
    }

    /// The `len` header field.
    ///
    /// # Safety
    ///
    /// The node has not been released, and nothing writes its header
    /// concurrently.
    pub(super) unsafe fn len(self) -> usize {
        // SAFETY: the header was written by `from_fresh_page` and is aligned;
        // the page is allocated (caller).
        usize::from(unsafe { (*self.header()).len })
    }

    /// Whether a leaf's head slot is initialized (D17).
    ///
    /// # Safety
    ///
    /// The node has not been released, and nothing writes its header
    /// concurrently.
    pub(super) unsafe fn has_head(self) -> bool {
        // SAFETY: the header was written by `from_fresh_page` and is aligned;
        // the page is allocated (caller).
        unsafe { (*self.header()).has_head }
    }

    /// # Safety
    ///
    /// The node is a leaf that has not been released, its handle has
    /// exclusive access, and its head slot is initialized exactly if
    /// `has_head` by the time the node is used again.
    pub(super) unsafe fn set_has_head(self, has_head: bool) {
        // SAFETY: the header is in bounds and aligned; the caller has
        // exclusive access.
        unsafe { (*self.header()).has_head = has_head }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> NodePtr<K, V, S> {
    /// The width of the inline keys nodes of `Kind` hold, or `None` if they
    /// hold none (D18).
    ///
    /// Every layout decision below asks this, and it is a pure function of
    /// `K`, `S` and `Kind` that folds to a constant. Every page of a map and
    /// kind gets the same answer, so accesses match its allocation layout.
    #[inline]
    pub(super) fn inline_width<Kind: PageKind>() -> Option<InlineWidth> {
        KeyInliner::<K>::resolve::<S>().map(KeyInliner::node_width::<Kind>)
    }

    /// Whether the map's nodes hold inline keys (D18).
    pub(super) fn has_inline_keys() -> bool {
        KeyInliner::<K>::resolve::<S>().is_some()
    }

    pub(super) fn leaf_layout() -> LeafLayout {
        NodeLayout::<K, V>::leaf(Self::inline_width::<Leaf>())
    }

    pub(super) fn internal_layout() -> InternalLayout {
        NodeLayout::<K, V>::internal::<S>()
    }

    /// Where the keys and inline keys of a node of kind `Kind` sit.
    pub(super) fn key_area<Kind: PageKind>() -> KeyArea {
        if Kind::IS_LEAF {
            Self::leaf_layout().keys
        } else {
            Self::internal_layout().keys
        }
    }

    /// The maximum number of keys of a node of kind `Kind`.
    pub(super) fn capacity<Kind: PageKind>() -> usize {
        Self::key_area::<Kind>().capacity
    }

    /// Optional length tags occupy only unused leaf padding. Explicit
    /// `InlineKey` strategies and fixed arrays never use this filter.
    fn length_tags_offset<Kind: PageKind>() -> Option<usize> {
        if !Kind::IS_LEAF {
            return None;
        }
        let KeyInliner::ByteString(bytes) = KeyInliner::<K>::resolve::<S>()? else {
            return None;
        };
        if bytes.inline_width() != InlineWidth::Eight {
            return None;
        }
        Self::leaf_layout().length_tags_offset
    }

    /// # Safety
    /// The allocated node is of kind `Kind`.
    pub(super) unsafe fn length_tag_slots<Kind: PageKind>(self) -> Option<Slots<u16>> {
        let offset = Self::length_tags_offset::<Kind>()?;
        // SAFETY: the layout places this aligned array after the summaries
        // and before the head and real keys, within the allocation.
        Some(unsafe { Slots::new(self.page.byte_add(offset).cast(), Self::capacity::<Kind>()) })
    }

    /// Whether a node of kind `Kind` pairs its keys with its trailing slots
    /// in entries (D29, D32), which then move whole: in a leaf with inline
    /// keys, unless the values take no room, which leaves the keys side by
    /// side. Internal keys and edges use separate arrays.
    pub(super) fn has_paired_entries<Kind: PageKind>() -> bool {
        Self::key_area::<Kind>().key_stride != size_of::<K>()
    }

    /// Turns a fresh page into an empty node of the page's kind: no keys
    /// and, for a leaf, no head. An internal node still needs its first edge
    /// written.
    ///
    /// With inline keys, it also fills every inline-key slot up to the
    /// capacity with the greatest inline key, all ones, so that searches may
    /// count the slots past `len` (handle.rs, invariant 6).
    pub(super) fn from_fresh_page<Kind: PageKind>(page: FreshPage<K, V, S, Kind>) -> Self {
        let page = page.into_raw();
        let common = NodeHeader {
            len: 0,
            has_head: false,
        };
        // SAFETY: the page is aligned to its layout (checked in
        // `FreshPage::new`), whose alignment either header's divides, and the
        // header of its layout fits before its keys and inline keys, whose
        // `capacity` slots lie in the page, aligned (layout invariants); the
        // page is exclusively ours.
        unsafe {
            if let Some(width) = Self::inline_width::<Kind>() {
                page.cast::<InlineKeyNodeHeader>()
                    .write(InlineKeyNodeHeader {
                        common,
                        prefix_len: 0,
                        search_prefix_len: 0,
                    });
                let area = Self::key_area::<Kind>();
                if let Some(offset) = area.inline_keys_offset {
                    page.byte_add(offset)
                        .write_bytes(u8::MAX, area.capacity * width.bytes());
                }
            } else {
                page.cast::<NodeHeader>().write(common);
            }
            if let Some(offset) = Self::length_tags_offset::<Kind>() {
                page.byte_add(offset)
                    .write_bytes(0, Self::capacity::<Kind>() * size_of::<u16>());
            }
        }
        Self {
            page,
            _marker: PhantomData,
            _strategy: PhantomData,
        }
    }

    /// The `prefix_len` of a node with inline keys (05-fence-key.md), or
    /// `None` for a node without.
    ///
    /// # Safety
    ///
    /// The node has not been released, and nothing writes its header
    /// concurrently.
    pub(super) unsafe fn prefix_len(self) -> Option<usize> {
        Self::has_inline_keys().then(|| {
            let header = self.page.cast::<InlineKeyNodeHeader>().as_ptr();
            // SAFETY: a node with inline keys starts with an
            // `InlineKeyNodeHeader`, written by `from_fresh_page` and
            // aligned; the page is allocated (caller).
            usize::from(unsafe { (*header).prefix_len })
        })
    }

    /// Optional local search offset, zero when the windows use the fences.
    ///
    /// # Safety
    /// The node is allocated, has inline keys, and its header is not being
    /// written concurrently.
    pub(super) unsafe fn search_prefix_len(self) -> usize {
        let header = self.page.cast::<InlineKeyNodeHeader>().as_ptr();
        // SAFETY: the caller guarantees the initialized inline-key header.
        usize::from(unsafe { (*header).search_prefix_len })
    }

    /// # Safety
    /// The node is allocated, has inline keys, and is exclusively borrowed.
    pub(super) unsafe fn set_search_prefix_len(self, prefix_len: usize) {
        let header = self.page.cast::<InlineKeyNodeHeader>().as_ptr();
        let prefix_len = prefix_len.min(InlineKeyNodeHeader::MAX_PREFIX_LEN) as u16;
        // SAFETY: the caller guarantees the initialized, exclusive header.
        unsafe { (*header).search_prefix_len = prefix_len };
    }

    /// Sets the `prefix_len` of a node with inline keys; does nothing to a
    /// node without. Keeping the inline keys at that prefix length is the
    /// caller's business: it decides answers, not memory safety.
    ///
    /// # Safety
    ///
    /// The node has not been released, and its handle has exclusive access.
    pub(super) unsafe fn set_prefix_len(self, prefix_len: usize) {
        if Self::has_inline_keys() {
            debug_assert!(prefix_len <= InlineKeyNodeHeader::MAX_PREFIX_LEN);
            let prefix_len = prefix_len.min(InlineKeyNodeHeader::MAX_PREFIX_LEN) as u16;
            let header = self.page.cast::<InlineKeyNodeHeader>().as_ptr();
            // SAFETY: a node with inline keys starts with an aligned
            // `InlineKeyNodeHeader`; the caller has exclusive access.
            unsafe {
                (*header).prefix_len = prefix_len;
                (*header).search_prefix_len = 0;
            }
        }
    }

    /// # Safety
    ///
    /// The node has not been released, its handle has exclusive access, and
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
    /// slots from theirs (D18). Mutating operations normalize local search
    /// windows first, so `KeySlots` always moves conservative fence windows.
    ///
    /// # Safety
    ///
    /// The node has not been released, and is of kind `Kind`; nothing writes
    /// its header concurrently.
    pub(super) unsafe fn key_slots<Kind: PageKind>(self) -> KeySlots<K, S> {
        let area = Self::key_area::<Kind>();
        // SAFETY: forwarded.
        let prefix_len = unsafe { self.prefix_len() }.unwrap_or(0);
        // SAFETY: the keys' offset is aligned for `K`, and so is their
        // stride, the inline keys' offset for inline keys of any width, and
        // `capacity` of each from there stay within a page of the node's
        // kind, `Kind`, and clear of each other (layout invariants); the
        // inline keys are of the width the layout was made for; the page is
        // allocated (caller).
        unsafe {
            let keys = Slots::with_stride(
                self.page.byte_add(area.keys_offset).cast(),
                area.capacity,
                area.key_stride,
            );
            let inline_keys = Option::zip(Self::inline_width::<Kind>(), area.inline_keys_offset)
                .map(|(width, offset)| {
                    InlineKeySlots::new(width, self.page.byte_add(offset), area.capacity)
                });
            KeySlots::new(
                keys,
                inline_keys,
                self.length_tag_slots::<Kind>(),
                prefix_len,
            )
        }
    }

    /// A leaf's value slots: `capacity` of them, value `i` beside key `i`
    /// (D29). They are [`Self::head_and_value_slots`] without the head:
    /// value slot `i` is head-and-value slot `i + 1`.
    ///
    /// # Safety
    ///
    /// The node is a leaf that has not been released.
    pub(super) unsafe fn value_slots(self) -> Slots<V> {
        let layout = Self::leaf_layout();
        // SAFETY: the page is a leaf's (caller), of this layout. Value 0 is
        // a stride past the head, both aligned for `V`, and so is the
        // stride, and `capacity` values from there stay in the page and
        // clear of the keys (layout invariants); the page is allocated
        // (caller).
        unsafe {
            Slots::with_stride(
                self.page
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
    /// The node is a leaf that has not been released.
    pub(super) unsafe fn head_and_value_slots(self) -> Slots<V> {
        let layout = Self::leaf_layout();
        // SAFETY: the page is a leaf's (caller), of this layout: the head
        // and `capacity` values after it, a stride apart, are aligned for
        // `V`, in the page and clear of the keys (layout invariants); the
        // page is allocated (caller).
        unsafe {
            Slots::with_stride(
                self.page.byte_add(layout.head_offset).cast(),
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
    /// The node is an internal node that has not been released.
    pub(super) unsafe fn edge_slots(self) -> Slots<Self> {
        let layout = Self::internal_layout();
        // SAFETY: the page is an internal node's (caller), of this layout.
        // The edges' offset and stride are aligned for an edge, a `NodePtr`
        // whatever the strategy, and the `capacity + 1` edges stay in the
        // page and clear of the keys (layout invariants); the page is
        // allocated (caller).
        unsafe {
            Slots::with_stride(
                self.page.byte_add(layout.edges_offset).cast(),
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
    pub(in crate::node) struct ByteString<T: ?Sized> {
        representation: ByteStringType,
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
    enum ByteStringType {
        /// The value is its bytes: `str` and `[u8]`.
        InPlace,
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
        #[inline(always)]
        pub(in crate::node) fn recognize() -> Option<Self> {
            let representation = if ErasedTypeId::is::<T, str>() || ErasedTypeId::is::<T, [u8]>() {
                ByteStringType::InPlace
            } else if ErasedTypeId::is::<T, [u8; 16]>()
                || ErasedTypeId::is::<T, [u8; 20]>()
                || ErasedTypeId::is::<T, [u8; 32]>()
            {
                ByteStringType::Array
            } else if ErasedTypeId::is::<T, String>() {
                ByteStringType::String
            } else if ErasedTypeId::is::<T, Vec<u8>>() {
                ByteStringType::ByteVec
            } else if ErasedTypeId::is::<T, Box<str>>() {
                ByteStringType::BoxedStr
            } else if ErasedTypeId::is::<T, Box<[u8]>>() {
                ByteStringType::BoxedBytes
            } else if ErasedTypeId::is::<T, &str>() {
                ByteStringType::StrRef
            } else if ErasedTypeId::is::<T, &[u8]>() {
                ByteStringType::BytesRef
            } else {
                return None;
            };
            Some(Self {
                representation,
                _type: PhantomData,
            })
        }

        /// Fixed arrays keep their bytes in the node, so a prefix tie
        /// needs no heap read. Two bytes retain a dense search array while
        /// leaving more room for entries. Heap-backed strings keep eight.
        #[inline(always)]
        pub(in crate::node) fn inline_width(self) -> InlineWidth {
            match self.representation {
                ByteStringType::Array => InlineWidth::Two,
                _ => InlineWidth::Eight,
            }
        }

        /// The bytes of `value`. Their length comes from `value` itself: for
        /// the types that hold their bytes elsewhere, from its length field,
        /// without reading what it points to.
        #[inline(always)]
        pub(in crate::node) fn bytes<'v>(self, value: &'v T) -> &'v [u8] {
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
                    ByteStringType::InPlace | ByteStringType::Array => {
                        slice::from_raw_parts(value.cast::<u8>(), size)
                    }
                    ByteStringType::String => (&*value.cast::<String>()).as_bytes(),
                    ByteStringType::ByteVec => (&*value.cast::<Vec<u8>>()).as_slice(),
                    ByteStringType::BoxedStr => (&*value.cast::<Box<str>>()).as_bytes(),
                    ByteStringType::BoxedBytes => &*value.cast::<Box<[u8]>>(),
                    ByteStringType::StrRef => (*value.cast::<&'v str>()).as_bytes(),
                    ByteStringType::BytesRef => *value.cast::<&'v [u8]>(),
                }
            }
        }
    }

    /// Proof that `T` is one of the std integer types that [`Auto`] gives
    /// inline keys, `u64`, `i64`, `u128` or `i128`, with that type's
    /// [`InlineKey`] impl as one for `T`.
    ///
    /// [`Auto`]: crate::Auto
    pub(in crate::node) struct StdInteger<T> {
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
        #[inline(always)]
        pub(in crate::node) fn recognize() -> Option<Self> {
            // SAFETY: each arm passes the type that `T` was just found to be;
            // integers have no lifetimes to erase.
            unsafe {
                if ErasedTypeId::is::<T, u64>() {
                    Some(Self::of::<u64>())
                } else if ErasedTypeId::is::<T, i64>() {
                    Some(Self::of::<i64>())
                } else if ErasedTypeId::is::<T, u128>() {
                    Some(Self::of::<u128>())
                } else if ErasedTypeId::is::<T, i128>() {
                    Some(Self::of::<i128>())
                } else {
                    None
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
        pub(in crate::node) fn inline_key_at(self) -> fn(&T, usize) -> InlineWord {
            self.inline_key_at
        }

        /// The integer's [`InlineKey::WIDTH`].
        #[inline(always)]
        pub(in crate::node) fn width(self) -> InlineWidth {
            self.width
        }
    }

    /// Proof that a lookup key type `Q` is the key type `K` itself,
    /// lifetimes included.
    pub(in crate::node) struct SameType<K, Q: ?Sized>(PhantomData<fn(&Q) -> &K>);

    impl<K: Borrow<Q>, Q: ?Sized> SameType<K, Q> {
        /// The proof, if `Q` is `K` lifetimes aside. Then they are the same
        /// type, lifetimes included, as `K: Borrow<Q>` holds: the only impl
        /// that can prove `Borrow<Q>` for a type that is `Q` lifetimes aside
        /// is std's `impl<T: ?Sized> Borrow<T> for T`, because any other
        /// would overlap with it (coherence ignores lifetimes) and is
        /// rejected; and that impl makes them one type. Folds to a constant
        /// after monomorphization.
        #[inline(always)]
        pub(in crate::node) fn check() -> Option<Self> {
            ErasedTypeId::is::<K, Q>().then_some(Self(PhantomData))
        }

        /// `key` as the key type it is.
        #[inline(always)]
        pub(in crate::node) fn cast(self, key: &Q) -> &K {
            // SAFETY: `Q` is `K`, lifetimes included (`check`), so `key`
            // points to a valid `K`, borrowed for as long as it says.
            unsafe { &*ptr::from_ref(key).cast::<K>() }
        }
    }
}

#[cfg(test)]
pub(super) use test_pages::TestPages;

#[cfg(test)]
mod test_pages {
    use core::alloc::Layout;
    use core::cell::RefCell;
    use core::marker::PhantomData;
    use core::ptr::NonNull;
    use std::alloc::{alloc, dealloc, handle_alloc_error};
    use std::vec::Vec;

    use super::{FreshPage, PageByKind, PageKind};
    use crate::inline_key::{Auto, InlineKeyStrategy};
    use crate::node::handle::{ForceResult, Internal, Leaf};

    /// A page allocator for tests: hands out pages for either node kind of a
    /// map with keys `K`, values `V` and strategy `S` from the global
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
    pub(in crate::node) struct TestPages<K, V, S = Auto> {
        /// Every page this pool allocated, with its layout.
        allocated: RefCell<Vec<(NonNull<u8>, Layout)>>,
        /// Released pages, with their layouts, ready for reuse.
        released: RefCell<Vec<(NonNull<u8>, Layout)>>,
        _marker: PhantomData<fn() -> (K, V)>,
        _strategy: PhantomData<fn() -> S>,
    }

    impl<K, V, S: InlineKeyStrategy<K>> TestPages<K, V, S> {
        pub(in crate::node) fn new() -> Self {
            Self {
                allocated: RefCell::new(Vec::new()),
                released: RefCell::new(Vec::new()),
                _marker: PhantomData,
                _strategy: PhantomData,
            }
        }

        /// A page for a node of kind `Kind`, reusing the most recently
        /// released page of that kind's layout if any.
        pub(in crate::node) fn fresh<Kind: PageKind>(&self) -> FreshPage<K, V, S, Kind> {
            let layout = FreshPage::<K, V, S, Kind>::layout();
            let mut released = self.released.borrow_mut();
            let reusable = (released.iter()).rposition(|&(_, page_layout)| page_layout == layout);
            let page = match reusable {
                Some(index) => released.remove(index).0,
                None => {
                    // SAFETY: `layout` has a non-zero size (at least 1 KB).
                    let raw = unsafe { alloc(layout) };
                    let page = NonNull::new(raw).unwrap_or_else(|| handle_alloc_error(layout));
                    self.allocated.borrow_mut().push((page, layout));
                    page
                }
            };
            // SAFETY: allocated by this pool from the global allocator with
            // `layout`, `Kind`'s, and either fresh or released, so nothing
            // else owns it. It is freed only in `drop`, and only if it was
            // released again by then.
            unsafe { FreshPage::new(page) }
        }

        /// Takes a page for either kind back, and scribbles over it so that
        /// nothing can rely on what it held.
        ///
        /// # Panics
        ///
        /// If the page is not one of this pool's, was allocated with another
        /// layout than its kind's, or is released twice.
        pub(in crate::node) fn release(&self, page: impl Into<PageByKind<K, V, S>>) {
            let (page, layout) = match page.into() {
                ForceResult::Leaf(page) => (page.into_raw(), FreshPage::<K, V, S, Leaf>::layout()),
                ForceResult::Internal(page) => {
                    (page.into_raw(), FreshPage::<K, V, S, Internal>::layout())
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
        pub(in crate::node) fn allocated_count(&self) -> usize {
            self.allocated.borrow().len()
        }

        /// The number of pages handed out and not released.
        pub(in crate::node) fn in_use(&self) -> usize {
            self.allocated.borrow().len() - self.released.borrow().len()
        }

        pub(in crate::node) fn assert_all_released(&self) {
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

    use super::{ByteString, FreshPage, NodeHeader, NodePtr, PageKind, SameType, TestPages};
    use crate::inline_key::{Auto, InlineKeyStrategy, InlineWidth, InlineWord, WithInlineKey};
    use crate::node::handle::{Internal, Leaf};
    use crate::node::layout::NodeLayout;
    use crate::node::slots::{InlineKeySlots, Slots};
    use crate::node::test_support::{CacheLineAligned, PageAligned, Tracked};

    /// The bytes `[start, end)` that an array of slots spans, and those of
    /// each of its slots, which may lie a stride apart (D29).
    struct SlotRange(Range<usize>, Vec<Range<usize>>);

    impl SlotRange {
        /// The bytes of the page of `node`, a node of kind `Kind`, checked to
        /// be aligned to the kind's layout.
        fn page_bytes<K, V, S: InlineKeyStrategy<K>, Kind: PageKind>(
            node: NodePtr<K, V, S>,
        ) -> Range<usize> {
            let layout = FreshPage::<K, V, S, Kind>::layout();
            let start = node.page.addr().get();
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
    /// kinds.
    fn assert_slots_are_aligned_and_disjoint<K, V, S: InlineKeyStrategy<K>>() {
        let pages = TestPages::<K, V, S>::new();
        let leaf = NodePtr::from_fresh_page(pages.fresh::<Leaf>());
        let internal = NodePtr::from_fresh_page(pages.fresh::<Internal>());
        let leaf_page = SlotRange::page_bytes::<K, V, S, Leaf>(leaf);
        let internal_page = SlotRange::page_bytes::<K, V, S, Internal>(internal);
        let leaf_capacity = NodePtr::<K, V, S>::capacity::<Leaf>();
        let internal_capacity = NodePtr::<K, V, S>::capacity::<Internal>();

        // SAFETY: both pages are allocated, and each is asked for the slots
        // of its own kind; nothing is dereferenced.
        let (leaf_keys, values, head_and_values, internal_keys, edges) = unsafe {
            (
                leaf.key_slots::<Leaf>(),
                SlotRange::of(leaf.value_slots(), leaf_capacity),
                SlotRange::of(leaf.head_and_value_slots(), leaf_capacity + 1),
                internal.key_slots::<Internal>(),
                SlotRange::of(internal.edge_slots(), internal_capacity + 1),
            )
        };
        assert_eq!(
            leaf_keys.inline_keys().map(InlineKeySlots::width),
            NodePtr::<K, V, S>::inline_width::<Leaf>()
        );
        assert_eq!(
            internal_keys.inline_keys().map(InlineKeySlots::width),
            NodePtr::<K, V, S>::inline_width::<Internal>()
        );
        let leaf_inline_keys =
            (leaf_keys.inline_keys()).map(|slots| SlotRange::of_inline_keys(slots, leaf_capacity));
        let internal_inline_keys = (internal_keys.inline_keys())
            .map(|slots| SlotRange::of_inline_keys(slots, internal_capacity));
        let leaf_keys = SlotRange::of(leaf_keys.keys(), leaf_capacity);
        let internal_keys = SlotRange::of(internal_keys.keys(), internal_capacity);

        // SAFETY: allocated nodes of the requested kind; only addresses
        // and the allocation-initialized tag slots are inspected.
        unsafe {
            assert!(internal.length_tag_slots::<Internal>().is_none());
            if let Some(tags) = leaf.length_tag_slots::<Leaf>() {
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
            }
        }

        for slots in [&leaf_keys, &values, &head_and_values] {
            slots.assert_within(&leaf_page);
        }
        for slots in [&internal_keys, &edges] {
            slots.assert_within(&internal_page);
        }
        let keys_offset = leaf_keys.0.start - leaf_page.start;
        assert!(size_of::<NodeHeader>() <= keys_offset);
        assert_eq!(values.1, head_and_values.1[1..]);
        leaf_keys.assert_disjoint(&head_and_values);
        internal_keys.assert_disjoint(&edges);
        match (leaf_inline_keys, internal_inline_keys) {
            (None, None) => {
                assert_eq!(internal_keys.0.start - internal_page.start, keys_offset);
            }
            (Some(leaf_inline_keys), Some(internal_inline_keys)) => {
                leaf_inline_keys.assert_within(&leaf_page);
                internal_inline_keys.assert_within(&internal_page);
                // Right after the header, and before the keys.
                assert_eq!(leaf_inline_keys.0.start - leaf_page.start, 8);
                assert_eq!(internal_inline_keys.0.start - internal_page.start, 8);
                assert!(leaf_inline_keys.0.end <= leaf_keys.0.start);
                assert!(internal_inline_keys.0.end <= internal_keys.0.start);
                leaf_inline_keys.assert_disjoint(&head_and_values);
                internal_inline_keys.assert_disjoint(&edges);
            }
            _ => unreachable!("both kinds have inline keys or neither"),
        }

        // SAFETY: the nodes hold nothing, each is of the kind it is released
        // as, and nothing uses them afterwards.
        unsafe {
            pages.release(leaf.into_fresh_page::<Leaf>());
            pages.release(internal.into_fresh_page::<Internal>());
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
            NodeLayout::<String, u64>::EIGHT_BYTE_INLINE_LEAF
        );
        assert_eq!(
            NodePtr::<Vec<u8>, u64, Auto>::internal_layout(),
            NodeLayout::<Vec<u8>, u64>::FOUR_BYTE_INLINE_INTERNAL
        );
        assert_eq!(
            NodePtr::<String, u64, Auto>::inline_width::<Leaf>(),
            Some(InlineWidth::Eight)
        );
        assert_eq!(
            NodePtr::<String, u64, Auto>::inline_width::<Internal>(),
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
        let node = NodePtr::from_fresh_page(pages.fresh::<Leaf>());
        // SAFETY: the page is allocated and holds a node.
        assert_eq!(unsafe { (node.len(), node.has_head()) }, (0, false));
        // SAFETY: as above.
        assert_eq!(unsafe { node.prefix_len() }, None);
        // SAFETY: the node is a leaf that holds nothing, and nothing uses it
        // afterwards.
        pages.release(unsafe { node.into_fresh_page::<Leaf>() });
        pages.assert_all_released();
    }

    /// The header of a node with inline keys reserves the prefix length of
    /// 05-fence-key.md, at 0.
    #[test]
    fn a_new_node_with_inline_keys_has_no_prefix() {
        let pages = TestPages::<String, u64>::new();
        // A page full of stale bytes, which the header must overwrite.
        pages.release(pages.fresh::<Internal>());
        let node = NodePtr::from_fresh_page(pages.fresh::<Internal>());
        // SAFETY: the page is allocated and holds a node.
        assert_eq!(unsafe { (node.len(), node.prefix_len()) }, (0, Some(0)));
        // SAFETY: the node is an internal node that holds nothing, and
        // nothing uses it afterwards.
        pages.release(unsafe { node.into_fresh_page::<Internal>() });
        pages.assert_all_released();
    }

    #[test]
    fn released_pages_are_handed_out_again() {
        let pages = TestPages::<u64, u64>::new();
        let first = pages.fresh::<Leaf>().into_raw();
        let second = pages.fresh::<Leaf>().into_raw();
        assert_ne!(first, second);
        // SAFETY: both came from `pages` as leaf pages, hold nothing, and
        // nothing else uses them.
        unsafe {
            pages.release(FreshPage::<_, _, Auto, Leaf>::new(first));
            pages.release(FreshPage::<_, _, Auto, Leaf>::new(second));
        }
        assert_eq!(pages.in_use(), 0);
        let reused = pages.fresh::<Leaf>();
        assert_eq!(pages.in_use(), 1);
        assert_eq!(reused.into_raw(), second);
        // SAFETY: as above.
        pages.release(unsafe { FreshPage::<_, _, Auto, Leaf>::new(second) });
        pages.assert_all_released();
    }

    /// A released page serves a node of either kind whose layout it has, and
    /// no other.
    #[test]
    fn released_pages_are_handed_out_for_their_layout_only() {
        // Both kinds are 1 KB pages: a leaf's page can hold an internal node.
        let pages = TestPages::<u64, u64>::new();
        pages.release(pages.fresh::<Leaf>());
        pages.release(pages.fresh::<Internal>());
        assert_eq!(pages.allocated_count(), 1);
        pages.assert_all_released();

        // A 13 KB leaf and a 1 KB internal node: neither reuses the other's.
        let pages = TestPages::<u64, [u8; 1000]>::new();
        pages.release(pages.fresh::<Leaf>());
        let internal = pages.fresh::<Internal>();
        assert_eq!((pages.allocated_count(), pages.in_use()), (2, 1));
        pages.release(internal);
        pages.release(pages.fresh::<Leaf>());
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
        let same = SameType::<&str, &str>::check().expect("the same type");
        assert_eq!(*same.cast(&borrowed), "same");
        assert!(SameType::<String, String>::check().is_some());
        assert!(SameType::<String, str>::check().is_none());
        assert!(SameType::<Vec<u8>, [u8]>::check().is_none());
        assert!(SameType::<&str, str>::check().is_none());
    }
}
