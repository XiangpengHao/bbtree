//! Removing the entries a predicate picks: [`ExtractIf`], and `retain` on
//! it, as std's.
//!
//! The walk cuts the map's tree at the range's bounds when made
//! (`split_off.rs`): the entries before the range stay where they are, as
//! the start of the tree it keeps, and those after it are held aside. It
//! takes the range's entries out in order, by value, and appends those the
//! predicate passes over to the kept tree, as `collect` builds one
//! (`node/build.rs`); each one the predicate picks is handed out as it is
//! taken, or, by `retain`, dropped. When dropped, the walk appends the
//! range's entries it has not visited, untested, and joins the entries
//! after the range back on (`node/join.rs`). That is O(log n) beyond the
//! range's entries, where std's walk passes over them in place.
//!
//! The only `Ord` calls are the two searches for the bounds, each before
//! anything changes. The range's entries are visited in order and never
//! compared, nor peeked at, which would copy both ends of the walk. The
//! entries kept between two that are handed out go into the kept tree in
//! one `extend`, which finds its rightmost leaf once rather than once per
//! entry; `retain` keeps them all in one.
//!
//! The map stays empty while the walk lasts, its pieces held by owners that
//! take them apart if dropped. When the walk is dropped, finished or not, or
//! unwinds from a panicking predicate or `Ord`, the entries not handed out
//! go back into the map, the one being tested included, as std's walk
//! leaves them in place.
//!
//! The walk itself, [`ExtractionWalk`], takes the predicate at each step,
//! as std's `ExtractIfInner` does, so that the set's `ExtractIf`, whose
//! predicate sees elements alone, shares it.

#![forbid(unsafe_code)]

use core::fmt;
use core::iter::{self, FusedIterator};
use core::mem;
use core::ops::{Bound, ControlFlow, RangeBounds, RangeFull};

use super::BBTreeMap;
use super::into_iter::Deallocate;
use crate::inline_key::{Auto, InlineKeyStrategy};
use crate::node::build::TreeBuilder;
use crate::node::handle::Subtree;
use crate::node::page::{FreshPage, PageByKind};
use crate::node::path::{FirstEdge, KeySide, Path, PathMut};
use crate::node::walk::DyingWalk;

#[cfg(test)]
#[path = "concat_tests.rs"]
mod tests;

/// An iterator produced by calling `extract_if` on [`BBTreeMap`].
#[must_use = "iterators are lazy and do nothing unless consumed; \
              use `retain` to remove and drop elements"]
pub struct ExtractIf<'a, K, V, R, F, S: InlineKeyStrategy<K> = Auto> {
    walk: ExtractionWalk<'a, K, V, R, S>,
    pred: F,
}

/// The walk of an `extract_if`, but for its predicate, which each step
/// takes.
pub(crate) struct ExtractionWalk<'a, K, V, R, S: InlineKeyStrategy<K>> {
    /// Empty until the walk is dropped, when it gets back the entries not
    /// handed out.
    map: &'a mut BBTreeMap<K, V, S>,
    /// Whether the map had a tree, which it keeps even if no entry stays.
    had_tree: bool,
    /// The map's entries not handed out: its length less those the walk has
    /// handed out, which std's walk counts down in the map as it goes.
    length: usize,
    /// The entries before the range, where they were, and then those in it
    /// visited and kept, in order.
    kept: TreeBuilder<K, V, S>,
    /// The range's entries not kept or handed out yet.
    source: ExtractionSource<K, V, S>,
    /// The entries after the range, which come back as they are.
    after: DetachedTree<K, V, S>,
    /// The range, done with once the tree is cut at its bounds, and dropped
    /// with the walk, as std's is.
    range: R,
}

/// The range's entries an extraction has neither kept nor handed out yet,
/// in order: apart from the tree it keeps them in, so that one `extend` of
/// that tree can take a run of them.
struct ExtractionSource<K, V, S: InlineKeyStrategy<K>> {
    /// The range's entries not taken out yet, from the front of their own
    /// tree, which this walk owns and frees as it goes.
    rest: DyingWalk<K, V, S, Deallocate<K, V, S>>,
    /// The entry taken out last, while it is tested: kept if that panics.
    testing: Option<(K, V)>,
    /// Whether the walk went past the range's last entry: it takes nothing
    /// more out, and peeks at nothing.
    finished: bool,
}

impl<K, V, S: InlineKeyStrategy<K>> ExtractionSource<K, V, S> {
    /// The entries to keep, in order, up to one that `pick` picks, which
    /// goes to `on_pick`, and on past it only if that says to continue: the
    /// range's entries that `pick` passes over.
    fn kept_until<'s>(
        &'s mut self,
        mut pick: impl FnMut(&K, &mut V) -> bool + 's,
        mut on_pick: impl FnMut((K, V)) -> ControlFlow<()> + 's,
    ) -> impl Iterator<Item = (K, V)> + 's {
        iter::from_fn(move || {
            loop {
                if self.finished {
                    return None;
                }
                let Some(entry) = self.rest.next() else {
                    self.finished = true;
                    return None;
                };
                let (key, value) = self.testing.insert(entry);
                if !pick(key, value) {
                    return self.testing.take();
                }
                let picked = self.testing.take().expect("the entry just tested");
                if on_pick(picked).is_break() {
                    return None;
                }
            }
        })
    }
}

/// A tree held apart from any map, as the entries after the range are while
/// the walk lasts: taken apart if dropped.
struct DetachedTree<K, V, S: InlineKeyStrategy<K>>(Option<Subtree<K, V, S>>);

impl<K, V, S: InlineKeyStrategy<K>> DetachedTree<K, V, S> {
    /// The first entry, for `Debug` to print where std's walk would.
    fn first_entry(&self) -> Option<(&K, &V)> {
        let mut path = Path::detached();
        path.attach(self.0.as_ref()?.borrow());
        if path.descend_to_first_key() {
            Some(path.kv().into_key_value())
        } else {
            None
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Drop for DetachedTree<K, V, S> {
    fn drop(&mut self) {
        if let Some(tree) = self.0.take() {
            tree.destroy(PageByKind::deallocate);
        }
    }
}

impl<'a, K: Ord, V, R: RangeBounds<K>, S: InlineKeyStrategy<K>> ExtractionWalk<'a, K, V, R, S> {
    /// Takes the entries out of `map` and cuts out those in `range`.
    pub(crate) fn new(map: &'a mut BBTreeMap<K, V, S>, range: R) -> Self {
        let length = map.length;
        let had_tree = map.root.is_some();
        let tree = mem::replace(map, BBTreeMap::empty()).root.take();
        let mut walk = Self {
            map,
            had_tree,
            length,
            kept: TreeBuilder::new(),
            source: ExtractionSource {
                rest: DyingWalk::new(None, 0, PageByKind::deallocate),
                testing: None,
                finished: false,
            },
            after: DetachedTree(tree),
            range,
        };
        if length > 0 {
            walk.cut_out_range();
        }
        walk
    }

    /// Cuts the tree, which `after` holds, at the range's bounds: what lies
    /// before the start becomes the start of the kept tree, what lies past
    /// the end stays in `after`, and what lies between goes to the source.
    /// Each piece is held where a panic can't lose it before the nodes
    /// beside the cuts fit their prefixes, which reads keys.
    fn cut_out_range(&mut self) {
        let tree = self
            .after
            .0
            .as_mut()
            .expect("a map with entries has a tree");
        let start = match self.range.start_bound() {
            Bound::Included(start) => Some((start, KeySide::Before)),
            Bound::Excluded(start) => Some((start, KeySide::After)),
            Bound::Unbounded => None,
        };
        if let Some((start, side)) = start {
            let from_start = tree.split_off_at(start, side);
            let before = mem::replace(tree, from_start);
            self.kept = TreeBuilder::continuing(before);
            self.kept.fit_border_siblings();
            tree.fit_border_siblings::<FirstEdge>();
        }
        let end = match self.range.end_bound() {
            Bound::Included(end) => Some((end, KeySide::After)),
            Bound::Excluded(end) => Some((end, KeySide::Before)),
            Bound::Unbounded => None,
        };
        let range = match end {
            Some((end, side)) => {
                let past_end = tree.split_off_at(end, side);
                let range = mem::replace(tree, past_end);
                tree.fit_border_siblings::<FirstEdge>();
                range
            }
            None => self.after.0.take().expect("the tree, cut or not"),
        };
        self.source.rest = DyingWalk::new(Some(range), usize::MAX, PageByKind::deallocate);
    }
}

impl<K, V, R, S: InlineKeyStrategy<K>> ExtractionWalk<'_, K, V, R, S> {
    /// Hands out the next entry in the range that `pick` picks, keeping
    /// those it passes over.
    pub(crate) fn next(&mut self, pick: &mut impl FnMut(&K, &mut V) -> bool) -> Option<(K, V)> {
        let mut picked = None;
        self.kept.extend(self.source.kept_until(pick, |entry| {
            picked = Some(entry);
            ControlFlow::Break(())
        }));
        if picked.is_some() {
            self.length -= 1;
        }
        picked
    }

    /// Drops every entry left in the range that `keep` passes over, as it
    /// comes, and keeps the others: `retain`, in one `extend`.
    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&K, &mut V) -> bool) {
        let length = &mut self.length;
        self.kept.extend(self.source.kept_until(
            |key, value| !keep(key, value),
            |entry| {
                *length -= 1;
                drop(entry);
                ControlFlow::Continue(())
            },
        ));
    }

    pub(crate) fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(self.length))
    }

    /// The next entry to visit, as std's `Debug` prints it: past the
    /// range's last entry, the first one after the range, until the walk
    /// has found it is past the range.
    pub(crate) fn peek(&self) -> Option<(&K, &V)> {
        let source = &self.source;
        if source.finished {
            return None;
        }
        if let Some((key, value)) = &source.testing {
            return Some((key, value));
        }
        let mut rest = source.rest.remaining();
        rest.next().or_else(|| self.after.first_entry())
    }
}

// The entry being tested is held by value, but never pinned: like the map,
// the walk is `Unpin` whatever `K` and `V` are, and as std's, if its range
// is; an `ExtractIf` also needs its predicate to be.
impl<K, V, R: Unpin, S: InlineKeyStrategy<K>> Unpin for ExtractionWalk<'_, K, V, R, S> {}

impl<K, V, R, S: InlineKeyStrategy<K>> Drop for ExtractionWalk<'_, K, V, R, S> {
    /// Gives the map back the entries not handed out, in order: appends the
    /// range's entries not visited to the kept tree and joins the entries
    /// after the range back on.
    fn drop(&mut self) {
        let source = &mut self.source;
        let rest = &mut source.rest;
        self.kept.extend(
            source
                .testing
                .take()
                .into_iter()
                .chain(iter::from_fn(|| rest.next())),
        );
        let (kept, _) = mem::replace(&mut self.kept, TreeBuilder::new()).finish();
        self.map.root = Subtree::concat(kept, self.after.0.take());
        self.map.length = self.length;
        if self.had_tree {
            self.map.keep_a_tree();
        }
    }
}

impl<K, V, S: InlineKeyStrategy<K>> Subtree<K, V, S> {
    /// The tree of `left`'s entries and then `right`'s, every one of whose
    /// keys must be greater, joined around `left`'s last entry
    /// (`node/join.rs`); or the one with entries; or `None` if neither has
    /// any. Keys out of order make a tree whose searches give wrong answers,
    /// but that is memory-safe.
    fn concat(left: Option<Self>, right: Option<Self>) -> Option<Self> {
        let (mut left, mut right) = match (Self::with_entries(left), Self::with_entries(right)) {
            (Some(left), Some(right)) => (left, right),
            (left, right) => return left.or(right),
        };
        let mut left_length = 1;
        let separator = left
            .pop_last(PageByKind::deallocate, &mut left_length)
            .expect("a tree with entries has a last one");
        let Some(mut left) = Self::with_entries(Some(left)) else {
            let (key, value) = separator;
            let mut right_length = 0;
            {
                let mut path = PathMut::new(&mut right);
                path.start_at_root();
                path.descend_to_leaf::<FirstEdge>();
                path.insert(
                    key,
                    value,
                    FreshPage::allocate,
                    FreshPage::allocate,
                    &mut right_length,
                );
            }
            return Some(right);
        };
        left.join(separator, right);
        Some(left)
    }

    /// `tree`, if it has entries; otherwise it is taken apart, and `None`.
    fn with_entries(tree: Option<Self>) -> Option<Self> {
        let tree = tree?;
        if tree.borrow().len() > 0 {
            return Some(tree);
        }
        tree.destroy(PageByKind::deallocate);
        None
    }
}

impl<K, V, R, F, S> Iterator for ExtractIf<'_, K, V, R, F, S>
where
    K: PartialOrd,
    R: RangeBounds<K>,
    F: FnMut(&K, &mut V) -> bool,
    S: InlineKeyStrategy<K>,
{
    type Item = (K, V);

    fn next(&mut self) -> Option<(K, V)> {
        self.walk.next(&mut self.pred)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.walk.size_hint()
    }
}

impl<K, V, R, F, S> FusedIterator for ExtractIf<'_, K, V, R, F, S>
where
    K: PartialOrd,
    R: RangeBounds<K>,
    F: FnMut(&K, &mut V) -> bool,
    S: InlineKeyStrategy<K>,
{
}

/// Prints the next entry to visit, as std's does.
impl<K: fmt::Debug, V: fmt::Debug, R, F, S: InlineKeyStrategy<K>> fmt::Debug
    for ExtractIf<'_, K, V, R, F, S>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExtractIf")
            .field("peek", &self.walk.peek())
            .finish_non_exhaustive()
    }
}

impl<K: Ord, V, S: InlineKeyStrategy<K>> BBTreeMap<K, V, S> {
    /// Creates an iterator that visits elements (key-value pairs) in the
    /// specified range in ascending key order and uses a closure to
    /// determine if an element should be removed.
    ///
    /// If the closure returns `true`, the element is removed from the map
    /// and yielded. If the closure returns `false`, or panics, the element
    /// remains in the map and will not be yielded.
    ///
    /// The iterator also lets you mutate the value of each element in the
    /// closure, regardless of whether you choose to keep or remove it.
    ///
    /// If the returned `ExtractIf` is not exhausted, e.g. because it is
    /// dropped without iterating or the iteration short-circuits, then the
    /// remaining elements will be retained. Use [`retain`] with a negated
    /// predicate if you do not need the returned iterator.
    ///
    /// Unlike std's, this walk takes every entry out of the map and puts the
    /// kept ones back, wherever the range is: O(n).
    ///
    /// [`retain`]: BBTreeMap::retain
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// // Splitting a map into even and odd keys, reusing the original map:
    /// let mut map: BBTreeMap<i32, i32> = (0..8).map(|x| (x, x)).collect();
    /// let evens: BBTreeMap<_, _> = map.extract_if(.., |k, _v| k % 2 == 0).collect();
    /// let odds = map;
    /// assert_eq!(evens.keys().copied().collect::<Vec<_>>(), [0, 2, 4, 6]);
    /// assert_eq!(odds.keys().copied().collect::<Vec<_>>(), [1, 3, 5, 7]);
    ///
    /// // Splitting a map into low and high halves, reusing the original map:
    /// let mut map: BBTreeMap<i32, i32> = (0..8).map(|x| (x, x)).collect();
    /// let low: BBTreeMap<_, _> = map.extract_if(0..4, |_k, _v| true).collect();
    /// let high = map;
    /// assert_eq!(low.keys().copied().collect::<Vec<_>>(), [0, 1, 2, 3]);
    /// assert_eq!(high.keys().copied().collect::<Vec<_>>(), [4, 5, 6, 7]);
    /// ```
    pub fn extract_if<F, R>(&mut self, range: R, pred: F) -> ExtractIf<'_, K, V, R, F, S>
    where
        R: RangeBounds<K>,
        F: FnMut(&K, &mut V) -> bool,
    {
        ExtractIf {
            walk: ExtractionWalk::new(self, range),
            pred,
        }
    }

    /// Retains only the elements specified by the predicate.
    ///
    /// In other words, remove all pairs `(k, v)` for which `f(&k, &mut v)`
    /// returns `false`. The elements are visited in ascending key order.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::BBTreeMap;
    ///
    /// let mut map: BBTreeMap<i32, i32> = (0..8).map(|x| (x, x * 10)).collect();
    /// // Keep only the elements with even-numbered keys.
    /// map.retain(|&k, _| k % 2 == 0);
    /// assert!(map.into_iter().eq(vec![(0, 0), (2, 20), (4, 40), (6, 60)]));
    /// ```
    pub fn retain<F>(&mut self, mut f: F)
    where
        F: FnMut(&K, &mut V) -> bool,
    {
        ExtractionWalk::<_, _, RangeFull, _>::new(self, ..).retain(|key, value| f(key, value));
    }
}
