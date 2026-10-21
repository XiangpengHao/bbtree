//! [`HeapPage`]: a page of records from the allocator's heap method
//! ([`Allocator::allocate_heap`]), of `C::HEAP_PAGE_SIZE` bytes, aligned to
//! its size, so that a record finds its page by rounding its address down.
//!
//! ```text
//! | next | top | live | records … | room |
//! ```
//!
//! - **Records** go one after another, from the header to `top`, each where
//!   `top` was. One that dies leaves a hole, until its leaf slides the
//!   page's records together (`leaf_heap.rs`).
//! - **A chain** of pages is a leaf's (`leaf_heap.rs`): `next` links each
//!   to the one before it. Only the thread holding the chain's leaf
//!   exclusively reads or writes `next` and `top`, and frees the page.
//! - **`live`** counts the bytes of the page's live records. A record of
//!   another leaf may lie in a leaf's chain, once entries move between
//!   leaves; a thread holding that other leaf kills it, through `live`, an
//!   atomic.
//! - **An orphan** is a page no chain holds any more: a compaction moved
//!   its leaf's records out, and other leaves' are left. `live` keeps a bit
//!   for it, so that whichever thread empties an orphan, by its last kill
//!   or by orphaning it, frees it: one of them, once.

use core::alloc::Layout;
use core::marker::PhantomData;
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicU32, Ordering};

use bbtree::{AllocError, Allocator, NodeConfig};

/// A page's header, at its start.
#[repr(C)]
struct PageHeader {
    /// The page before this one in its chain; null at the chain's end.
    next: *mut u8,
    /// Where the next record goes, from the page's start.
    top: u32,
    /// The bytes of the page's live records, and [`HeapPage::ORPHAN`] once
    /// no chain holds it.
    live: AtomicU32,
}

/// A heap page of node config `C`, by its start.
pub(crate) struct HeapPage<C> {
    page: NonNull<u8>,
    _config: PhantomData<fn() -> C>,
}

// By hand: derives would bound `C`, which the marker doesn't hold.

impl<C> Clone for HeapPage<C> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<C> Copy for HeapPage<C> {}

impl<C> PartialEq for HeapPage<C> {
    fn eq(&self, other: &Self) -> bool {
        self.page == other.page
    }
}

impl<C: NodeConfig> HeapPage<C> {
    /// A page's size and alignment: a power of two, from 1 KiB to 1 GiB.
    pub(crate) const SIZE: usize = {
        let size = C::HEAP_PAGE_SIZE;
        assert!(
            size.is_power_of_two() && size >= 1024 && size <= 1 << 30,
            "a NodeConfig's HEAP_PAGE_SIZE is a power of two, from 1 KiB to 1 GiB"
        );
        size
    };

    /// Where the records start.
    const RECORDS_OFFSET: usize = size_of::<PageHeader>();

    /// The largest record a page holds.
    pub(crate) const ROOM: usize = Self::SIZE - Self::RECORDS_OFFSET;

    /// The bit of `live` that marks an orphan.
    const ORPHAN: u32 = 1 << 31;

    pub(crate) fn layout() -> Layout {
        Layout::from_size_align(Self::SIZE, Self::SIZE).expect("a heap page's layout is valid")
    }

    /// A new page from `alloc`, with no record, in no chain yet.
    pub(crate) fn allocate(alloc: &impl Allocator) -> Result<Self, AllocError> {
        let page = alloc.allocate_heap(Self::layout())?;
        // SAFETY: the page is new, aligned for its header, and ours alone.
        unsafe {
            page.cast::<PageHeader>().write(PageHeader {
                next: ptr::null_mut(),
                top: Self::RECORDS_OFFSET as u32,
                live: AtomicU32::new(0),
            });
        }
        Ok(Self {
            page,
            _config: PhantomData,
        })
    }

    /// Gives the page back to `alloc`.
    ///
    /// # Safety
    ///
    /// The page came from `alloc` or a clone of it, and no thread reads it
    /// again: its records are dead, and it is in no chain.
    pub(crate) unsafe fn deallocate(self, alloc: &impl Allocator) {
        // SAFETY: as the caller promises.
        unsafe { alloc.deallocate_heap(self.page, Self::layout()) }
    }

    /// The page at `page`; `None` for null.
    pub(crate) fn at(page: *mut u8) -> Option<Self> {
        NonNull::new(page).map(|page| Self {
            page,
            _config: PhantomData,
        })
    }

    /// The page that holds the record at `record`.
    pub(crate) fn of_record(record: NonNull<u8>) -> Self {
        let page = record.as_ptr().map_addr(|addr| addr & !(Self::SIZE - 1));
        Self::at(page).expect("a record lies in a heap page")
    }

    pub(crate) fn addr(self) -> *mut u8 {
        self.page.as_ptr()
    }

    fn header(self) -> *mut PageHeader {
        self.page.cast::<PageHeader>().as_ptr()
    }

    fn live_word(&self) -> &AtomicU32 {
        // SAFETY: the header is initialized, and `live` is an atomic every
        // thread reaches only as one.
        unsafe { &(*self.header()).live }
    }

    // The fields below are the chain's holder's alone (module
    // documentation): the caller holds the page's leaf exclusively.

    /// The page before this one in its chain.
    pub(crate) fn next(self) -> Option<Self> {
        // SAFETY: the header is initialized; the caller holds the chain.
        Self::at(unsafe { (*self.header()).next })
    }

    /// Links `next` after this page in its chain.
    pub(crate) fn set_next(self, next: Option<Self>) {
        // SAFETY: as for `next`.
        unsafe { (*self.header()).next = next.map_or(ptr::null_mut(), Self::addr) }
    }

    fn top(self) -> usize {
        // SAFETY: as for `next`.
        unsafe { (*self.header()).top as usize }
    }

    /// The bytes free after the last record.
    pub(crate) fn room(self) -> usize {
        Self::SIZE - self.top()
    }

    /// The bytes records took: the live ones' and the holes'.
    pub(crate) fn used(self) -> usize {
        self.top() - Self::RECORDS_OFFSET
    }

    /// The bytes of the page's live records.
    pub(crate) fn live(self) -> usize {
        (self.live_word().load(Ordering::Acquire) & !Self::ORPHAN) as usize
    }

    /// Room for a record of `size` bytes, live, after the last one; the
    /// page has the room.
    pub(crate) fn append(self, size: usize) -> NonNull<u8> {
        assert!(size <= self.room(), "a record goes where it fits");
        let top = self.top();
        // SAFETY: as for `next`; `top + size` is within the page.
        unsafe { (*self.header()).top = (top + size) as u32 }
        self.live_word().fetch_add(size as u32, Ordering::Relaxed);
        // SAFETY: `top` is within the page.
        unsafe { self.page.add(top) }
    }

    /// Where the first record goes.
    pub(crate) fn records_start(self) -> NonNull<u8> {
        // SAFETY: the records start within the page.
        unsafe { self.page.add(Self::RECORDS_OFFSET) }
    }

    /// Whether the record at `record` lies in this page.
    pub(crate) fn holds(self, record: NonNull<u8>) -> bool {
        Self::of_record(record) == self
    }

    /// Sets where the next record goes to `used` bytes past the first: the
    /// page's records, once slid together, end there.
    pub(crate) fn set_used(self, used: usize) {
        debug_assert!(used <= Self::ROOM);
        // SAFETY: as for `next`.
        unsafe { (*self.header()).top = (Self::RECORDS_OFFSET + used) as u32 }
    }

    /// `size` bytes of live records moved from this page to `to`, both of
    /// one chain, where only its leaf's records live.
    pub(crate) fn move_live(self, to: Self, size: usize) {
        if self != to {
            self.live_word().fetch_sub(size as u32, Ordering::Relaxed);
            to.live_word().fetch_add(size as u32, Ordering::Relaxed);
        }
    }

    /// `size` bytes of the page's live records died. Whether the page is an
    /// orphan they left empty, which the caller frees.
    pub(crate) fn kill(self, size: usize) -> bool {
        let before = self.live_word().fetch_sub(size as u32, Ordering::AcqRel);
        before - size as u32 == Self::ORPHAN
    }

    /// Lets the page go from its chain, which no longer holds it. Whether it
    /// was empty, which the caller frees.
    pub(crate) fn orphan(self) -> bool {
        self.set_next(None);
        self.live_word().fetch_or(Self::ORPHAN, Ordering::AcqRel) == 0
    }
}
