//! The pages an insert's split takes, from the map's allocator before the
//! tree changes (`insert.rs`): an allocator that refuses one leaves the tree
//! holding what it held, and the insert gives the entry back. The split then
//! takes them as it goes, from [`ReservedPages`], which can't run out. How
//! many of each kind it takes is [`SplitPages`], which the insert counts
//! from its node stack. A latched tree's splits reserve theirs too, off its
//! free lists first (`latched/pages.rs`).

use core::alloc::Layout;
use core::cell::Cell;
use core::iter;
use core::ptr::NonNull;

use super::node::internal::Internal;
use super::node::leaf::Leaf;
use super::node::page::{PageAllocator, PageType};
use super::node_stack::MAX_DEPTH;
use crate::allocator::{AllocError, Allocator, PageKind};
use crate::strategy::InlineKeyStrategy;

/// The pages a split that inserts into a full leaf takes: a leaf's, for its
/// right half; an internal node's for each internal node above it that is
/// full too, and splits; and if the root splits, one of the root's kind for
/// the root's node, which moves out of the root page as the tree grows.
#[derive(Clone, Copy)]
pub(in crate::tree) struct SplitPages {
    leaves: usize,
    internals: usize,
}

impl SplitPages {
    /// No split: the leaf has room.
    pub(in crate::tree) const NONE: Self = Self {
        leaves: 0,
        internals: 0,
    };

    /// The pages for a full leaf `depth` levels below the root, whose lowest
    /// ancestor with room, if any, is at level `room`, the root's 0.
    pub(in crate::tree) fn of_full_leaf(depth: usize, room: Option<usize>) -> Self {
        let splits = depth - room.map_or(0, |level| level + 1);
        let (leaves, internals) = match (room, depth) {
            (None, 0) => (2, 0),
            (None, _) => (1, splits + 1),
            (Some(_), _) => (1, splits),
        };
        Self { leaves, internals }
    }

    /// Each page's kind, the leaves' first.
    pub(in crate::tree) fn kinds(self) -> impl Iterator<Item = PageKind> {
        iter::repeat_n(PageKind::Leaf, self.leaves)
            .chain(iter::repeat_n(PageKind::Internal, self.internals))
    }
}

/// Pages taken from a map's allocator for one split, which it hands out as
/// the split asks, by kind. Those left go back to the allocator as it drops.
pub(in crate::tree) struct ReservedPages<'a, A: Allocator> {
    alloc: &'a A,
    /// Room for every page a split takes: a leaf's, and an internal node's
    /// for each level of a node stack and the root's node.
    pages: [Cell<Option<ReservedPage>>; MAX_DEPTH + 2],
}

/// A page taken from the map's allocator, for `kind`'s `layout`.
#[derive(Clone, Copy)]
struct ReservedPage {
    page: NonNull<u8>,
    kind: PageKind,
    layout: Layout,
}

impl<'a, A: Allocator> ReservedPages<'a, A> {
    /// The pages `split` takes, of a tree with keys `K`, values `V` and
    /// strategy `S`, from `alloc`; or, if it refuses one, the layout it was
    /// asked for, every page taken given back.
    pub(in crate::tree) fn reserve<K, V, S: InlineKeyStrategy<K>>(
        alloc: &'a A,
        split: SplitPages,
    ) -> Result<Self, Layout> {
        let reserved = Self {
            alloc,
            pages: [const { Cell::new(None) }; MAX_DEPTH + 2],
        };
        let mut slots = reserved.pages.iter();
        for kind in split.kinds() {
            let layout = match kind {
                PageKind::Leaf => Leaf::<K, V, S>::page_layout(),
                PageKind::Internal => Internal::<K, V, S>::page_layout(),
            };
            // Returning drops `reserved`, which gives back those taken.
            let page = kind.allocate(alloc, layout).map_err(|AllocError| layout)?;
            let slot = slots.next().expect("a slot for every page a split takes");
            slot.set(Some(ReservedPage { page, kind, layout }));
        }
        Ok(reserved)
    }
}

// SAFETY: each page handed out is one taken from the map's allocator for its
// kind's layout, zeroed, and handed out once; a page given back goes to the
// map's allocator, which it came from. Neither unwinds but on a broken
// invariant: a split asking for a page it didn't reserve.
unsafe impl<A: Allocator> PageAllocator for ReservedPages<'_, A> {
    fn allocate_page(&self, kind: PageKind, layout: Layout) -> NonNull<u8> {
        let reserved = (self.pages.iter())
            .find(|slot| slot.get().is_some_and(|reserved| reserved.kind == kind))
            .and_then(Cell::take)
            .expect("a page reserved for every page a split takes");
        debug_assert_eq!(reserved.layout, layout, "a kind's pages have one layout");
        reserved.page
    }

    unsafe fn deallocate_page(&self, kind: PageKind, page: NonNull<u8>, layout: Layout) {
        // SAFETY: the page came from the map's allocator, for `kind` and
        // `layout` (caller).
        unsafe { kind.deallocate(self.alloc, page, layout) }
    }
}

impl<A: Allocator> Drop for ReservedPages<'_, A> {
    fn drop(&mut self) {
        for reserved in self.pages.iter().filter_map(Cell::take) {
            let ReservedPage { page, kind, layout } = reserved;
            // SAFETY: the page came from the map's allocator, for `kind` and
            // `layout`, and was handed out to no one.
            unsafe { kind.deallocate(self.alloc, page, layout) };
        }
    }
}
