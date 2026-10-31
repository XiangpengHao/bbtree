//! Type-stable pages: a latched tree's node pages stay node pages of their
//! kind for as long as threads share the tree (`design/09-concurrency.md`).
//!
//! An optimistic reader follows pointers it read without a latch, and only
//! later checks they were current. A node a merge emptied may be such a
//! pointer's target, so its page can't go back to the allocator, which
//! could hand it to anything. It goes on the tree's free list for its kind
//! instead ([`FreeLists`]). It comes back only as a node of that kind, with
//! the latch word it had, whose version only grows. So a reader that
//! reaches it reads a node of the kind its pointer says, and its version
//! check fails. The pages go back to the allocator when the map is dropped.
//!
//! An operation allocates and frees through [`NodePages`]. It takes the
//! pages an operation will allocate before the operation changes anything,
//! free pages of their kind first, so that an allocator with none to give
//! fails the operation whole. It holds every node page it hands out until
//! the operation is done. It keeps the pages the operation frees until then
//! too, after the operation lets go of its nodes: a page is free only once
//! its latch word says it changed.

use alloc::alloc::handle_alloc_error;
use core::alloc::Layout;
use core::cell::RefCell;
use core::mem::size_of;
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

use super::super::insert::SplitPages;
use super::super::interleaving::{Backoff, Interleaving};
use super::super::node::latch::Version;
use super::super::node::page::{LatchedNodeHeader, PageAllocator};
use super::super::node_stack::MAX_DEPTH;
use crate::allocator::{AllocError, Allocator, PageKind};

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

    fn with_locked<R>(&self, f: impl FnOnce() -> R) -> R {
        let mut backoff = Backoff::new();
        loop {
            Interleaving::switch_point();
            if self
                .locked
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }
            backoff.wait();
        }
        let result = f();
        Interleaving::switch_point();
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
        Interleaving::switch_point();
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

    /// The free list of `kind`'s pages.
    fn list(&self, kind: PageKind) -> &FreeList {
        match kind {
            PageKind::Leaf => &self.leaves,
            PageKind::Internal => &self.internals,
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
        let lists = [
            (&self.leaves, PageKind::Leaf, leaf),
            (&self.internals, PageKind::Internal, internal),
        ];
        for (list, kind, layout) in lists {
            while let Some(page) = list.pop() {
                // SAFETY: a free page came from `alloc`, the map's allocator,
                // for its kind and layout, and nothing reaches it any more.
                unsafe { kind.deallocate(alloc, page, layout) };
            }
        }
    }
}

/// The pages one operation on a latched tree allocates and frees: an
/// allocator of node pages over the map's, `alloc`, which the node code
/// takes as any.
///
/// - The operation first takes the pages it will allocate
///   ([`Self::reserve`]): each off the free list of its kind if there is
///   one, as it was; otherwise fresh from `alloc`, zeroed. Allocating a
///   leaf's or an internal node's page then hands out one of those, latched
///   exclusively, and it stays so until this is dropped: once the operation
///   links it into the tree, other threads find it held until the operation
///   is done with it.
/// - Freeing a node page keeps it until this is dropped, then puts it on
///   the free list of its kind. The operation drops this last, after it
///   lets go of its nodes, which tells readers the page changed; only then
///   may another thread take the page.
///
/// It deals in node pages only: the tree keeps its root page for life.
pub(super) struct NodePages<'t, A> {
    free: &'t FreeLists,
    alloc: &'t A,
    leaf: Layout,
    internal: Layout,
    taken: RefCell<TakenPages>,
}

/// The node pages an operation took:
/// - those it reserved and hasn't allocated yet, each with its kind;
/// - those it allocated, each latched at a version: a page per level split,
///   and one for a raised root's node;
/// - those it freed, each with its kind: a page per level merged, and the
///   root's child when the root lowers.
struct TakenPages {
    reserved: [Option<(NonNull<u8>, PageKind)>; MAX_DEPTH + 2],
    reserved_len: usize,
    allocated: [Option<(NonNull<u8>, Version)>; MAX_DEPTH + 2],
    allocated_len: usize,
    freed: [Option<(NonNull<u8>, PageKind)>; MAX_DEPTH + 2],
    freed_len: usize,
}

impl TakenPages {
    /// Puts every reserved page on its kind's free list in `free`.
    fn give_back_reserved(&mut self, free: &FreeLists) {
        for (page, kind) in self.reserved.iter_mut().filter_map(Option::take) {
            free.list(kind).push(page);
        }
        self.reserved_len = 0;
    }

    /// A reserved page of `kind`, taken off the reserved ones.
    fn take_reserved(&mut self, kind: PageKind) -> Option<NonNull<u8>> {
        let index = (self.reserved[..self.reserved_len].iter())
            .position(|page| page.is_some_and(|(_, reserved)| reserved == kind))?;
        let last = self.reserved_len - 1;
        self.reserved.swap(index, last);
        self.reserved_len = last;
        self.reserved[last].take().map(|(page, _)| page)
    }
}

impl<'t, A: Allocator> NodePages<'t, A> {
    /// Pages from `alloc` and `free`, leaves of layout `leaf` and internal
    /// nodes of layout `internal`.
    pub(super) fn new(free: &'t FreeLists, alloc: &'t A, leaf: Layout, internal: Layout) -> Self {
        Self {
            free,
            alloc,
            leaf,
            internal,
            taken: RefCell::new(TakenPages {
                reserved: [None; MAX_DEPTH + 2],
                reserved_len: 0,
                allocated: [None; MAX_DEPTH + 2],
                allocated_len: 0,
                freed: [None; MAX_DEPTH + 2],
                freed_len: 0,
            }),
        }
    }

    /// A page for a new node of `kind`: one off the free list of its kind,
    /// as it was; otherwise fresh from `alloc`, zeroed, so that its latch
    /// word is unheld at the first version, and every edge an optimistic
    /// reader may read before its check is initialized.
    fn take(&self, kind: PageKind) -> Result<NonNull<u8>, AllocError> {
        match self.free.list(kind).pop() {
            Some(page) => Ok(page),
            None => kind.allocate(self.alloc, self.layout(kind)),
        }
    }

    /// Takes `split`'s pages, every page the operation will allocate, before
    /// it changes anything; or, if `alloc` has none to give, takes none, and
    /// returns the layout it was asked for.
    pub(super) fn reserve(&self, split: SplitPages) -> Result<(), Layout> {
        let mut taken = self.taken.borrow_mut();
        debug_assert_eq!(
            taken.reserved_len, 0,
            "an operation reserves its pages once"
        );
        for kind in split.kinds() {
            let Ok(page) = self.take(kind) else {
                taken.give_back_reserved(self.free);
                return Err(self.layout(kind));
            };
            let len = taken.reserved_len;
            taken.reserved[len] = Some((page, kind));
            taken.reserved_len += 1;
        }
        Ok(())
    }

    /// Puts back the pages [`Self::reserve`] took, on the free lists: the
    /// operation changed its mind, and allocates none of them.
    pub(super) fn unreserve(&self) {
        self.taken.borrow_mut().give_back_reserved(self.free);
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

// SAFETY: every page is `alloc`'s, for its kind and layout: one taken off
// a free list was allocated for the same, and is in no node any more. A
// freed page goes back to `alloc` only through `FreeLists::deallocate_all`.
// Nothing here unwinds but the debug assertions on the operation's own
// count.
unsafe impl<A: Allocator> PageAllocator for NodePages<'_, A> {
    fn allocate_page(&self, kind: PageKind, layout: Layout) -> NonNull<u8> {
        debug_assert_eq!(
            layout,
            self.layout(kind),
            "a node kind's pages have one layout"
        );
        let mut taken = self.taken.borrow_mut();
        let reserved = taken.take_reserved(kind);
        debug_assert!(
            reserved.is_some(),
            "an operation allocates only pages it reserved"
        );
        let page = reserved.unwrap_or_else(|| {
            self.take(kind)
                .unwrap_or_else(|AllocError| handle_alloc_error(layout))
        });
        // SAFETY: the page is a node page of the tree's, allocated, its latch
        // word zeroed fresh or kept since. A thread that reached a free page
        // through a stale pointer may hold it a moment, and waits for nothing
        // meanwhile.
        let version = unsafe { LatchedNodeHeader::latch_of(page) }.lock_exclusive();
        let len = taken.allocated_len;
        taken.allocated[len] = Some((page, version));
        taken.allocated_len += 1;
        page
    }

    unsafe fn deallocate_page(&self, kind: PageKind, page: NonNull<u8>, _layout: Layout) {
        let mut taken = self.taken.borrow_mut();
        let len = taken.freed_len;
        taken.freed[len] = Some((page, kind));
        taken.freed_len += 1;
    }
}

impl<A> NodePages<'_, A> {
    /// The layout of `kind`'s pages.
    fn layout(&self, kind: PageKind) -> Layout {
        match kind {
            PageKind::Leaf => self.leaf,
            PageKind::Internal => self.internal,
        }
    }
}

impl<A> Drop for NodePages<'_, A> {
    fn drop(&mut self) {
        let taken = self.taken.get_mut();
        debug_assert_eq!(
            taken.reserved_len, 0,
            "an operation allocates every page it reserved"
        );
        for (page, version) in taken.allocated[..taken.allocated_len].iter().flatten() {
            // SAFETY: the page is a node page the operation allocated and
            // latched, still allocated: on a free list at most.
            unsafe { LatchedNodeHeader::latch_of(*page) }.unlock_exclusive(*version);
        }
        for &(page, kind) in taken.freed[..taken.freed_len].iter().flatten() {
            self.free.list(kind).push(page);
        }
    }
}
