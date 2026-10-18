//! Two adjacent children of an internal node, of either kind ([`Child`]),
//! and the separator between them. A removal that leaves a child underfull
//! fixes it here, as does a cut or a build that leaves a border short. They
//! merge the two, or move entries from one to the other through the
//! separator (D22).

use super::Child;
use super::internal::Internal;
use super::page::PageByType;
use crate::inline_key::InlineKeyStrategy;

/// Separator `index` of `parent` and the children on either side of it, of
/// kind `C`, ready to be merged or to move an entry from one child to the
/// other, as each kind does it ([`Child`]).
///
/// Invariant: `left` and `right` are the children at edges `index` and `index +
/// 1` of `parent`, of kind `C`. The right one's trailing slot 0 is
/// initialized: for leaves, that is its head, the separator's (D17, D49).
pub(in crate::tree) struct Siblings<K, V, S, C> {
    parent: Internal<K, V, S>,
    index: usize,
    left: C,
    right: C,
}

/// Why a steal moved nothing.
#[derive(Debug, PartialEq, Eq)]
pub(in crate::tree) enum StealError {
    /// The child to take an entry from has none.
    DonorEmpty,
    /// The child to put an entry into is full.
    ReceiverFull,
}

impl<K, V, S: InlineKeyStrategy<K>, C: Child<K, V, S>> Siblings<K, V, S, C> {
    /// See [`Internal::siblings`], which checks the type invariant.
    pub(super) fn new(parent: Internal<K, V, S>, index: usize, left: C, right: C) -> Self {
        Self {
            parent,
            index,
            left,
            right,
        }
    }

    pub(in crate::tree) fn parent(&self) -> Internal<K, V, S> {
        self.parent
    }

    /// The separator's index in the parent.
    pub(in crate::tree) fn index(&self) -> usize {
        self.index
    }

    pub(in crate::tree) fn left(&self) -> C {
        self.left
    }

    pub(in crate::tree) fn right(&self) -> C {
        self.right
    }

    pub(in crate::tree) fn left_len(&self) -> usize {
        self.left.len()
    }

    pub(in crate::tree) fn right_len(&self) -> usize {
        self.right.len()
    }

    /// The merge decision: whether both children and the parent's separator
    /// fit in one node.
    pub(in crate::tree) fn can_merge(&self) -> bool {
        C::fit_merged(self.left.len(), self.right.len())
    }

    /// Moves the parent's separator and all of the right child into the left
    /// child. Removes the separator and the right child's edge from the
    /// parent, whose edge `index` then leads to the merged child. Returns the
    /// right child's emptied page.
    ///
    /// # Panics
    ///
    /// If the children don't fit in one node with the separator
    /// ([`Self::can_merge`]); nothing changes then.
    ///
    /// Between leaves, the right child's head rejoins the separator as an
    /// ordinary entry, and the left child keeps its own head, if any (D17,
    /// D49). Keys move with their inline keys.
    ///
    /// The merged child takes the parent's prefix length, that of the
    /// separator's window (05-fence-key.md). Both children's inline keys move
    /// back to it, filled from the separator's window, a fence of both,
    /// without reading a key. The caller fits the merged child's prefix to
    /// its fences, which reads keys, as for [`Self::steal_left`].
    pub(in crate::tree) fn merge(self) -> PageByType<K, V, S> {
        assert!(
            self.can_merge(),
            "the children don't fit in one node with their separator"
        );
        let fill = self.parent.window(self.index);
        let (separator, _) = self.parent.remove_with_right_child(self.index);
        self.left.move_prefix_back_to(fill);
        self.right.move_prefix_back_to(fill);
        // SAFETY: both children and the separator fit in one node (checked
        // just above). The right child is another node of kind `C`, at the left
        // one's depth, in the same map, with its trailing slot 0 initialized
        // (type invariant). Its edge is gone from the parent, so nothing
        // reaches its page but to free it, with nothing left in it. The
        // separator's window is at the parent's prefix length, `fill`'s, which
        // is at most either child's; both children are at it now.
        unsafe {
            C::append(self.left, fill, separator, self.right);
            self.right.free_page()
        }
    }

    /// Rotates one entry rightwards ([`Child::steal_left_unchecked`]). The
    /// left child's last key goes up into the parent, and the parent's
    /// separator comes down to the front of the right child, each with its
    /// inline key. The left child's last trailing slot becomes the right
    /// child's first.
    ///
    /// Between leaves, the value rotates through the right child's head. The
    /// left child's last value becomes the head, and the old head joins the
    /// separator that came down (D17). With inline keys, the keys rotate
    /// through the head with their values, and only windows pass through the
    /// parent (D49). Between internal children, the left child's last edge
    /// becomes the right child's first.
    ///
    /// The right child takes the parent's prefix length, that of the
    /// separator's window (05-fence-key.md). Its inline keys, and the key
    /// going up, move back to it, filled from the separator's window, a fence
    /// of both children, without reading a key. Once its operation is done,
    /// the caller fits both children's prefixes to their new fences, which
    /// reads keys (`tree/prefix.rs`).
    pub(in crate::tree) fn steal_left(&mut self) -> Result<(), StealError> {
        if self.left.len() == 0 {
            return Err(StealError::DonorEmpty);
        }
        if self.right.len() == C::capacity() {
            return Err(StealError::ReceiverFull);
        }
        let fill = self.parent.window(self.index);
        self.right.move_prefix_back_to(fill);
        // SAFETY: checked just above: the left child has an entry to give,
        // and the right child room for one; the right child is at `fill`'s
        // prefix length.
        unsafe { C::steal_left_unchecked(self, fill) };
        Ok(())
    }

    /// Rotates one entry leftwards ([`Child::steal_right_unchecked`]). The
    /// right child's first key goes up into the parent, and the parent's
    /// separator comes down to the end of the left child, each with its
    /// inline key. The right child's first trailing slot becomes the left
    /// child's last. The left child takes the parent's prefix length, as in
    /// [`Self::steal_left`].
    ///
    /// Between leaves, the value rotates through the right child's head. The
    /// old head joins the separator that came down, and the key that went up
    /// becomes the head (D17, D49). Between internal children, the right
    /// child's first edge becomes the left child's last.
    pub(in crate::tree) fn steal_right(&mut self) -> Result<(), StealError> {
        if self.right.len() == 0 {
            return Err(StealError::DonorEmpty);
        }
        if self.left.len() == C::capacity() {
            return Err(StealError::ReceiverFull);
        }
        let fill = self.parent.window(self.index);
        self.left.move_prefix_back_to(fill);
        // SAFETY: checked just above: the right child has an entry to give,
        // and the left child room for one; the left child is at `fill`'s
        // prefix length.
        unsafe { C::steal_right_unchecked(self, fill) };
        Ok(())
    }
}
