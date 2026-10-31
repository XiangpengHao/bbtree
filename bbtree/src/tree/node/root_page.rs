//! The root page: the root node, of either kind, in room for the larger: a
//! node's page, the larger kind's. What the tree keeps in place of the
//! root's parent, the bytes every key shares (D50), lies beside its pointer
//! to the page (`tree/root.rs`).
//!
//! A tree keeps its root page for life, however tall it grows; the root
//! node's kind changes in place, as InnoDB's root page does (D52). Raising
//! the tree moves the root node to a fresh page of its kind and makes the
//! root an internal node above it. Lowering it moves the root's only child
//! up into the root page and frees the child's page. Every other page holds
//! the kind it was allocated for, at that kind's size (D21). The tree's
//! pointer to its root page is the root's edge: its tag tells the root's
//! kind, and changes with it.

use core::alloc::Layout;
use core::ptr::NonNull;

use super::internal::Internal;
use super::leaf::Leaf;
use super::page::{FreshPage, NodePtr, PageAllocator, PageByType, PageType};
use super::{Child, Node};
use crate::allocator::PageKind;
use crate::inline_key::InlineKeyStrategy;

/// A tree's root page, by a pointer to it tagged with the root node's kind,
/// as an edge to the node would be.
///
/// A handle like the node handles, with their contracts (`tree/node/mod.rs`);
/// the tree's owner gives it its access (`tree/root.rs`). Changing the root's
/// kind ([`Self::raise`], [`Self::lower`]) consumes the handle and returns
/// the page's new one. A copy of the old handle tells the old kind, and is
/// not used again.
pub(crate) struct RootPage<K, V, S> {
    node: NodePtr<K, V, S>,
}

impl<K, V, S> Clone for RootPage<K, V, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K, V, S> Copy for RootPage<K, V, S> {}

// SAFETY: a root page is used with this layout only: the larger kind's
// page, at least as aligned as the other kind's. The root node is at its
// start, with its kind's layout, which fits in the page and whose alignment
// divides the page's.
unsafe impl<K, V, S: InlineKeyStrategy<K>> PageType for RootPage<K, V, S> {
    #[inline(always)]
    fn kind() -> PageKind {
        Self::larger_kind().0
    }

    #[inline(always)]
    fn page_layout() -> Layout {
        Self::larger_kind().1
    }
}

impl<K, V, S: InlineKeyStrategy<K>> RootPage<K, V, S> {
    /// The larger node kind, a leaf if they are as large, and its page's
    /// layout: what a root page is, room for a node of either kind.
    ///
    /// # Panics
    ///
    /// If the larger kind's pages are less aligned than the other's, which
    /// only a node config of internal pages larger than leaves', whose leaves
    /// are larger still, makes (`node_config.rs`). Folded after
    /// monomorphization, as the layouts are.
    #[inline(always)]
    fn larger_kind() -> (PageKind, Layout) {
        let (leaf, internal) = (
            Leaf::<K, V, S>::page_layout(),
            Internal::<K, V, S>::page_layout(),
        );
        let (larger, smaller) = if leaf.size() >= internal.size() {
            ((PageKind::Leaf, leaf), internal)
        } else {
            ((PageKind::Internal, internal), leaf)
        };
        assert!(
            larger.1.align() >= smaller.align(),
            "the larger node kind's pages are the more aligned"
        );
        larger
    }

    /// A new tree's root page, in `page`: an empty leaf.
    pub(in crate::tree) fn new(page: FreshPage<Self>) -> Self {
        // SAFETY: the page is a fresh root page, aligned to and as large as
        // a leaf's, which nothing else uses.
        let node = unsafe { NodePtr::initialize::<Leaf<K, V, S>>(page.into_raw()) };
        Self { node }
    }

    /// Makes `page` the root page for `top`, moving `top` there, and returns
    /// it with `top`'s emptied page. `top` is the top node of a tree made on
    /// plain pages: a copy (`tree/clone.rs`), or the part a cut makes
    /// (`tree/cut.rs`).
    pub(in crate::tree) fn from_node(
        page: FreshPage<Self>,
        top: Node<K, V, S>,
    ) -> (Self, PageByType<K, V, S>) {
        // SAFETY: the page is a fresh root page, which nothing else uses, and
        // nothing points to a tree's top node but its owner, who gives it up.
        let (node, emptied) = unsafe { Self::move_in(page.into_raw(), top) };
        (Self { node }, emptied)
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

    /// The root node, of the kind the tag tells.
    #[inline(always)]
    pub(in crate::tree) fn node(self) -> Node<K, V, S> {
        Node::at(self.node)
    }

    /// Moves the root node to a fresh page of its kind, from `alloc`, and
    /// returns it there, as a tree's top node on a plain page. The root page
    /// holds nothing afterwards: the caller makes it a node again, or frees
    /// it.
    fn move_node_out(self, alloc: &impl PageAllocator) -> Node<K, V, S> {
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

    /// Makes the tree a level taller. Moves the root node to a fresh page of
    /// its kind, from `alloc`, and makes the root an internal node with that
    /// node as its only child. The new root, on both borders, is at
    /// `shared_len`, the tree's shared prefix length (D50), as the old one
    /// was. Returns the page's new handle.
    pub(in crate::tree) fn raise(self, alloc: &impl PageAllocator, shared_len: usize) -> Self {
        let child = self.move_node_out(alloc);
        // SAFETY: the root page is aligned to and as large as an internal
        // node's, the access is exclusive, and the root node's contents
        // moved to `child`: nothing in it needs dropping.
        let node = unsafe { NodePtr::initialize::<Internal<K, V, S>>(self.node.page()) };
        let root = Internal::at(node);
        root.put_only_child(child);
        root.set_empty_prefix_len(shared_len);
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
