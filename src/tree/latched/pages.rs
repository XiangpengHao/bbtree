//! Type-stable pages: a latched tree's node pages stay node pages of their
//! kind for as long as threads share the tree (`doc/09-concurrency.md`).
//!
//! An optimistic reader follows pointers it read without a latch, and only
//! later checks they were current. A node a merge emptied may be such a
//! pointer's target, so its page can't go back to the allocator, which
//! could hand it to anything. It goes on the tree's free list for its kind
//! instead ([`FreeLists`]), and comes back only as a node of that kind, with
//! the latch word it had, whose version only grows: a reader that reaches it
//! reads a node of the kind its pointer says, and its check fails. The pages
//! go back to the allocator when the map is dropped.
//!
//! An operation allocates and frees through [`NodePages`], which hands out a
//! free page of the kind first, holds every node page it hands out until
//! the operation is done, and keeps the pages the operation frees until
//! then too, after the operation lets go of its nodes: a page is free only
//! once its latch word says it changed.

use core::alloc::Layout;
use core::cell::RefCell;
use core::hint;
use core::mem::size_of;
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use super::super::node::latch::Version;
use super::super::node::page::LatchedNodeHeader;
use super::super::node_stack::MAX_DEPTH;
use crate::allocator::{AllocError, Allocator};

/// The free pages of one kind, a stack linked through the pages, behind a
/// spin lock: only splits and merges take and give pages.
struct FreeList {
    locked: AtomicBool,
    top: AtomicPtr<u8>,
}

/// The free lists of a latched tree's two kinds of node.
pub(super) struct FreeLists {
    leaves: FreeList,
    internals: FreeList,
}

impl FreeList {
    /// Where a free page keeps the next one: past the header and its latch
    /// word, in the first inline-key slots, which a reader that still reads
    /// the page as a node reads as plain data.
    const LINK_OFFSET: usize = size_of::<LatchedNodeHeader>();

    const fn new() -> Self {
        Self {
            locked: AtomicBool::new(false),
            top: AtomicPtr::new(ptr::null_mut()),
        }
    }

    /// Runs `f` with the list locked.
    fn with_locked<R>(&self, f: impl FnOnce() -> R) -> R {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            hint::spin_loop();
        }
        let result = f();
        self.locked.store(false, Ordering::Release);
        result
    }

    /// The link slot of free page `page`.
    fn link(page: NonNull<u8>) -> *mut *mut u8 {
        // SAFETY: every node page is larger than its header and a pointer.
        unsafe { page.byte_add(Self::LINK_OFFSET).cast::<*mut u8>().as_ptr() }
    }

    /// Puts `page`, a node page of this list's kind that no thread changes
    /// any more, on the list.
    fn push(&self, page: NonNull<u8>) {
        self.with_locked(|| {
            // SAFETY: the page is allocated, and no thread writes it: what
            // it held is gone, and readers that still read it only read.
            unsafe { Self::link(page).write(self.top.load(Ordering::Relaxed)) };
            self.top.store(page.as_ptr(), Ordering::Relaxed);
        });
    }

    /// The page on top of the list, taken off it, if any.
    fn pop(&self) -> Option<NonNull<u8>> {
        // Most allocations find the list empty: they write nothing to it.
        if self.top.load(Ordering::Relaxed).is_null() {
            return None;
        }
        self.with_locked(|| {
            let page = NonNull::new(self.top.load(Ordering::Relaxed))?;
            // SAFETY: a page on the list keeps its successor in its link
            // slot, which only the list writes.
            self.top
                .store(unsafe { Self::link(page).read() }, Ordering::Relaxed);
            Some(page)
        })
    }
}

impl FreeLists {
    pub(super) const fn new() -> Self {
        Self {
            leaves: FreeList::new(),
            internals: FreeList::new(),
        }
    }

    /// Gives every free page back to `alloc`, leaves for `leaf` and internal
    /// nodes for `internal`, once no other thread reaches the tree.
    pub(super) fn deallocate_all(
        &mut self,
        alloc: &impl Allocator,
        leaf: Layout,
        internal: Layout,
    ) {
        for (list, layout) in [(&self.leaves, leaf), (&self.internals, internal)] {
            while let Some(page) = list.pop() {
                // SAFETY: a free page came from `alloc`, the map's allocator,
                // for its kind's layout, and nothing reaches it any more.
                unsafe { alloc.deallocate(page, layout) };
            }
        }
    }
}

/// The pages one operation on a latched tree allocates and frees: an
/// allocator over the map's, `alloc`, which the node code takes as any.
///
/// - Allocating a leaf's or an internal node's page takes one off the free
///   list of its kind if there is one, as it was; otherwise a fresh page
///   from `alloc` ([`Self::allocate_fresh`]). Either way, the page comes
///   latched exclusively, and stays so until this is dropped: once the
///   operation links it into the tree, other threads find it held until
///   the operation is done with it.
/// - Freeing a node page keeps it until this is dropped, then puts it on
///   the free list of its kind: the operation lets go of its nodes first,
///   which tells readers the page changed, before another thread may take
///   it.
/// - Any other page, the root's, comes fresh and unlatched, and goes back
///   to `alloc` at once: the tree keeps its root page for life.
pub(super) struct NodePages<'t, A> {
    free: &'t FreeLists,
    alloc: &'t A,
    leaf: Layout,
    internal: Layout,
    taken: RefCell<TakenPages>,
}

/// The node pages an operation took: those it allocated, latched at a
/// version, a page per level split and one for a raised root's node, and
/// those it freed, a page per level merged and the root's child when the
/// root lowers.
struct TakenPages {
    allocated: [Option<(NonNull<u8>, Version)>; MAX_DEPTH + 2],
    allocated_len: usize,
    freed: [Option<(NonNull<u8>, bool)>; MAX_DEPTH + 2],
    freed_len: usize,
}

impl<'t, A: Allocator> NodePages<'t, A> {
    /// Pages from `alloc` and `free`, leaves of layout `leaf` and internal
    /// nodes of layout `internal`, which differ.
    pub(super) fn new(free: &'t FreeLists, alloc: &'t A, leaf: Layout, internal: Layout) -> Self {
        debug_assert_ne!(leaf, internal, "a leaf's page tells its kind by its layout");
        Self {
            free,
            alloc,
            leaf,
            internal,
            taken: RefCell::new(TakenPages {
                allocated: [None; MAX_DEPTH + 2],
                allocated_len: 0,
                freed: [None; MAX_DEPTH + 2],
                freed_len: 0,
            }),
        }
    }

    /// The free list for pages of `layout`, if they are a node kind's.
    fn list(&self, layout: Layout) -> Option<&'t FreeList> {
        if layout == self.leaf {
            Some(&self.free.leaves)
        } else if layout == self.internal {
            Some(&self.free.internals)
        } else {
            None
        }
    }

    /// A page fresh from `alloc` for `layout`, its latch word unheld. A
    /// leaf's is read past what its node writes only under its latch; any
    /// other page is zeroed, an internal node's or the root's, which may hold
    /// one, so that every edge an optimistic reader may read before its
    /// check is initialized.
    fn allocate_fresh(&self, layout: Layout) -> Result<NonNull<u8>, AllocError> {
        let page = self.alloc.allocate(layout)?;
        // SAFETY: the block was just allocated for `layout`, a latched tree's
        // page, which no other thread reaches yet.
        unsafe {
            if layout == self.leaf {
                LatchedNodeHeader::initialize_latch(page);
            } else {
                page.write_bytes(0, layout.size());
            }
        }
        Ok(page)
    }

    /// Whether the operation freed the node page at `page`.
    pub(super) fn has_freed(&self, page: NonNull<u8>) -> bool {
        let taken = self.taken.borrow();
        taken.freed[..taken.freed_len]
            .iter()
            .flatten()
            .any(|&(freed, _)| freed == page)
    }
}

// SAFETY: every block is `alloc`'s, for its layout: one taken off a free
// list was allocated for the same layout, and is in no node any more. A
// freed block goes back to `alloc` only through `FreeLists::deallocate_all`,
// or at once for a page that is no node's, whose layout it came with.
unsafe impl<A: Allocator> Allocator for NodePages<'_, A> {
    fn allocate(&self, layout: Layout) -> Result<NonNull<u8>, AllocError> {
        let Some(list) = self.list(layout) else {
            return self.allocate_fresh(layout);
        };
        let page = match list.pop() {
            Some(page) => page,
            None => self.allocate_fresh(layout)?,
        };
        // SAFETY: the page is a node page of the tree's, allocated, its latch
        // word initialized fresh or kept since. A thread that reached a free
        // page through a stale pointer may hold it a moment, and waits for
        // nothing meanwhile.
        let version = unsafe { LatchedNodeHeader::latch_of(page) }.lock_exclusive();
        let mut taken = self.taken.borrow_mut();
        let len = taken.allocated_len;
        taken.allocated[len] = Some((page, version));
        taken.allocated_len += 1;
        Ok(page)
    }

    unsafe fn deallocate(&self, ptr: NonNull<u8>, layout: Layout) {
        match self.list(layout) {
            Some(_) => {
                let mut taken = self.taken.borrow_mut();
                let len = taken.freed_len;
                taken.freed[len] = Some((ptr, layout == self.leaf));
                taken.freed_len += 1;
            }
            // SAFETY: forwarded.
            None => unsafe { self.alloc.deallocate(ptr, layout) },
        }
    }
}

impl<A> Drop for NodePages<'_, A> {
    fn drop(&mut self) {
        let taken = self.taken.get_mut();
        for (page, version) in taken.allocated[..taken.allocated_len].iter().flatten() {
            // SAFETY: the page is a node page the operation allocated and
            // latched, still allocated: on a free list at most.
            unsafe { LatchedNodeHeader::latch_of(*page) }.unlock_exclusive(*version);
        }
        for &(page, is_leaf) in taken.freed[..taken.freed_len].iter().flatten() {
            if is_leaf {
                self.free.leaves.push(page);
            } else {
                self.free.internals.push(page);
            }
        }
    }
}
