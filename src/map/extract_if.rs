//! Removing the entries a predicate picks: [`ExtractIf`], and `retain` on
//! it, as std's.
//!
//! The walk cuts the map's tree at the range's bounds when made
//! (`tree/cut.rs`): the entries before the range stay where they are, as
//! the start of the tree it keeps, and those after it are held aside. It
//! takes the range's entries out in order, by value, and appends those the
//! predicate passes over to the kept tree, as `collect` builds one
//! (`tree/build.rs`); each one the predicate picks is handed out as it is
//! taken, or, by `retain`, dropped. When dropped, the walk appends the
//! range's entries it has not visited, untested, and joins the entries
//! after the range back on (`tree/join.rs`). That is O(log n) beyond the
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
use core::ops::{ControlFlow, RangeBounds};

use super::BBTreeMap;
use crate::allocator::{Allocator, Global};
use crate::inline_key::{Auto, InlineKeyStrategy};
use crate::tree::build::TreeBuilder;
use crate::tree::join::DetachedTree;
use crate::tree::path::{FirstEdge, KeySide};
use crate::tree::root::Tree;
use crate::tree::walk::DyingWalk;

/// An iterator produced by calling `extract_if` on [`BBTreeMap`].
#[must_use = "iterators are lazy and do nothing unless consumed; \
              use `retain` to remove and drop elements"]
pub struct ExtractIf<'a, K, V, R, F, S: InlineKeyStrategy<K> = Auto, A: Allocator + Clone = Global>
{
    walk: ExtractionWalk<'a, K, V, R, S, A>,
    pred: F,
}

/// The walk of an `extract_if`, but for its predicate, which each step
/// takes. Every piece of the map's tree it holds frees its pages to a clone
/// of the map's allocator.
pub(crate) struct ExtractionWalk<'a, K, V, R, S: InlineKeyStrategy<K>, A: Allocator + Clone> {
    /// Empty until the walk is dropped, when it gets back the entries not
    /// handed out.
    map: &'a mut BBTreeMap<K, V, S, A>,
    /// Whether the map had a tree, which it keeps even if no entry stays, as
    /// std's map keeps an empty root after losing its entries, which decides
    /// whether `range` checks its bounds.
    had_tree: bool,
    /// The map's entries not handed out: its length less those the walk has
    /// handed out, which std's walk counts down in the map as it goes. The
    /// kept tree's builder counts those it dropped, which a panicking
    /// `InlineKey` stopped it from placing (`tree/build.rs`).
    length: usize,
    /// The entries before the range, where they were, and then those in it
    /// visited and kept, in order.
    kept: TreeBuilder<K, V, S, A>,
    /// The range's entries not kept or handed out yet.
    source: ExtractionSource<K, V, S, A>,
    /// The entries after the range, which come back as they are.
    after: DetachedTree<K, V, S, A>,
    /// The range, done with once the tree is cut at its bounds, and dropped
    /// with the walk, as std's is.
    range: R,
}

/// The range's entries an extraction has neither kept nor handed out yet,
/// in order: apart from the tree it keeps them in, so that one `extend` of
/// that tree can take a run of them.
struct ExtractionSource<K, V, S: InlineKeyStrategy<K>, A: Allocator> {
    /// The range's entries not taken out yet, from the front of their own
    /// tree, which this walk owns and frees as it goes.
    rest: DyingWalk<K, V, S, A>,
    /// The entry taken out last, while it is tested: kept if that panics.
    under_test: Option<(K, V)>,
    /// Whether the walk went past the range's last entry: it takes nothing
    /// more out, and peeks at nothing.
    finished: bool,
}

impl<K, V, S: InlineKeyStrategy<K>, A: Allocator> ExtractionSource<K, V, S, A> {
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
                let (key, value) = self.under_test.insert(entry);
                if !pick(key, value) {
                    return self.under_test.take();
                }
                let picked = self.under_test.take().expect("the entry just tested");
                if on_pick(picked).is_break() {
                    return None;
                }
            }
        })
    }
}

impl<'a, K: Ord, V, R: RangeBounds<K>, S: InlineKeyStrategy<K>, A: Allocator + Clone>
    ExtractionWalk<'a, K, V, R, S, A>
{
    /// Takes the entries out of `map` and cuts out those in `range`.
    pub(crate) fn new(map: &'a mut BBTreeMap<K, V, S, A>, range: R) -> Self {
        let length = mem::take(&mut map.length);
        let had_tree = map.tree.has_root();
        let tree = map.tree.take();
        let alloc = &map.alloc;
        let (kept, after) = (
            TreeBuilder::new(alloc.clone()),
            DetachedTree::new(tree, alloc.clone()),
        );
        let rest = DyingWalk::new(None, 0, alloc.clone());
        let mut walk = Self {
            map,
            had_tree,
            length,
            kept,
            source: ExtractionSource {
                rest,
                under_test: None,
                finished: false,
            },
            after,
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
        let alloc = &self.map.alloc;
        let tree = self.after.tree_mut();
        if let Some((start, side)) = KeySide::of_start(self.range.start_bound()) {
            let from_start = tree.split_off_at(start, side, alloc);
            let before = mem::replace(tree, from_start);
            self.kept = TreeBuilder::continuing(before, alloc.clone());
            self.kept.refit_border_siblings();
            tree.refit_border_siblings::<FirstEdge>();
        }
        let end = KeySide::of_end(self.range.end_bound());
        let cut_past_end = end.is_some();
        let range_tree = match end {
            Some((end, side)) => {
                let past_end = tree.split_off_at(end, side, alloc);
                mem::replace(tree, past_end)
            }
            None => self.after.take(),
        };
        self.source.rest = DyingWalk::new(range_tree.root_page(), usize::MAX, alloc.clone());
        if cut_past_end {
            self.after.tree_mut().refit_border_siblings::<FirstEdge>();
        }
    }
}

impl<K, V, R, S: InlineKeyStrategy<K>, A: Allocator + Clone> ExtractionWalk<'_, K, V, R, S, A> {
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
        if let Some((key, value)) = &source.under_test {
            return Some((key, value));
        }
        let mut rest = source.rest.remaining();
        rest.next().or_else(|| self.after.first_key_value())
    }
}

// The entry being tested is held by value, but never pinned: like the map,
// the walk is `Unpin` whatever `K` and `V` are, and as std's, if its range
// is; an `ExtractIf` also needs its predicate to be.
impl<K, V, R: Unpin, S: InlineKeyStrategy<K>, A: Allocator + Clone + Unpin> Unpin
    for ExtractionWalk<'_, K, V, R, S, A>
{
}

impl<K, V, R, S: InlineKeyStrategy<K>, A: Allocator + Clone> Drop
    for ExtractionWalk<'_, K, V, R, S, A>
{
    /// Gives the map back the entries not handed out, in order: appends the
    /// range's entries not visited to the kept tree and joins the entries
    /// after the range back on.
    fn drop(&mut self) {
        let source = &mut self.source;
        let rest = &mut source.rest;
        self.kept.extend(
            source
                .under_test
                .take()
                .into_iter()
                .chain(iter::from_fn(|| rest.next())),
        );
        let map = &mut *self.map;
        let kept = mem::replace(&mut self.kept, TreeBuilder::new(map.alloc.clone()));
        let dropped = kept.dropped();
        let (kept, _) = kept.finish();
        map.tree = Tree::concat(kept, self.after.take(), &map.alloc);
        map.length = self.length - dropped;
        if self.had_tree {
            map.tree.ensure_root_page(&map.alloc);
        }
    }
}

impl<K, V, R, F, S, A> Iterator for ExtractIf<'_, K, V, R, F, S, A>
where
    K: PartialOrd,
    R: RangeBounds<K>,
    F: FnMut(&K, &mut V) -> bool,
    S: InlineKeyStrategy<K>,
    A: Allocator + Clone,
{
    type Item = (K, V);

    fn next(&mut self) -> Option<(K, V)> {
        self.walk.next(&mut self.pred)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.walk.size_hint()
    }
}

impl<K, V, R, F, S, A> FusedIterator for ExtractIf<'_, K, V, R, F, S, A>
where
    K: PartialOrd,
    R: RangeBounds<K>,
    F: FnMut(&K, &mut V) -> bool,
    S: InlineKeyStrategy<K>,
    A: Allocator + Clone,
{
}

/// Prints the next entry to visit, as std's does.
impl<K: fmt::Debug, V: fmt::Debug, R, F, S: InlineKeyStrategy<K>, A: Allocator + Clone> fmt::Debug
    for ExtractIf<'_, K, V, R, F, S, A>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExtractIf")
            .field("peek", &self.walk.peek())
            .finish_non_exhaustive()
    }
}

impl<K: Ord, V, S: InlineKeyStrategy<K>, A: Allocator + Clone> BBTreeMap<K, V, S, A> {
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
    /// Unlike std's, which passes over the range in place, this walk cuts the
    /// range out of the map, and puts its kept entries back: O(log n) beyond
    /// the range's entries.
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
    pub fn extract_if<F, R>(&mut self, range: R, pred: F) -> ExtractIf<'_, K, V, R, F, S, A>
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
    pub fn retain<F>(&mut self, f: F)
    where
        F: FnMut(&K, &mut V) -> bool,
    {
        ExtractionWalk::new(self, ..).retain(f);
    }
}
