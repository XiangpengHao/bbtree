//! Node pages: the allocation boundary ([`FreshPage`]) and raw access to an
//! initialized node's header and slot arrays ([`NodePtr`]).

use alloc::alloc::{alloc, dealloc, handle_alloc_error};
use core::alloc::Layout;
use core::fmt;
use core::marker::PhantomData;
use core::ptr::NonNull;

use super::handle::{ForceResult, Internal, Leaf, NodeKind};
use super::layout::NodeLayout;
use super::slots::{Down, Slots, Up};

/// A node kind that pages are allocated for: [`Leaf`] or [`Internal`]. Each
/// kind has a page layout of its own (D21), and a page holds nodes of the
/// kind it was allocated for only.
///
/// # Safety
///
/// `IS_LEAF` is what [`NodeKind::is_leaf`] says at every height: a page of
/// this kind gets the layout of that kind of node, and the node code
/// accesses the node made from it with that kind's offsets and capacity.
pub(crate) unsafe trait PageKind: NodeKind {
    const IS_LEAF: bool;
}

// SAFETY: `Leaf::is_leaf` is always `true`.
unsafe impl PageKind for Leaf {
    const IS_LEAF: bool = true;
}

// SAFETY: `Internal::is_leaf` is always `false`.
unsafe impl PageKind for Internal {
    const IS_LEAF: bool = false;
}

/// An allocated page for a node of kind `Kind` ([`Leaf`] or [`Internal`])
/// that holds no node.
///
/// A `FreshPage` owns its page, like a `Box` that has nothing in it: this is
/// the only way to create a node, and what a node turns back into once it is
/// released, ready to be reused or deallocated. The kinds have different
/// layouts (D21), and the type carries the kind, so a page only ever holds a
/// node of the kind it was allocated for, and is deallocated with the layout
/// it was allocated with.
///
/// Invariant: the page comes from the global allocator, with
/// [`Self::LAYOUT`], and nothing else uses it.
pub(crate) struct FreshPage<K, V, Kind> {
    page: NonNull<u8>,
    _marker: PhantomData<fn() -> (K, V)>,
    /// The kind of node the page is for.
    _kind: PhantomData<Kind>,
}

/// A fresh page sorted by the kind of node it is for: what releasing a node
/// whose kind is known only at run time gives back.
pub(crate) type PageByKind<K, V> = ForceResult<FreshPage<K, V, Leaf>, FreshPage<K, V, Internal>>;

impl<K, V, Kind: PageKind> FreshPage<K, V, Kind> {
    /// The allocation layout of every page of this kind (D21). Evaluating it
    /// also runs the layout's compile-time assertions.
    const LAYOUT: Layout = if Kind::IS_LEAF {
        NodeLayout::<K, V>::LEAF_LAYOUT
    } else {
        NodeLayout::<K, V>::INTERNAL_LAYOUT
    };

    /// Takes ownership of a page. Its contents are treated as uninitialized.
    ///
    /// Panics if `page` is not aligned to [`Self::LAYOUT`].
    ///
    /// # Safety
    ///
    /// - `page` was allocated by the global allocator with [`Self::LAYOUT`].
    /// - Until the page is handed back by [`Self::into_raw`], it stays
    ///   allocated and is accessed only through the returned value and the
    ///   nodes built from it.
    pub(crate) unsafe fn new(page: NonNull<u8>) -> Self {
        let layout = Self::LAYOUT;
        assert!(
            page.addr().get().is_multiple_of(layout.align()),
            "node page is not aligned to {}",
            layout.align()
        );
        Self {
            page,
            _marker: PhantomData,
            _kind: PhantomData,
        }
    }

    /// Hands the page back, e.g. for deallocation by the global allocator
    /// with [`Self::LAYOUT`].
    pub(crate) fn into_raw(self) -> NonNull<u8> {
        self.page
    }
}

/// Temporary allocation boundary, until the allocator trait (D8) replaces it:
/// one global-allocator call per node page. Every node is aligned to its own
/// size, at least 1 KB (D15), and glibc serves such aligned allocations
/// poorly (D8's measurement: 8200 B of RSS per 4 KB-aligned 4 KB page);
/// carving pages from large chunks, as the allocator trait will, avoids that.
impl<K, V, Kind: PageKind> FreshPage<K, V, Kind> {
    /// A page from the global allocator.
    ///
    /// Calls [`handle_alloc_error`] if the allocation fails.
    pub(crate) fn allocate() -> Self {
        let layout = Self::LAYOUT;
        // SAFETY: `layout` has a non-zero size: nodes are at least 1 KB.
        let page = unsafe { alloc(layout) };
        let Some(page) = NonNull::new(page) else {
            handle_alloc_error(layout)
        };
        // SAFETY: the page was just allocated by the global allocator with
        // `LAYOUT`, and nothing else has it.
        unsafe { Self::new(page) }
    }

    /// Gives the page back to the global allocator.
    pub(crate) fn deallocate(self) {
        let page = self.into_raw();
        // SAFETY: the page came from the global allocator with `LAYOUT` and is
        // used by nothing else (type invariant); consuming `self` gives up the
        // last use of it.
        unsafe { dealloc(page.as_ptr(), Self::LAYOUT) }
    }
}

impl<K, V, Kind> fmt::Debug for FreshPage<K, V, Kind> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("FreshPage").field(&self.page).finish()
    }
}

impl<K, V> PageByKind<K, V> {
    /// Gives the page back to the global allocator, with its kind's layout.
    pub(crate) fn deallocate(self) {
        match self {
            ForceResult::Leaf(page) => page.deallocate(),
            ForceResult::Internal(page) => page.deallocate(),
        }
    }
}

impl<K, V> From<FreshPage<K, V, Leaf>> for PageByKind<K, V> {
    fn from(page: FreshPage<K, V, Leaf>) -> Self {
        ForceResult::Leaf(page)
    }
}

impl<K, V> From<FreshPage<K, V, Internal>> for PageByKind<K, V> {
    fn from(page: FreshPage<K, V, Internal>) -> Self {
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

/// A pointer to an initialized node of `NodeLayout<K, V>`: the page of a
/// former [`FreshPage`] whose header has been written.
///
/// It does not know the node's kind (the height does, D11), and it is what
/// internal nodes store as edges. The kinds have different layouts (D21), so
/// every slot array it hands out needs the caller to know the kind. Everything
/// it offers is raw: the typed handles in `handle.rs` are the only users.
#[repr(transparent)]
pub(super) struct NodePtr<K, V> {
    page: NonNull<u8>,
    _marker: PhantomData<*const (K, V)>,
}

impl<K, V> Clone for NodePtr<K, V> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K, V> Copy for NodePtr<K, V> {}

impl<K, V> NodePtr<K, V> {
    /// Turns a fresh page into an empty node of the page's kind: no keys
    /// and, for a leaf, no head. An internal node still needs its first edge
    /// written.
    pub(super) fn from_fresh_page<Kind: PageKind>(page: FreshPage<K, V, Kind>) -> Self {
        let page = page.into_raw();
        // SAFETY: the page is aligned to its kind's layout (checked in
        // `FreshPage::new`), whose alignment the header's divides, and at
        // least 1 KB long, so the header is in bounds and aligned (layout
        // invariants); the page is exclusively ours.
        unsafe {
            page.cast::<NodeHeader>().write(NodeHeader {
                len: 0,
                has_head: false,
            });
        }
        Self {
            page,
            _marker: PhantomData,
        }
    }

    /// Gives the page back, as a fresh page for the node's kind.
    ///
    /// # Safety
    ///
    /// - The node is of kind `Kind`.
    /// - The node's keys, values, head and edges have all been moved out,
    ///   dropped or deliberately leaked, and neither this pointer nor any
    ///   copy of it (an edge, a handle) is used afterwards.
    pub(super) unsafe fn into_fresh_page<Kind: PageKind>(self) -> FreshPage<K, V, Kind> {
        // The page came from a `FreshPage` (`from_fresh_page` is the only
        // constructor) for the node's kind, which is `Kind` (caller), so it
        // is allocated with `FreshPage::<K, V, Kind>::LAYOUT` and aligned,
        // and the caller hands over every use of it: it is a `FreshPage`
        // again.
        FreshPage {
            page: self.page,
            _marker: PhantomData,
            _kind: PhantomData,
        }
    }

    /// The address of the node's first byte, for prefetching. Every access
    /// through it goes through the methods here.
    pub(super) fn page_start(self) -> *const u8 {
        self.page.as_ptr()
    }

    /// The header, written by `from_fresh_page`.
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

    /// The key slots: `capacity` of them, growing up from `KEYS_OFFSET`.
    ///
    /// # Safety
    ///
    /// The node has not been released, and is a leaf exactly if `is_leaf`.
    pub(super) unsafe fn key_slots(self, is_leaf: bool) -> Slots<K, Up> {
        let capacity = Self::capacity(is_leaf);
        // SAFETY: `KEYS_OFFSET` is aligned for `K`, and `capacity` keys from
        // there stay within a page of the node's kind, which `is_leaf` names
        // (layout invariants); the page is allocated (caller).
        unsafe {
            Slots::new(
                self.page.byte_add(NodeLayout::<K, V>::KEYS_OFFSET).cast(),
                capacity,
            )
        }
    }

    /// A leaf's value slots: `LEAF_CAPACITY` of them, growing down from the
    /// head (D17). They are [`Self::head_and_value_slots`] without the head:
    /// value slot `i` is head-and-value slot `i + 1`.
    ///
    /// # Safety
    ///
    /// The node is a leaf that has not been released.
    pub(super) unsafe fn value_slots(self) -> Slots<V, Down> {
        // SAFETY: the page is a leaf's (caller), `LEAF_SIZE` long.
        // `HEAD_OFFSET` is aligned for `V`, and `LEAF_CAPACITY` values below
        // it stay clear of the keys (layout invariants); the page is
        // allocated (caller).
        unsafe {
            Slots::new(
                self.page.byte_add(NodeLayout::<K, V>::HEAD_OFFSET).cast(),
                NodeLayout::<K, V>::LEAF_CAPACITY,
            )
        }
    }

    /// A leaf's head and value slots as one array: `LEAF_CAPACITY + 1` slots
    /// growing down from the end of the node, slot 0 the head and slot
    /// `i + 1` value `i` (D17). Moves that carry a head along with values use
    /// it.
    ///
    /// # Safety
    ///
    /// The node is a leaf that has not been released.
    pub(super) unsafe fn head_and_value_slots(self) -> Slots<V, Down> {
        // SAFETY: the page is a leaf's (caller), so its end is at
        // `LEAF_SIZE`, aligned for `V`, and the head and `LEAF_CAPACITY`
        // values below it stay clear of the keys (layout invariants); the
        // page is allocated (caller).
        unsafe {
            Slots::new(
                self.page.byte_add(NodeLayout::<K, V>::LEAF_SIZE).cast(),
                NodeLayout::<K, V>::LEAF_CAPACITY + 1,
            )
        }
    }

    /// The `INTERNAL_CAPACITY + 1` edge slots of an internal node, growing up
    /// from `EDGES_OFFSET` to the end of the node (D12).
    ///
    /// # Safety
    ///
    /// The node is an internal node that has not been released.
    pub(super) unsafe fn edge_slots(self) -> Slots<Self, Up> {
        // SAFETY: the page is an internal node's (caller), `INTERNAL_SIZE`
        // long. `EDGES_OFFSET` is aligned for `NodePtr`, and the
        // `INTERNAL_CAPACITY + 1` edges end exactly at `INTERNAL_SIZE`
        // (layout invariants); the page is allocated (caller).
        unsafe {
            Slots::new(
                self.page.byte_add(NodeLayout::<K, V>::EDGES_OFFSET).cast(),
                NodeLayout::<K, V>::INTERNAL_CAPACITY + 1,
            )
        }
    }

    /// The maximum number of keys of a node of either kind.
    pub(super) fn capacity(is_leaf: bool) -> usize {
        if is_leaf {
            NodeLayout::<K, V>::LEAF_CAPACITY
        } else {
            NodeLayout::<K, V>::INTERNAL_CAPACITY
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
    use crate::node::handle::{ForceResult, Internal, Leaf};

    /// A page allocator for tests: hands out pages for either node kind from
    /// the global allocator, takes released pages back for reuse, and frees
    /// them when dropped.
    ///
    /// It keeps each page's layout: a released page is handed out again only
    /// for a kind of the same layout, and a page released under a kind of
    /// another layout than it was allocated with panics.
    ///
    /// Pages still out when it is dropped (owned by a leaked node) are never
    /// freed, so nothing can dangle; tests check
    /// [`TestPages::assert_all_released`] to catch such leaks.
    pub(in crate::node) struct TestPages<K, V> {
        /// Every page this pool allocated, with its layout.
        allocated: RefCell<Vec<(NonNull<u8>, Layout)>>,
        /// Released pages, with their layouts, ready for reuse.
        released: RefCell<Vec<(NonNull<u8>, Layout)>>,
        _marker: PhantomData<fn() -> (K, V)>,
    }

    impl<K, V> TestPages<K, V> {
        pub(in crate::node) fn new() -> Self {
            Self {
                allocated: RefCell::new(Vec::new()),
                released: RefCell::new(Vec::new()),
                _marker: PhantomData,
            }
        }

        /// A page for a node of kind `Kind`, reusing the most recently
        /// released page of that kind's layout if any.
        pub(in crate::node) fn fresh<Kind: PageKind>(&self) -> FreshPage<K, V, Kind> {
            let layout = FreshPage::<K, V, Kind>::LAYOUT;
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
        pub(in crate::node) fn release(&self, page: impl Into<PageByKind<K, V>>) {
            let (page, layout) = match page.into() {
                ForceResult::Leaf(page) => (page.into_raw(), FreshPage::<K, V, Leaf>::LAYOUT),
                ForceResult::Internal(page) => {
                    (page.into_raw(), FreshPage::<K, V, Internal>::LAYOUT)
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

    impl<K, V> Drop for TestPages<K, V> {
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
    use std::string::String;
    use std::vec::Vec;

    use super::{FreshPage, NodeHeader, NodePtr, PageKind, TestPages};
    use crate::node::handle::{Internal, Leaf};
    use crate::node::slots::{Direction, Slots};
    use crate::node::test_support::{CacheLineAligned, PageAligned};

    /// The bytes `[start, end)` that an array of slots occupies.
    struct SlotRange(Range<usize>);

    impl SlotRange {
        /// The bytes of the page of `node`, a node of kind `Kind`, checked to
        /// be aligned to the kind's layout.
        fn page_bytes<K, V, Kind: PageKind>(node: NodePtr<K, V>) -> Range<usize> {
            let layout = FreshPage::<K, V, Kind>::LAYOUT;
            let start = node.page.addr().get();
            assert!(start.is_multiple_of(layout.align()));
            start..start + layout.size()
        }

        /// The bytes of all `capacity` slots, each checked to be aligned.
        fn of<T, Dir: Direction>(slots: Slots<T, Dir>, capacity: usize) -> Self {
            for index in 0..capacity {
                // SAFETY: `index < capacity`; nothing is dereferenced.
                let slot = unsafe { slots.slot(index) };
                assert!(slot.addr().get().is_multiple_of(align_of::<T>()));
            }
            // SAFETY: `capacity` is the slots' own capacity; nothing is
            // dereferenced.
            let start = unsafe { slots.run(0, capacity) }.cast::<T>().addr().get();
            Self(start..start + capacity * size_of::<T>())
        }

        fn assert_within(&self, page: &Range<usize>) {
            assert!(page.start <= self.0.start && self.0.end <= page.end);
        }

        fn assert_disjoint(&self, other: &Self) {
            assert!(self.0.end <= other.0.start || other.0.end <= self.0.start);
        }
    }

    /// Checks the slot arrays of both node kinds, each in a page of its own
    /// kind: inside the page, aligned, after the header, and pairwise
    /// disjoint, with the keys at the same offset in both kinds, a leaf's
    /// head the last `V` of its page and its values right below it (D17),
    /// and an internal node's edges at the end of its page.
    fn assert_slots_are_aligned_and_disjoint<K, V>() {
        let pages = TestPages::<K, V>::new();
        let leaf = NodePtr::from_fresh_page(pages.fresh::<Leaf>());
        let internal = NodePtr::from_fresh_page(pages.fresh::<Internal>());
        let leaf_page = SlotRange::page_bytes::<K, V, Leaf>(leaf);
        let internal_page = SlotRange::page_bytes::<K, V, Internal>(internal);
        let leaf_capacity = NodePtr::<K, V>::capacity(true);
        let internal_capacity = NodePtr::<K, V>::capacity(false);

        // SAFETY: both pages are allocated, and each is asked for the slots
        // of its own kind; nothing is dereferenced.
        let (leaf_keys, values, head_and_values, internal_keys, edges) = unsafe {
            (
                SlotRange::of(leaf.key_slots(true), leaf_capacity),
                SlotRange::of(leaf.value_slots(), leaf_capacity),
                SlotRange::of(leaf.head_and_value_slots(), leaf_capacity + 1),
                SlotRange::of(internal.key_slots(false), internal_capacity),
                SlotRange::of(internal.edge_slots(), internal_capacity + 1),
            )
        };
        for slots in [&leaf_keys, &values, &head_and_values] {
            slots.assert_within(&leaf_page);
        }
        for slots in [&internal_keys, &edges] {
            slots.assert_within(&internal_page);
        }
        let keys_offset = leaf_keys.0.start - leaf_page.start;
        assert!(size_of::<NodeHeader>() <= keys_offset);
        assert_eq!(internal_keys.0.start - internal_page.start, keys_offset);
        assert_eq!(head_and_values.0.end, leaf_page.end);
        assert_eq!(
            values.0,
            head_and_values.0.start..head_and_values.0.end - size_of::<V>()
        );
        leaf_keys.assert_disjoint(&head_and_values);
        assert_eq!(edges.0.end, internal_page.end);
        internal_keys.assert_disjoint(&edges);

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
        assert_slots_are_aligned_and_disjoint::<u64, u64>();
        assert_slots_are_aligned_and_disjoint::<u8, ()>();
        assert_slots_are_aligned_and_disjoint::<(), ()>();
        assert_slots_are_aligned_and_disjoint::<u64, [u8; 1000]>();
        assert_slots_are_aligned_and_disjoint::<[u8; 77], ()>();
        assert_slots_are_aligned_and_disjoint::<CacheLineAligned, u64>();
        assert_slots_are_aligned_and_disjoint::<u8, CacheLineAligned>();
        assert_slots_are_aligned_and_disjoint::<u64, PageAligned>();
        assert_slots_are_aligned_and_disjoint::<String, Vec<u8>>();
    }

    #[test]
    fn a_new_node_is_empty() {
        let pages = TestPages::<u64, u64>::new();
        let node = NodePtr::from_fresh_page(pages.fresh::<Leaf>());
        // SAFETY: the page is allocated and holds a node.
        assert_eq!(unsafe { (node.len(), node.has_head()) }, (0, false));
        // SAFETY: the node is a leaf that holds nothing, and nothing uses it
        // afterwards.
        pages.release(unsafe { node.into_fresh_page::<Leaf>() });
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
            pages.release(FreshPage::<_, _, Leaf>::new(first));
            pages.release(FreshPage::<_, _, Leaf>::new(second));
        }
        assert_eq!(pages.in_use(), 0);
        let reused = pages.fresh::<Leaf>();
        assert_eq!(pages.in_use(), 1);
        assert_eq!(reused.into_raw(), second);
        // SAFETY: as above.
        pages.release(unsafe { FreshPage::<_, _, Leaf>::new(second) });
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
}
