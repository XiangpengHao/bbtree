//! Whether one set holds another: `is_subset` and `is_superset`, as std's.
//!
//! Both ends settle first, by the sets' first and last elements; then, as
//! for `difference`, a lookup in `other` for each element of `self` left if
//! `other` is much larger, or otherwise a walk of both sets side by side.

#![forbid(unsafe_code)]

use core::cmp::Ordering;
use core::ops::ControlFlow;

use super::BBTreeSet;
use super::merge::{MergeSides, MergeStep};
use crate::inline_key::InlineKeyStrategy;

impl<T: Ord, S: InlineKeyStrategy<T>> BBTreeSet<T, S> {
    /// Returns `true` if the set is a subset of another, i.e., `other`
    /// contains at least all the elements in `self`.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeSet;
    ///
    /// let sup = BBTreeSet::from([1, 2, 3]);
    /// let mut set = BBTreeSet::new();
    ///
    /// assert_eq!(set.is_subset(&sup), true);
    /// set.insert(2);
    /// assert_eq!(set.is_subset(&sup), true);
    /// set.insert(4);
    /// assert_eq!(set.is_subset(&sup), false);
    /// ```
    #[must_use]
    pub fn is_subset(&self, other: &Self) -> bool {
        // As `self.difference(other).next().is_none()`, but it gives up at
        // the first element missing, and settles the ends without walking.
        if self.len() > other.len() {
            return false;
        }
        let (Some(self_min), Some(self_max)) = (self.first(), self.last()) else {
            return true;
        };
        let (Some(other_min), Some(other_max)) = (other.first(), other.last()) else {
            return false;
        };
        let mut self_iter = self.iter();
        // After these, neither of `other`'s ends is left in `self_iter`.
        match self_min.cmp(other_min) {
            Ordering::Less => return false,
            Ordering::Equal => {
                self_iter.next();
            }
            Ordering::Greater => {}
        }
        match self_max.cmp(other_max) {
            Ordering::Greater => return false,
            Ordering::Equal => {
                self_iter.next_back();
            }
            Ordering::Less => {}
        }
        if other.prefers_lookups(self_iter.len()) {
            return self_iter.all(|element| other.contains(element));
        }
        let mut other_iter = other.iter();
        other_iter.next();
        other_iter.next_back();
        // An element of `self` alone, or left once `other` ends, is missing.
        let all_found = MergeSides::new(self_iter, other_iter).try_fold_steps::<_, _, true, false>(
            (),
            |(), step| match step {
                MergeStep::Left(_) => ControlFlow::Break(()),
                MergeStep::Right(_) | MergeStep::Both(_) => ControlFlow::Continue(()),
            },
        );
        all_found.is_continue()
    }

    /// Returns `true` if the set is a superset of another, i.e., `self`
    /// contains at least all the elements in `other`.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeSet;
    ///
    /// let sub = BBTreeSet::from([1, 2]);
    /// let mut set = BBTreeSet::new();
    ///
    /// assert_eq!(set.is_superset(&sub), false);
    ///
    /// set.insert(0);
    /// set.insert(1);
    /// assert_eq!(set.is_superset(&sub), false);
    ///
    /// set.insert(2);
    /// assert_eq!(set.is_superset(&sub), true);
    /// ```
    #[must_use]
    pub fn is_superset(&self, other: &Self) -> bool {
        other.is_subset(self)
    }
}
