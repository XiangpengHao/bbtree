//! Moving entries between maps in bulk, as std's map does: `append` merges
//! another map in, and `split_off` cuts off the entries from a key on.
//!
//! For now both take entries out of the trees one by one, from an end, and
//! build the trees they make into full nodes (`node/build.rs`): `append` in
//! O(n + m), as std's, and `split_off` in the size of the smaller part,
//! where std's cuts the tree along a path.

#![forbid(unsafe_code)]

use alloc::vec::Vec;
use core::borrow::Borrow;
use core::cmp::Ordering;
use core::iter;
use core::mem;
use core::ops::Bound;

use super::BBTreeMap;
use crate::inline_key::InlineKeyStrategy;
use crate::node::build::TreeBuilder;

impl<K: Ord, V, S: InlineKeyStrategy<K>> BBTreeMap<K, V, S> {
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
        let ours = mem::replace(self, Self::empty());
        let theirs = mem::replace(other, Self::empty());
        let mut merge = Merge {
            target: self,
            ours,
            theirs,
            merged: TreeBuilder::new(),
            took_from_theirs: false,
            first_duplicate_key: None,
        };
        merge.run();
    }

    /// Splits the collection into two at the given key. Returns everything
    /// after the given key, including the key.
    ///
    /// Every `Ord` call happens first, before any entry moves. Both maps keep
    /// a tree, even an empty one, as std's do.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let mut a = BBTreeMap::new();
    /// a.insert(1, "a");
    /// a.insert(2, "b");
    /// a.insert(3, "c");
    /// a.insert(17, "d");
    /// a.insert(41, "e");
    ///
    /// let b = a.split_off(&3);
    ///
    /// assert_eq!(a.len(), 2);
    /// assert_eq!(b.len(), 3);
    ///
    /// assert_eq!(a[&1], "a");
    /// assert_eq!(a[&2], "b");
    ///
    /// assert_eq!(b[&3], "c");
    /// assert_eq!(b[&17], "d");
    /// assert_eq!(b[&41], "e");
    /// ```
    pub fn split_off<Q>(&mut self, key: &Q) -> Self
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        if self.is_empty() {
            return Self::empty();
        }
        let upper_len = self
            .range::<Q, _>((Bound::Included(key), Bound::Unbounded))
            .count();
        let lower_len = self.len() - upper_len;
        let mut upper = if lower_len < upper_len {
            // The lower part is smaller: it moves to a new tree, which
            // becomes this map's, and the old tree, with the upper part
            // left, is handed back.
            let mut lower = TreeBuilder::new();
            lower.extend(iter::from_fn(|| self.take_first()).take(lower_len));
            mem::replace(self, Self::from_builder(lower))
        } else {
            // The upper part moves, taken from the back, so in descending
            // order, which the builder takes reversed.
            let mut entries: Vec<(K, V)> =
                iter::from_fn(|| self.take_last()).take(upper_len).collect();
            entries.reverse();
            let mut upper = TreeBuilder::new();
            upper.extend(entries);
            Self::from_builder(upper)
        };
        self.keep_a_tree();
        upper.keep_a_tree();
        upper
    }
}

/// `append` under way: the entries of both maps, merged in key order into a
/// new tree, which becomes the target's when the merge is dropped, finished
/// or unwinding from a panicking `Ord`. The target then also gets the
/// entries of its own not merged yet; those of the other map not merged yet
/// are dropped.
struct Merge<'a, K, V, S: InlineKeyStrategy<K>> {
    target: &'a mut BBTreeMap<K, V, S>,
    ours: BBTreeMap<K, V, S>,
    theirs: BBTreeMap<K, V, S>,
    merged: TreeBuilder<K, V, S>,
    /// Whether an entry of the other map was merged yet.
    took_from_theirs: bool,
    /// The key of the other map's first entry, if the target had that key:
    /// std's `append` holds that entry in a variable until it returns, so it
    /// drops the key last, where it drops the keys of later duplicates as it
    /// meets them.
    first_duplicate_key: Option<K>,
}

impl<K: Ord, V, S: InlineKeyStrategy<K>> Merge<'_, K, V, S> {
    fn run(&mut self) {
        loop {
            let order = match (self.ours.iter().next(), self.theirs.iter().next()) {
                (Some((our_key, _)), Some((their_key, _))) => our_key.cmp(their_key),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => return,
            };
            let entry = match order {
                Ordering::Less => self.ours.take_first(),
                Ordering::Greater => {
                    self.took_from_theirs = true;
                    self.theirs.take_first()
                }
                Ordering::Equal => self.take_duplicate(),
            };
            let (key, value) = entry.expect("the side picked has an entry");
            self.merged.push(key, value);
        }
    }

    /// The first entries of both maps, whose keys are equal, as one: the
    /// target's key and the other map's value. The target's old value is
    /// dropped, then the other map's key, unless it is the first of that map
    /// merged.
    fn take_duplicate(&mut self) -> Option<(K, V)> {
        let (key, old_value) = self.ours.take_first()?;
        let (their_key, value) = self.theirs.take_first()?;
        drop(old_value);
        if mem::replace(&mut self.took_from_theirs, true) {
            drop(their_key);
        } else {
            self.first_duplicate_key = Some(their_key);
        }
        Some((key, value))
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Drop for Merge<'_, K, V, S> {
    fn drop(&mut self) {
        let ours = &mut self.ours;
        self.merged.extend(iter::from_fn(|| ours.take_first()));
        let merged = mem::replace(&mut self.merged, TreeBuilder::new());
        *self.target = BBTreeMap::from_builder(merged);
        // Before the other map's entries not merged, as std's unwinding
        // drops them.
        drop(self.first_duplicate_key.take());
    }
}
