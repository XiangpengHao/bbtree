//! Looking a key up in a latched tree (`design/09-concurrency.md`, Lookups,
//! D57). The descent reads the internal nodes optimistically, as every
//! operation's does, and the leaf too, at the version it was at
//! ([`Optimistic`]).
//!
//! - **A key the leaf's inline keys rule out** is absent once the leaf's
//!   version, unchanged after the read, confirms it: a miss writes no
//!   latch.
//! - **A copy** of a `Copy` value ([`LatchedTree::get_copied`]) is read
//!   unlatched, then the version checked, as a sequence lock reads. This
//!   works where the leaf settles the key. A key ordered by its own bytes
//!   is read and compared as a copy, and a byte string that ends within its
//!   inline key is the candidate of the same length (D36). Other keys
//!   compare latched.
//! - **Otherwise** the leaf is latched shared and, if it is still at that
//!   version, searched. The caller's closure runs on the entry while the
//!   latch is held.
//!
//! A leaf that changed meanwhile sends the lookup the latched way, which
//! waits for a writer rather than race it again.

use core::borrow::Borrow;
use core::cmp::Ordering as KeyOrdering;
use core::mem::{ManuallyDrop, MaybeUninit};

use super::super::node::inline_keys::{KeyInliner, SearchKey};
use super::super::node::latch::{Latched, Version};
use super::super::node::leaf::{CandidateLine, Leaf};
use super::super::node::optimistic::RacingEntry;
use super::LatchedTree;
use super::descent::{HeldLeaf, Optimistic, Shared};
use crate::inline_key::InlineKeyStrategy;

/// A lookup that reading unlatched couldn't settle: the leaf changed, or the
/// key's comparison needs it latched. It goes the latched way.
pub(super) struct Unsettled;

/// What a lookup read of its leaf unlatched, which holds only if the leaf's
/// version is unchanged after ([`Self::settle`]).
pub(super) enum RacingRead<K, V> {
    /// The leaf's inline keys rule the key out.
    Absent,
    /// The leaf alone says the candidate is the key: its value.
    Value(MaybeUninit<V>),
    /// The head, or a candidate, whose key the lookup compares, a key
    /// ordered by its own bytes.
    Entry(RacingEntry<K, V>),
}

impl<K, V> RacingRead<K, V> {
    /// Reads the head of `leaf`, which the caller read at a version; the
    /// descent found the key at the head's separator. [`Unsettled`] if the
    /// leaf has no head, or the keys aren't ordered by their own bytes: only
    /// such a key can be compared as a copy.
    #[inline(always)]
    pub(super) fn of_head<S: InlineKeyStrategy<K>>(
        leaf: Leaf<K, V, Latched<S>>,
    ) -> Result<Self, Unsettled> {
        match leaf.load_racing_head_entry() {
            Some(head) if KeyInliner::<K>::orders_by_own_bytes::<S>() => Ok(Self::Entry(head)),
            // A leaf without a head lost the separator the key matched.
            _ => Err(Unsettled),
        }
    }

    /// Reads candidate `index` of `leaf`, which the caller read at a
    /// version: the one key whose inline key, at `prefix_len` (the leaf's
    /// then), equals `key`'s. [`Unsettled`] if only the keys' bytes tell,
    /// and they aren't ordered by their own bytes.
    #[inline(always)]
    pub(super) fn of_candidate<S: InlineKeyStrategy<K>, Q>(
        leaf: Leaf<K, V, Latched<S>>,
        key: &SearchKey<'_, K, Latched<S>, Q>,
        index: usize,
        prefix_len: usize,
    ) -> Result<Self, Unsettled>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if KeyInliner::<K>::orders_by_own_bytes::<S>() {
            return Ok(Self::Entry(leaf.load_racing_entry(index)));
        }
        match leaf.racing_candidate_is(key, index, prefix_len) {
            Some(true) => Ok(Self::Value(leaf.load_racing_value(index))),
            Some(false) => Ok(Self::Absent),
            None => Err(Unsettled),
        }
    }

    /// The value read, if the entry read is `key`'s. If `at_separator`, the
    /// entry read is the head, and [`Unsettled`] if it isn't `key`'s: the
    /// separator `key` matched was removed since.
    ///
    /// # Safety
    ///
    /// The leaf was at one version from before the read to after it.
    #[inline(always)]
    pub(super) unsafe fn settle<S: InlineKeyStrategy<K>, Q>(
        self,
        key: &SearchKey<'_, K, Latched<S>, Q>,
        at_separator: bool,
    ) -> Result<Option<ManuallyDrop<V>>, Unsettled>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        match self {
            Self::Absent => Ok(None),
            // SAFETY: the leaf said, at that version, that the candidate
            // read is the key, and so below its length (caller).
            Self::Value(value) => Ok(Some(ManuallyDrop::new(unsafe { value.assume_init() }))),
            Self::Entry(entry) => {
                // SAFETY: the read was of its head, which it had, or of a
                // candidate below its length, as read, at that version
                // (caller).
                let (stored, value) = unsafe { entry.assume_read() };
                match key.cmp_full(&stored) {
                    KeyOrdering::Equal => Ok(Some(value)),
                    _ if at_separator => Err(Unsettled),
                    _ => Ok(None),
                }
            }
        }
    }
}

/// Where a lookup that read its leaf unlatched found its entry
/// ([`LatchedTree::find_optimistic`]).
enum LeafFind<K, V, S: InlineKeyStrategy<K>> {
    /// The tree holds no entry with the key.
    Absent,
    /// A copy of the entry's value, settled unlatched.
    Copied(ManuallyDrop<V>),
    /// The entry, in its leaf, which the lookup holds latched shared.
    Held(HeldLeaf<K, V, S, Shared>, EntryPlace),
    /// The leaf changed while the lookup read it, or lost the head its key
    /// matched: the lookup goes the latched way.
    Changed,
}

/// Where an entry is in its leaf: its head (D17), or key `index`.
#[derive(Clone, Copy)]
enum EntryPlace {
    Head,
    Key(usize),
}

impl<K, V, S: InlineKeyStrategy<K>> LatchedTree<K, V, S> {
    /// Runs `f` on the entry whose key equals `key`, while its leaf is
    /// latched shared, and returns what `f` returned; `None` without one,
    /// which the leaf's inline keys may tell unlatched.
    #[inline]
    pub(crate) fn read<Q, R>(&self, key: &Q, f: impl FnOnce(&K, &V) -> R) -> Option<R>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.root_page()?;
        let key = SearchKey::new(key);
        match self.find_optimistic::<Q, false>(&key) {
            LeafFind::Absent => None,
            LeafFind::Held(held, place) => {
                let (key, value) = Self::entry_at(held.leaf(), place);
                let result = f(key, value);
                drop(held);
                Some(result)
            }
            LeafFind::Copied(_) => unreachable!("a read copies nothing"),
            LeafFind::Changed => self.read_latched(&key, f),
        }
    }

    /// A copy of the value of `key`'s entry; `None` without one. Unlatched
    /// where the leaf settles it (module documentation).
    #[inline]
    pub(crate) fn get_copied<Q>(&self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
        V: Copy,
    {
        self.root_page()?;
        let key = SearchKey::new(key);
        match self.find_optimistic::<Q, true>(&key) {
            LeafFind::Absent => None,
            LeafFind::Copied(value) => Some(ManuallyDrop::into_inner(value)),
            LeafFind::Held(held, place) => {
                let value = *Self::entry_at(held.leaf(), place).1;
                drop(held);
                Some(value)
            }
            LeafFind::Changed => self.read_latched(&key, |_, value| *value),
        }
    }

    /// Whether the tree holds an entry whose key equals `key`. Unlatched
    /// where the leaf settles it, as for [`Self::get_copied`].
    #[inline]
    pub(crate) fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if self.root_page().is_none() {
            return false;
        }
        let key = SearchKey::new(key);
        match self.find_optimistic::<Q, true>(&key) {
            LeafFind::Absent => false,
            LeafFind::Copied(_) | LeafFind::Held(..) => true,
            LeafFind::Changed => self.read_latched(&key, |_, _| ()).is_some(),
        }
    }

    /// Where `key`'s entry is, by an optimistic descent and a read of its
    /// leaf in place. Settled unlatched where the leaf's inline keys rule
    /// the key out, or, if `COPIES`, where the leaf settles a candidate,
    /// whose value it copies ([`RacingRead`]). Otherwise found in the leaf,
    /// latched shared if still at the version the descent read.
    ///
    /// # Panics
    ///
    /// Without a tree.
    #[inline(always)]
    fn find_optimistic<Q, const COPIES: bool>(
        &self,
        key: &SearchKey<'_, K, Latched<S>, Q>,
    ) -> LeafFind<K, V, S>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let Ok((at, at_separator)) = self.descend::<Q, Optimistic, ()>(key, &mut ()) else {
            return LeafFind::Absent;
        };
        let leaf = at.leaf();
        let (racing, candidate) = if at_separator {
            (COPIES.then(|| RacingRead::of_head(leaf)), None)
        } else {
            match leaf.candidate_line_racing(key) {
                (CandidateLine::Absent, _) => (Some(Ok(RacingRead::Absent)), None),
                (CandidateLine::One(index, _), prefix_len) => (
                    COPIES.then(|| RacingRead::of_candidate(leaf, key, index, prefix_len)),
                    Some((index, prefix_len)),
                ),
                (CandidateLine::Unknown, _) => (None, None),
            }
        };
        if let Some(Ok(read)) = racing {
            if !at.is_unchanged() {
                return LeafFind::Changed;
            }
            // SAFETY: the leaf stayed at the version the descent read, from
            // before the read to after it.
            return match unsafe { read.settle(key, at_separator) } {
                Ok(Some(value)) => LeafFind::Copied(value),
                Ok(None) => LeafFind::Absent,
                Err(Unsettled) => LeafFind::Changed,
            };
        }
        // A leaf that changed since the descent may have merged into its
        // sibling and been freed, its bytes a stale copy of entries that
        // live on there.
        let Some(held) = Self::latch_unchanged(leaf, at.version()) else {
            return LeafFind::Changed;
        };
        if at_separator {
            return if Self::is_head(leaf, key) {
                LeafFind::Held(held, EntryPlace::Head)
            } else {
                LeafFind::Changed
            };
        }
        match Self::find_latched(leaf, key, candidate, ()) {
            Some(index) => LeafFind::Held(held, EntryPlace::Key(index)),
            None => LeafFind::Absent,
        }
    }

    /// The entry at `place` in `leaf`, which the caller holds.
    #[inline(always)]
    fn entry_at<'a>(leaf: Leaf<K, V, Latched<S>>, place: EntryPlace) -> (&'a K, &'a V) {
        match place {
            EntryPlace::Head => (leaf.head_key(), leaf.head_value()),
            // SAFETY: the place is a search's, of a key of the leaf, which
            // hasn't changed since.
            EntryPlace::Key(index) => unsafe { leaf.key_value_unchecked(index) },
        }
    }

    /// [`Self::read`] the latched way: the leaf latched shared at the end of
    /// the descent, which waits for a writer that holds it.
    #[inline(never)]
    pub(super) fn read_latched<Q, R>(
        &self,
        key: &SearchKey<'_, K, Latched<S>, Q>,
        f: impl FnOnce(&K, &V) -> R,
    ) -> Option<R>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let (held, at_separator) = self.descend::<Q, Shared, ()>(key, &mut ()).ok()?;
        let leaf = held.leaf();
        let (key, value) = if at_separator {
            (leaf.head_key(), leaf.head_value())
        } else {
            let index = leaf.find(key, ())?;
            // SAFETY: a search returns the index of a key of the leaf.
            unsafe { leaf.key_value_unchecked(index) }
        };
        let result = f(key, value);
        drop(held);
        Some(result)
    }

    /// `leaf`, latched shared, if it is still at `version`, which the caller
    /// read it at; `None`, let go, otherwise.
    #[inline(always)]
    pub(super) fn latch_unchanged(
        leaf: Leaf<K, V, Latched<S>>,
        version: Version,
    ) -> Option<HeldLeaf<K, V, S, Shared>> {
        let held = HeldLeaf::<K, V, S, Shared>::lock(leaf);
        leaf.latch().is_unchanged(version).then_some(held)
    }

    /// The index of `key`'s entry in `leaf`, which the caller holds at the
    /// version it read `candidate` at. `candidate` is the one key whose
    /// inline key, at the prefix length given with it, equals `key`'s: the
    /// index is its, if it is `key`. Without a candidate, the leaf's own
    /// search, compiled separately for each `call_site` (`Leaf::find`).
    #[inline(always)]
    pub(super) fn find_latched<Q, CallSite>(
        leaf: Leaf<K, V, Latched<S>>,
        key: &SearchKey<'_, K, Latched<S>, Q>,
        candidate: Option<(usize, usize)>,
        call_site: CallSite,
    ) -> Option<usize>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        match candidate {
            // SAFETY: the leaf is at the version it was read at to find the
            // candidate and its prefix length, and latched: it hasn't
            // changed since, and doesn't while held.
            Some((index, prefix_len)) => unsafe { leaf.find_candidate(key, index, prefix_len) },
            None => leaf.find(key, call_site),
        }
    }
}
