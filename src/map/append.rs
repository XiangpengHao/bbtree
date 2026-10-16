//! Moving another map's entries into a map, as std's map does: `append`
//! merges them in.
//!
//! It takes the entries of both maps by value, from the front (`IntoIter`),
//! merges them in key order and builds the tree they make into full nodes
//! (`tree/build.rs`): in O(n + m), as std's.

#![forbid(unsafe_code)]

use core::cmp::Ordering;
use core::mem;

use super::{BBTreeMap, IntoIter};
use crate::allocator::Allocator;
use crate::inline_key::InlineKeyStrategy;
use crate::tree::build::TreeBuilder;

impl<K: Ord, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> BBTreeMap<K, V, S, A> {
    /// Moves all elements from `other` into `self`, leaving `other` empty.
    ///
    /// If a key from `other` is already present in `self`, the respective
    /// value from `self` will be overwritten with the respective value from
    /// `other`. `self` keeps its key, and drops its old value, then the key
    /// of `other`, as std's map does.
    ///
    /// If `K`'s `Ord` panics, `self` keeps its entries and those of `other`
    /// merged so far, and the rest of `other` is dropped, as with std's map.
    ///
    /// The merged nodes come from `self`'s allocator; `other` keeps its own.
    /// If `self` is empty, the two maps swap, allocators too, as std's do.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let mut a = BBTreeMap::new();
    /// a.insert(1, "a");
    /// a.insert(2, "b");
    /// a.insert(3, "c"); // Note: Key (3) also present in b.
    ///
    /// let mut b = BBTreeMap::new();
    /// b.insert(3, "d"); // Note: Key (3) also present in a.
    /// b.insert(4, "e");
    /// b.insert(5, "f");
    ///
    /// a.append(&mut b);
    ///
    /// assert_eq!(a.len(), 5);
    /// assert_eq!(b.len(), 0);
    ///
    /// assert_eq!(a[&1], "a");
    /// assert_eq!(a[&2], "b");
    /// assert_eq!(a[&3], "d"); // Note: "c" has been overwritten.
    /// assert_eq!(a[&4], "e");
    /// assert_eq!(a[&5], "f");
    /// ```
    pub fn append(&mut self, other: &mut Self) {
        if other.is_empty() {
            return;
        }
        if self.is_empty() {
            mem::swap(self, other);
            return;
        }
        let (our_alloc, their_alloc) = (self.alloc.clone(), other.alloc.clone());
        let merged = TreeBuilder::new(self.alloc.clone());
        let ours = mem::replace(self, Self::empty(our_alloc)).into_iter();
        let theirs = mem::replace(other, Self::empty(their_alloc)).into_iter();
        let mut merge = Merge {
            target: self,
            entries: MergedEntries {
                ours,
                theirs,
                took_from_theirs: false,
                first_duplicate_key: None,
            },
            merged,
        };
        merge.merged.extend(&mut merge.entries);
    }
}

/// `append` under way: the entries of both maps, merged in key order into a
/// new tree, which becomes the target's when the merge is dropped, finished
/// or unwinding from a panicking `Ord`. The target then also gets the
/// entries of its own not merged yet; those of the other map not merged yet
/// are dropped.
struct Merge<'a, K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> {
    /// Empty, with its allocator, until the merge is dropped.
    target: &'a mut BBTreeMap<K, V, S, A>,
    entries: MergedEntries<K, V, S, A>,
    /// On pages from the target's allocator.
    merged: TreeBuilder<K, V, S, A>,
}

/// The entries of both maps in key order, each key once: where both maps
/// have a key, the target's key with the other map's value.
struct MergedEntries<K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> {
    ours: IntoIter<K, V, S, A>,
    theirs: IntoIter<K, V, S, A>,
    /// Whether an entry of the other map was merged yet.
    took_from_theirs: bool,
    /// The key of the other map's first entry, if the target had that key:
    /// std's `append` holds that entry in a variable until it returns, so it
    /// drops the key last, where it drops the keys of later duplicates as it
    /// meets them.
    first_duplicate_key: Option<K>,
}

impl<K: Ord, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> Iterator
    for MergedEntries<K, V, S, A>
{
    type Item = (K, V);

    fn next(&mut self) -> Option<(K, V)> {
        let order = match (self.ours.peek(), self.theirs.peek()) {
            (Some((our_key, _)), Some((their_key, _))) => our_key.cmp(their_key),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => return None,
        };
        let entry = match order {
            Ordering::Less => self.ours.next(),
            Ordering::Greater => {
                self.took_from_theirs = true;
                self.theirs.next()
            }
            Ordering::Equal => self.take_duplicate(),
        };
        Some(entry.expect("the side picked has an entry"))
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> MergedEntries<K, V, S, A> {
    /// The first entries of both maps, whose keys are equal, as one: the
    /// target's key and the other map's value. The target's old value is
    /// dropped, then the other map's key, unless it is the first of that map
    /// merged.
    fn take_duplicate(&mut self) -> Option<(K, V)> {
        let (key, old_value) = self.ours.next()?;
        let (their_key, value) = self.theirs.next()?;
        drop(old_value);
        if mem::replace(&mut self.took_from_theirs, true) {
            drop(their_key);
        } else {
            self.first_duplicate_key = Some(their_key);
        }
        Some((key, value))
    }
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> Drop for Merge<'_, K, V, S, A> {
    fn drop(&mut self) {
        self.merged.extend(&mut self.entries.ours);
        let merged = mem::replace(
            &mut self.merged,
            TreeBuilder::new(self.target.alloc.clone()),
        );
        // The target has no tree to drop: its entries were in `ours`.
        (self.target.tree, self.target.length) = merged.finish();
        // Before the other map's entries not merged, as std's unwinding
        // drops them.
        drop(self.entries.first_duplicate_key.take());
    }
}
