//! Per-node prefix stripping (`doc/05-fence-key.md`): a node with inline keys
//! skips the leading bytes that its two fences share, and stores each key's
//! window past them.
//!
//! A node's fences are the separators beside it in its ancestors: the key
//! left of its edge in the nearest ancestor where it is not the first edge,
//! and the key right of it in the nearest where it is not the last. Nothing
//! stores them. A node's prefix length is what they share, 0 if one is
//! missing, along the tree's left and right borders; every key in the node
//! lies between them, so it shares that prefix, whatever is inserted.
//!
//! A child's fences lie within its parent's, so its prefix is never shorter
//! than its parent's: for keys a ≤ b ≤ c, what a and c share is the shorter
//! of what a and b share and what b and c share.
//!
//! A prefix changes only when a fence does, which only a separator change
//! does: a split adds one, a merge takes one out, a move between siblings or
//! a removal replaces one, and a cut leaves nodes without one. The nodes
//! whose fences changed then read their keys for their new windows
//! ([`NodeMut::refit_prefix`]).
//!
//! An operation changes the tree's shape without calling user encodings,
//! so that a panicking `InlineKey` can't stop it halfway. Active local
//! search windows first read one key's window, a std key's, to restore the
//! fence windows (`tree/node/inline_keys.rs`). A prefix may have to shrink
//! during it: it shrinks to the parent's, the shift of a window filled from
//! the parent's separator ([`Window::moved_back_to`]), which bounds the node
//! (`tree/node/handle.rs`). Once it is done, and its entry counted, the nodes
//! whose fences changed are fit to them: the halves of each split
//! ([`PathMut::fit_split_halves`]), the nodes a removal merged or moved entries
//! between (`remove.rs`), the siblings a cut's borders took entries from
//! ([`Subtree::fit_border_siblings`]), and a built tree's nodes as they close
//! (`build.rs`). A removed separator's successor is the one change read first
//! ([`PathMut::fit_left_of_separator`]).
//!
//! [`Window::moved_back_to`]: super::node::inline_keys::Window::moved_back_to

#![forbid(unsafe_code)]

use super::node::handle::{ForceResult, Immut, Internal, Leaf, NodeMut, NodeRef, Subtree};
use super::node::inline_keys::{InlineSlot, KeyInliner};
use super::node::layout::KeyArea;
use super::node::page::PageKind;
use super::path::{MAX_HEIGHT, NodeEnd, PathMut};
use crate::inline_key::{InlineKeyStrategy, InlineWidth, InlineWord};

/// The inline keys of a node's keys at a new prefix length, computed before
/// any is written ([`NodeMut::refit_prefix`]), kept at the node's width: as
/// many as [`KeyArea::MAX_INLINE_KEYS_SIZE`] bytes hold, which every node's
/// inline keys fit in (`tree/node/layout.rs`), on the stack.
enum RefitBuffer {
    Two([u16; KeyArea::MAX_INLINE_KEYS_SIZE / 2]),
    Four([u32; KeyArea::MAX_INLINE_KEYS_SIZE / 4]),
    Eight([u64; KeyArea::MAX_INLINE_KEYS_SIZE / 8]),
}

impl RefitBuffer {
    fn new(width: InlineWidth) -> Self {
        match width {
            InlineWidth::Two => Self::Two([0; KeyArea::MAX_INLINE_KEYS_SIZE / 2]),
            InlineWidth::Four => Self::Four([0; KeyArea::MAX_INLINE_KEYS_SIZE / 4]),
            InlineWidth::Eight => Self::Eight([0; KeyArea::MAX_INLINE_KEYS_SIZE / 8]),
        }
    }

    /// Keeps the first bytes of `window` as inline key `index`.
    ///
    /// # Panics
    ///
    /// If `index` is past the buffer, which no node's key is.
    fn set(&mut self, index: usize, window: InlineWord) {
        match self {
            Self::Two(inline_keys) => inline_keys[index] = InlineSlot::of_window(window),
            Self::Four(inline_keys) => inline_keys[index] = InlineSlot::of_window(window),
            Self::Eight(inline_keys) => inline_keys[index] = InlineSlot::of_window(window),
        }
    }

    /// Inline key `index`, as the window it starts.
    fn inline_key(&self, index: usize) -> InlineWord {
        match self {
            Self::Two(inline_keys) => inline_keys[index].window(),
            Self::Four(inline_keys) => inline_keys[index].window(),
            Self::Eight(inline_keys) => inline_keys[index].window(),
        }
    }

    /// The first `len` inline keys.
    fn inline_keys(&self, len: usize) -> impl ExactSizeIterator<Item = InlineWord> {
        (0..len).map(|index| self.inline_key(index))
    }
}

impl<K, V, S: InlineKeyStrategy<K>, Kind: PageKind> NodeRef<Immut<'_>, K, V, S, Kind> {
    /// Whether the node's first and last keys differ in the first byte past
    /// its prefix, as their inline keys tell: then its fences, which lie
    /// outside its keys, share no more than its prefix, and fitting it can
    /// skip reading them. Fitting only grows a prefix: every change that
    /// shrinks one shrinks it to fit already.
    fn keys_differ_past_prefix(self) -> bool {
        if self.local_prefix_len().is_some() {
            return false;
        }
        let len = self.len();
        self.inline_keys().is_some_and(|inline_keys| {
            len > 0
                && (inline_keys.inline_key(0) ^ inline_keys.inline_key(len - 1)).leading_zeros()
                    < u8::BITS
        })
    }
}

/// How far a node's prefix may grow, by its kind: a leaf's to what its
/// fences share; an internal node's no further than its children's too. A
/// prefix that fits its fences never passes them, but a panic may have left
/// one of the children's short of their fences.
pub(crate) trait PrefixGrowth: PageKind {
    /// `prefix_len`, or as far as `node`'s prefix may grow toward it.
    fn grown_prefix_len<K, V, S: InlineKeyStrategy<K>>(
        node: NodeRef<Immut<'_>, K, V, S, Self>,
        prefix_len: usize,
    ) -> usize;
}

impl PrefixGrowth for Leaf {
    fn grown_prefix_len<K, V, S: InlineKeyStrategy<K>>(
        _node: NodeRef<Immut<'_>, K, V, S, Self>,
        prefix_len: usize,
    ) -> usize {
        prefix_len
    }
}

impl PrefixGrowth for Internal {
    fn grown_prefix_len<K, V, S: InlineKeyStrategy<K>>(
        node: NodeRef<Immut<'_>, K, V, S, Self>,
        prefix_len: usize,
    ) -> usize {
        if prefix_len <= node.prefix_len() {
            return prefix_len;
        }
        (0..=node.len())
            .map(|index| node.edge_at(index).descend().prefix_len())
            .fold(prefix_len, usize::min)
    }
}

impl<K, V, S: InlineKeyStrategy<K>, Kind: PrefixGrowth> NodeMut<'_, K, V, S, Kind> {
    /// Sets the node's prefix length to `prefix_len`, at most what its
    /// fences share, as far as its kind lets it grow ([`PrefixGrowth`]), and
    /// every inline key to its key's window there, read from the key.
    /// Computes them all first, then writes them, so that a panicking
    /// `InlineKey` leaves the node as it was. Does nothing if the node has
    /// no inline keys.
    fn refit_prefix(&mut self, prefix_len: usize) {
        let plan = Self::plan();
        let (Some(inliner), Some(inline_width)) = (plan.inliner(), plan.inline_width()) else {
            return;
        };
        let node = self.reborrow();
        let prefix_len = Kind::grown_prefix_len(node, prefix_len);
        if prefix_len == node.prefix_len() {
            self.refresh_local_prefix();
            return;
        }
        let keys = node.keys();
        let mut inline_keys = RefitBuffer::new(inline_width);
        for (index, key) in keys.iter().enumerate() {
            inline_keys.set(index, inliner.inline_key_at(key, prefix_len));
        }
        self.set_windows(prefix_len, inline_keys.inline_keys(keys.len()));
    }

    /// Shrinks the node's prefix length to `prefix_len` if it is longer.
    fn shrink_prefix_to(&mut self, prefix_len: usize) {
        if self.prefix_len() > prefix_len {
            self.refit_prefix(prefix_len);
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> PathMut<'_, K, V, S> {
    /// Fits the prefixes of the nodes an insert split to their fences, from
    /// the leaves up, so that each node's sees its children's; the path,
    /// which ends at the new entry, ends there again afterwards.
    ///
    /// `separators[height]`, for each height from 1 to `top`, is where the
    /// node the path goes through there holds the separator between the
    /// two halves of the node below it that split.
    ///
    /// Calls `InlineKey`: an insert does this last, once its entry is in and
    /// counted, so that a panic leaves a map that has it.
    pub(crate) fn fit_split_halves(&mut self, separators: &[usize; MAX_HEIGHT + 1], top: usize) {
        if KeyInliner::<K>::resolve::<S>().is_none() {
            return;
        }
        let bottom = self.height();
        let mut indexes = [0; MAX_HEIGHT + 1];
        indexes[bottom] = self.index();
        while self.height() < top {
            self.ascend();
            indexes[self.height()] = self.index();
            let separator = separators[self.height()];
            self.fit_children_to_fences([separator, separator + 1]);
        }
        while self.height() > bottom {
            self.descend();
            self.set_index(indexes[self.height()]);
        }
    }

    /// Fits the prefixes of the bottom node's children at `edges`, an
    /// internal node's, to what their fences share, reading the fences
    /// unless the child's own inline keys tell they share no more
    /// ([`NodeRef::keys_differ_past_prefix`]), and, if a prefix changes, the
    /// child's keys.
    pub(crate) fn fit_children_to_fences<const N: usize>(&mut self, edges: [usize; N]) {
        let Some(inliner) = KeyInliner::<K>::resolve::<S>() else {
            return;
        };
        if self.internal().has_leaf_children() {
            self.fit_children_of_kind::<Leaf, N>(inliner, edges);
        } else {
            self.fit_children_of_kind::<Internal, N>(inliner, edges);
        }
    }

    /// [`Self::fit_children_to_fences`], for children of kind `ChildKind`,
    /// as decided once for the bottom node.
    fn fit_children_of_kind<ChildKind: PrefixGrowth, const N: usize>(
        &mut self,
        inliner: KeyInliner<K>,
        edges: [usize; N],
    ) {
        let prefix_lens = {
            let (low, high) = self.bounds();
            let node = self.internal();
            let keys = node.keys();
            edges.map(|edge| {
                let child = node.edge_at(edge).descend_as::<ChildKind>();
                if child.keys_differ_past_prefix() {
                    return None;
                }
                let child_low = match edge.checked_sub(1) {
                    Some(left) => keys.get(left),
                    None => low,
                };
                // Well-ordered encodings already imply this floor. Keep
                // the numerical parent/child invariant even when a user
                // encoding lies: later structural moves must only move
                // windows backwards, without invoking that encoding.
                Some(
                    inliner
                        .fence_prefix_len(child_low, keys.get(edge).or(high))
                        .max(node.prefix_len()),
                )
            })
        };
        let mut node = self.internal_mut();
        for (edge, prefix_len) in edges.into_iter().zip(prefix_lens) {
            if let Some(prefix_len) = prefix_len {
                node.reborrow_mut()
                    .edge_at(edge)
                    .descend_as::<ChildKind>()
                    .refit_prefix(prefix_len);
            }
        }
    }

    /// Before the separator the path ends at, `s`, in an internal node,
    /// gives way to its successor, `s'`, the first key of the leftmost leaf
    /// right of it (D17): shrinks the prefixes of the nodes along the right
    /// border of the subtree left of `s`, whose upper fence `s'` becomes, to
    /// what they will share with it, from the top down, reading keys. Their
    /// fences are still what they were, which a shorter prefix fits too, so
    /// a removal does this before it changes anything.
    ///
    /// A node there has fences `low` and `s`, so its new prefix is the
    /// shorter of its own and what `s` and `s'` share: `low ≤ s ≤ s'`.
    pub(crate) fn fit_left_of_separator(&mut self) {
        let Some(inliner) = KeyInliner::<K>::resolve::<S>() else {
            return;
        };
        let index = self.index();
        let shared = {
            let node = self.internal();
            let mut right = node.edge_at(index + 1).descend();
            let successor = loop {
                right = match right.force() {
                    ForceResult::Leaf(leaf) => break leaf.keys().first(),
                    ForceResult::Internal(internal) => internal.first_edge().descend(),
                };
            }
            .expect("the leaf right of a separator has keys");
            // Arbitrary user encodings may report fewer shared bytes than
            // the parent's current prefix. Shrinking the border below it
            // would break the numerical invariant used by key movement.
            inliner
                .common_prefix_len(&node.keys()[index], successor)
                .max(node.prefix_len())
        };
        let mut left = self.internal_mut().edge_at(index).descend();
        loop {
            left = match left.force() {
                ForceResult::Leaf(mut leaf) => {
                    leaf.shrink_prefix_to(shared);
                    break;
                }
                ForceResult::Internal(mut internal) => {
                    internal.shrink_prefix_to(shared);
                    internal.last_edge().descend()
                }
            };
        }
    }

    /// Moves the prefix of every node on the path back to 0, from the root
    /// down, each filled from its parent's key beside the path, which is one
    /// of its fences, once the parent's is 0 too. Before a cut along the
    /// path, whose nodes become the borders of two trees, where every
    /// prefix is 0. Local search windows may read one built-in byte string
    /// when normalized; no user encoding runs. The path ends where it did.
    pub(crate) fn clear_prefixes(&mut self) {
        if KeyInliner::<K>::resolve::<S>().is_none() {
            return;
        }
        let bottom = self.height();
        let mut indexes = [0; MAX_HEIGHT + 1];
        indexes[bottom] = self.index();
        while self.height() < self.root_height() {
            self.ascend();
            indexes[self.height()] = self.index();
        }
        let mut node = self.node_mut();
        debug_assert_eq!(node.prefix_len(), 0, "the root's prefix is 0");
        // Each node's kind is decided once: it takes the window its parent
        // fills it from, then, above the bottom, fills its child's.
        let mut fill = None;
        loop {
            node = match node.force() {
                ForceResult::Leaf(mut leaf) => {
                    if let Some(fill) = fill {
                        leaf.move_prefix_back_to(fill);
                    }
                    break;
                }
                ForceResult::Internal(mut internal) => {
                    if let Some(fill) = fill {
                        internal.move_prefix_back_to(fill);
                    }
                    if internal.height() == bottom {
                        break;
                    }
                    let index = indexes[internal.height()];
                    fill = Some(
                        internal
                            .reborrow()
                            .bound_window(index)
                            .expect("a node on a path down holds a key"),
                    );
                    internal.edge_at(index).descend()
                }
            };
        }
        while self.height() > bottom {
            self.descend();
            self.set_index(indexes[self.height()]);
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Subtree<K, V, S> {
    /// Fits the prefix of the node beside each node along the tree's `End`
    /// border to its fences, from the root down: after
    /// [`Subtree::fix_border`], which moves entries over from those nodes,
    /// into the border, without reading a key. The border's own nodes keep
    /// prefix 0.
    pub(crate) fn fit_border_siblings<End: NodeEnd>(&mut self) {
        if KeyInliner::<K>::resolve::<S>().is_none() {
            return;
        }
        let mut path = PathMut::new(self);
        path.start_at_root();
        while path.height() > 0 {
            let len = path.view().len();
            let (sibling, border) = if End::IS_LAST {
                (len.saturating_sub(1), len)
            } else {
                (1, 0)
            };
            if len > 0 {
                path.fit_children_to_fences([sibling]);
            }
            path.set_index(border);
            path.descend();
        }
    }
}
