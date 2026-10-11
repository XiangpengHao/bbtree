//! Internal byte-string nodes may search beyond their conservative fences.
//! A local prefix belongs only to this node's keys, never to its children.
//! Queries validate it before using local windows. Before a structural
//! mutation, the original fence windows are restored; only mutated nodes
//! are refit, so ordinary leaf insertion leaves ancestors untouched.

#![forbid(unsafe_code)]

use super::handle::{NodeMut, NodeRef};
use super::inline_keys::KeyInliner;
use super::page::{InlineKeyNodeHeader, PageKind};
use crate::inline_key::{InlineKeyStrategy, InlineWidth, InlineWord};

impl<B, K, V, S: InlineKeyStrategy<K>, Kind: PageKind> NodeRef<B, K, V, S, Kind> {
    /// Only built-in heap-byte encodings participate: reading a full
    /// window cannot call user code or panic, whatever this kind stores.
    /// The leaf path remains unchanged.
    pub(super) fn local_prefix_inliner() -> Option<KeyInliner<K>> {
        if Kind::IS_LEAF {
            return None;
        }
        let inliner = KeyInliner::<K>::resolve::<S>()?;
        match inliner {
            KeyInliner::ByteString(_) if inliner.width() == InlineWidth::Eight => Some(inliner),
            _ => None,
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>, Kind: PageKind> NodeMut<'_, K, V, S, Kind> {
    /// Active prefixes skip at least a whole window beyond the fences.
    /// Thus every original fence window is this same constant; restoring
    /// the array takes one string read followed by integer stores.
    pub(super) fn normalize_local_prefix(&mut self) {
        if self.reborrow().local_prefix_len().is_none() {
            return;
        }
        let inliner = Self::local_prefix_inliner().expect("local byte-string prefix");
        let fence = self.prefix_len();
        let window = inliner.inline_key_at(&self.reborrow().keys()[0], fence);
        self.set_local_windows(0, (0..self.len()).map(|_| window));
    }

    /// Called only after the key-changing primitive has finished. A node
    /// with useful fence windows pays only two inline-slot reads. A fully
    /// tied node computes its actual byte LCP, bounded by both key lengths.
    pub(super) fn refresh_local_prefix(&mut self) {
        let Some(inliner @ KeyInliner::ByteString(bytes)) = Self::local_prefix_inliner() else {
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
            windows[index] = inliner.inline_key_at(key, prefix);
        }
        self.set_local_windows(prefix, windows[..len].iter().copied());
    }
}
