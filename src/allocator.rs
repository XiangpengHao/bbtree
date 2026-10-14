//! Where a map's memory comes from: an [`Allocator`], the map's last type
//! parameter, `A`, as in std's collections (D8). The default is [`Global`],
//! the global allocator.
//!
//! An allocator deals in sizes and alignments only. A map asks it for one
//! block per node, of the few layouts the map's types fix, and makes each
//! block a leaf, an internal node or the root's page itself
//! (`tree/node/page.rs`). The trait is the part of std's unstable
//! `core::alloc::Allocator` a map needs, with its names and contract, so
//! that it works on stable.

use core::alloc::Layout;
use core::error::Error;
use core::fmt;
use core::ptr::NonNull;

/// An allocator of the blocks a map keeps its nodes in.
///
/// A map asks only for blocks of non-zero size, and gives every block
/// back, with the layout it asked for it with, to the allocator it came
/// from or a clone of it.
///
/// # Safety
///
/// - A block `allocate` returns is valid for reads and writes of
///   `layout.size()` bytes, aligned to `layout.align()`, and overlaps no
///   other block handed out and not given back.
/// - It stays valid until it is given back to `deallocate`, or until the
///   allocator and every clone of it are dropped. Moving the allocator
///   leaves it valid.
/// - A clone of the allocator is the same allocator: it can take back what
///   the original handed out, and the other way round.
pub unsafe trait Allocator {
    /// A block of memory for `layout`, uninitialized; or [`AllocError`] if
    /// none is to be had.
    fn allocate(&self, layout: Layout) -> Result<NonNull<u8>, AllocError>;

    /// Takes back the block at `ptr`.
    ///
    /// # Safety
    ///
    /// `ptr` was handed out by `allocate`, of this allocator or of a clone
    /// of it, for `layout`, and not given back since.
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout);
}

/// The global allocator, which `#[global_allocator]` picks: what a map
/// uses unless given another.
///
/// It refuses blocks of size zero, which a map never asks for.
#[derive(Clone, Copy, Debug, Default)]
pub struct Global;

// SAFETY: every block comes from the global allocator, which keeps it valid
// until it is freed, whichever `Global` frees it: all of them are the same
// allocator.
unsafe impl Allocator for Global {
    fn allocate(&self, layout: Layout) -> Result<NonNull<u8>, AllocError> {
        if layout.size() == 0 {
            return Err(AllocError);
        }
        // SAFETY: `layout` has a non-zero size, checked above.
        NonNull::new(unsafe { alloc::alloc::alloc(layout) }).ok_or(AllocError)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        // SAFETY: `ptr` came from `allocate`, from the global allocator,
        // for `layout`, and is given back once (the caller's contract).
        unsafe { alloc::alloc::dealloc(ptr.as_ptr(), layout) }
    }
}

// SAFETY: a reference hands out and takes back what the allocator it refers
// to does, which outlives it.
unsafe impl<A: Allocator + ?Sized> Allocator for &A {
    fn allocate(&self, layout: Layout) -> Result<NonNull<u8>, AllocError> {
        (**self).allocate(layout)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        // SAFETY: the caller's contract, which is the referred allocator's.
        unsafe { (**self).deallocate(ptr, layout) }
    }
}

/// An allocator had no block to give.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AllocError;

impl fmt::Display for AllocError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("memory allocation failed")
    }
}

impl Error for AllocError {}
