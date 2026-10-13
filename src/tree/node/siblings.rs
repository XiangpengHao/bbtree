//! Two children of an internal node side by side, and the separator between
//! them, of either kind ([`Child`]): what a removal that leaves a child
//! underfull, a cut or a build that leaves a border short, and a join that
//! hangs a root beside another node change, by merging the two or moving
//! entries from one to the other through the separator (D22).

use super::Child;
use super::internal::Internal;
use super::page::PageByKind;
use crate::inline_key::InlineKeyStrategy;

/// Separator `index` of `parent` and the children at the edges left and
/// right of it, of kind `C`, ready to be merged or to move an entry from one
/// child to the other, as each kind does it ([`Child`]).
///
/// Invariant: `left` and `right` are the children at edges `index` and
/// `index + 1` of `parent`, of kind `C`, and the right one's trailing slot 0
/// is initialized: for leaves, the head that holds the separator's value
/// (D17), and where separator keys are heads, its key (D49).
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

    pub(crate) fn parent(&self) -> Internal<K, V, S> {
        self.parent
    }

    /// The separator's index in the parent.
    pub(crate) fn index(&self) -> usize {
        self.index
    }

    pub(crate) fn left(&self) -> C {
        self.left
    }

    pub(crate) fn right(&self) -> C {
        self.right
    }

    pub(crate) fn left_len(&self) -> usize {
        self.left.len()
    }

    pub(crate) fn right_len(&self) -> usize {
        self.right.len()
    }

    /// The merge decision: whether both children and the parent's separator
    /// fit in one node.
    pub(crate) fn can_merge(&self) -> bool {
        self.left.len() + 1 + self.right.len() <= C::capacity()
    }

    /// Moves the parent's separator and all of the right child into the left
    /// child, and removes the separator and the edge to the right child from
    /// the parent, whose edge `index` leads to the merged child afterwards.
    /// Returns the right child's emptied page. The children fit in one node
    /// with the separator ([`Self::can_merge`]), which debug builds check.
    ///
    /// Between leaves, the separator rejoins its value, the right child's
    /// head, as an ordinary entry, and the left child keeps its own head, if
    /// any (D17); where separator keys are heads, the head's key is the
    /// separator's (D49). Keys move with their inline keys.
    ///
    /// The merged child takes the parent's prefix length (05-fence-key.md),
    /// at which the separator's inline key is: both children's inline keys
    /// move back to it, filled from the separator's, a fence of both, reading
    /// no key. Fitting the merged child's prefix to its fences, which reads
    /// keys, is the caller's business, as for [`Self::steal_left`].
    pub(crate) fn merge(self) -> PageByKind<K, V, S> {
        debug_assert!(self.can_merge());
        let fill = self.parent.window(self.index);
        let (separator, _) = self.parent.remove_with_right_child(self.index);
        self.left.move_prefix_back_to(fill);
        self.right.move_prefix_back_to(fill);
        // SAFETY: both children and the separator fit in one node (the
        // caller). The right child is another node of kind `C`, at the left
        // one's height, in the same map, with its trailing slot 0 initialized
        // (type invariant); its edge is gone from the parent, so nothing
        // reaches its page but to free it, with nothing left in it. The
        // separator's window is at the parent's prefix length, `fill`'s, at
        // most either child's, which are at it now.
        unsafe {
            C::append(self.left, fill, separator, self.right);
            self.right.free_page()
        }
    }

    /// Rotates one entry rightwards: the left child's last key goes up into
    /// the parent, and the parent's separator comes down to the front of the
    /// right child, each with its inline key; the left child's last trailing
    /// slot becomes the right child's first ([`Child::rotate_right`]).
    ///
    /// Between leaves, the value rotates through the right child's head: the
    /// left child's last value becomes the head, and the old head joins the
    /// separator that came down (D17); where separator keys are heads, the
    /// keys rotate through the head with their values, and only windows
    /// through the parent (D49). Between internal children, the left child's
    /// last edge becomes the right child's first.
    ///
    /// The right child takes the parent's prefix length (05-fence-key.md),
    /// at which the separator's inline key is: its inline keys, and the key
    /// going up, move back to it, filled from the separator's, a fence of
    /// both children, reading no key. Fitting both children's prefixes to
    /// their new fences, which reads keys, is the caller's business, once
    /// its operation is done (`tree/prefix.rs`).
    pub(crate) fn steal_left(&mut self) -> Result<(), StealError> {
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
        unsafe { C::rotate_right(self, fill) };
        Ok(())
    }

    /// Rotates one entry leftwards: the right child's first key goes up into
    /// the parent, and the parent's separator comes down to the end of the
    /// left child, each with its inline key; the right child's first
    /// trailing slot becomes the left child's last ([`Child::rotate_left`]).
    /// The left child takes the parent's prefix length, as in
    /// [`Self::steal_left`].
    ///
    /// Between leaves, the value rotates through the right child's head: the
    /// old head joins the separator that came down, and the value of the key
    /// that went up becomes the head (D17), with that key where separator
    /// keys are heads (D49). Between internal children, the right child's
    /// first edge becomes the left child's last.
    pub(crate) fn steal_right(&mut self) -> Result<(), StealError> {
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
        unsafe { C::rotate_left(self, fill) };
        Ok(())
    }
}
