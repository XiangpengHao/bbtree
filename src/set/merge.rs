//! Walking two sets side by side in order, the core of every operation on
//! two sets that goes through both: [`SetCursor`], one set's elements, a
//! leaf's run of keys at a time, and [`MergeSides`], two of them merged.
//!
//! A cursor takes its set's keys from the walk a run at a time
//! (`tree/walk.rs`): all the keys left in a leaf, which the walk moves past
//! at once, or a separator alone. Stepping through a run touches neither the
//! walk nor its count. A merge's fold keeps both runs in locals, which live
//! in registers, and touches each walk once per run; `next` keeps them in the
//! iterator and does as little per element.
//!
//! The iterators' `next` is always inlined: called out of line, once per
//! element, it reaches the cursors through a pointer, and each step waits on
//! the last one's stores to them; inlined into the caller's loop, they stay
//! in its frame, which the CPU forwards from cheaply. Measured on a union of
//! two 10k sets: 162k cycles out of line, 105k inlined, std's 108k.
//!
//! The iterators built on cursors print what std's print, rebuilding std's
//! iterators from what is left and from which element std's would hold,
//! taken out of its walk ([`HeldSide`]).

#![forbid(unsafe_code)]

use core::cmp::Ordering;
use core::convert::Infallible;
use core::fmt;
use core::ops::ControlFlow;

use super::Iter;
use crate::inline_key::InlineKeyStrategy;
use crate::tree::node::leaf::KeyRun;

/// A set's elements in order, a leaf's run of keys at a time: the run being
/// visited, and the walk past it.
pub(super) struct SetCursor<'a, T, S> {
    iter: Iter<'a, T, S>,
    run: KeyRun<'a, T>,
}

impl<T, S> Clone for SetCursor<'_, T, S> {
    fn clone(&self) -> Self {
        Self {
            iter: self.iter.clone(),
            run: self.run,
        }
    }
}

impl<'a, T, S: InlineKeyStrategy<T>> SetCursor<'a, T, S> {
    pub(super) fn new(iter: Iter<'a, T, S>) -> Self {
        Self {
            iter,
            run: KeyRun::EMPTY,
        }
    }

    /// The next element, staying before it.
    #[inline]
    pub(super) fn peek(&mut self) -> Option<&'a T> {
        match self.run.peek() {
            Some(element) => Some(element),
            None => self.next_run(),
        }
    }

    /// Takes the next run from the walk, once the current one is done, and
    /// peeks at its first element.
    #[inline(never)]
    fn next_run(&mut self) -> Option<&'a T> {
        self.run = self.iter.next_key_run()?;
        self.run.peek()
    }

    /// Moves past the element [`Self::peek`] returned.
    #[inline]
    pub(super) fn advance(&mut self) {
        self.run.advance();
    }

    /// The next element, moving past it.
    #[inline]
    pub(super) fn next(&mut self) -> Option<&'a T> {
        let element = self.peek()?;
        self.advance();
        Some(element)
    }

    /// How many elements are left.
    pub(super) fn len(&self) -> usize {
        self.run.len() + self.iter.len()
    }

    /// The elements left, for printing.
    fn remaining(&self) -> impl Iterator<Item = &'a T> + Clone {
        self.run.chain(self.iter.clone())
    }

    /// The next element, without moving: for printing one std's walk holds
    /// taken out.
    pub(super) fn first_remaining(&self) -> Option<&'a T> {
        self.remaining().next()
    }

    /// What is left, printed as std's `Iter` over it is: `Iter([1, 2])`;
    /// without its first element if `first_held`, as std's walk holds that
    /// one taken out.
    pub(super) fn printed_as_iter(&self, first_held: bool) -> PrintedAsIter<'_, 'a, T, S> {
        PrintedAsIter {
            cursor: self,
            first_held,
        }
    }
}

/// A cursor's elements left, which print as std's `Iter` over them does.
pub(super) struct PrintedAsIter<'c, 'a, T, S> {
    cursor: &'c SetCursor<'a, T, S>,
    first_held: bool,
}

impl<T: fmt::Debug, S: InlineKeyStrategy<T>> fmt::Debug for PrintedAsIter<'_, '_, T, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let elements = self.cursor.remaining().skip(usize::from(self.first_held));
        f.debug_tuple("Iter").field(&PrintedList(elements)).finish()
    }
}

/// The elements of an iterator, printed as a list.
struct PrintedList<I>(I);

impl<I: Iterator + Clone> fmt::Debug for PrintedList<I>
where
    I::Item: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.0.clone()).finish()
    }
}

/// What a merge meets at each step: an element of the left set alone, of
/// the right one alone, or one of both, as the left set holds it.
pub(super) enum MergeStep<'a, T> {
    Left(&'a T),
    Right(&'a T),
    Both(&'a T),
}

impl<'a, T> MergeStep<'a, T> {
    /// The step's element, the left set's where both hold one.
    pub(super) fn element(self) -> &'a T {
        match self {
            Self::Left(element) | Self::Right(element) | Self::Both(element) => element,
        }
    }
}

/// Which side's element std's merge (`MergeIterInner`) holds, taken out of
/// its iterator: after a step that compared two elements and handed out the
/// other side's, that side's; otherwise neither. For printing as std's does.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum HeldSide {
    Neither,
    Left,
    Right,
}

/// The element std's merge holds, printed as std's `Peeked` is.
#[derive(Debug)]
#[expect(dead_code, reason = "read by `Debug` alone, printing as std's")]
enum Peeked<'a, T> {
    A(&'a T),
    B(&'a T),
}

/// Two sets' cursors, merged in order.
pub(super) struct MergeSides<'a, T, S> {
    pub(super) left: SetCursor<'a, T, S>,
    pub(super) right: SetCursor<'a, T, S>,
}

impl<T, S> Clone for MergeSides<'_, T, S> {
    fn clone(&self) -> Self {
        Self {
            left: self.left.clone(),
            right: self.right.clone(),
        }
    }
}

impl<'a, T: Ord, S: InlineKeyStrategy<T>> MergeSides<'a, T, S> {
    pub(super) fn new(left: Iter<'a, T, S>, right: Iter<'a, T, S>) -> Self {
        Self {
            left: SetCursor::new(left),
            right: SetCursor::new(right),
        }
    }

    /// The next step of the merge, moving past it, and which side std's
    /// merge would hold an element of then; `None` once both sides are done.
    #[inline]
    pub(super) fn next_step(&mut self) -> Option<(MergeStep<'a, T>, HeldSide)> {
        match (self.left.peek(), self.right.peek()) {
            (Some(left), Some(right)) => Some(match left.cmp(right) {
                Ordering::Less => {
                    self.left.advance();
                    (MergeStep::Left(left), HeldSide::Right)
                }
                Ordering::Greater => {
                    self.right.advance();
                    (MergeStep::Right(right), HeldSide::Left)
                }
                Ordering::Equal => {
                    self.left.advance();
                    self.right.advance();
                    (MergeStep::Both(left), HeldSide::Neither)
                }
            }),
            (Some(left), None) => {
                self.left.advance();
                Some((MergeStep::Left(left), HeldSide::Neither))
            }
            (None, Some(right)) => {
                self.right.advance();
                Some((MergeStep::Right(right), HeldSide::Neither))
            }
            (None, None) => None,
        }
    }

    /// Folds the merge's steps through `on`, which may stop it, with both
    /// sides' runs in locals. Once one side is done, the other's elements
    /// left go through `on` if `LEFT_TAIL` or `RIGHT_TAIL` says so for that
    /// side; otherwise the fold stops there.
    pub(super) fn try_fold_steps<A, R, const LEFT_TAIL: bool, const RIGHT_TAIL: bool>(
        self,
        mut accumulator: A,
        mut on: impl FnMut(A, MergeStep<'a, T>) -> ControlFlow<R, A>,
    ) -> ControlFlow<R, A> {
        let Self { left, right } = self;
        let (mut left_iter, mut left_run) = (left.iter, left.run);
        let (mut right_iter, mut right_run) = (right.iter, right.run);
        loop {
            let Some(left) = left_run.peek() else {
                match left_iter.next_key_run() {
                    Some(run) => {
                        left_run = run;
                        continue;
                    }
                    None if RIGHT_TAIL => {
                        return Self::fold_tail(
                            accumulator,
                            right_run,
                            right_iter,
                            |acc, right| on(acc, MergeStep::Right(right)),
                        );
                    }
                    None => return ControlFlow::Continue(accumulator),
                }
            };
            let Some(right) = right_run.peek() else {
                match right_iter.next_key_run() {
                    Some(run) => {
                        right_run = run;
                        continue;
                    }
                    None if LEFT_TAIL => {
                        return Self::fold_tail(accumulator, left_run, left_iter, |acc, left| {
                            on(acc, MergeStep::Left(left))
                        });
                    }
                    None => return ControlFlow::Continue(accumulator),
                }
            };
            let step = match left.cmp(right) {
                Ordering::Less => {
                    left_run.advance();
                    MergeStep::Left(left)
                }
                Ordering::Greater => {
                    right_run.advance();
                    MergeStep::Right(right)
                }
                Ordering::Equal => {
                    left_run.advance();
                    right_run.advance();
                    MergeStep::Both(left)
                }
            };
            accumulator = on(accumulator, step)?;
        }
    }

    /// [`Self::try_fold_steps`], for a fold that never stops early.
    pub(super) fn fold_steps<A, const LEFT_TAIL: bool, const RIGHT_TAIL: bool>(
        self,
        accumulator: A,
        mut on: impl FnMut(A, MergeStep<'a, T>) -> A,
    ) -> A {
        let ControlFlow::Continue(accumulator) = self
            .try_fold_steps::<A, Infallible, LEFT_TAIL, RIGHT_TAIL>(
                accumulator,
                |accumulator, step| ControlFlow::Continue(on(accumulator, step)),
            );
        accumulator
    }

    /// Folds one side's elements left, its run and then its walk's runs.
    fn fold_tail<A, R>(
        mut accumulator: A,
        run: KeyRun<'a, T>,
        mut iter: Iter<'a, T, S>,
        mut on: impl FnMut(A, &'a T) -> ControlFlow<R, A>,
    ) -> ControlFlow<R, A> {
        for element in run {
            accumulator = on(accumulator, element)?;
        }
        while let Some(run) = iter.next_key_run() {
            for element in run {
                accumulator = on(accumulator, element)?;
            }
        }
        ControlFlow::Continue(accumulator)
    }

    /// How many elements each side has left.
    pub(super) fn lens(&self) -> (usize, usize) {
        (self.left.len(), self.right.len())
    }

    /// The merge, printed as std's `MergeIterInner` is after the same steps,
    /// holding the element of `held`'s side taken out of its iterator.
    pub(super) fn printed_as_merge_iter(&self, held: HeldSide) -> PrintedAsMergeIter<'_, 'a, T, S> {
        PrintedAsMergeIter { sides: self, held }
    }
}

/// A merge's sides, which print as std's `MergeIterInner` over them does.
pub(super) struct PrintedAsMergeIter<'m, 'a, T, S> {
    sides: &'m MergeSides<'a, T, S>,
    held: HeldSide,
}

impl<T: fmt::Debug, S: InlineKeyStrategy<T>> fmt::Debug for PrintedAsMergeIter<'_, '_, T, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { sides, held } = *self;
        let peeked = match held {
            HeldSide::Neither => None,
            HeldSide::Left => sides.left.first_remaining().map(Peeked::A),
            HeldSide::Right => sides.right.first_remaining().map(Peeked::B),
        };
        f.debug_tuple("MergeIterInner")
            .field(&sides.left.printed_as_iter(held == HeldSide::Left))
            .field(&sides.right.printed_as_iter(held == HeldSide::Right))
            .field(&peeked)
            .finish()
    }
}
