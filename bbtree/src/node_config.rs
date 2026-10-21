//! Node configs (D59): what a map may tune about its nodes, the sizes of
//! their pages first, as constants of one trait, [`NodeConfig`], that the
//! map's strategy carries.
//!
//! Every constant has a default, and a config overrides the ones it names.
//! A knob added later is one more defaulted constant: configs written before
//! keep compiling, with its default. A strategy hands its config on as one
//! associated type (`Sealed::Config`), so the strategies that wrap another
//! (`Latched`, `InFile`) forward every knob at once.

#![forbid(unsafe_code)]

use core::fmt;
use core::marker::PhantomData;

use crate::inline_key::InlineKeyStrategy;
use crate::inline_key::sealed::{InlineKeySource, Sealed};

/// How a map builds its nodes: constants the map reads at compile time, so
/// that a map with a config runs as one without would at the same sizes.
///
/// Every constant has a default; an impl overrides the ones it names, and
/// [`Configured`] gives a map the config:
///
/// ```
/// use bbtree::{Auto, BBTreeMap, Configured, NodeConfig};
///
/// struct LargeLeaves;
///
/// impl NodeConfig for LargeLeaves {
///     const LEAF_PAGE_SIZE: usize = 4096;
/// }
///
/// let map = BBTreeMap::<u64, u64, Configured<Auto, LargeLeaves>>::with_strategy(Configured::new());
/// map.insert(1, 2).unwrap();
/// assert_eq!(map.get_copied(&1), Some(2));
/// ```
///
/// A page size is a power of two. A node is the smallest multiple of its
/// kind's page size that holds 12 keys, and is aligned to that page size,
/// or more if its keys or values need it. A map whose config breaks a
/// bound below fails to build:
///
/// ```compile_fail,E0080
/// use bbtree::{Auto, Configured, NodeConfig, unsync};
///
/// struct SmallLeaves;
///
/// impl NodeConfig for SmallLeaves {
///     const LEAF_PAGE_SIZE: usize = 512;
/// }
///
/// let mut map = unsync::BBTreeMap::<u64, u64, Configured<Auto, SmallLeaves>>::with_strategy(Configured::new());
/// map.insert(1, 2);
/// ```
///
///
/// Every page of one kind in a map has one layout: an allocator that pools
/// pages by kind ([`Allocator`](crate::Allocator)) pools pages of one size.
pub trait NodeConfig {
    /// The page size of leaves: at least 1024. Larger leaves are fewer,
    /// but an insert or a remove shifts more of a leaf's bytes.
    const LEAF_PAGE_SIZE: usize = 1024;

    /// The page size of internal nodes of keys with inline keys, which keep
    /// only the inline keys and their children: at least 64.
    const INTERNAL_PAGE_SIZE: usize = 512;

    /// The page size of internal nodes of keys without inline keys, which
    /// keep their keys: at least 64.
    const PLAIN_INTERNAL_PAGE_SIZE: usize = 1024;

    /// The page size of a store's heap pages, which hold its leaves' records
    /// (`design/16-bbtree-store.md`): the leaves' by default, at least 1024.
    /// A record larger than a heap page is refused. Only a store reads it.
    const HEAP_PAGE_SIZE: usize = Self::LEAF_PAGE_SIZE;
}

/// The config of [`Auto`](crate::Auto) and
/// [`WithInlineKey`](crate::WithInlineKey): every default. `pub` only as
/// their `Sealed::Config` must be (E0446); nothing exports it.
pub struct DefaultNodeConfig;

impl NodeConfig for DefaultNodeConfig {}

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
