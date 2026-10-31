//! Appending to a latched tree's last leaf, as the unsync tree appends to
//! its own (D45, `tree/root.rs`). An insert whose key goes after every key,
//! into a last leaf with room, latches that leaf and inserts there without
//! a descent.
//!
//! The tree points at its last leaf, and only a thread holding that leaf
//! exclusively changes the pointer:
//! - a split of it points it at the leaf that is last after the split
//!   (`insert.rs`);
//! - a merge that frees it, or a lowering that moves it into the root page,
//!   points it at the leaf that took its entries (`remove.rs`).
//!
//! An insert that latches the leaf the pointer names, and then finds the
//! pointer still naming it, has the last leaf. The leaf stays last while the
//! insert holds it. As the leaf is on the tree's border, holding it also
//! holds what every key shares still (`shared_prefix.rs`).

use core::cmp::Ordering as KeyOrdering;
use core::ptr::NonNull;
use core::sync::atomic::Ordering;

use super::super::interleaving::Interleaving;
use super::super::node::annexed_leaf::AnnexedLeaf;
use super::super::node::inline_keys::SearchKey;
use super::super::node::latch::Latched;
use super::super::node::leaf::Leaf;
use super::super::node::page::NodePtr;
use super::super::prefix::PrefixedKey;
use super::LatchedTree;
use super::descent::{Exclusive, HeldLeaf};
use super::insert::InsertPolicy;
use crate::allocator::InsertError;
use crate::inline_key::InlineKeyStrategy;

impl<K, V, S: InlineKeyStrategy<K>> LatchedTree<K, V, S> {
    /// Appends `key` and `value` to the last leaf, and returns what `policy`
    /// returns of a new entry, or the entry if it refuses. That takes a leaf
    /// with room, `key` greater than its last key, and `key` sharing what
    /// every key shares. Otherwise gives all back, for the ordinary
    /// insert.
    #[inline(always)]
    #[expect(
        clippy::type_complexity,
        reason = "the insert's outcome, or all back for the ordinary insert"
    )]
    pub(super) fn append<P: InsertPolicy<K, V, S>>(
        &self,
        key: K,
        value: V,
        policy: P,
    ) -> Result<Result<P::Output, InsertError<K, V>>, (K, V, P)>
    where
        K: Ord,
    {
        // A tree another thread is making has no last leaf yet.
        let Some(leaf) = self.rightmost_leaf() else {
            return Err((key, value, policy));
        };
        // Whether the key may go last, read unlatched at the leaf's version,
        // which is checked before the read chooses to latch the leaf
        // (`node/optimistic.rs`).
        let version = leaf.latch().read_version();
        let may_go_last = leaf.may_go_last_racing(&SearchKey::new(&key));
        if !leaf.latch().is_unchanged(version) || !may_go_last {
            return Err((key, value, policy));
        }
        let held = HeldLeaf::<K, V, S, Exclusive>::lock(leaf);
        if !self.is_rightmost_leaf(leaf) {
            return Err((key, value, policy));
        }
        let len = leaf.len();
        if len >= Leaf::<K, V, Latched<S>>::capacity() {
            return Err((key, value, policy));
        }
        let Some(last) = len.checked_sub(1) else {
            return Err((key, value, policy));
        };
        if SearchKey::<K, Latched<S>, K>::new(&key).cmp_full(leaf.key(last)) != KeyOrdering::Greater
        {
            return Err((key, value, policy));
        }
        // SAFETY: the thread holds the last leaf, on the right border.
        let shared_prefix = unsafe { self.held_shared_prefix() };
        let key = match PrefixedKey::checked::<Latched<S>>(shared_prefix, key) {
            Ok(key) => key,
            // A key that shares less shrinks the shared prefix first.
            Err(key) => return Err((key, value, policy)),
        };
        let output = match policy.vacant(AnnexedLeaf::new(leaf), key.key(), &value) {
            Ok(output) => output,
            Err(layout) => return Ok(Err(InsertError::new(key.into_key(), value, layout))),
        };
        leaf.insert_fit(len, key.window_at(leaf.prefix_len()), value);
        drop(held);
        Ok(Ok(output))
    }

    /// The leaf appends go to, the last leaf when no thread holds it; `None`
    /// until the thread that makes the tree points it there.
    #[inline(always)]
    pub(super) fn rightmost_leaf(&self) -> Option<Leaf<K, V, Latched<S>>> {
        Interleaving::switch_point();
        let tagged = NonNull::new(self.rightmost_leaf.load(Ordering::Acquire))?;
        // SAFETY: the pointer is to a leaf page of the tree, tagged, which
        // stays a leaf page while threads share the tree (`pages.rs`).
        Some(Leaf::at(unsafe { NodePtr::from_tagged(tagged) }))
    }

    /// Whether appends go to `leaf`, which the caller holds exclusively:
    /// whether it is the tree's last leaf.
    #[inline(always)]
    pub(super) fn is_rightmost_leaf(&self, leaf: Leaf<K, V, Latched<S>>) -> bool {
        Interleaving::switch_point();
        self.rightmost_leaf.load(Ordering::Relaxed) == leaf.node_ptr().tagged().as_ptr()
    }

    /// Points appends at `leaf`, the tree's last leaf now, which the caller
    /// holds, as it holds the leaf that was last.
    pub(super) fn point_appends_to(&self, leaf: Leaf<K, V, Latched<S>>) {
        Interleaving::switch_point();
        self.rightmost_leaf
            .store(leaf.node_ptr().tagged().as_ptr(), Ordering::Release);
    }
}
