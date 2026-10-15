//! The tree's nodes: leaves ([`leaf::Leaf`]) and internal nodes
//! ([`internal::Internal`]), what their pages hold, and how one node is read
//! and changed. Nothing here follows a node stack through the tree, which is
//! the tree's (`tree/mod.rs`), but an internal node's operations reach the
//! children right below it, and the leaf that holds a separator's key (D49).
//!
//! The two kinds are different things, and each is its own type with its own
//! operations. A leaf holds the entries, each a key and its value, and a
//! head: the value of the separator just left of the leaf, and with inline
//! keys, its key (D17, D49). An internal node routes: it holds its
//! separators' inline keys and its children, and keeps the separators' keys
//! only for keys without inline keys. A leaf is a page of a
//! `NodeLayout::<K, V>` leaf layout, sized by its keys and values, and an
//! internal node a page of an internal layout, sized by its inline keys and
//! edges, and without inline keys by its keys (D21, D49). Every pointer to a
//! node tells its kind, in a tag bit, and nothing else does, so that
//! following an edge decides the child's kind before reading it ([`Node`],
//! D52); nothing tells a node's parent: navigation uses node stacks instead of
//! parent pointers. The root node is in the root page, which holds either
//! kind, and the tree's shared prefix after it; the tree's pointer to it
//! tells the root's kind (`root_page.rs`).
//!
//! Every node type also carries the map's inline-key strategy `S` (D18). If
//! it gives the keys inline keys, every node stores them in a dense array
//! before its keys, if it keeps any, and searches run over that array first
//! (`inline_key_search.rs`); in a leaf, each key then shares an entry with
//! its value, so that the key a search ends at brings it along (D29), and
//! the head shares one with its separator's key.
//!
//! What both kinds share they share by composition: the page and its
//! header (`page.rs`), laid out as the node's plan says (`layout.rs`,
//! `node_plan.rs`), the slot arrays that keys, values and children move
//! through (`slots.rs`), and the inline-key column and its searches
//! (`inline_keys.rs`, `inline_key_search.rs`, `vector_count.rs`;
//! `length_tags.rs` filters byte-string keys by their lengths, D36). What
//! the code working at any level needs of either kind, to balance a node
//! with its siblings (`siblings.rs`), join it with another tree's root, or
//! fit its prefix, is [`Child`].
//!
//! # Handles and their contracts
//!
//! A node handle is a page pointer, and carries no access of its own: code
//! holding one has the access of the operation it is part of, which the
//! tree's borrow gives (`tree/root.rs`, `tree/node_stack.rs`). The handles
//! leave to that code what the type system could check, so that a node's
//! layout, search and changes stay plain code to change and measure; the
//! contracts below are what it upholds by hand:
//!
//! - **Access.** A reference a method returns lives for as long as its
//!   caller picks, which is no longer than the operation's borrow of the
//!   tree. A method that changes a node runs only with exclusive access to
//!   it, and while no reference into what it changes is alive.
//! - **Positions.** An index a method takes is a key (`index < len`) or an
//!   edge (`index <= len`), as it says; a method that inserts needs room
//!   (`len < capacity`). Methods on the hot path check these in debug builds
//!   only; the others check them always.
//!
//! # Safety argument
//!
//! This module, `page.rs`, `slots.rs`, `leaf.rs`, `internal.rs`,
//! `root_page.rs`, `siblings.rs`, `inline_key_search.rs`, `vector_count.rs`
//! and `type_identity.rs` hold all of `tree/node/`'s `unsafe` code; the
//! other node modules are `#![forbid(unsafe_code)]`.
//! Given the handles' contracts, each `// SAFETY:` comment relies on these
//! invariants:
//!
//! 1. **Nodes.** A node holds a well-formed node of its kind: `len` is at
//!    most its capacity, and a leaf's keys and values `0..len` are
//!    initialized, and an internal node's separators `0..len` (keys, or
//!    windows where separator keys are heads), with their inline keys if the
//!    node has them. An internal node holds no values (D17); its edges
//!    `0..=len` are initialized.
//! 2. **Edges own nodes.** Every initialized edge of a node is the only
//!    pointer to a distinct node one level below it, and every leaf lies at
//!    the same depth below the root. The tag of every pointer to a node,
//!    the tree's to its root too, tells whether it is a leaf (`page.rs`).
//!    Edges are written only from a node taken off another edge, a split,
//!    or a new page, or moved between siblings. Only the root page
//!    (`root_page.rs`) holds a node of either kind over its life; every
//!    other page, the kind it was allocated for.
//! 3. **Heads are flagged.** A leaf's head slot is initialized exactly when
//!    its header says the leaf has a head, and so is its head key slot in a
//!    map whose separator keys are heads; every operation that fills or
//!    empties them updates the header with them (D17, D49).
//! 4. **Inline keys follow keys.** In a node with inline keys (D18), inline
//!    key `i` is key `i`'s window at the node's prefix length for every
//!    `i < len` (05-fence-key.md); an internal node's key `i` is its
//!    separator `i`'s, the head key of the leftmost leaf right of it (D49),
//!    which every operation that moves a head key keeps. Every operation
//!    moves keys through `KeySlots`, and separators through
//!    `SeparatorSlots`, which move their inline keys with them; a key that
//!    moves up into a node, or a node whose prefix length
//!    shrinks, gets its window moved back with `Window::moved_back_to`, and a
//!    node whose prefix length grows reads its keys (`tree/prefix.rs`). All
//!    `capacity` inline-key slots are initialized, from the node's creation
//!    on (`NodePtr::from_fresh_page`), so a search may read past `len`; slots
//!    `len..` hold the greatest inline key, all ones, so that a search may
//!    count every slot without masking those out: a fresh page starts so,
//!    and setting a shorter length refills the slots it leaves.
//! 5. **Length tags follow keys.** Leaves whose plan keeps length tags
//!    (`node_plan.rs`) keep each live key's byte length modulo 65536 in the
//!    corresponding tag slot. `KeySlots` maintains these through every
//!    mutation. All capacity slots start initialized; a search reads only
//!    the tags of its candidates, and always confirms each one it selects
//!    against the full key.
//!
//! Inline keys and prefix lengths steer searches only: whatever they hold,
//! every position a search returns is in bounds, so a wrong `InlineKey`, or
//! a wrong prefix length, gives wrong answers, never undefined behavior.

pub(in crate::tree) mod inline_key_search;
pub(crate) mod inline_keys;
pub(in crate::tree) mod internal;
pub(super) mod layout;
pub(crate) mod leaf;
mod length_tags;
pub(super) mod node_plan;
pub(crate) mod page;
pub(in crate::tree) mod root_page;
pub(in crate::tree) mod siblings;
mod slots;
#[cfg(test)]
mod tests;
mod type_identity;
mod vector_count;

use core::ptr::NonNull;

use self::inline_key_search::NodeInlineKeys;
use self::inline_keys::{Separator, Window};
use self::internal::Internal;
use self::leaf::Leaf;
use self::node_plan::NodePlan;
use self::page::{NodeKind, NodePtr, PageByType};
use self::siblings::Siblings;
use crate::inline_key::{InlineKeyStrategy, InlineWord};

/// A node, of the kind its pointer's tag tells: what an edge or the root
/// leads to.
pub(crate) enum Node<K, V, S> {
    Leaf(Leaf<K, V, S>),
    Internal(Internal<K, V, S>),
}

impl<K, V, S> Clone for Node<K, V, S> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K, V, S> Copy for Node<K, V, S> {}

impl<K, V, S> Node<K, V, S> {
    /// The node `node` leads to, of the kind its tag tells: how an edge,
    /// or a node stack's level, is followed, deciding the kind before the node
    /// is read.
    #[inline(always)]
    pub(in crate::tree) fn at(node: NodePtr<K, V, S>) -> Self {
        if node.is_leaf() {
            Self::Leaf(Leaf::at(node))
        } else {
            Self::Internal(Internal::at(node))
        }
    }

    /// The node's pointer, tagged with its kind: what an edge or a node stack
    /// keeps.
    #[inline(always)]
    pub(in crate::tree) fn node_ptr(self) -> NodePtr<K, V, S> {
        match self {
            Self::Leaf(leaf) => leaf.node_ptr(),
            Self::Internal(internal) => internal.node_ptr(),
        }
    }

    /// The number of keys, in the header both kinds share.
    #[inline(always)]
    pub(in crate::tree) fn len(self) -> usize {
        // SAFETY: the node is allocated and initialized (invariant 1), and
        // nothing writes its header meanwhile (the handles' contract).
        unsafe { self.node_ptr().len() }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Node<K, V, S> {
    /// Gives the node's page back, sorted by its kind.
    ///
    /// # Safety
    ///
    /// As for [`Leaf::free`] or [`Internal::free`].
    pub(in crate::tree) unsafe fn free(self) -> PageByType<K, V, S> {
        // SAFETY: forwarded.
        unsafe {
            match self {
                Self::Leaf(leaf) => leaf.free().into(),
                Self::Internal(internal) => internal.free().into(),
            }
        }
    }

    /// Absorbs `right`, the root of another tree, with leaves as far below
    /// it as this node's are below this node (the caller), around
    /// `separator`, if they fit in one node; see [`Child::absorb`].
    ///
    /// # Panics
    ///
    /// If `right` is not of this node's kind.
    #[expect(
        clippy::type_complexity,
        reason = "a refused absorb hands back the separator and the node it took"
    )]
    pub(in crate::tree) fn absorb(
        self,
        separator: Separator<K, S>,
        right: Self,
    ) -> Result<PageByType<K, V, S>, (Separator<K, S>, Self)> {
        match (self, right) {
            (Self::Leaf(left), Self::Leaf(right)) => left
                .absorb(separator, right)
                .map_err(|(separator, right)| (separator, Self::Leaf(right))),
            (Self::Internal(left), Self::Internal(right)) => left
                .absorb(separator, right)
                .map_err(|(separator, right)| (separator, Self::Internal(right))),
            _ => panic!("a node absorbs a node of its own kind"),
        }
    }

    /// How many leading bytes the node's fences share, which its inline keys
    /// skip (05-fence-key.md); 0 without inline keys.
    pub(in crate::tree) fn prefix_len(self) -> usize {
        match self {
            Self::Leaf(leaf) => leaf.prefix_len(),
            Self::Internal(internal) => internal.prefix_len(),
        }
    }
}

/// A full node split in two: the node keeps its lower entries, `separator`
/// is the separator for the parent, with the inline key it had, and `right`
/// is a new node at the same depth with the upper entries, on a page of
/// its own that the parent's edge is to own. The separator's value is the
/// head of `right`'s leftmost leaf: in a leaf split, of `right` itself
/// (D17), and so is its key where separator keys are heads (D49).
/// `inserted` tells where the new entry went.
pub(in crate::tree) struct Split<K, S, Kind> {
    pub(in crate::tree) separator: Separator<K, S>,
    pub(in crate::tree) right: Kind,
    pub(in crate::tree) inserted: InsertionSide,
}

/// What inserting into one node did.
pub(in crate::tree) enum Insertion<K, S, Kind> {
    /// The node had room; the new entry's index.
    Fit(usize),
    /// The node was full and split; the new entry is in one of the halves,
    /// and the separator and the right half are for the parent.
    Split(Split<K, S, Kind>),
}

/// The half of a split that takes the new entry, and its index there: of the
/// edge where it goes, until it is inserted, and of its key after.
#[derive(Clone, Copy)]
pub(in crate::tree) enum InsertionSide {
    Left(usize),
    Right(usize),
}

impl InsertionSide {
    pub(in crate::tree) fn index(self) -> usize {
        match self {
            Self::Left(index) | Self::Right(index) => index,
        }
    }

    pub(in crate::tree) fn is_right(self) -> bool {
        matches!(self, Self::Right(_))
    }

    /// Where a full node of `capacity` entries splits for an insertion at
    /// edge `insertion_index`, and where the new entry goes then. Leaves
    /// room where entries are arriving: an insertion at either end leaves
    /// that side about one quarter full and the other side denser; an
    /// interior insertion divides the free space evenly. The active side
    /// keeps another minimum's worth of entries above the removal threshold,
    /// so deleting the new entry cannot immediately merge the halves again.
    /// Small nodes fall back to an even split to keep headroom on both
    /// sides.
    ///
    /// Both sides meet `minimum` after the new entry is inserted, and the
    /// separator's index is below `capacity`. Always inlined: its callers
    /// pass their kind's constants, which fold.
    #[inline(always)]
    pub(in crate::tree) fn of_split(
        capacity: usize,
        insertion_index: usize,
        minimum: usize,
    ) -> (usize, Self) {
        let arriving_len = (2 * minimum).min(capacity / 2);
        let separator_index = if insertion_index == 0 {
            arriving_len - 1
        } else if insertion_index == capacity {
            capacity - arriving_len
        } else {
            capacity / 2
        };
        debug_assert!(separator_index < capacity);
        let side = if insertion_index <= separator_index {
            Self::Left(insertion_index)
        } else {
            Self::Right(insertion_index - separator_index - 1)
        };
        (separator_index, side)
    }
}

/// Where an entry's key and its value are, for a walk to hand them out as
/// its borrow allows (`tree/walk.rs`): both initialized when made, in slots
/// apart. A leaf's entry is a key and the value beside it; a separator's,
/// its key, in its internal node or its head (D49), and its head's value.
pub(crate) struct EntrySlots<K, V> {
    pub(in crate::tree) key: NonNull<K>,
    pub(in crate::tree) value: NonNull<V>,
}

/// What the leaf right of a separator being removed gives up and hands up
/// (D17): the separator's value, its head, and where separator keys are
/// heads, its key (D49); and the separator that takes its place, the leaf's
/// first key, now its head's.
pub(in crate::tree) struct HeadSuccession<K, V, S> {
    pub(in crate::tree) removed_key: Option<K>,
    pub(in crate::tree) value: V,
    pub(in crate::tree) successor: Separator<K, S>,
}

/// A node that hangs under an internal node, a leaf or an internal node:
/// what the code that works at either level needs of it, which each kind
/// does its own way. Balancing a node with its siblings ([`Siblings`]),
/// joining it with another tree's top node ([`Child::absorb`]), and fitting its
/// prefix to its fences (`tree/prefix.rs`) run on either kind through this.
pub(in crate::tree) trait Child<K, V, S: InlineKeyStrategy<K>>:
    NodeKind + Copy
{
    /// The child at edge `index` of `parent`, whose children are of this
    /// kind: the caller's decision ([`Internal::has_leaf_children`]), which
    /// debug builds check.
    ///
    /// # Panics
    ///
    /// If `index > len`.
    fn child_of(parent: Internal<K, V, S>, index: usize) -> Self;

    /// The number of keys.
    fn len(self) -> usize;

    /// The most keys a node of this kind holds.
    fn capacity() -> usize;

    /// The fewest keys a node below the root holds between removals (D22).
    fn min_len() -> usize {
        (Self::capacity() / 8).max(4)
    }

    /// See [`Leaf::prefix_len`].
    fn prefix_len(self) -> usize;

    /// Whether the node's trailing slot 0 is initialized: a leaf's head,
    /// which every leaf right of a separator has (D17), or an internal
    /// node's first edge, always.
    fn has_first_trailing_slot(self) -> bool;

    /// See [`Leaf::move_prefix_back_to`].
    fn move_prefix_back_to(self, fill: Window);

    /// The node's page, sorted by its kind, for a node all of whose contents
    /// went elsewhere: see [`Leaf::free`].
    ///
    /// # Safety
    ///
    /// As for [`Leaf::free`].
    unsafe fn free_page(self) -> PageByType<K, V, S>;

    /// The left child's last entry to the right child's front, through the
    /// parent's separator; see [`Siblings::steal_left`]. The right child is
    /// at the parent's prefix length already, `fill`'s, the separator's
    /// window there.
    ///
    /// # Safety
    ///
    /// The left child has an entry, and the right child room for one.
    unsafe fn steal_left_unchecked(siblings: &Siblings<K, V, S, Self>, fill: Window);

    /// The right child's first entry to the left child's end, mirrored; see
    /// [`Siblings::steal_right`]. The left child is at `fill`'s prefix length
    /// already.
    ///
    /// # Safety
    ///
    /// The right child has an entry, and the left child room for one.
    unsafe fn steal_right_unchecked(siblings: &Siblings<K, V, S, Self>, fill: Window);

    /// Puts the separator at position `len` of `left`, and every key and
    /// trailing slot of `right` after it, both nodes at `fill`'s prefix
    /// length already: the core of a merge and of [`Child::absorb`].
    /// Between leaves, `right`'s head, the separator's value, joins it as an
    /// entry, with the separator's key, or where separator keys are heads
    /// (D49), the head's, whose window is `fill`; between internal nodes,
    /// the separator joins as one. `separator`'s window should be at
    /// `fill`'s prefix length, with every byte a leaf keeps of it (D43):
    /// otherwise searches give wrong answers, never undefined behavior.
    ///
    /// # Safety
    ///
    /// `left`, `separator` and `right` fit in one node: `left.len() + 1 +
    /// right.len() <= capacity`. `right` is another node of this kind, its
    /// leaves as deep below it as this node's, in a tree of the same map
    /// type, whose trailing slot 0 is initialized, and nothing reaches its
    /// page afterwards but to free it.
    unsafe fn append(left: Self, fill: Window, separator: Separator<K, S>, right: Self);

    /// Moves `separator`, and every key and trailing slot of `right` after
    /// it, to the end of this node, as a merge moves its parent's separator
    /// and right child into the left one, and returns `right`'s emptied
    /// page: for a node and the top node of another tree, on a plain page of
    /// its kind ([`Tree::into_node`](crate::tree::root::Tree::into_node)),
    /// which share no parent, as when two trees join (`tree/join.rs`).
    /// Nothing but the caller reaches `right`, whose page this frees. For
    /// leaves, `right`'s
    /// head is the separator's value, which rejoins it as an entry (D17),
    /// and where separator keys are heads, its key (D49).
    ///
    /// Both nodes take the separator's prefix length, as merged children
    /// take their parent's. If they don't fit in one node with the
    /// separator, nothing changes, and both come back.
    ///
    /// # Panics
    ///
    /// If `right` is a leaf without a head, or if the separator's prefix
    /// length is longer than either node's; nothing changes then.
    #[expect(
        clippy::type_complexity,
        reason = "a refused absorb hands back the separator and the node it took"
    )]
    fn absorb(
        self,
        separator: Separator<K, S>,
        right: Self,
    ) -> Result<PageByType<K, V, S>, (Separator<K, S>, Self)> {
        if self.len() + 1 + right.len() > Self::capacity() {
            return Err((separator, right));
        }
        assert!(
            right.has_first_trailing_slot(),
            "the leaf right of a separator has no head"
        );
        let fill = separator.window();
        assert!(
            fill.prefix_len() <= self.prefix_len().min(right.prefix_len()),
            "a separator past the prefix of a node it joins"
        );
        self.move_prefix_back_to(fill);
        right.move_prefix_back_to(fill);
        // SAFETY: checked just above: the fit, `right`'s initialized
        // trailing slot 0, and the prefix lengths, both nodes at `fill`'s
        // now. `right` is another tree's top node on a plain page of its
        // kind, which only the caller reaches, with leaves as deep below it
        // (the caller), and once appended, its page is freed with nothing
        // left in it.
        Ok(unsafe {
            Self::append(self, fill, separator, right);
            right.free_page()
        })
    }

    /// Key `index`, the one its inline key is taken from: a leaf's own, or
    /// an internal node's separator key, wherever the map keeps it
    /// ([`Internal::separator_key`]): in a map with inline keys, a walk down
    /// to the separator's head leaf.
    ///
    /// # Panics
    ///
    /// If `index >= len`.
    fn key<'a>(self, index: usize) -> &'a K;

    /// The node's inline keys, if it has them (D18).
    fn inline_keys<'a>(self) -> Option<NodeInlineKeys<'a>>;

    /// The plan of the map's nodes of this kind (`node_plan.rs`).
    fn plan() -> NodePlan<K>;

    /// The node's keys' length tags, if its plan keeps them (D36): only a
    /// leaf's may.
    #[cfg(any(test, fuzzing))]
    fn length_tags<'a>(self) -> Option<&'a [u16]>;

    /// `prefix_len`, or as far as the node's prefix may grow toward it: a
    /// leaf's to what its fences share; an internal node's no further than
    /// its children's too.
    fn allowed_prefix_len(self, prefix_len: usize) -> usize;

    /// See [`Leaf::set_windows`].
    fn set_windows(self, prefix_len: usize, inline_keys: impl ExactSizeIterator<Item = InlineWord>);

    /// Whether the node's first and last keys differ in the first byte past
    /// its prefix, as their inline keys tell: then its fences, which lie
    /// outside its keys, share no more than its prefix, and fitting it can
    /// skip reading them. Fitting only grows a prefix: every change that
    /// shrinks one shrinks it to fit already.
    fn keys_differ_past_prefix(self) -> bool {
        let len = self.len();
        self.inline_keys().is_some_and(|inline_keys| {
            len > 0
                && (inline_keys.inline_key(0) ^ inline_keys.inline_key(len - 1)).leading_zeros()
                    < u8::BITS
        })
    }

    /// Sets the node's prefix length to `prefix_len`, at most what its
    /// fences share, as far as its kind lets it grow
    /// ([`Child::allowed_prefix_len`]), and every inline key to its key's
    /// window there, read from the key. Does nothing if the node has no
    /// inline keys.
    fn refit_prefix(self, prefix_len: usize) {
        let Some(inliner) = Self::plan().inliner() else {
            return;
        };
        let prefix_len = self.allowed_prefix_len(prefix_len);
        if prefix_len == self.prefix_len() {
            return;
        }
        let windows =
            (0..self.len()).map(|index| inliner.inline_key_at(self.key(index), prefix_len));
        self.set_windows(prefix_len, windows);
    }

    /// Shrinks the node's prefix length to `prefix_len` if it is longer.
    fn shrink_prefix_to(self, prefix_len: usize) {
        if self.prefix_len() > prefix_len {
            self.refit_prefix(prefix_len);
        }
    }
}
