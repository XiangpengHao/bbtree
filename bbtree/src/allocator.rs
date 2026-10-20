//! Where a map's memory comes from: an [`Allocator`], the map's last type
//! parameter, `A`, as in std's collections (D8). The default is [`Global`],
//! the global allocator.
//!
//! An allocator deals in sizes and alignments only. A map asks it for one
//! zeroed block per node, of the few layouts the map's types fix, and makes
//! each block a leaf, an internal node or the root's page itself
//! (`tree/node/page.rs`). The trait is the part of std's unstable
//! `core::alloc::Allocator` a map needs, with its names and contract, so
//! that it works on stable.

use core::alloc::Layout;
use core::error::Error;
use core::fmt;
use core::ptr::NonNull;

/// An allocator of the zeroed blocks a map keeps its nodes in.
///
/// A map asks only for blocks of non-zero size, and gives every block
/// back, with the layout it asked for it with, to the allocator it came
/// from or a clone of it.
///
/// A failed allocation is an [`AllocError`]. The map for one thread,
/// [`unsync::BBTreeMap`](crate::unsync::BBTreeMap), then calls
/// [`handle_alloc_error`](alloc::alloc::handle_alloc_error), as std's
/// collections do. [`BBTreeMap`](crate::BBTreeMap) hands it to the caller,
/// with the map unchanged.
///
/// # Safety
///
/// - A block `allocate_zeroed` returns is valid for reads and writes of
///   `layout.size()` bytes, aligned to `layout.align()`, all zero, and
///   overlaps no other block handed out and not given back.
/// - It stays valid until it is given back to `deallocate`, or until the
///   allocator and every clone of it are dropped. Moving the allocator
///   leaves it valid.
/// - A clone of the allocator is the same allocator: it can take back what
///   the original handed out, and the other way round.
/// - Neither method unwinds. A map calls them in the middle of changing its
///   nodes, and doesn't guard against a panic there.
pub unsafe trait Allocator {
    /// A block of memory for `layout`, zeroed; or [`AllocError`] if none is
    /// to be had.
    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<u8>, AllocError>;

    /// Takes back the block at `ptr`.
    ///
    /// # Safety
    ///
    /// `ptr` was handed out by `allocate_zeroed`, of this allocator or of a
    /// clone of it, for `layout`, and not given back since.
    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout);
}

/// The global allocator, which `#[global_allocator]` picks: what a map
/// uses unless given another.
///
/// It refuses blocks of size zero, which a map never asks for.
#[derive(Clone, Copy, Debug, Default)]
pub struct Global;

// SAFETY: every block comes zeroed from the global allocator, which keeps
// it valid until it is freed, whichever `Global` frees it: all of them are
// the same allocator. A `GlobalAlloc` must not unwind.
unsafe impl Allocator for Global {
    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<u8>, AllocError> {
        if layout.size() == 0 {
            return Err(AllocError);
        }
        // SAFETY: `layout` has a non-zero size, checked above.
        NonNull::new(unsafe { alloc::alloc::alloc_zeroed(layout) }).ok_or(AllocError)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        // SAFETY: `ptr` came from `allocate_zeroed`, from the global
        // allocator, for `layout`, and is given back once (the caller's
        // contract).
        unsafe { alloc::alloc::dealloc(ptr.as_ptr(), layout) }
    }
}

// SAFETY: a reference hands out and takes back what the allocator it refers
// to does, which outlives it.
unsafe impl<A: Allocator + ?Sized> Allocator for &A {
    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<u8>, AllocError> {
        (**self).allocate_zeroed(layout)
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

/// An insert into a [`BBTreeMap`](crate::BBTreeMap) whose allocator had no
/// page to give: the map is as it was, and the entry comes back.
pub struct InsertError<K, V> {
    key: K,
    value: V,
    /// The layout of the page the allocator was asked for.
    layout: Layout,
}

impl<K, V> InsertError<K, V> {
    pub(crate) fn new(key: K, value: V, layout: Layout) -> Self {
        Self { key, value, layout }
    }

    /// The key and value the insert was given.
    pub fn into_entry(self) -> (K, V) {
        (self.key, self.value)
    }

    /// Calls [`handle_alloc_error`](alloc::alloc::handle_alloc_error), as
    /// std's collections do where they can't hand a failure back.
    pub(crate) fn handle_alloc_error(self) -> ! {
        alloc::alloc::handle_alloc_error(self.layout)
    }
}

impl<K, V> From<InsertError<K, V>> for AllocError {
    fn from(_: InsertError<K, V>) -> Self {
        AllocError
    }
}

impl<K, V> fmt::Debug for InsertError<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InsertError").finish_non_exhaustive()
    }
}

impl<K, V> fmt::Display for InsertError<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&AllocError, f)
    }
}

impl<K, V> Error for InsertError<K, V> {}
