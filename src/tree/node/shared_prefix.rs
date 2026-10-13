//! What every key of a tree shares, from its first byte (D50): the prefix of
//! the nodes along the tree's borders, which lack a fence on one side, and
//! of its root, which lacks both (05-fence-key.md). A search checks a query
//! against it once, at the top, so that every node may skip its prefix
//! unchecked.

#![forbid(unsafe_code)]

use alloc::boxed::Box;
use core::borrow::Borrow;
use core::cmp::Ordering;

use super::inline_keys::{KeyInliner, SearchKey, Window};
use crate::inline_key::{InlineKey, InlineKeyStrategy};

/// The leading bytes of the encoding of every key of a tree (D18), as many
/// as they all share, at most [`PrefixBytes::MAX_LEN`]; none for a tree
/// whose keys have no inline keys, share no byte, or that held no key yet.
///
/// Invariant: every key of the tree's encoding starts with these bytes, and
/// a byte string key is at least as long; every node along the tree's
/// borders has this prefix length.
///
/// One pointer, null while nothing is shared: the map grows by a word, not
/// a vector's three, and a lookup in a tree whose keys share nothing reads
/// no bytes for it.
pub(crate) struct SharedPrefix {
    bytes: Option<Box<PrefixBytes>>,
}

/// The bytes of a [`SharedPrefix`] that holds some.
#[derive(Clone)]
struct PrefixBytes {
    /// At least 1, at most [`Self::MAX_LEN`].
    len: u8,
    bytes: [u8; Self::MAX_LEN],
}

impl PrefixBytes {
    /// The most bytes a tree keeps. A search compares a query with all of
    /// them; keys that share more share at least these, which suffice.
    const MAX_LEN: usize = 64;

    fn as_slice(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

impl SharedPrefix {
    /// Nothing shared: what an empty tree, or a part of one, starts with.
    pub(crate) const fn none() -> Self {
        Self { bytes: None }
    }

    /// What a tree of `key` alone shares: its encoding's first bytes.
    pub(crate) fn of_key<K, S: InlineKeyStrategy<K>>(key: &K) -> Self {
        let Some(inliner) = KeyInliner::resolve::<S>() else {
            return Self::none();
        };
        let mut bytes = Box::new(PrefixBytes {
            len: 0,
            bytes: [0; PrefixBytes::MAX_LEN],
        });
        let len = inliner.leading_bytes(key, &mut bytes.bytes);
        bytes.len = len as u8;
        Self {
            bytes: (len > 0).then_some(bytes),
        }
    }

    fn as_bytes_or_empty(&self) -> &[u8] {
        self.bytes.as_deref().map_or(&[], PrefixBytes::as_slice)
    }

    /// How many bytes every key shares: the prefix length of the nodes
    /// along the borders.
    pub(crate) fn len(&self) -> usize {
        self.bytes
            .as_ref()
            .map_or(0, |bytes| usize::from(bytes.len))
    }

    /// How many of these bytes `key`'s encoding starts with: what a tree
    /// that takes it shares. Calls `InlineKey` with
    /// [`WithInlineKey`](crate::WithInlineKey).
    pub(crate) fn len_shared_with<K, S: InlineKeyStrategy<K>>(&self, key: &K) -> usize {
        let Some(bytes) = &self.bytes else {
            return 0;
        };
        KeyInliner::resolve::<S>().map_or(0, |inliner| {
            inliner.common_prefix_len_with_bytes(key, bytes.as_slice())
        })
    }

    /// Shrinks to the first `len` bytes, if there are more.
    pub(crate) fn shrink_to(&mut self, len: usize) {
        if len == 0 {
            self.bytes = None;
        } else if let Some(bytes) = &mut self.bytes {
            bytes.len = bytes.len.min(len as u8);
        }
    }

    /// The window of these bytes at `prefix_len`, at most their length:
    /// what a node whose prefix shrinks from these bytes' length to
    /// `prefix_len` fills its windows from ([`Window::moved_back_to`]), as
    /// every key of the node shares these bytes. Taken before they shrink.
    pub(crate) fn window_at(&self, prefix_len: usize) -> Window {
        let bytes = self.as_bytes_or_empty();
        debug_assert!(prefix_len <= bytes.len());
        Window::new(prefix_len, bytes.inline_key_at(prefix_len))
    }

    /// Where `key` sorts against every key of the tree, if it lacks these
    /// bytes: before all of them, or after. `None` if it has them, or if the
    /// nodes can't compare its encoding, whose searches compare full keys.
    #[inline]
    pub(crate) fn place<K, S, Q>(&self, key: &SearchKey<'_, K, S, Q>) -> Option<Ordering>
    where
        K: Borrow<Q>,
        S: InlineKeyStrategy<K>,
        Q: ?Sized,
    {
        let bytes = self.bytes.as_deref()?;
        key.cmp_shared_prefix(bytes.as_slice())
            .filter(|&order| order != Ordering::Equal)
    }

    /// The bytes themselves, for the visualizer (`vis/`).
    #[cfg(any(test, feature = "inspect"))]
    pub(crate) fn as_bytes(&self) -> &[u8] {
        self.as_bytes_or_empty()
    }

    /// Whether `key`'s encoding starts with these bytes: an invariant of
    /// every key of the tree.
    #[cfg(any(test, fuzzing))]
    pub(crate) fn is_shared_by<K, S: InlineKeyStrategy<K>>(&self, key: &K) -> bool {
        self.len_shared_with::<K, S>(key) == self.len()
    }
}

impl Clone for SharedPrefix {
    fn clone(&self) -> Self {
        Self {
            bytes: self.bytes.clone(),
        }
    }
}
