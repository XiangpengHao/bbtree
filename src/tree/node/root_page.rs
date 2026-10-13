//! The root's page: the root node, of either kind, in room for the larger,
//! and after it the root area, what the tree keeps in place of a parent: its
//! height, and what every key shares (D50, `tree/shared_prefix.rs`).
//!
//! A tree keeps its root page for life, whatever its height: the root
//! node's kind changes in place, as InnoDB's root page does. Raising the
//! tree moves the root node to a fresh page of its kind and makes the root
//! an internal node above it; lowering it moves the root's only child up
//! into the root page and frees the child's page. Every other page holds
//! the kind it was allocated for, at that kind's size (D21), and an edge's
//! tag tells its kind (`page.rs`); the root node's header tells the root's.

use core::alloc::Layout;
use core::mem::size_of;
#[cfg(any(test, feature = "inspect"))]
use core::ops::Range;

use super::Node;
use super::internal::Internal;
use super::leaf::Leaf;
use super::page::{FreshPage, NodePtr, PageByKind, PageKind, PageType};
use crate::inline_key::InlineKeyStrategy;
use crate::tree::shared_prefix::SharedPrefix;

/// A tree's root page, by its address: the root node at its start, and the
/// root area after room for a node of either kind.
///
/// A handle like the node handles, with their contracts (`tree/node/mod.rs`):
/// the tree's owner gives it its access (`tree/root.rs`).
pub(crate) struct RootPage<K, V, S> {
    node: NodePtr<K, V, S>,
}

impl<K, V, S> Clone for RootPage<K, V, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K, V, S> Copy for RootPage<K, V, S> {}

/// What a tree keeps beside its root node, in the root page.
#[repr(C)]
struct RootArea {
    /// The root node's height: 0 for a leaf.
    height: u8,
    shared_prefix: SharedPrefix,
}

// SAFETY: a root page is used with this layout only: the root node at its
// start, with its kind's layout, which fits in the page and whose alignment
// divides the page's, and the root area after the larger kind's page.
unsafe impl<K, V, S: InlineKeyStrategy<K>> PageType for RootPage<K, V, S> {
    fn page_layout() -> Layout {
        let (leaf, internal) = (
            Leaf::<K, V, S>::page_layout(),
            Internal::<K, V, S>::page_layout(),
        );
        let size = Self::area_offset() + size_of::<RootArea>();
        Layout::from_size_align(size, leaf.align().max(internal.align()))
            .expect("a root page's layout is valid")
    }
}

impl<K, V, S: InlineKeyStrategy<K>> RootPage<K, V, S> {
    /// A new tree's root page, in `page`: an empty leaf at height 0, whose
    /// keys share nothing yet.
    pub(crate) fn new(page: FreshPage<Self>) -> Self {
        let root = Self {
            node: NodePtr::from_raw(page.into_raw()),
        };
        // SAFETY: the page is a fresh root page, aligned to and larger than
        // a leaf's, which nothing else uses; its root area lies past the
        // node, in the page, and needs no alignment.
        unsafe {
            root.node.initialize::<Leaf<K, V, S>>();
            root.area().write(RootArea {
                height: 0,
                shared_prefix: SharedPrefix::none(),
            });
        }
        root
    }

    /// The root page for `top`, the root of a tree `height` levels above
    /// the leaves whose keys share `shared_prefix`, in `page`: moves `top`
    /// there, and returns `top`'s emptied page. For a tree made on plain
    /// pages: a copy (`tree/clone.rs`), or the part a cut makes
    /// (`tree/cut.rs`).
    pub(crate) fn from_node(
        page: FreshPage<Self>,
        top: Node<K, V, S>,
        height: usize,
        shared_prefix: SharedPrefix,
    ) -> (Self, PageByKind<K, V, S>) {
        let root = Self {
            node: NodePtr::from_raw(page.into_raw()),
        };
        // SAFETY: the page is a fresh root page, aligned to and as large as
        // a node of either kind, which nothing else uses. `top`'s contents
        // move there whole, and its page holds nothing anymore: the root
        // takes its place, as nothing points to a tree's top node but its
        // owner. The root area lies past the node, in the page.
        let emptied = unsafe {
            root.area().write(RootArea {
                height: height as u8,
                shared_prefix,
            });
            match top {
                Node::Leaf(leaf) => {
                    leaf.node_ptr().move_node_to::<Leaf<K, V, S>>(root.node);
                    leaf.free().into()
                }
                Node::Internal(internal) => {
                    internal
                        .node_ptr()
                        .move_node_to::<Internal<K, V, S>>(root.node);
                    internal.free().into()
                }
            }
        };
        (root, emptied)
    }

    /// The root page `top`, the root node of a tree, is in: for a walk that
    /// kept the node alone (`tree/walk.rs`).
    pub(crate) fn of_top(top: Node<K, V, S>) -> Self {
        Self {
            node: top.node_ptr(),
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
    pub(crate) fn area_bytes() -> Range<usize> {
        let start = Self::area_offset();
        start..start + size_of::<RootArea>()
    }

    /// The root area.
    #[inline(always)]
    fn area(self) -> *mut RootArea {
        // SAFETY: the root area lies within the page (`page_layout`).
        unsafe {
            self.node
                .page()
                .byte_add(Self::area_offset())
                .cast::<RootArea>()
                .as_ptr()
        }
    }

    /// The root node, of the kind its header tells.
    #[inline(always)]
    pub(crate) fn top(self) -> Node<K, V, S> {
        Node::from_header(self.node)
    }

    /// The root node's height: how many levels of internal nodes the tree
    /// has.
    #[inline(always)]
    pub(crate) fn height(self) -> usize {
        // SAFETY: the root area was written with the page (`new`,
        // `from_node`); nothing writes it meanwhile (the handles' contract).
        usize::from(unsafe { (*self.area()).height })
    }

    fn set_height(self, height: usize) {
        // SAFETY: the root area was written with the page; the access is
        // exclusive (the handles' contract).
        unsafe { (*self.area()).height = height as u8 }
    }

    /// What every key of the tree shares (D50).
    #[inline(always)]
    pub(crate) fn shared_prefix<'a>(self) -> &'a SharedPrefix {
        // SAFETY: the root area was written with the page; nothing writes it
        // while the caller borrows it (the handles' contract).
        unsafe { &(*self.area()).shared_prefix }
    }

    /// What every key of the tree shares, to change: keeping every node
    /// along the tree's borders at its length is the caller's business
    /// (`tree/prefix.rs`).
    pub(crate) fn shared_prefix_mut<'a>(self) -> &'a mut SharedPrefix {
        // SAFETY: the root area was written with the page; the access is
        // exclusive while the caller borrows it (the handles' contract).
        unsafe { &mut (*self.area()).shared_prefix }
    }

    /// Moves the root node to `page`, a fresh page of its kind, `Kind`, and
    /// returns it there, as a tree's top node on a plain page. The root page
    /// holds nothing afterwards: the caller makes it a node again, or frees
    /// it.
    ///
    /// # Panics
    ///
    /// If the root node is not of kind `Kind`; nothing changes then.
    fn move_top_to<Kind: PageKind>(self, page: FreshPage<Kind>) -> Node<K, V, S> {
        assert_eq!(
            matches!(self.top(), Node::Leaf(_)),
            Kind::IS_LEAF,
            "moving the root node to a page of the other kind"
        );
        let moved = NodePtr::from_raw(page.into_raw());
        // SAFETY: the root node is of kind `Kind` (checked), and `page` a
        // fresh page of that kind, which nothing else uses: the node's
        // contents move there whole.
        unsafe { self.node.move_node_to::<Kind>(moved) };
        Node::from_header(moved)
    }

    /// Makes the tree a level taller: moves the root node to `page`, a fresh
    /// page of its kind, `Kind`, and makes the root an internal node with no
    /// separators above it, at the tree's shared prefix length (D50), as the
    /// old one had, on both borders. Returns the moved node, the root's only
    /// child now.
    ///
    /// # Panics
    ///
    /// If the root node is not of kind `Kind`; nothing changes then.
    pub(crate) fn raise<Kind: PageKind>(self, page: FreshPage<Kind>) -> Node<K, V, S> {
        let child = self.move_top_to(page);
        // SAFETY: the root page is aligned to and as large as an internal
        // node's, the access is exclusive, and the root node's contents
        // moved to `child`: nothing in it needs dropping.
        unsafe { self.node.initialize::<Internal<K, V, S>>() };
        let root = Internal::at(self.node);
        root.put_only_child(child);
        root.set_empty_prefix_len(self.shared_prefix().len());
        self.set_height(self.height() + 1);
        child
    }

    /// Makes the tree a level shorter: moves the root's only child up into
    /// the root page, and returns the child's emptied page. The inverse of
    /// [`Self::raise`]: the child, on both borders, has the tree's shared
    /// prefix length already.
    ///
    /// # Panics
    ///
    /// If the root is a leaf; and in debug builds, if it holds separators,
    /// which would be leaked with the other children. Nothing changes then.
    pub(crate) fn lower(self) -> PageByKind<K, V, S> {
        let Node::Internal(root) = self.top() else {
            panic!("lowering a leaf root");
        };
        debug_assert_eq!(root.len(), 0, "lowering a root with separators");
        let child = root.child(0);
        self.set_height(self.height() - 1);
        // SAFETY: the root holds nothing but its edge to `child`, which it
        // owned (invariant 2): overwriting it leaks nothing. `child`'s
        // contents move up whole, of its kind, which fits in the root page,
        // and its page holds nothing afterwards: nothing pointed to it but
        // the root's edge.
        unsafe {
            match child {
                Node::Leaf(leaf) => {
                    leaf.node_ptr().move_node_to::<Leaf<K, V, S>>(self.node);
                    leaf.free().into()
                }
                Node::Internal(internal) => {
                    internal
                        .node_ptr()
                        .move_node_to::<Internal<K, V, S>>(self.node);
                    internal.free().into()
                }
            }
        }
    }

    /// Moves the root node to `page`, a fresh page of its kind, `Kind`, for
    /// another tree to hang under an edge (`tree/join.rs`), and returns it
    /// there, with this page, which holds nothing anymore.
    ///
    /// # Panics
    ///
    /// If the root node is not of kind `Kind`; nothing changes then.
    pub(crate) fn into_node<Kind: PageKind>(
        self,
        page: FreshPage<Kind>,
    ) -> (Node<K, V, S>, FreshPage<Self>) {
        let top = self.move_top_to(page);
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
    pub(crate) unsafe fn free(self) -> FreshPage<Self> {
        // SAFETY: the page was allocated as a root page (`new`, `from_node`),
        // and the caller hands over everything in it and every use of it.
        unsafe { self.node.into_fresh_page() }
    }
}
