//! Walking two sets side by side in order, the core of every operation on
//! two sets that goes through both: [`SetCursor`], one set's elements, a
//! leaf's run of keys at a time, and [`MergeSides`], two of them merged.
//!
//! A cursor takes its set's keys from the walk a run at a time
//! (`tree/walk.rs`): all the keys left in a leaf, which the walk moves past
//! at once, or, where the walk crosses into another leaf, the next key
//! alone. Stepping through a run touches neither the walk nor its count. A
//! merge's fold keeps both runs in locals, which live in registers, and
//! touches each walk once per run; `next` keeps them in the iterator and
//! does as little per element.
//!
//! The iterators' `next` is always inlined. Called out of line, once per
//! element, it reaches the cursors through a pointer, and each step waits on
//! the previous step's stores to them. Inlined into the caller's loop, the
//! cursors stay in its frame, which the CPU forwards from cheaply.

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
    fn new(iter: Iter<'a, T, S>) -> Self {
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
    fn len(&self) -> usize {
        self.run.len() + self.iter.len()
    }
}

/// Prints the elements left.
impl<T: fmt::Debug, S: InlineKeyStrategy<T>> fmt::Debug for SetCursor<'_, T, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.run.chain(self.iter.clone()))
            .finish()
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

impl<'a, T, S: InlineKeyStrategy<T>> MergeSides<'a, T, S> {
    pub(super) fn new(left: Iter<'a, T, S>, right: Iter<'a, T, S>) -> Self {
        Self {
            left: SetCursor::new(left),
            right: SetCursor::new(right),
        }
    }

    /// How many elements each side has left.
    pub(super) fn lens(&self) -> (usize, usize) {
        (self.left.len(), self.right.len())
    }
}

/// Prints each side's elements left.
impl<T: fmt::Debug, S: InlineKeyStrategy<T>> fmt::Debug for MergeSides<'_, T, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MergeSides")
            .field("left", &self.left)
            .field("right", &self.right)
            .finish()
    }
}

impl<'a, T: Ord, S: InlineKeyStrategy<T>> MergeSides<'a, T, S> {
    /// The next step of the merge, moving past it; `None` once both sides are
    /// done.
    #[inline]
    pub(super) fn next_step(&mut self) -> Option<MergeStep<'a, T>> {
        match (self.left.peek(), self.right.peek()) {
            (Some(left), Some(right)) => Some(match left.cmp(right) {
                Ordering::Less => {
                    self.left.advance();
                    MergeStep::Left(left)
                }
                Ordering::Greater => {
                    self.right.advance();
                    MergeStep::Right(right)
                }
                Ordering::Equal => {
                    self.left.advance();
                    self.right.advance();
                    MergeStep::Both(left)
                }
            }),
            (Some(left), None) => {
                self.left.advance();
                Some(MergeStep::Left(left))
            }
            (None, Some(right)) => {
                self.right.advance();
                Some(MergeStep::Right(right))
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
}
