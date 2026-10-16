//! Appending to a latched tree's last leaf, as bbtree appends to its own
//! (D45, `tree/root.rs`): an insert whose key goes after every key, into a
//! last leaf with room, latches that leaf and inserts there, without a
//! descent.
//!
//! The tree points at its last leaf, and only a thread holding that leaf
//! exclusively changes the pointer: a split of it points it at the leaf
//! last after the split (`insert.rs`), and a merge that frees it, or a
//! lowering that moves it into the root page, at the leaf that took its
//! entries (`remove.rs`). An insert that latches the leaf the pointer names,
//! and finds the pointer naming it still, has the last leaf, which stays
//! last while the insert holds it. Being on the tree's border, it holds what
//! every key shares still too (`shared_prefix.rs`).

use core::cmp::Ordering as KeyOrdering;
use core::ptr::NonNull;
use core::sync::atomic::Ordering;

use super::super::node::inline_keys::SearchKey;
use super::super::node::latch::Latched;
use super::super::node::leaf::Leaf;
use super::super::node::page::NodePtr;
use super::super::prefix::PrefixedKey;
use super::LatchedTree;
use super::descent::{Exclusive, HeldLeaf};
use super::insert::InsertPolicy;
use crate::inline_key::InlineKeyStrategy;

impl<K, V, S: InlineKeyStrategy<K>> LatchedTree<K, V, S> {
    /// Appends `key` and `value` to the last leaf, if it has room, and `key`
    /// is greater than its last key, and shares what every key shares, and
    /// returns what `policy` returns of a new entry; or gives all back, for
    /// the ordinary insert.
    #[inline(always)]
    pub(super) fn append<P: InsertPolicy<K, V>>(
        &self,
        key: K,
        value: V,
        policy: P,
    ) -> Result<P::Output, (K, V, P)>
    where
        K: Ord,
    {
        // A tree another thread is making has no last leaf yet.
        let Some(leaf) = self.rightmost_leaf() else {
            return Err((key, value, policy));
        };
        if !leaf.may_go_last_racing(&SearchKey::new(&key)) {
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
        let root_page = self.root_page().expect("a tree with a last leaf");
        let key = match PrefixedKey::checked::<Latched<S>>(root_page.shared_prefix(), key) {
            Ok(key) => key,
            // A key that shares less shrinks the shared prefix first.
            Err(key) => return Err((key, value, policy)),
        };
        let output = policy.vacant(key.key(), &value);
        leaf.insert_fit(len, key.window_at(leaf.prefix_len()), value);
        drop(held);
        Ok(output)
    }

    /// The leaf appends go to, the last leaf when no thread holds it; `None`
    /// until the thread that makes the tree points it there.
    #[inline(always)]
    pub(super) fn rightmost_leaf(&self) -> Option<Leaf<K, V, Latched<S>>> {
        let tagged = NonNull::new(self.rightmost_leaf.load(Ordering::Acquire))?;
        // SAFETY: the pointer is to a leaf page of the tree, tagged, which
        // stays a leaf page while threads share the tree (`pages.rs`).
        Some(Leaf::at(unsafe { NodePtr::from_tagged(tagged) }))
    }

    /// Whether appends go to `leaf`, which the caller holds exclusively:
    /// whether it is the tree's last leaf.
    #[inline(always)]
    pub(super) fn is_rightmost_leaf(&self, leaf: Leaf<K, V, Latched<S>>) -> bool {
        self.rightmost_leaf.load(Ordering::Relaxed) == leaf.node_ptr().tagged().as_ptr()
    }

    /// Points appends at `leaf`, the tree's last leaf now, which the caller
    /// holds, as it holds the leaf that was last.
    pub(super) fn point_appends_to(&self, leaf: Leaf<K, V, Latched<S>>) {
        self.rightmost_leaf
            .store(leaf.node_ptr().tagged().as_ptr(), Ordering::Release);
    }
}
