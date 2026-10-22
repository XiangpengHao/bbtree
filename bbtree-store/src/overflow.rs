//! [`Overflow`]: the pages of a value too long for its record
//! (`record.rs`), heap pages of their own ([`Allocator::allocate_heap`]),
//! each the address of the next, then value bytes, every one full but,
//! perhaps, the last.
//!
//! ```text
//! | next | value bytes … |
//! ```
//!
//! The pages are their record's: a record that moves (`leaf_heap.rs`)
//! takes its link to them along, and leaves them where they are; they go
//! back when it dies. No chain holds them, and only their record points to
//! them, so a thread reads them only while it holds the record's leaf, as
//! it reads the record.
//!
//! [`Allocator::allocate_heap`]: bbtree::Allocator::allocate_heap

use core::marker::PhantomData;
use core::ptr::{self, NonNull};
use std::vec::Vec;

use bbtree::{AllocError, Allocator, NodeConfig};

use crate::heap_page::HeapPage;

/// A value's overflow pages, by the first, of node config `C`.
pub(crate) struct Overflow<C> {
    first: NonNull<u8>,
    _config: PhantomData<fn() -> C>,
}

// By hand: derives would bound `C`, which the marker doesn't hold.

impl<C> Clone for Overflow<C> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<C> Copy for Overflow<C> {}

impl<C: NodeConfig> Overflow<C> {
    /// The size of a page's link to the next.
    const LINK: usize = size_of::<*mut u8>();

    /// The value bytes a page holds.
    pub(crate) const ROOM: usize = HeapPage::<C>::SIZE - Self::LINK;

    /// Writes `bytes`, not empty, into new pages from `alloc`; or
    /// [`AllocError`], with the pages it took given back, if one is not to
    /// be had.
    pub(crate) fn write(bytes: &[u8], alloc: &impl Allocator) -> Result<Self, AllocError> {
        // From the last page back to the first, each linking the next.
        let mut next = None::<Self>;
        for part in bytes.chunks(Self::ROOM).rev() {
            let page = match alloc.allocate_heap(HeapPage::<C>::layout()) {
                Ok(page) => page,
                Err(error) => {
                    if let Some(next) = next {
                        // SAFETY: the pages are new, from `alloc`, and no
                        // record links them.
                        unsafe { next.free(alloc) };
                    }
                    return Err(error);
                }
            };
            let link = next.map_or(ptr::null_mut(), |next| next.first.as_ptr());
            // SAFETY: the page is new, a heap page's size, aligned for the
            // link, and ours alone; the part fits after the link.
            unsafe {
                page.cast::<*mut u8>().write(link);
                (page.add(Self::LINK))
                    .copy_from_nonoverlapping(NonNull::from(part).cast(), part.len());
            }
            next = Some(Self::at(page));
        }
        Ok(next.expect("an overflow holds some bytes"))
    }

    /// The pages a record's link points to.
    pub(crate) fn at(first: NonNull<u8>) -> Self {
        Self {
            first,
            _config: PhantomData,
        }
    }

    /// The link to the pages, which their record keeps.
    pub(crate) fn link(self) -> NonNull<u8> {
        self.first
    }

    /// Appends the `len` bytes the pages hold to `buf`.
    pub(crate) fn append_to(self, len: usize, buf: &mut Vec<u8>) {
        let (mut page, mut left) = (self.first, len);
        while left > 0 {
            let part = left.min(Self::ROOM);
            // SAFETY: a page holds its link, then its bytes: the pages hold
            // `len` (caller), every one full but the last. Their record is
            // read, so they stay.
            unsafe {
                let bytes = core::slice::from_raw_parts(page.add(Self::LINK).as_ptr(), part);
                buf.extend_from_slice(bytes);
                left -= part;
                if left > 0 {
                    page =
                        NonNull::new(page.cast::<*mut u8>().read()).expect("a page links the next");
                }
            }
        }
    }

    /// Gives the pages back to `alloc`.
    ///
    /// # Safety
    ///
    /// The pages came from `alloc` or a clone of it, and no thread reads
    /// them again: their record is dead, or never went in.
    pub(crate) unsafe fn free(self, alloc: &impl Allocator) {
        let mut page = Some(self.first);
        while let Some(this) = page {
            // SAFETY: a page starts with its link, null in the last; the
            // page goes back once it is read, as the caller lets it.
            unsafe {
                page = NonNull::new(this.cast::<*mut u8>().read());
                alloc.deallocate_heap(this, HeapPage::<C>::layout());
            }
        }
    }
}
