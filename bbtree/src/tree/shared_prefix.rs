//! What every key of a tree shares, from its first byte (D50). It is the prefix
//! of the nodes along the tree's borders, which lack a fence on one side, and
//! of its root, which lacks both (`prefix.rs`). A search checks a query against
//! it once, at the top, so that every node may skip its prefix unchecked. The
//! tree keeps it beside its root page pointer, in the map (`root.rs`,
//! `latched/mod.rs`).

#![forbid(unsafe_code)]

use core::borrow::Borrow;
use core::cmp::Ordering;
use core::mem::offset_of;

use super::node::inline_keys::{KeyInliner, SearchKey, Window};
use crate::inline_key::{InlineKey, InlineKeyStrategy};

/// The leading bytes of the encoding of every key of a tree (D18), as many
/// as they all share, at most [`Self::MAX_LEN`]. None for a tree whose keys
/// have no inline keys, share no byte, or that held no key yet.
///
/// Invariant: the encoding of every key of the tree starts with these
/// bytes, and a byte-string key is at least as long. Every node along the
/// tree's borders has this prefix length. `len <= MAX_LEN`.
///
/// A plain value, 40 bytes, which the map holds: with the root page
/// pointer, the length and the remembered leaf, an unsync map is 64 bytes.
/// A lookup in a tree whose keys share nothing reads only its length, which
/// a latched tree's descent reads at [`Self::LEN_OFFSET`] while another
/// thread may write it (`latched/mod.rs`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(C)]
pub(in crate::tree) struct SharedPrefix {
    len: u8,
    bytes: [u8; Self::MAX_LEN],
}

impl SharedPrefix {
    /// Where the length is in the value.
    pub(in crate::tree) const LEN_OFFSET: usize = offset_of!(Self, len);

    /// The most bytes a tree keeps: what fits beside the length in 40
    /// bytes. A search compares a query with all of them; keys that share
    /// more share at least these, which suffice.
    pub(in crate::tree) const MAX_LEN: usize = 39;

    /// The value's size, its bytes in a file map's header (`tree/file.rs`).
    #[cfg(feature = "file")]
    pub(in crate::tree) const SIZE: usize = 1 + Self::MAX_LEN;

    /// Nothing shared: what an empty tree, or a part of one, starts with.
    pub(in crate::tree) const fn none() -> Self {
        Self {
            len: 0,
            bytes: [0; Self::MAX_LEN],
        }
    }

    /// What a tree of `key` alone shares: its encoding's first bytes.
    pub(in crate::tree) fn of_key<K, S: InlineKeyStrategy<K>>(key: &K) -> Self {
        let mut shared = Self::none();
        shared.len = KeyInliner::write_leading_bytes::<S>(key, &mut shared.bytes) as u8;
        shared
    }

    pub(in crate::tree) fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }

    /// How many bytes every key shares: the prefix length of the nodes
    /// along the borders.
    pub(in crate::tree) fn len(&self) -> usize {
        usize::from(self.len)
    }

    /// How many of these bytes `key`'s encoding starts with: what a tree
    /// that takes it shares. Calls `InlineKey` with
    /// [`WithInlineKey`](crate::WithInlineKey).
    pub(in crate::tree) fn len_shared_with<K, S: InlineKeyStrategy<K>>(&self, key: &K) -> usize {
        if self.len == 0 {
            return 0;
        }
        KeyInliner::common_prefix_len_with_bytes::<S>(key, self.as_bytes())
    }

    /// Shrinks to the first `len` bytes, if there are more.
    pub(in crate::tree) fn shrink_to(&mut self, len: usize) {
        self.len = self.len().min(len) as u8;
    }

    /// The window of these bytes at `prefix_len`, at most their length. A
    /// node whose prefix shrinks from these bytes' length to `prefix_len`
    /// fills its windows from it ([`Window::moved_back_to`]), as every key of
    /// the node shares these bytes. Taken before they shrink.
    pub(in crate::tree) fn window_at(&self, prefix_len: usize) -> Window {
        let bytes = self.as_bytes();
        debug_assert!(prefix_len <= bytes.len());
        Window::new(prefix_len, bytes.inline_key_at(prefix_len))
    }

    /// Where `key` sorts against every key of the tree, if it lacks these
    /// bytes: before all of them, or after. `None` if it has them, or if the
    /// nodes can't compare its encoding, whose searches compare full keys.
    #[inline]
    pub(in crate::tree) fn place_outside<K, S, Q>(
        &self,
        key: &SearchKey<'_, K, S, Q>,
    ) -> Option<Ordering>
    where
        K: Borrow<Q>,
        S: InlineKeyStrategy<K>,
        Q: ?Sized,
    {
        if self.len == 0 {
            return None;
        }
        key.cmp_shared_prefix(self.as_bytes())
            .filter(|&order| order != Ordering::Equal)
    }

    /// Whether `key`'s encoding starts with these bytes: an invariant of
    /// every key of the tree.
    pub(in crate::tree) fn is_shared_by<K, S: InlineKeyStrategy<K>>(&self, key: &K) -> bool {
        self.len_shared_with::<K, S>(key) == self.len()
    }

    /// The value as a file map's header keeps it: the length, then the
    /// bytes, the unused ones zero.
    #[cfg(feature = "file")]
    pub(in crate::tree) fn to_file_bytes(self) -> [u8; Self::SIZE] {
        let mut bytes = [0; Self::SIZE];
        bytes[0] = self.len;
        bytes[1..][..self.len()].copy_from_slice(self.as_bytes());
        bytes
    }

    /// The value [`Self::to_file_bytes`] wrote; `None` if the length is
    /// past [`Self::MAX_LEN`]. Any bytes within it are a valid prefix: they
    /// steer searches, never past a node.
    #[cfg(feature = "file")]
    pub(in crate::tree) fn from_file_bytes(bytes: &[u8; Self::SIZE]) -> Option<Self> {
        let (&len, shared) = bytes.split_first().expect("a length byte");
        (usize::from(len) <= Self::MAX_LEN).then(|| Self {
            len,
            bytes: shared.try_into().expect("MAX_LEN bytes"),
        })
    }
}
