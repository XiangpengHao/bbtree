//! [`BCTreeMap`]: an ordered map that threads share, on the same nodes as
//! [`BBTreeMap`](crate::BBTreeMap) (`doc/09-concurrency.md`).

#![forbid(unsafe_code)]

#[cfg(test)]
mod tests;

use core::borrow::Borrow;
use core::fmt;

use crate::allocator::{Allocator, Global};
use crate::inline_key::{Auto, InlineKeyStrategy};
use crate::tree::latched::LatchedTree;

/// An ordered map that threads share, through `&self`: a B+-tree whose
/// internal nodes are read optimistically, and whose leaves are latched
/// (`doc/09-concurrency.md`).
///
/// Nothing it hands out outlives a call: an operation that exposes an entry
/// runs the caller's closure while the entry's leaf is latched, and returns
/// what the closure returned ([`Self::read`], [`Self::update`]). No user
/// takes a latch by hand, or holds a guard.
///
/// The keys need inline keys (`InlineKey`): the std integers and byte
/// strings have them under [`Auto`]; other keys implement the trait and use
/// [`WithInlineKey`](crate::WithInlineKey).
///
/// Phase 1 of the design: it only grows. There is no removal yet, and no
/// iteration.
///
/// # Deadlocks
///
/// A closure that calls into the same map may deadlock: [`Self::update`]'s
/// on any key of its entry's leaf, [`Self::read`]'s if a writer waits for
/// that leaf meanwhile.
///
/// It is `Sync` if its keys and values are `Send` and `Sync`, as threads
/// move them in, read them, change them and drop them through `&self`, and
/// its allocator is `Sync`.
pub struct BCTreeMap<K, V, S: InlineKeyStrategy<K> = Auto, A: Allocator = Global> {
    tree: LatchedTree<K, V, S>,
    /// Where the tree's pages come from and go back to.
    alloc: A,
}

impl<K, V> BCTreeMap<K, V> {
    /// Makes a new, empty `BCTreeMap`, with the default strategy, [`Auto`],
    /// and the global allocator.
    ///
    /// # Panics
    ///
    /// If the keys have no inline keys under [`Auto`].
    pub fn new() -> Self {
        Self::new_in(Global)
    }
}

impl<K, V> Default for BCTreeMap<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V, S: InlineKeyStrategy<K>> BCTreeMap<K, V, S> {
    /// Makes a new, empty `BCTreeMap` with strategy `S`, as
    /// [`BBTreeMap::with_strategy`](crate::BBTreeMap::with_strategy) does.
    ///
    /// # Panics
    ///
    /// If the keys have no inline keys under `S`.
    pub fn with_strategy(strategy: S) -> Self {
        let _ = strategy;
        Self::new_in(Global)
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator> BCTreeMap<K, V, S, A> {
    /// Makes a new, empty `BCTreeMap` whose pages come from `alloc`.
    ///
    /// # Panics
    ///
    /// If the keys have no inline keys under `S`.
    pub fn new_in(alloc: A) -> Self {
        Self {
            tree: LatchedTree::new(),
            alloc,
        }
    }

    /// Runs `f` on the entry whose key equals `key`, while no thread
    /// changes it, and returns what `f` returned; `None` without one.
    #[inline]
    pub fn read<Q, R>(&self, key: &Q, f: impl FnOnce(&K, &V) -> R) -> Option<R>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.tree.read(key, f)
    }

    /// Runs `f` on the entry whose key equals `key`, its value exclusive,
    /// and returns what `f` returned; `None` without one.
    #[inline]
    pub fn update<Q, R>(&self, key: &Q, f: impl FnOnce(&K, &mut V) -> R) -> Option<R>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.tree.update(key, f)
    }

    /// A clone of the value of `key`'s entry; `None` without one.
    #[inline]
    pub fn get_cloned<Q>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
        V: Clone,
    {
        self.read(key, |_, value| value.clone())
    }

    /// Whether the map holds an entry whose key equals `key`.
    #[inline]
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.read(key, |_, _| ()).is_some()
    }

    /// Inserts `key` and `value`, and returns `None`; or, if the map holds
    /// an equal key, keeps that key, drops `key`, and returns the value it
    /// replaces, as std's `BTreeMap::insert` does.
    #[inline]
    pub fn insert(&self, key: K, value: V) -> Option<V>
    where
        K: Ord,
    {
        self.tree.insert(key, value, &self.alloc)
    }
}

#[cfg(test)]
impl<K: Ord, V, S: InlineKeyStrategy<K>, A: Allocator> BCTreeMap<K, V, S, A> {
    /// Panics unless the tree is well formed and holds `length` entries.
    pub(crate) fn check_invariants(&mut self, length: usize) {
        self.tree.check_invariants(length);
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator> Drop for BCTreeMap<K, V, S, A> {
    fn drop(&mut self) {
        self.tree.destroy(&self.alloc);
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator> fmt::Debug for BCTreeMap<K, V, S, A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BCTreeMap").finish_non_exhaustive()
    }
}
