//! Internal byte-string nodes may search beyond their conservative fences
//! (D33), where their plan says so (`node_plan.rs`): only std heap byte
//! strings, whose bytes the nodes read without calling user code, so that
//! reading a window can't panic while keys move.
//! A local prefix belongs only to this node's keys, never to its children.
//! Queries validate it before using local windows. Before a structural
//! mutation, the original fence windows are restored; only mutated nodes
//! are refit, so ordinary leaf insertion leaves ancestors untouched.

#![forbid(unsafe_code)]

use super::handle::NodeMut;
use super::page::{InlineKeyNodeHeader, PageKind};
use crate::inline_key::{InlineKey, InlineKeyStrategy, InlineWord};

impl<K, V, S: InlineKeyStrategy<K>, Kind: PageKind> NodeMut<'_, K, V, S, Kind> {
    /// Active prefixes skip at least a whole window beyond the fences.
    /// Thus every original fence window is this same constant; restoring
    /// the array takes one string read followed by integer stores.
    pub(super) fn normalize_local_prefix(&mut self) {
        if self.reborrow().local_prefix_len().is_none() {
            return;
        }
        let bytes = Self::plan()
            .local_prefix_bytes()
            .expect("local byte-string prefix");
        let fence = self.prefix_len();
        let window = bytes.bytes(&self.reborrow().keys()[0]).inline_key_at(fence);
        self.set_local_windows(0, (0..self.len()).map(|_| window));
    }

    /// Called only after the key-changing primitive has finished. A node
    /// with useful fence windows pays only two inline-slot reads. A fully
    /// tied node computes its actual byte LCP, bounded by both key lengths.
    pub(super) fn refresh_local_prefix(&mut self) {
        let Some(bytes) = Self::plan().local_prefix_bytes() else {
            return;
        };
        self.normalize_local_prefix();
        let node = self.reborrow();
        let len = node.len();
        if len < 2 {
            return;
        }
        let slots = node.inline_keys().expect("byte-string inline keys");
        if slots.inline_key(0) != slots.inline_key(len - 1) {
            return;
        }
        let keys = node.keys();
        let prefix = bytes
            .bytes(&keys[0])
            .iter()
            .zip(bytes.bytes(&keys[len - 1]))
            .take(InlineKeyNodeHeader::MAX_PREFIX_LEN)
            .take_while(|(a, b)| a == b)
            .count();
        if prefix < node.prefix_len() + 8 {
            return;
        }
        // Each recognized heap-byte key needs at least two machine words,
        // plus one child pointer and a four-byte summary per internal slot.
        // A 1 KiB page thus stays below 128 full windows on supported pointer
        // widths (at most 63 keys on 32-bit, 36 on 64-bit). Prepare the
        // windows before changing the stored slots.
        let mut windows =
            [0; super::layout::KeyArea::MAX_INLINE_KEYS_SIZE / size_of::<InlineWord>()];
        for (index, key) in keys.iter().enumerate() {
            windows[index] = bytes.bytes(key).inline_key_at(prefix);
        }
        self.set_local_windows(prefix, windows[..len].iter().copied());
    }
}
