//! Checks of what every node of a well-formed tree satisfies: its inline
//! keys, length tags and local prefix match its keys, and its prefix length
//! is what its fences share. The map's whole-tree check
//! (`map/invariants.rs`) runs them on every node, in tests and in fuzzing
//! builds (`--cfg fuzzing`, which `cargo fuzz` sets).

#![forbid(unsafe_code)]

use alloc::vec::Vec;

use super::handle::{Immut, Internal, NodeRef};
use super::inline_keys::KeyInliner;
use super::node_plan::NodePlan;
use super::page::PageKind;
use crate::inline_key::{InlineKeyStrategy, InlineWidth, InlineWord};

impl<K, V, S: InlineKeyStrategy<K>, Kind: PageKind> NodeRef<Immut<'_>, K, V, S, Kind> {
    /// Panics unless the node has inline keys exactly if its map's strategy
    /// gives its keys some, and then has the first bytes of its keys'
    /// windows at its prefix length, as many as this kind keeps, in order
    /// (D18, 05-fence-key.md); a node without has prefix 0.
    pub(crate) fn assert_inline_keys_match(self) {
        let plan = Self::plan();
        if let Some(tags) = self.length_tags() {
            assert!(plan.has_length_tags());
            for (index, key) in self.keys().iter().enumerate() {
                assert_eq!(Some(tags[index]), plan.length_tag(key));
            }
        }
        if let Some(prefix) = self.local_prefix_len() {
            assert!(self.len() >= 2);
            assert!(prefix >= self.prefix_len() + 8);
            let bytes = plan
                .local_prefix_bytes()
                .expect("only nodes whose plan says so have local prefixes");
            let keys = self.keys();
            let prefix_bytes = &bytes.bytes(&keys[0])[..prefix];
            for key in keys.iter() {
                assert!(bytes.bytes(key).starts_with(prefix_bytes));
            }
        }
        let prefix_len = self.local_prefix_len().unwrap_or_else(|| self.prefix_len());
        if let Some(inliner) = plan.inliner()
            && NodePlan::<K>::of::<S, Internal>().rehydrates_windows()
        {
            // A narrow internal window may next fill a leaf's prefix, so
            // every window a node of this map hands out keeps all the bytes
            // a leaf does, also for an inactive local prefix, where the
            // stored slots are narrow.
            for (index, key) in self.keys().iter().enumerate() {
                let window = self.kv_at(index).window();
                assert_eq!(window.prefix_len(), self.prefix_len());
                assert_eq!(
                    window.inline_key(),
                    inliner.inline_key_at(key, self.prefix_len())
                );
            }
        }
        let expected: Option<Vec<InlineWord>> = Option::zip(plan.inliner(), plan.inline_width())
            .map(|(inliner, inline_width)| {
                let dropped_bits = 8 * (InlineWidth::Eight.bytes() - inline_width.bytes()) as u32;
                (self.keys().iter())
                    .map(|key| {
                        inliner.inline_key_at(key, prefix_len) >> dropped_bits << dropped_bits
                    })
                    .collect()
            });
        let actual: Option<Vec<InlineWord>> = self.inline_keys().map(|inline_keys| {
            assert!(
                inline_keys.pads_with_greatest(),
                "an inline-key slot past the length is not the greatest"
            );
            (0..self.len())
                .map(|index| inline_keys.inline_key(index))
                .collect()
        });
        assert_eq!(
            actual, expected,
            "the inline keys are not the keys' windows at prefix length {prefix_len}"
        );
        if expected.is_none() {
            assert_eq!(prefix_len, 0, "a node without inline keys has a prefix");
        }
    }

    /// Panics unless the node's prefix length is what its fences, `lower`
    /// and `upper`, share, 0 if either is missing (05-fence-key.md).
    pub(crate) fn assert_prefix_fits_fences(self, lower: Option<&K>, upper: Option<&K>) {
        let shared = KeyInliner::<K>::resolve::<S>()
            .map_or(0, |inliner| inliner.fence_prefix_len(lower, upper));
        assert_eq!(
            self.prefix_len(),
            shared,
            "a node's prefix length is not what its fences share"
        );
    }
}
