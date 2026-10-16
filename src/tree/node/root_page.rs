//! The root's page: the root node, of either kind, in room for the larger,
//! and after it the root area, what the tree keeps in place of a parent:
//! what every key shares (D50, `tree/shared_prefix.rs`).
//!
//! A tree keeps its root page for life, however tall it grows: the root
//! node's kind changes in place, as InnoDB's root page does. Raising the
//! tree moves the root node to a fresh page of its kind and makes the root
//! an internal node above it; lowering it moves the root's only child up
//! into the root page and frees the child's page. Every other page holds
//! the kind it was allocated for, at that kind's size (D21), and an edge's
//! tag tells its kind (`page.rs`). The tree's pointer to its root page is
//! the root's edge: its tag tells the root's kind, and changes with it.

use core::alloc::Layout;
use core::mem::size_of;
#[cfg(any(test, feature = "inspect"))]
use core::ops::Range;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU8, Ordering};

use super::Node;
use super::internal::Internal;
use super::leaf::Leaf;
use super::page::{FreshPage, NodePtr, PageByType, PageType};
use crate::allocator::Allocator;
use crate::inline_key::InlineKeyStrategy;
use crate::tree::shared_prefix::SharedPrefix;

/// A tree's root page, by a pointer to it tagged with the root node's kind,
/// as an edge to the node would be: the root node at its start, and the
/// root area after room for a node of either kind.
///
/// A handle like the node handles, with their contracts (`tree/node/mod.rs`):
/// the tree's owner gives it its access (`tree/root.rs`). Changing the root's
/// kind ([`Self::raise`], [`Self::lower`]) consumes the handle and returns
/// the page's new one: a copy of the old one tells the old kind, and is not
/// used again.
pub(crate) struct RootPage<K, V, S> {
    node: NodePtr<K, V, S>,
}

impl<K, V, S> Clone for RootPage<K, V, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K, V, S> Copy for RootPage<K, V, S> {}

// SAFETY: a root page is used with this layout only: the root node at its
// start, with its kind's layout, which fits in the page and whose alignment
// divides the page's, and the root area, what every key shares, after the
// larger kind's page.
unsafe impl<K, V, S: InlineKeyStrategy<K>> PageType for RootPage<K, V, S> {
    fn page_layout() -> Layout {
        let (leaf, internal) = (
            Leaf::<K, V, S>::page_layout(),
            Internal::<K, V, S>::page_layout(),
        );
        let size = Self::area_offset() + size_of::<SharedPrefix>();
        Layout::from_size_align(size, leaf.align().max(internal.align()))
            .expect("a root page's layout is valid")
    }
}

impl<K, V, S: InlineKeyStrategy<K>> RootPage<K, V, S> {
    /// A new tree's root page, in `page`: an empty leaf, whose keys share
    /// nothing yet.
    pub(in crate::tree) fn new(page: FreshPage<Self>) -> Self {
        // SAFETY: the page is a fresh root page, aligned to and larger than
        // a leaf's, which nothing else uses.
        let node = unsafe { NodePtr::initialize::<Leaf<K, V, S>>(page.into_raw()) };
        let root_page = Self { node };
        // SAFETY: the root area lies past the node, in the page, and needs no
        // alignment.
        unsafe { root_page.area().write(SharedPrefix::none()) };
        root_page
    }

    /// The root page for `top`, the top node of a tree on plain pages whose
    /// keys share `shared_prefix`, in `page`: moves `top` there, and returns
    /// `top`'s emptied page. For a tree made on plain pages: a copy
    /// (`tree/clone.rs`), or the part a cut makes (`tree/cut.rs`).
    pub(in crate::tree) fn from_node(
        page: FreshPage<Self>,
        top: Node<K, V, S>,
        shared_prefix: SharedPrefix,
    ) -> (Self, PageByType<K, V, S>) {
        // SAFETY: the page is a fresh root page, which nothing else uses, and
        // nothing points to a tree's top node but its owner, who gives it up.
        let (node, emptied) = unsafe { Self::move_in(page.into_raw(), top) };
        let root_page = Self { node };
        // SAFETY: the root area lies past the node, in the page.
        unsafe { root_page.area().write(shared_prefix) };
        (root_page, emptied)
    }

    /// Moves `node`, on a plain page of its kind, whole into the root page at
    /// `page`: returns the node there, and `node`'s emptied page.
    ///
    /// # Safety
    ///
    /// `page` is a root page, aligned to and as large as a node of either
    /// kind, with exclusive access, which holds nothing that needs dropping;
    /// nothing reaches `node`'s page afterwards but to free it.
    unsafe fn move_in(
        page: NonNull<u8>,
        node: Node<K, V, S>,
    ) -> (NodePtr<K, V, S>, PageByType<K, V, S>) {
        // SAFETY: as the caller promises; each arm moves the node as its own
        // kind, and its page then holds nothing.
        unsafe {
            let moved = match node {
                Node::Leaf(leaf) => leaf.node_ptr().move_node_to::<Leaf<K, V, S>>(page),
                Node::Internal(internal) => {
                    internal.node_ptr().move_node_to::<Internal<K, V, S>>(page)
                }
            };
            (moved, node.free())
        }
    }

    /// The root page `node`, a tree's root node, is in: for a walk that kept
    /// the node alone (`tree/walk.rs`).
    pub(in crate::tree) fn of_node(node: Node<K, V, S>) -> Self {
        Self {
            node: node.node_ptr(),
        }
    }

    /// Where the root area starts: past room for a node of either kind.
    #[inline(always)]
    fn area_offset() -> usize {
        let (leaf, internal) = (
            Leaf::<K, V, S>::page_layout(),
            Internal::<K, V, S>::page_layout(),
        );
        leaf.size().max(internal.size())
    }

    /// Where the root area lies in the page (`tree/inspect.rs`).
    #[cfg(any(test, feature = "inspect"))]
    pub(in crate::tree) fn area_bytes() -> Range<usize> {
        let start = Self::area_offset();
        start..start + size_of::<SharedPrefix>()
    }

    /// The root area: what every key of the tree shares.
    #[inline(always)]
    fn area(self) -> *mut SharedPrefix {
        // SAFETY: the root area lies within the page (`page_layout`).
        unsafe {
            self.node
                .page()
                .byte_add(Self::area_offset())
                .cast::<SharedPrefix>()
                .as_ptr()
        }
    }

    /// The root node, of the kind the tag tells.
    #[inline(always)]
    pub(in crate::tree) fn node(self) -> Node<K, V, S> {
        Node::at(self.node)
    }

    /// What every key of the tree shares (D50).
    #[inline(always)]
    pub(in crate::tree) fn shared_prefix<'a>(self) -> &'a SharedPrefix {
        // SAFETY: the root area was written with the page; nothing writes it
        // while the caller borrows it (the handles' contract).
        unsafe { &*self.area() }
    }

    /// [`Self::shared_prefix`] of a latched tree, while a thread holding the
    /// root may be changing it: a copy, read through the raw pointer, which
    /// the root's version vouches for once checked (`tree/latched`); `None`
    /// if, as read first, it has no bytes, which most trees read alone.
    ///
    /// # Safety
    ///
    /// The page is a latched tree's root page, allocated.
    #[inline(always)]
    pub(in crate::tree) unsafe fn load_racing_shared_prefix(self) -> Option<SharedPrefix> {
        // SAFETY: the root area was written with the page, which is
        // allocated (caller); its length is a byte, read as an atomic, and a
        // racing write makes the copy one that the version check throws
        // away.
        unsafe {
            let len = AtomicU8::from_ptr(self.area().byte_add(SharedPrefix::LEN_OFFSET).cast());
            (len.load(Ordering::Relaxed) != 0).then(|| self.area().read())
        }
    }

    /// What every key of the tree shares, to change: keeping every node
    /// along the tree's borders at its length is the caller's business
    /// (`tree/prefix.rs`).
    pub(in crate::tree) fn shared_prefix_mut<'a>(self) -> &'a mut SharedPrefix {
        // SAFETY: the root area was written with the page; the access is
        // exclusive while the caller borrows it (the handles' contract).
        unsafe { &mut *self.area() }
    }

    /// Moves the root node to a fresh page of its kind, from `alloc`, and
    /// returns it there, as a tree's top node on a plain page. The root page
    /// holds nothing afterwards: the caller makes it a node again, or frees
    /// it.
    fn move_node_out(self, alloc: &impl Allocator) -> Node<K, V, S> {
        let moved = match self.node() {
            Node::Leaf(_) => {
                let page = FreshPage::<Leaf<K, V, S>>::allocate(alloc).into_raw();
                // SAFETY: the root node is a leaf, and `page` a fresh leaf
                // page, which nothing else uses: the node moves there whole.
                unsafe { self.node.move_node_to::<Leaf<K, V, S>>(page) }
            }
            Node::Internal(_) => {
                let page = FreshPage::<Internal<K, V, S>>::allocate(alloc).into_raw();
                // SAFETY: as for a leaf, of the internal kind.
                unsafe { self.node.move_node_to::<Internal<K, V, S>>(page) }
            }
        };
        Node::at(moved)
    }

    /// Makes the tree a level taller: moves the root node to a fresh page of
    /// its kind, from `alloc`, and makes the root an internal node with that
    /// node as its only child, at the tree's shared prefix length (D50), as
    /// the old one had, on both borders. Returns the page's new handle.
    pub(in crate::tree) fn raise(self, alloc: &impl Allocator) -> Self {
        let child = self.move_node_out(alloc);
        // SAFETY: the root page is aligned to and as large as an internal
        // node's, the access is exclusive, and the root node's contents
        // moved to `child`: nothing in it needs dropping.
        let node = unsafe { NodePtr::initialize::<Internal<K, V, S>>(self.node.page()) };
        let root = Internal::at(node);
        root.put_only_child(child);
        root.set_empty_prefix_len(self.shared_prefix().len());
        Self { node }
    }

    /// Makes the tree a level shorter: moves the root's only child up into
    /// the root page, and returns the page's new handle and the child's
    /// emptied page. The inverse of [`Self::raise`]: the child, on both
    /// borders, has the tree's shared prefix length already.
    ///
    /// # Panics
    ///
    /// If the root is a leaf; and in debug builds, if it holds separators,
    /// which would be leaked with the other children. Nothing changes then.
    pub(in crate::tree) fn lower(self) -> (Self, PageByType<K, V, S>) {
        let Node::Internal(root) = self.node() else {
            panic!("lowering a leaf root");
        };
        debug_assert_eq!(root.len(), 0, "lowering a root with separators");
        // SAFETY: the root holds nothing but its edge to its only child,
        // which it owned (invariant 2): overwriting it leaks nothing, and
        // nothing pointed to the child's page but that edge.
        let (node, emptied) = unsafe { Self::move_in(self.node.page(), root.child(0)) };
        (Self { node }, emptied)
    }

    /// Moves the root node to a fresh page of its kind, from `alloc`, for
    /// another tree to hang under an edge (`tree/join.rs`), and returns it
    /// there, with this page, which holds nothing anymore.
    pub(in crate::tree) fn into_node(
        self,
        alloc: &impl Allocator,
    ) -> (Node<K, V, S>, FreshPage<Self>) {
        let top = self.move_node_out(alloc);
        // SAFETY: the root node's contents moved to `top`, and the root area
        // holds nothing to drop; the caller gives up the page with `self`.
        (top, unsafe { self.free() })
    }

    /// Gives the page back.
    ///
    /// # Safety
    ///
    /// Every key, value and edge of the root node has been moved out,
    /// dropped or freed, or deliberately leaked, and nothing uses the page,
    /// through this handle or a copy, again.
    pub(in crate::tree) unsafe fn free(self) -> FreshPage<Self> {
        // SAFETY: the page was allocated as a root page (`new`, `from_node`),
        // and the caller hands over everything in it and every use of it.
        unsafe { self.node.into_fresh_page() }
    }
}
