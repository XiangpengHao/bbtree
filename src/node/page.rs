//! Node pages: the allocation boundary ([`FreshPage`]) and raw access to an
//! initialized node's header and slot arrays ([`NodePtr`]).

use alloc::alloc::{alloc, dealloc, handle_alloc_error};
use core::fmt;
use core::marker::PhantomData;
use core::ptr::NonNull;

use super::layout::NodeLayout;
use super::slots::{Down, Slots, Up};

/// An allocated page of `NodeLayout::<K, V>::LAYOUT` that holds no node.
///
/// A `FreshPage` owns its page, like a `Box` that has nothing in it: this is
/// the only way to create a node, and what a node turns back into once it is
/// released, ready to be reused or deallocated.
///
/// Invariant: the page comes from the global allocator, with
/// `NodeLayout::<K, V>::LAYOUT`, and nothing else uses it.
pub(crate) struct FreshPage<K, V> {
    page: NonNull<u8>,
    _marker: PhantomData<fn() -> (K, V)>,
}

/// Temporary allocation boundary, until the allocator trait (D8) replaces it:
/// one global-allocator call per node page. Every node is 4 KB-aligned (D7),
/// and an aligned allocation per page costs glibc about twice the memory of
/// the page itself (D8's measurement: 8200 B of RSS per 4 KB node); carving
/// pages from large chunks, as the allocator trait will, avoids that.
impl<K, V> FreshPage<K, V> {
    /// A page from the global allocator.
    ///
    /// Calls [`handle_alloc_error`] if the allocation fails.
    pub(crate) fn allocate() -> Self {
        let layout = NodeLayout::<K, V>::LAYOUT;
        // SAFETY: `layout` has a non-zero size: nodes are at least 4 KB.
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
        unsafe { dealloc(page.as_ptr(), NodeLayout::<K, V>::LAYOUT) }
    }
}

impl<K, V> FreshPage<K, V> {
    /// Takes ownership of a page. Its contents are treated as uninitialized.
    ///
    /// Panics if `page` is not aligned to `NodeLayout::<K, V>::LAYOUT`.
    ///
    /// # Safety
    ///
    /// - `page` was allocated by the global allocator with
    ///   `NodeLayout::<K, V>::LAYOUT`.
    /// - Until the page is handed back by [`Self::into_raw`], it stays
    ///   allocated and is accessed only through the returned value and the
    ///   nodes built from it.
    pub(crate) unsafe fn new(page: NonNull<u8>) -> Self {
        // Evaluating `LAYOUT` also runs the layout's compile-time assertions.
        let layout = NodeLayout::<K, V>::LAYOUT;
        assert!(
            page.addr().get().is_multiple_of(layout.align()),
            "node page is not aligned to {}",
            layout.align()
        );
        Self {
            page,
            _marker: PhantomData,
        }
    }

    /// Hands the page back, e.g. for deallocation by the global allocator
    /// with `NodeLayout::<K, V>::LAYOUT`.
    pub(crate) fn into_raw(self) -> NonNull<u8> {
        self.page
    }
}

impl<K, V> fmt::Debug for FreshPage<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("FreshPage").field(&self.page).finish()
    }
}

/// A pointer to an initialized node of `NodeLayout<K, V>`: the page of a
/// former [`FreshPage`] whose `len` header has been written.
///
/// It does not know the node's kind (the height does, D11), and it is what
/// internal nodes store as edges. Everything it offers is raw: the typed
/// handles in `handle.rs` are the only users.
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
    /// Turns a fresh page into an empty node (`len == 0`). An internal node
    /// still needs its first edge written.
    pub(super) fn from_fresh_page(page: FreshPage<K, V>) -> Self {
        let page = page.into_raw();
        // SAFETY: the page is at least `NODE_ALIGN`-aligned (checked in
        // `FreshPage::new`) and at least 4 KB long, so the `u16` header is in
        // bounds and aligned; the page is exclusively ours.
        unsafe { page.cast::<u16>().write(0) };
        Self {
            page,
            _marker: PhantomData,
        }
    }

    /// Gives the page back.
    ///
    /// # Safety
    ///
    /// The node's keys, values and edges have all been moved out, dropped or
    /// deliberately leaked, and neither this pointer nor any copy of it (an
    /// edge, a handle) is used afterwards.
    pub(super) unsafe fn into_fresh_page(self) -> FreshPage<K, V> {
        // The page came from a `FreshPage` (`from_fresh_page` is the only
        // constructor), so it is allocated with `LAYOUT` and aligned, and the
        // caller hands over every use of it: it is a `FreshPage` again.
        FreshPage {
            page: self.page,
            _marker: PhantomData,
        }
    }

    /// The `len` header.
    ///
    /// # Safety
    ///
    /// The node has not been released, and nothing writes its header
    /// concurrently.
    pub(super) unsafe fn len(self) -> usize {
        // SAFETY: the header was written by `from_fresh_page` and is aligned;
        // the page is allocated (caller).
        usize::from(unsafe { self.page.cast::<u16>().read() })
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
        unsafe { self.page.cast::<u16>().write(len) }
    }

    /// The key slots: `capacity` of them, growing up from `KEYS_OFFSET`.
    ///
    /// # Safety
    ///
    /// The node has not been released.
    pub(super) unsafe fn key_slots(self, is_leaf: bool) -> Slots<K, Up> {
        let capacity = Self::capacity(is_leaf);
        // SAFETY: `KEYS_OFFSET` is aligned for `K`, and `capacity` keys from
        // there stay within the page (layout invariants); the page is
        // allocated (caller).
        unsafe {
            Slots::new(
                self.page.byte_add(NodeLayout::<K, V>::KEYS_OFFSET).cast(),
                capacity,
            )
        }
    }

    /// The value slots: `capacity` of them, growing down from the values end
    /// of the node's kind (D9, D12).
    ///
    /// # Safety
    ///
    /// The node has not been released.
    pub(super) unsafe fn value_slots(self, is_leaf: bool) -> Slots<V, Down> {
        let capacity = Self::capacity(is_leaf);
        let values_end = if is_leaf {
            NodeLayout::<K, V>::LEAF_VALUES_END
        } else {
            NodeLayout::<K, V>::INTERNAL_VALUES_END
        };
        // SAFETY: the values end is aligned for `V` and `capacity` values
        // below it stay clear of the keys (layout invariants); the page is
        // allocated (caller).
        unsafe { Slots::new(self.page.byte_add(values_end).cast(), capacity) }
    }

    /// The `INTERNAL_CAPACITY + 1` edge slots of an internal node, growing up
    /// from `EDGES_OFFSET` to the end of the node (D12).
    ///
    /// # Safety
    ///
    /// The node has not been released.
    pub(super) unsafe fn edge_slots(self) -> Slots<Self, Up> {
        // SAFETY: `EDGES_OFFSET` is aligned for `NodePtr`, and the
        // `INTERNAL_CAPACITY + 1` edges end exactly at `NODE_SIZE` (layout
        // invariants); the page is allocated (caller).
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
    use core::cell::RefCell;
    use core::marker::PhantomData;
    use core::ptr::NonNull;
    use std::alloc::{alloc, dealloc, handle_alloc_error};
    use std::vec::Vec;

    use super::FreshPage;
    use crate::node::layout::NodeLayout;

    /// A page allocator for tests: hands out pages from the global allocator,
    /// takes released pages back for reuse, and frees them when dropped.
    ///
    /// Pages still out when it is dropped (owned by a leaked node) are never
    /// freed, so nothing can dangle; tests check
    /// [`TestPages::assert_all_released`] to catch such leaks.
    pub(in crate::node) struct TestPages<K, V> {
        /// Every page this pool allocated.
        allocated: RefCell<Vec<NonNull<u8>>>,
        /// Released pages, ready for reuse.
        released: RefCell<Vec<NonNull<u8>>>,
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

        /// A page, reusing the most recently released one if any.
        pub(in crate::node) fn fresh(&self) -> FreshPage<K, V> {
            let page = self.released.borrow_mut().pop().unwrap_or_else(|| {
                let layout = NodeLayout::<K, V>::LAYOUT;
                // SAFETY: `layout` has a non-zero size (at least 4 KB).
                let raw = unsafe { alloc(layout) };
                let page = NonNull::new(raw).unwrap_or_else(|| handle_alloc_error(layout));
                self.allocated.borrow_mut().push(page);
                page
            });
            // SAFETY: allocated by this pool from the global allocator with
            // `LAYOUT`, and either fresh or released, so nothing else owns it.
            // It is freed only in `drop`, and only if it was released again
            // by then.
            unsafe { FreshPage::new(page) }
        }

        /// Takes a page back, and scribbles over it so that nothing can rely
        /// on what it held.
        ///
        /// # Panics
        ///
        /// If the page is not one of this pool's, or is released twice.
        pub(in crate::node) fn release(&self, page: FreshPage<K, V>) {
            let page = page.into_raw();
            assert!(
                self.allocated.borrow().contains(&page),
                "released a page from elsewhere"
            );
            assert!(
                !self.released.borrow().contains(&page),
                "released a page twice"
            );
            // SAFETY: the page was allocated by this pool with `LAYOUT`, and as
            // a `FreshPage` it is used by nothing else.
            unsafe { page.write_bytes(0xA5, NodeLayout::<K, V>::LAYOUT.size()) };
            self.released.borrow_mut().push(page);
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
            for page in self.released.get_mut().drain(..) {
                // SAFETY: allocated by `fresh` with this layout; a released
                // page has no owner left, and each is freed once.
                unsafe { dealloc(page.as_ptr(), NodeLayout::<K, V>::LAYOUT) };
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

    use super::{FreshPage, NodePtr, TestPages};
    use crate::node::layout::NodeLayout;
    use crate::node::slots::{Direction, Slots};
    use crate::node::test_support::CacheLineAligned;

    /// The bytes `[start, end)` that an array of slots occupies.
    struct SlotRange(Range<usize>);

    impl SlotRange {
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

    /// Checks the slot arrays of both node kinds: inside the page, aligned,
    /// after the header, and pairwise disjoint.
    fn assert_slots_are_aligned_and_disjoint<K, V>() {
        let pages = TestPages::<K, V>::new();
        let node = NodePtr::from_fresh_page(pages.fresh());
        let start = node.page.addr().get();
        assert!(start.is_multiple_of(4096));
        let page = start..start + NodeLayout::<K, V>::NODE_SIZE;
        let leaf_capacity = NodePtr::<K, V>::capacity(true);
        let internal_capacity = NodePtr::<K, V>::capacity(false);

        // SAFETY: the page is allocated; nothing is dereferenced.
        let (leaf_keys, leaf_values, internal_keys, internal_values, edges) = unsafe {
            (
                SlotRange::of(node.key_slots(true), leaf_capacity),
                SlotRange::of(node.value_slots(true), leaf_capacity),
                SlotRange::of(node.key_slots(false), internal_capacity),
                SlotRange::of(node.value_slots(false), internal_capacity),
                SlotRange::of(node.edge_slots(), internal_capacity + 1),
            )
        };
        for slots in [
            &leaf_keys,
            &leaf_values,
            &internal_keys,
            &internal_values,
            &edges,
        ] {
            slots.assert_within(&page);
        }
        assert!(start + size_of::<u16>() <= leaf_keys.0.start);
        assert_eq!(leaf_keys.0.start, internal_keys.0.start);
        leaf_keys.assert_disjoint(&leaf_values);
        internal_keys.assert_disjoint(&internal_values);
        internal_keys.assert_disjoint(&edges);
        internal_values.assert_disjoint(&edges);

        // SAFETY: the node holds nothing, and nothing uses it afterwards.
        pages.release(unsafe { node.into_fresh_page() });
        pages.assert_all_released();
    }

    #[test]
    fn slots_are_aligned_and_disjoint() {
        assert_slots_are_aligned_and_disjoint::<u64, u64>();
        assert_slots_are_aligned_and_disjoint::<u8, ()>();
        assert_slots_are_aligned_and_disjoint::<(), ()>();
        assert_slots_are_aligned_and_disjoint::<u64, [u8; 1000]>();
        assert_slots_are_aligned_and_disjoint::<CacheLineAligned, u64>();
        assert_slots_are_aligned_and_disjoint::<u8, CacheLineAligned>();
        assert_slots_are_aligned_and_disjoint::<String, Vec<u8>>();
    }

    #[test]
    fn a_new_node_is_empty() {
        let pages = TestPages::<u64, u64>::new();
        let node = NodePtr::from_fresh_page(pages.fresh());
        // SAFETY: the page is allocated and holds a node.
        assert_eq!(unsafe { node.len() }, 0);
        // SAFETY: the node holds nothing, and nothing uses it afterwards.
        pages.release(unsafe { node.into_fresh_page() });
        pages.assert_all_released();
    }

    #[test]
    fn released_pages_are_handed_out_again() {
        let pages = TestPages::<u64, u64>::new();
        let first = pages.fresh().into_raw();
        let second = pages.fresh().into_raw();
        assert_ne!(first, second);
        // SAFETY: both came from `pages` as `FreshPage`s, hold nothing, and
        // nothing else uses them.
        unsafe {
            pages.release(FreshPage::new(first));
            pages.release(FreshPage::new(second));
        }
        assert_eq!(pages.in_use(), 0);
        let reused = pages.fresh();
        assert_eq!(pages.in_use(), 1);
        assert_eq!(reused.into_raw(), second);
        // SAFETY: as above.
        pages.release(unsafe { FreshPage::new(second) });
        pages.assert_all_released();
    }
}
