//! Where a map's memory comes from: an [`Allocator`], the map's last type
//! parameter, `A`, as in std's collections (D8). The default is [`Global`],
//! the global allocator.
//!
//! A map asks it for pages of three kinds, each by a method of its own:
//! internal nodes', leaves', and the root page, which holds the root node of
//! either kind (`tree/node/page.rs`); a fourth kind, heap pages, is for the
//! code that keeps a map's keys and values beside it (`bbtree-store`). Every
//! page of one kind in a map has one layout,
//! which the map's types and node config fix. By default every kind comes
//! from [`Allocator::allocate_zeroed`], the part of std's unstable
//! `core::alloc::Allocator` a map needs, with its names and contract, so
//! that it works on stable; an allocator that tells the kinds apart, to
//! budget, pool or place them apart, overrides the kinds' methods.

use core::alloc::Layout;
use core::error::Error;
use core::fmt;
use core::ptr::NonNull;

/// An allocator of the pages a map keeps its nodes in, and that the code
/// keeping a map's keys and values beside it keeps them in (heap pages).
///
/// A map asks only for pages of non-zero size, each kind by its own method:
/// [`Self::allocate_internal`], [`Self::allocate_leaf`] and
/// [`Self::allocate_root`]; heap pages come from [`Self::allocate_heap`].
/// It asks for every
/// page of one kind with one layout, the same each time: so an allocator
/// may keep each kind in a pool of pages of one size. By default each kind
/// comes from [`Self::allocate_zeroed`] and goes back to
/// [`Self::deallocate`].
///
/// A map gives every page back to its kind's method, with the layout it
/// asked for it with, to the allocator it came from or a clone of it. A
/// shared [`BBTreeMap`](crate::BBTreeMap) gives its internal nodes' and
/// leaves' pages back when it is dropped, as threads may still read them
/// until then; its root page too.
///
/// A failed allocation is an [`AllocError`]. The map for one thread,
/// [`unsync::BBTreeMap`](crate::unsync::BBTreeMap), then calls
/// [`handle_alloc_error`](alloc::alloc::handle_alloc_error), as std's
/// collections do. [`BBTreeMap`](crate::BBTreeMap) hands it to the caller,
/// with the map unchanged.
///
/// # Safety
///
/// - A page any of the allocating methods returns is valid for reads and
///   writes of `layout.size()` bytes, aligned to `layout.align()`, and
///   overlaps no other page handed out and not given back. It is all zero,
///   but for a heap page, which may hold anything.
/// - It stays valid until it is given back to its kind's deallocating
///   method, or until the allocator and every clone of it are dropped.
///   Moving the allocator leaves it valid.
/// - A clone of the allocator is the same allocator: it can take back what
///   the original handed out, and the other way round.
/// - No method unwinds. A map calls them in the middle of changing its
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

    /// A page for an internal node, zeroed; by default
    /// [`Self::allocate_zeroed`]'s.
    fn allocate_internal(&self, layout: Layout) -> Result<NonNull<u8>, AllocError> {
        self.allocate_zeroed(layout)
    }

    /// Takes back an internal node's page; by default with
    /// [`Self::deallocate`].
    ///
    /// # Safety
    ///
    /// `page` was handed out by `allocate_internal`, of this allocator or of
    /// a clone of it, for `layout`, and not given back since.
    unsafe fn deallocate_internal(&self, page: NonNull<u8>, layout: Layout) {
        // SAFETY: the default `allocate_internal` hands out
        // `allocate_zeroed`'s pages (caller).
        unsafe { self.deallocate(page, layout) }
    }

    /// A page for a leaf, zeroed; by default [`Self::allocate_zeroed`]'s.
    fn allocate_leaf(&self, layout: Layout) -> Result<NonNull<u8>, AllocError> {
        self.allocate_zeroed(layout)
    }

    /// Takes back a leaf's page; by default with [`Self::deallocate`].
    ///
    /// # Safety
    ///
    /// `page` was handed out by `allocate_leaf`, of this allocator or of a
    /// clone of it, for `layout`, and not given back since.
    unsafe fn deallocate_leaf(&self, page: NonNull<u8>, layout: Layout) {
        // SAFETY: as in `deallocate_internal`.
        unsafe { self.deallocate(page, layout) }
    }

    /// A tree's root page, zeroed, one a tree, which holds its root node of
    /// either kind; by default [`Self::allocate_zeroed`]'s.
    fn allocate_root(&self, layout: Layout) -> Result<NonNull<u8>, AllocError> {
        self.allocate_zeroed(layout)
    }

    /// Takes back a root page; by default with [`Self::deallocate`].
    ///
    /// # Safety
    ///
    /// `page` was handed out by `allocate_root`, of this allocator or of a
    /// clone of it, for `layout`, and not given back since.
    unsafe fn deallocate_root(&self, page: NonNull<u8>, layout: Layout) {
        // SAFETY: as in `deallocate_internal`.
        unsafe { self.deallocate(page, layout) }
    }

    /// A heap page, which holds keys and values that a map's leaves point
    /// to (`bbtree-store`, `HEAP_PAGE_SIZE`), of any contents; by default
    /// [`Self::allocate_zeroed`]'s.
    fn allocate_heap(&self, layout: Layout) -> Result<NonNull<u8>, AllocError> {
        self.allocate_zeroed(layout)
    }

    /// Takes back a heap page; by default with
    /// [`Self::deallocate`].
    ///
    /// # Safety
    ///
    /// `page` was handed out by `allocate_heap`, of this allocator or of a
    /// clone of it, for `layout`, and not given back since.
    unsafe fn deallocate_heap(&self, page: NonNull<u8>, layout: Layout) {
        // SAFETY: as in `deallocate_internal`.
        unsafe { self.deallocate(page, layout) }
    }
}

/// What a page is for, which tells which of an [`Allocator`]'s methods
/// hands it out and takes it back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PageKind {
    Internal,
    Leaf,
    Root,
}

impl PageKind {
    /// A page of this kind from `alloc`, for `layout`.
    pub(crate) fn allocate(
        self,
        alloc: &(impl Allocator + ?Sized),
        layout: Layout,
    ) -> Result<NonNull<u8>, AllocError> {
        match self {
            Self::Internal => alloc.allocate_internal(layout),
            Self::Leaf => alloc.allocate_leaf(layout),
            Self::Root => alloc.allocate_root(layout),
        }
    }

    /// Gives `page`, of this kind, back to `alloc`.
    ///
    /// # Safety
    ///
    /// `page` came from [`Self::allocate`] of this kind, of `alloc` or a
    /// clone of it, for `layout`, and is not given back since.
    pub(crate) unsafe fn deallocate(
        self,
        alloc: &(impl Allocator + ?Sized),
        page: NonNull<u8>,
        layout: Layout,
    ) {
        // SAFETY: the page came from this kind's allocating method (caller).
        unsafe {
            match self {
                Self::Internal => alloc.deallocate_internal(page, layout),
                Self::Leaf => alloc.deallocate_leaf(page, layout),
                Self::Root => alloc.deallocate_root(page, layout),
            }
        }
    }
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
// to does, which outlives it, kind by kind.
unsafe impl<A: Allocator + ?Sized> Allocator for &A {
    fn allocate_zeroed(&self, layout: Layout) -> Result<NonNull<u8>, AllocError> {
        (**self).allocate_zeroed(layout)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        // SAFETY: the caller's contract, which is the referred allocator's.
        unsafe { (**self).deallocate(ptr, layout) }
    }

    fn allocate_internal(&self, layout: Layout) -> Result<NonNull<u8>, AllocError> {
        (**self).allocate_internal(layout)
    }

    unsafe fn deallocate_internal(&self, page: NonNull<u8>, layout: Layout) {
        // SAFETY: as in `deallocate`.
        unsafe { (**self).deallocate_internal(page, layout) }
    }

    fn allocate_leaf(&self, layout: Layout) -> Result<NonNull<u8>, AllocError> {
        (**self).allocate_leaf(layout)
    }

    unsafe fn deallocate_leaf(&self, page: NonNull<u8>, layout: Layout) {
        // SAFETY: as in `deallocate`.
        unsafe { (**self).deallocate_leaf(page, layout) }
    }

    fn allocate_root(&self, layout: Layout) -> Result<NonNull<u8>, AllocError> {
        (**self).allocate_root(layout)
    }

    unsafe fn deallocate_root(&self, page: NonNull<u8>, layout: Layout) {
        // SAFETY: as in `deallocate`.
        unsafe { (**self).deallocate_root(page, layout) }
    }

    fn allocate_heap(&self, layout: Layout) -> Result<NonNull<u8>, AllocError> {
        (**self).allocate_heap(layout)
    }

    unsafe fn deallocate_heap(&self, page: NonNull<u8>, layout: Layout) {
        // SAFETY: as in `deallocate`.
        unsafe { (**self).deallocate_heap(page, layout) }
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
/// page to give, or whose closure refused for want of one
/// ([`BBTreeMap::update_or_insert_with_leaf`]): the map is as it was, and
/// the entry comes back.
///
/// [`BBTreeMap::update_or_insert_with_leaf`]: crate::BBTreeMap::update_or_insert_with_leaf
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
