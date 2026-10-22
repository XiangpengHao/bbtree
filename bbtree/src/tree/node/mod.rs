//! The tree's nodes: leaves ([`leaf::Leaf`]) and internal nodes
//! ([`internal::Internal`]), what their pages hold, and how one node is read
//! and changed. Following a node stack through the tree is the tree's job
//! (`tree/mod.rs`). An internal node's operations do reach the children right
//! below it, and the leaf that holds a separator's key (D49).
//!
//! Each kind is its own type with its own operations. A leaf holds the
//! entries, each a key and its value, and a head ([`leaf`]). An internal node
//! routes: it holds its children and its separators' inline keys, and keeps
//! the separators' keys only without inline keys ([`internal`]). A leaf's
//! page is sized by its keys and values. An internal node's is sized by its
//! inline keys and edges, and without inline keys by its keys (D21, D49).
//!
//! Every pointer to a node tells its kind in a tag bit, and nothing else
//! does, so following an edge decides the child's kind before reading it
//! ([`Node`], D52). No node points to its parent: navigation uses node
//! stacks. The root node is in the root page, which holds either kind
//! (`root_page.rs`).
//!
//! Every node type carries the map's inline-key strategy `S` (D18). With
//! inline keys, every node stores them in a dense array, and searches run
//! over that array first. In a leaf, each key then shares an entry with its
//! value, so the key a search ends at brings its value along (D29); the
//! head's value shares one with its key.
//!
//! [`Child`] is what code working at either level needs of a node: to
//! balance it with its siblings ([`Siblings`]), or to fit its prefix.
//!
//! # Handles and their contracts
//!
//! A node handle is a page pointer with no access of its own. Code holding
//! one has the access of the operation it is part of, which the tree's
//! borrow gives (`tree/root.rs`, `tree/node_stack.rs`). The handles leave
//! these contracts to that code rather than the type system, so that a
//! node's layout, search and changes stay plain code to change and measure
//! (D14):
//!
//! - **Access.** A reference a method returns lives as long as its caller
//!   picks, which is no longer than the operation's borrow of the tree. A
//!   method that changes a node runs only with exclusive access to it, and
//!   while no reference into what it changes is alive.
//! - **Positions.** An index a method takes is a key (`index < len`) or an
//!   edge (`index <= len`), as it says. A method that inserts needs room
//!   (`len < capacity`). Methods on the hot path check these in debug builds
//!   only; the others always check them.
//!
//! # Safety argument
//!
//! In a latched tree (`latch.rs`), latches uphold the access contract. A
//! leaf is read under its latch, except by the raw-pointer reads of
//! `optimistic.rs` (D57). An internal node that threads share is changed
//! only under its latch, and read without one only through `optimistic.rs`.
//! The node's version checks those reads (optimistic lock coupling).
//!
//! Given the handles' contracts, each `// SAFETY:` comment relies on these
//! invariants:
//!
//! 1. **Nodes.** A node holds a well-formed node of its kind. `len` is at
//!    most its capacity. A leaf's keys and values `0..len` are initialized;
//!    so are an internal node's separators `0..len` (keys, or windows with
//!    inline keys), with their inline keys if the node has them. An internal
//!    node holds no values (D17); its edges `0..=len` are initialized.
//! 2. **Edges own nodes.** Every initialized edge of a node is the only
//!    pointer to a distinct node one level below it, and every leaf lies at
//!    the same depth below the root. The tag of every pointer to a node,
//!    the tree's to its root included, tells whether it is a leaf
//!    (`page.rs`). Edges are written only from a node taken off another
//!    edge, a split, or a new page, or moved between siblings. Only the root
//!    page (`root_page.rs`) holds nodes of both kinds over its life; every
//!    other page holds the kind it was allocated for.
//! 3. **Heads are flagged.** A leaf's head slot is initialized exactly when
//!    its header says the leaf has a head, and so is its head key slot with
//!    inline keys. Every operation that fills or empties them updates the
//!    header with them (D17, D49).
//! 4. **Inline keys follow keys.** In a node with inline keys (D18), inline
//!    key `i` is key `i`'s window at the node's prefix length for every
//!    `i < len` (05-fence-key.md). An internal node's key `i` is separator
//!    `i`'s: the head key of the leftmost leaf right of it (D49), which every
//!    operation that moves a head key keeps. Every operation moves keys
//!    through `KeySlots`, and separators through `SeparatorSlots`, which move
//!    their inline keys with them. A key that moves up into a node, or a node
//!    whose prefix length shrinks, gets its window moved back with
//!    `Window::moved_back_to`; a node whose prefix length grows reads its
//!    keys (`tree/prefix.rs`). All `capacity` inline-key slots are
//!    initialized from the node's creation on (`NodePtr::from_fresh_page`),
//!    so a search may read past `len`. Slots `len..` hold the greatest
//!    inline key, all ones, so a search may count every slot without masking
//!    them out. A fresh page starts so, and setting a shorter length refills
//!    the slots it leaves.
//! 5. **Hash tags follow keys.** A leaf whose plan keeps hash tags
//!    (`node_plan.rs`) keeps each live key's byte length, saturated at 255,
//!    in the matching tag slot, a byte; `KeySlots` maintains them through
//!    every change. All `capacity` tag slots start initialized. A search
//!    reads only its candidates' tags, and confirms each one it selects
//!    against the full key. The one exception is an unlatched read of a key
//!    that ends within its inline key (`optimistic.rs`): it takes an equal
//!    tag below 255 as an equal length, without reading the key.
//!
//! Inline keys and prefix lengths steer searches only: whatever they hold,
//! every position a search returns is in bounds. A wrong `InlineKey`, or a
//! wrong prefix length, gives wrong answers, never undefined behavior.

pub(crate) mod annexed_leaf;
mod hash_tags;
pub(in crate::tree) mod inline_key_search;
pub(crate) mod inline_keys;
pub(in crate::tree) mod internal;
pub(crate) mod latch;
pub(super) mod layout;
pub(crate) mod leaf;
pub(super) mod node_plan;
pub(in crate::tree) mod optimistic;
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
    /// The node at `node`, of the kind its tag tells. Following an edge or a
    /// node stack's level goes through this, so the kind is decided before
    /// the node is read.
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

    /// How many leading bytes the node's fences share, which its inline keys
    /// skip (05-fence-key.md); 0 without inline keys.
    pub(in crate::tree) fn prefix_len(self) -> usize {
        match self {
            Self::Leaf(leaf) => leaf.prefix_len(),
            Self::Internal(internal) => internal.prefix_len(),
        }
    }
}

/// A full node split in two. The node keeps its lower entries. `separator`,
/// with the inline key it had, goes up to the parent. `right` is a new node at
/// the same depth with the upper entries, on its own page, for the parent's
/// new edge to own. `right`'s leftmost leaf, `right` itself in a leaf split,
/// has the separator as its head (D17, D49). `inserted` tells where the new
/// entry went.
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

/// The half of a split that takes the new entry, and its index there: the
/// edge where it goes until it is inserted, and its key's index after.
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
    /// edge `insertion_index`, and where the new entry goes then.
    ///
    /// The split leaves room where entries are arriving. An insertion at
    /// either end leaves that side about one quarter full and the other side
    /// denser; an interior insertion divides the free space evenly. The
    /// active side keeps another minimum's worth of entries above the removal
    /// threshold, so deleting the new entry cannot merge the halves again at
    /// once. Small nodes fall back to an even split, to keep headroom on both
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

/// Where an entry's key and value are, for a walk to hand out as its borrow
/// allows (`tree/walk.rs`). Both are initialized when made, in separate
/// slots. A leaf's entry is a key and the value beside it. A separator's is
/// its key, in its internal node or with inline keys in its head (D49), and
/// its head's value.
pub(crate) struct EntrySlots<K, V> {
    pub(in crate::tree) key: NonNull<K>,
    pub(in crate::tree) value: NonNull<V>,
}

/// What the leaf right of a separator being removed gives up and hands up
/// (D17). It gives up its head: `value`, and with inline keys `removed_key`
/// (D49). It hands up `successor`, its first key, as the separator that takes
/// the removed one's place; that key's entry is now its head.
pub(in crate::tree) struct HeadSuccession<K, V, S> {
    pub(in crate::tree) removed_key: Option<K>,
    pub(in crate::tree) value: V,
    pub(in crate::tree) successor: Separator<K, S>,
}

/// A child of an internal node, of either kind: what code working at either
/// level needs of it. Each kind implements the required methods its own way;
/// the provided ones work alike on both kinds' pages. Balancing a node with
/// its siblings ([`Siblings`]) and fitting its prefix to its fences
/// (`tree/prefix.rs`) run through this.
pub(in crate::tree) trait Child<K, V, S: InlineKeyStrategy<K>>:
    NodeKind + Copy
{
    /// The child at edge `index` of `parent`. The caller decides that
    /// `parent`'s children are of this kind ([`Internal::has_leaf_children`]);
    /// debug builds check it.
    ///
    /// # Panics
    ///
    /// If `index > len`.
    fn child_of(parent: Internal<K, V, S>, index: usize) -> Self;

    /// The node's page pointer, tagged with its kind.
    fn node_ptr(self) -> NodePtr<K, V, S>;

    fn len(self) -> usize;

    /// The most keys a node of this kind holds.
    fn capacity() -> usize;

    /// The fewest keys a node below the root holds between removals (D22).
    fn min_len() -> usize {
        (Self::capacity() / 8).max(4)
    }

    /// Whether two siblings of `left_len` and `right_len` keys, and the
    /// separator between them, fit in one node. If so, a removal's fixing
    /// merges them; otherwise it moves one entry over (D22).
    fn fit_merged(left_len: usize, right_len: usize) -> bool {
        left_len + 1 + right_len <= Self::capacity()
    }

    /// See [`Leaf::prefix_len`].
    fn prefix_len(self) -> usize;

    /// Whether the node's trailing slot 0 is initialized: a leaf's head,
    /// which every leaf right of a separator has (D17), or an internal
    /// node's first edge, always.
    fn has_first_trailing_slot(self) -> bool;

    /// Moves the node's prefix length back to `fill`'s, and every inline key
    /// with it ([`Window::moved_back_to`]). `fill` is the window, at that
    /// length, of a key whose encoding shares the node's prefix: one of its
    /// fences, one of its keys, or what every key of the tree shares (D50).
    /// Reads no key and runs no user code. Does nothing if the prefix is no
    /// longer than `fill`'s, as in every node without inline keys.
    fn move_prefix_back_to(self, fill: Window) {
        let prefix_len = self.prefix_len();
        if prefix_len <= fill.prefix_len() {
            return;
        }
        let len = self.len();
        let node = self.node_ptr();
        // SAFETY: the node is allocated, of kind `Self` (invariant 1).
        let Some(inline_keys) = (unsafe { node.key_columns::<Self>() }).inline_keys() else {
            return;
        };
        // SAFETY: inline keys `0..len` are initialized (invariants 1 and 4),
        // each is rewritten in place with the window of the same key, and
        // the header with them; the access is exclusive.
        unsafe {
            for index in 0..len {
                let window = Window::new(prefix_len, inline_keys.read(index)).moved_back_to(fill);
                inline_keys.write(index, window.inline_key());
            }
            node.set_prefix_len(fill.prefix_len());
        }
    }

    /// Sets the node's inline keys to `inline_keys`, which must be its keys'
    /// windows at `prefix_len`, in order, and then its prefix length to
    /// `prefix_len`: how a prefix is refit (`tree/prefix.rs`). Does nothing to
    /// a node without inline keys.
    ///
    /// # Panics
    ///
    /// If `inline_keys` does not say it has one inline key per key; the node
    /// is unchanged then.
    fn set_windows(
        self,
        prefix_len: usize,
        inline_keys: impl ExactSizeIterator<Item = InlineWord>,
    ) {
        let len = self.len();
        assert_eq!(inline_keys.len(), len, "one inline key per key");
        let node = self.node_ptr();
        // SAFETY: the node is allocated, of kind `Self` (invariant 1).
        let Some(slots) = (unsafe { node.key_columns::<Self>() }).inline_keys() else {
            return;
        };
        // SAFETY: each slot written has an index below `len <= capacity`,
        // however many inline keys the iterator gives; inline keys are plain
        // data, and the access is exclusive.
        unsafe {
            for (index, inline_key) in (0..len).zip(inline_keys) {
                slots.write(index, inline_key);
            }
            node.set_prefix_len(prefix_len);
        }
    }

    /// Sets the prefix length of a node with no keys, which any prefix
    /// length fits: how a copied node, or a new root, starts.
    ///
    /// # Panics
    ///
    /// If the node has keys.
    fn set_empty_prefix_len(self, prefix_len: usize) {
        assert_eq!(self.len(), 0, "a node with keys keeps their prefix length");
        // SAFETY: the node is allocated and the access exclusive.
        unsafe { self.node_ptr().set_prefix_len(prefix_len) }
    }

    /// Gives the node's page back, sorted by its kind; see [`Leaf::free`].
    ///
    /// # Safety
    ///
    /// As for [`Leaf::free`].
    unsafe fn free_page(self) -> PageByType<K, V, S>;

    /// Moves the left child's last entry to the right child's front, through
    /// the parent's separator; see [`Siblings::steal_left`]. `fill` is the
    /// separator's window, at the parent's prefix length, which the right
    /// child is at already.
    ///
    /// # Safety
    ///
    /// The left child has an entry, and the right child room for one.
    unsafe fn steal_left_unchecked(siblings: &Siblings<K, V, S, Self>, fill: Window);

    /// Moves the right child's first entry to the left child's end, mirrored;
    /// see [`Siblings::steal_right`]. The left child is at `fill`'s prefix
    /// length already.
    ///
    /// # Safety
    ///
    /// The right child has an entry, and the left child room for one.
    unsafe fn steal_right_unchecked(siblings: &Siblings<K, V, S, Self>, fill: Window);

    /// Puts the separator at position `len` of `left`, and every key and
    /// trailing slot of `right` after it; both nodes are at `fill`'s prefix
    /// length already. This is the core of a merge ([`Siblings::merge`]).
    ///
    /// Between leaves, `right`'s head value joins the separator's key as an
    /// entry; with inline keys (D49), that key is the head's, with `fill` as
    /// its window. Between internal nodes, the separator joins as one.
    /// `separator`'s window should be at `fill`'s prefix length, with every
    /// byte a leaf keeps of it (D43); otherwise searches give wrong answers,
    /// never undefined behavior.
    ///
    /// # Safety
    ///
    /// `left`, `separator` and `right` fit in one node: `left.len() + 1 +
    /// right.len() <= capacity`. `right` is another node of this kind, in a
    /// tree of the same map type, with its leaves as deep below it as
    /// `left`'s. Its trailing slot 0 is initialized, and nothing reaches its
    /// page afterwards but to free it. A leaf's annex takes `right`'s
    /// (`annexed_leaf.rs`).
    unsafe fn append(left: Self, fill: Window, separator: Separator<K, S>, right: Self);

    /// Key `index`, the one its inline key is taken from: a leaf's own, or an
    /// internal node's separator key ([`Internal::separator_key`]), which
    /// with inline keys is a walk down to a head.
    ///
    /// # Panics
    ///
    /// If `index >= len`.
    fn key<'a>(self, index: usize) -> &'a K;

    /// The node's inline keys, if it has them (D18).
    fn inline_keys<'a>(self) -> Option<NodeInlineKeys<'a>>;

    /// The plan of the map's nodes of this kind (`node_plan.rs`).
    fn plan() -> NodePlan<K>;

    /// The node's keys' hash tags, if its plan keeps them (D36): only a
    /// leaf's may.
    #[cfg(any(test, fuzzing))]
    fn hash_tags<'a>(self) -> Option<&'a [u8]>;

    /// `prefix_len`, or as far as the node's prefix may grow toward it: a
    /// leaf's to what its fences share; an internal node's no further than
    /// its children's too.
    fn allowed_prefix_len(self, prefix_len: usize) -> usize;

    /// Whether the node's first and last keys differ in the first byte past
    /// its prefix, as their inline keys tell. Then its fences, which lie
    /// outside its keys, share no more than its prefix, so fitting it can
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
    /// ([`Child::allowed_prefix_len`]). Every inline key becomes its key's
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
