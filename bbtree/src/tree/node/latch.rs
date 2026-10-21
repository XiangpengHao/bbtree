//! Latches for a tree that threads share (`design/09-concurrency.md`): a word
//! in every node's header ([`NodeLatch`]), and the strategy that puts it
//! there ([`Latched`]).
//!
//! One word serves both ways the tree is latched. So the root page, which
//! holds a leaf until the tree grows and an internal node after, keeps one
//! word through the change:
//!
//! - **Internal nodes, optimistically.** A reader reads the version, reads
//!   the node, and checks that the version is unchanged
//!   ([`NodeLatch::read_version`], [`NodeLatch::is_unchanged`]). A writer
//!   takes the node exclusively from the version it read
//!   ([`NodeLatch::try_lock_at`]), never waiting, and bumps the version as
//!   it lets go. The writer changes the node in place. The reader reads it
//!   through raw pointers, in bounds whatever it reads (`optimistic.rs`); a
//!   writer changing it meanwhile fails the check. This is optimistic lock
//!   coupling.
//! - **Leaves, pessimistically.** Shared for reads, exclusive for changes,
//!   spinning with backoff while the other mode holds it
//!   ([`NodeLatch::lock_shared`], [`NodeLatch::lock_exclusive`]). Letting
//!   go of it exclusively bumps the version too, so a lookup can also read
//!   a leaf optimistically (D57).
//!
//! The word, from the low bit: the exclusive bit, a count of shared holders,
//! and the version. A version is the word without its count, read while the
//! exclusive bit is clear. Shared holders don't change what a reader reads,
//! so an optimistic check ignores them.

use core::marker::PhantomData;
use core::sync::atomic::{AtomicU64, Ordering, fence};

use super::super::interleaving::Interleaving;
use super::internal::Internal;
use super::leaf::Leaf;
use crate::inline_key::InlineKeyStrategy;
use crate::inline_key::sealed::{InlineKeySource, Sealed};

/// The strategy of a latched tree's nodes: `S`'s inline keys, and a latch
/// word in every node's header (`page.rs`, `LatchedNodeHeader`). Nothing
/// public names it: a `BBTreeMap` with strategy `S` keeps its tree with
/// this one.
pub(crate) struct Latched<S>(PhantomData<fn() -> S>);

impl<K, S: InlineKeyStrategy<K>> Sealed<K> for Latched<S> {
    const SOURCE: InlineKeySource<K> = S::SOURCE;
    type Config = S::Config;
    const LATCHED: bool = true;
}

impl<K, S: InlineKeyStrategy<K>> InlineKeyStrategy<K> for Latched<S> {}

impl<K, V, S: InlineKeyStrategy<K>> Leaf<K, V, Latched<S>> {
    /// The leaf's latch word, for as long as the caller picks: a latched
    /// tree frees no node while threads share it.
    #[inline(always)]
    pub(in crate::tree) fn latch<'a>(self) -> &'a NodeLatch {
        // SAFETY: the tree is latched, and the handle's page allocated (the
        // handles' contract).
        unsafe { self.node_ptr().latch() }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Internal<K, V, Latched<S>> {
    /// The node's latch word; see [`Leaf::latch`].
    #[inline(always)]
    pub(in crate::tree) fn latch<'a>(self) -> &'a NodeLatch {
        // SAFETY: as for a leaf.
        unsafe { self.node_ptr().latch() }
    }
}

/// A node's latch word; see the module documentation. Only atomics ever
/// access it.
#[repr(transparent)]
pub(in crate::tree) struct NodeLatch(AtomicU64);

/// What an optimistic reader saw of a node: its latch word without the
/// shared count, unlatched exclusively. Equal versions of one node mean
/// that nothing changed it in between.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(in crate::tree) struct Version(u64);

impl Version {
    /// The version a node moves to when let go after being held exclusively
    /// since this one ([`NodeLatch::unlock_exclusive`]).
    #[inline(always)]
    pub(in crate::tree) fn next(self) -> Self {
        Self(self.0 + NodeLatch::VERSION_UNIT)
    }
}

impl NodeLatch {
    const EXCLUSIVE: u64 = 1;
    const SHARED_UNIT: u64 = 1 << 1;
    /// Up to 2^20 − 1 shared holders at once.
    const SHARED_MASK: u64 = ((1 << 20) - 1) << 1;
    /// 43 bits of version: a node read optimistically would have to change
    /// 2^43 times while one reader reads it for a check to pass wrongly.
    const VERSION_UNIT: u64 = 1 << 21;

    /// The node's version, to read it optimistically, once no thread holds
    /// it exclusively; spins until then.
    #[inline(always)]
    pub(in crate::tree) fn read_version(&self) -> Version {
        Interleaving::switch_point();
        let word = self.0.load(Ordering::Acquire);
        if word & Self::EXCLUSIVE == 0 {
            return Version(word & !Self::SHARED_MASK);
        }
        self.read_version_contended()
    }

    #[cold]
    #[inline(never)]
    fn read_version_contended(&self) -> Version {
        let mut backoff = Backoff::new();
        loop {
            backoff.spin();
            Interleaving::switch_point();
            let word = self.0.load(Ordering::Acquire);
            if word & Self::EXCLUSIVE == 0 {
                return Version(word & !Self::SHARED_MASK);
            }
        }
    }

    /// Whether the node is still at `version`: nothing took it exclusively
    /// since, so what the caller read of it since reading `version` is what
    /// the node held. Call it after the reads; a fence keeps them before the
    /// check.
    #[inline(always)]
    pub(in crate::tree) fn is_unchanged(&self, version: Version) -> bool {
        fence(Ordering::Acquire);
        Interleaving::switch_point();
        self.0.load(Ordering::Relaxed) & !Self::SHARED_MASK == version.0
    }

    /// Takes the node exclusively if it is still at `version`, with no
    /// shared holder; otherwise fails at once, without waiting. Let go with
    /// [`Self::unlock_exclusive`], at the same version.
    #[inline]
    pub(in crate::tree) fn try_lock_at(&self, version: Version) -> bool {
        Interleaving::switch_point();
        let locked = self
            .0
            .compare_exchange(
                version.0,
                version.0 | Self::EXCLUSIVE,
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .is_ok();
        // A reader that sees a write made under the latch sees the latch
        // taken when it checks its version: this fence pairs with its
        // acquire fence (`is_unchanged`).
        fence(Ordering::Release);
        locked
    }

    /// Lets go of the node, held exclusively since `version`, and moves it
    /// to the next version, which tells optimistic readers it changed.
    #[inline]
    pub(in crate::tree) fn unlock_exclusive(&self, version: Version) {
        Interleaving::switch_point();
        self.0.store(version.next().0, Ordering::Release);
    }

    /// The node's version, if no thread holds it exclusively now; `None`
    /// without waiting otherwise.
    #[inline]
    pub(in crate::tree) fn try_read_version(&self) -> Option<Version> {
        Interleaving::switch_point();
        let word = self.0.load(Ordering::Acquire);
        (word & Self::EXCLUSIVE == 0).then_some(Version(word & !Self::SHARED_MASK))
    }

    /// Takes the node exclusively if no thread holds it in either mode;
    /// `None` without waiting otherwise. Returns the version to let go at.
    #[inline]
    pub(in crate::tree) fn try_lock_exclusive(&self) -> Option<Version> {
        Interleaving::switch_point();
        let word = self.0.load(Ordering::Relaxed);
        let version = Version(word);
        (word & (Self::EXCLUSIVE | Self::SHARED_MASK) == 0 && self.try_lock_at(version))
            .then_some(version)
    }

    /// Takes the node exclusively, once no thread holds it in either mode;
    /// spins until then. Returns the version to let go at.
    #[inline]
    pub(in crate::tree) fn lock_exclusive(&self) -> Version {
        Interleaving::switch_point();
        let word = self.0.load(Ordering::Relaxed);
        let version = Version(word & !Self::SHARED_MASK);
        if word == version.0 && word & Self::EXCLUSIVE == 0 && self.try_lock_at(version) {
            return version;
        }
        self.lock_exclusive_contended()
    }

    #[cold]
    #[inline(never)]
    fn lock_exclusive_contended(&self) -> Version {
        let mut backoff = Backoff::new();
        loop {
            backoff.spin();
            Interleaving::switch_point();
            let word = self.0.load(Ordering::Relaxed);
            if word & (Self::EXCLUSIVE | Self::SHARED_MASK) == 0 && self.try_lock_at(Version(word))
            {
                return Version(word);
            }
        }
    }

    /// Takes the node shared, once no thread holds it exclusively; spins
    /// until then. Let go with [`Self::unlock_shared`].
    #[inline]
    pub(in crate::tree) fn lock_shared(&self) {
        Interleaving::switch_point();
        let word = self.0.load(Ordering::Relaxed);
        if word & Self::EXCLUSIVE == 0 && self.try_add_shared_holder(word) {
            return;
        }
        self.lock_shared_contended();
    }

    /// Adds a shared holder to the word, if it is still `word`: `false` if
    /// it changed, or spuriously.
    #[inline(always)]
    fn try_add_shared_holder(&self, word: u64) -> bool {
        Interleaving::switch_point();
        self.0
            .compare_exchange_weak(
                word,
                word + Self::SHARED_UNIT,
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .is_ok()
    }

    #[cold]
    #[inline(never)]
    fn lock_shared_contended(&self) {
        let mut backoff = Backoff::new();
        loop {
            Interleaving::switch_point();
            let word = self.0.load(Ordering::Relaxed);
            if word & Self::EXCLUSIVE == 0 {
                debug_assert!(word & Self::SHARED_MASK != Self::SHARED_MASK);
                if self.try_add_shared_holder(word) {
                    return;
                }
            }
            backoff.spin();
        }
    }

    /// Lets go of the node, held shared.
    #[inline]
    pub(in crate::tree) fn unlock_shared(&self) {
        Interleaving::switch_point();
        self.0.fetch_sub(Self::SHARED_UNIT, Ordering::Release);
    }
}

/// Spinning while another thread holds a latch: twice as long each round,
/// up to a cap. The crate has no `std`, so there is no scheduler to yield
/// to, except Shuttle's in tests (`tree/interleaving.rs`).
struct Backoff {
    round: u32,
}

impl Backoff {
    const MAX_ROUND: u32 = 10;

    fn new() -> Self {
        Self { round: 0 }
    }

    fn spin(&mut self) {
        Interleaving::wait(1 << self.round);
        self.round = (self.round + 1).min(Self::MAX_ROUND);
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::AtomicU64;

    use super::{NodeLatch, Version};

    fn latch() -> NodeLatch {
        NodeLatch(AtomicU64::new(0))
    }

    /// An exclusive hold changes the version; a shared one doesn't, and
    /// keeps a writer from taking the node.
    #[test]
    fn exclusive_holds_change_the_version_and_shared_ones_do_not() {
        let latch = latch();
        let before = latch.read_version();
        latch.lock_shared();
        assert!(latch.is_unchanged(before));
        assert!(
            !latch.try_lock_at(before),
            "a shared holder blocks a writer"
        );
        latch.unlock_shared();
        assert!(latch.try_lock_at(before));
        assert!(
            !latch.is_unchanged(before),
            "a held latch fails every check"
        );
        latch.unlock_exclusive(before);
        let after = latch.read_version();
        assert_ne!(after, before);
        assert!(!latch.try_lock_at(before), "a stale version takes nothing");
        let held_at = latch.lock_exclusive();
        assert_eq!(held_at, after);
        latch.unlock_exclusive(held_at);
        assert_ne!(latch.read_version(), Version(after.0));
    }
}
