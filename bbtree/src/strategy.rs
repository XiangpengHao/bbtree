//! A map's strategy parameter, [`InlineKeyStrategy`]: one type that bundles
//! how its nodes are built. It says which keys have inline keys and where
//! they come from ([`InlineKey`], `inline_key.rs`), the node config
//! ([`NodeConfig`], `node_config.rs`), and what each leaf keeps besides its
//! entries ([`LeafAnnex`], `leaf_annex.rs`). Those modules each own one of
//! these ideas; this one owns the bundle and the strategies that make one:
//!
//! - [`Auto`], the default, and [`WithInlineKey`] choose the inline keys,
//!   with every other part at its default;
//! - [`Configured`] gives a strategy another node config;
//! - [`WithLeafAnnex`] gives a strategy a leaf annex.

#![forbid(unsafe_code)]

use core::fmt;
use core::marker::PhantomData;

use crate::inline_key::InlineKey;
use crate::leaf_annex::LeafAnnex;
use crate::node_config::NodeConfig;
use sealed::{InlineKeySource, Sealed};

/// The default strategy: std byte-string keys and 64- and 128-bit integer
/// keys have inline keys, other keys have none, at no cost.
///
/// The key types recognized are the byte strings `String`, `str`,
/// `Vec<u8>`, `[u8]`, `Box<str>`, `Box<[u8]>`, `&str`, `&[u8]`, `[u8; 16]`,
/// `[u8; 20]` and `[u8; 32]`, with the `file` feature a file map's
/// `VarBytes`, and the integers `u64`, `i64`, `u128` and `i128`. Their
/// inline keys are those their [`InlineKey`] impls give. They
/// are recognized by their type alone: a newtype around one of them is
/// another type, and needs [`WithInlineKey`]. A lookup uses the inline keys
/// when the type it looks up with is recognized too, as `&str` is for
/// `String` keys; otherwise it compares full keys, as a map without inline
/// keys does.
///
/// Fixed byte arrays keep 2 bytes per inline key. Heap-backed byte strings
/// keep 8 in leaves, to read the heap less on prefix ties, and 4 in internal
/// nodes, to keep those compact.
#[derive(Clone, Copy, Debug, Default)]
pub struct Auto;

/// The opt-in strategy: every key has the inline key its [`InlineKey`] impl
/// gives, of which the nodes keep [`InlineKey::WIDTH`] bytes, and the hash
/// tag it gives if [`InlineKey::HAS_HASH_TAG`].
///
/// A lookup uses the inline keys when it looks up with the key type itself.
/// A lookup through another borrowed form of the key (a `Q` other than `K`
/// in [`BBTreeMap::read`]) has no inline key to compare, and compares full
/// keys instead. Inline keys only speed up the search, so the answer is the
/// same.
///
/// [`BBTreeMap::read`]: crate::BBTreeMap::read
#[derive(Clone, Copy, Debug, Default)]
pub struct WithInlineKey;

/// Which keys of a map with keys `K` have inline keys: [`Auto`] or
/// [`WithInlineKey`], with the default [`NodeConfig`], or either of them
/// with another, [`Configured`]; and what its leaves keep besides,
/// [`WithLeafAnnex`]. These are the only implementations.
///
/// [`NodeConfig`]: crate::NodeConfig
/// [`Configured`]: crate::Configured
/// [`WithLeafAnnex`]: crate::WithLeafAnnex
pub trait InlineKeyStrategy<K>: sealed::Sealed<K> {
    /// What each leaf keeps besides its entries ([`LeafAnnex`]): nothing,
    /// `()`, but with [`WithLeafAnnex`](crate::WithLeafAnnex).
    type LeafAnnex: LeafAnnex;
}

impl<K> InlineKeyStrategy<K> for Auto {
    type LeafAnnex = ();
}

impl<K: InlineKey> InlineKeyStrategy<K> for WithInlineKey {
    type LeafAnnex = ();
}

/// What a strategy tells the nodes, kept out of the public API.
pub(crate) mod sealed {
    use core::cmp::Ordering;

    use super::{Auto, WithInlineKey};
    use crate::inline_key::{InlineKey, InlineWidth, InlineWord};
    use crate::node_config::{DefaultNodeConfig, NodeConfig};

    /// Where a strategy's inline keys come from. Plain data: the node code
    /// resolves it for a key type (`tree/node/inline_keys.rs`), and page
    /// layouts depend on the answer.
    pub enum InlineKeySource<K> {
        /// Std byte strings and integers, and a file map's `VarBytes`,
        /// recognized by their type, have inline keys: windows of a byte
        /// string's bytes, and an integer's own [`InlineKey`] impl. Other
        /// keys have none ([`Auto`]).
        RecognizedStdTypes,
        /// Every key has the inline keys its [`InlineKey`] impl gives, of
        /// which the nodes keep `width` bytes, and its hash tag and
        /// comparison from where keys differ if the impl gives them
        /// ([`WithInlineKey`]).
        InlineKeyTrait {
            inline_key_at: fn(&K, usize) -> InlineWord,
            width: InlineWidth,
            hash_tag: Option<fn(&K) -> u8>,
            cmp_from: Option<fn(&K, &K, usize) -> Ordering>,
            cmp_prefix: fn(&K, &[u8]) -> Ordering,
        },
    }

    pub trait Sealed<K> {
        const SOURCE: InlineKeySource<K>;

        /// How the nodes are built (`node_config.rs`): every default, or a
        /// `Configured` strategy's config. A strategy that wraps another
        /// forwards the other's.
        type Config: NodeConfig;

        /// Whether the nodes keep a latch word in their header, for a tree
        /// that threads share (`tree/node/latch.rs`): only the strategy a
        /// shared `BBTreeMap` wraps its own in, never a public one.
        const LATCHED: bool = false;

        /// Whether the tree lies in a file map, whose edges are offsets from
        /// their node's page rather than pointers (`tree/file/mod.rs`): only
        /// the strategy a `FileMap` wraps its own in, never a public one.
        const IN_FILE: bool = false;
    }

    impl<K> Sealed<K> for Auto {
        const SOURCE: InlineKeySource<K> = InlineKeySource::RecognizedStdTypes;
        type Config = DefaultNodeConfig;
    }

    impl<K: InlineKey> Sealed<K> for WithInlineKey {
        const SOURCE: InlineKeySource<K> = InlineKeySource::InlineKeyTrait {
            inline_key_at: K::inline_key_at,
            width: K::WIDTH,
            hash_tag: if K::HAS_HASH_TAG {
                Some(K::hash_tag)
            } else {
                None
            },
            cmp_from: if K::HAS_CMP_FROM {
                Some(K::cmp_from)
            } else {
                None
            },
            cmp_prefix: K::cmp_prefix,
        };
        type Config = DefaultNodeConfig;
    }
}

/// The strategy whose inline keys are strategy `S`'s, and whose nodes are
/// built as config `C` says: `Configured<Auto, C>` or
/// `Configured<WithInlineKey, C>`.
pub struct Configured<S, C>(PhantomData<fn() -> (S, C)>);

impl<S, C> Configured<S, C> {
    #[must_use]
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}

// By hand: derives would bound `S` and `C`, which the marker doesn't hold.

impl<S, C> Clone for Configured<S, C> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<S, C> Copy for Configured<S, C> {}

impl<S, C> Default for Configured<S, C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S, C> fmt::Debug for Configured<S, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Configured")
    }
}

impl<K, S: InlineKeyStrategy<K>, C: NodeConfig> Sealed<K> for Configured<S, C> {
    const SOURCE: InlineKeySource<K> = S::SOURCE;
    type Config = C;
}

impl<K, S: InlineKeyStrategy<K>, C: NodeConfig> InlineKeyStrategy<K> for Configured<S, C> {
    type LeafAnnex = S::LeafAnnex;
}

/// The strategy whose inline keys and node config are strategy `S`'s, and
/// whose leaves keep an annex `L`: for a [`BBTreeMap`](crate::BBTreeMap).
pub struct WithLeafAnnex<S, L>(PhantomData<fn() -> (S, L)>);

impl<S, L> WithLeafAnnex<S, L> {
    #[must_use]
    pub const fn new() -> Self {
        Self(PhantomData)
    }
}

// By hand: derives would bound `S` and `L`, which the marker doesn't hold.

impl<S, L> Clone for WithLeafAnnex<S, L> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<S, L> Copy for WithLeafAnnex<S, L> {}

impl<S, L> Default for WithLeafAnnex<S, L> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S, L> fmt::Debug for WithLeafAnnex<S, L> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WithLeafAnnex")
    }
}

impl<K, S: InlineKeyStrategy<K>, L: LeafAnnex> Sealed<K> for WithLeafAnnex<S, L> {
    const SOURCE: InlineKeySource<K> = S::SOURCE;
    type Config = S::Config;
}

impl<K, S: InlineKeyStrategy<K>, L: LeafAnnex> InlineKeyStrategy<K> for WithLeafAnnex<S, L> {
    type LeafAnnex = L;
}
