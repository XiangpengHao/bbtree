//! A lookup's way down, played a search step at a time: each node's search
//! is the steps the inspector lists (skip the shared bytes, count the
//! smaller inline keys, search the tied keys, the outcome), root first.
//! The page shows the way down up to a number of steps: the path's cards
//! and the key draw what those steps did, CSS transitions carry them from
//! one step to the next, and the inspector shows the node the way down is
//! at. Playing adds a step at a pace a reader can follow; the controls
//! pause, or step either way.

use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;

use crate::icons::{Icon, IconGlyph};
use crate::inspected_tree::LoadedTree;
use crate::node_inspector::SearchStep;
use crate::trace::{Trace, Tracer};
use crate::tree_shape::NodeAt;

/// A lookup's way down, and how far it has played.
#[derive(Clone, PartialEq, Debug, Default)]
pub(crate) struct Descent {
    /// Each node's search, by row of the path, root first.
    searches: Vec<NodeSearch>,
    /// How many steps the page shows.
    shown: usize,
    /// While it plays, the node whose search it stops after.
    playing_to: Option<NodeAt>,
}

/// One node's search on the way down.
#[derive(Clone, PartialEq, Debug)]
struct NodeSearch {
    at: NodeAt,
    /// Each step, by the part of the search it is: 1 skips the prefix, 2
    /// compares, 3 searches the ties, 4 is the outcome
    /// (`SearchStep::phase`).
    phases: Vec<u8>,
}

impl Descent {
    /// How long a step lasts while playing.
    const STEP_MS: u32 = 700;

    /// The way down `trace` takes, before its first step, playing to its
    /// end.
    pub(crate) fn of(tree: &LoadedTree, trace: &Trace) -> Self {
        let shape = tree.shape();
        let searches: Vec<NodeSearch> = trace
            .steps()
            .iter()
            .map(|step| {
                let at = step.at();
                let prefix_len = shape.level(at.level()).prefix_len(at.index());
                NodeSearch {
                    at,
                    phases: SearchStep::for_step(step, prefix_len, false)
                        .iter()
                        .map(SearchStep::phase)
                        .collect(),
                }
            })
            .collect();
        Self {
            playing_to: searches.last().map(|search| search.at),
            searches,
            shown: 0,
        }
    }

    /// How many steps the way down takes.
    pub(crate) fn total(&self) -> usize {
        self.searches.iter().map(|search| search.phases.len()).sum()
    }

    pub(crate) fn playing(&self) -> bool {
        self.playing_to.is_some()
    }

    pub(crate) fn shown(&self) -> usize {
        self.shown
    }

    pub(crate) fn at_end(&self) -> bool {
        self.shown >= self.total()
    }

    /// How far the search at row `row` has gone: the part of its last step
    /// shown, 0 before its first.
    pub(crate) fn phase(&self, row: usize) -> u8 {
        let before = self.steps_before(row);
        let phases = &self.searches[row].phases;
        let done = self.shown.saturating_sub(before).min(phases.len());
        done.checked_sub(1).map_or(0, |last| phases[last])
    }

    /// The step shown last: its row and its index among the row's steps.
    pub(crate) fn current(&self) -> Option<(usize, usize)> {
        let mut left = self.shown.checked_sub(1)?;
        for (row, search) in self.searches.iter().enumerate() {
            if left < search.phases.len() {
                return Some((row, left));
            }
            left -= search.phases.len();
        }
        None
    }

    /// The node the way down is at: the one of the step shown last, or the
    /// root before the first.
    pub(crate) fn node(&self) -> Option<NodeAt> {
        let row = self.current().map_or(0, |(row, _)| row);
        self.searches.get(row).map(|search| search.at)
    }

    /// The step shown last, by its index among the steps of the search at
    /// `at`, if the way down is at `at`.
    pub(crate) fn step_in(&self, at: NodeAt) -> Option<usize> {
        self.current()
            .filter(|&(row, _)| self.searches[row].at == at)
            .map(|(_, index)| index)
    }

    /// Shows the next step, while it plays; stops after the search it
    /// plays to.
    pub(crate) fn play_on(&mut self) {
        let Some(goal) = self.playing_to else {
            return;
        };
        let end = self.steps_through(goal);
        if self.shown < end {
            self.shown += 1;
        }
        if self.shown >= end {
            self.playing_to = None;
        }
    }

    /// Pauses; or plays on to the end, from the start if it's there.
    pub(crate) fn toggle_playing(&mut self) {
        if self.playing() {
            self.playing_to = None;
            return;
        }
        if self.at_end() {
            self.shown = 0;
        }
        self.playing_to = self.searches.last().map(|search| search.at);
    }

    /// Pauses, and shows `delta` steps more, or fewer if negative.
    pub(crate) fn step_by(&mut self, delta: isize) {
        self.playing_to = None;
        self.shown = self.shown.saturating_add_signed(delta).min(self.total());
    }

    pub(crate) fn pause(&mut self) {
        self.playing_to = None;
    }

    /// Plays on to the end of the search at `at`, or to the end if the
    /// way down doesn't pass `at`.
    pub(crate) fn play_to(&mut self, at: NodeAt) {
        self.playing_to = Some(at);
    }

    fn steps_before(&self, row: usize) -> usize {
        self.searches[..row]
            .iter()
            .map(|search| search.phases.len())
            .sum()
    }

    /// How many steps it takes to finish the search at `at`: all of them
    /// if the way down doesn't pass `at`.
    fn steps_through(&self, at: NodeAt) -> usize {
        self.searches
            .iter()
            .position(|search| search.at == at)
            .map_or(self.total(), |row| self.steps_before(row + 1))
    }
}

/// Play, pause and step the way down, as the batch's controls play a
/// batch. Keyed by the lookup, so each lookup's timer starts afresh.
#[component]
pub(crate) fn DescentControls(descent: Descent, tracer: Tracer) -> Element {
    let mut tracer = tracer;
    use_future(move || async move {
        let mut waited = 0;
        loop {
            TimeoutFuture::new(100).await;
            if !tracer.descent_playing_now() {
                waited = 0;
                continue;
            }
            waited += 100;
            if waited >= Descent::STEP_MS {
                waited = 0;
                tracer.play_descent_on();
            }
        }
    });
    let playing = descent.playing();
    let at_end = descent.at_end();
    let play = if playing {
        "Pause"
    } else if at_end {
        "Play again"
    } else {
        "Play"
    };
    rsx! {
        div {
            class: "batch-controls",
            role: "group",
            "aria-label": "Play the lookup",
            button {
                class: "icon-button",
                title: "{play} (Space)",
                "aria-label": play,
                onclick: move |_| tracer.toggle_descent(),
                IconGlyph { icon: if playing { Icon::Pause } else if at_end { Icon::Replay } else { Icon::Play } }
            }
            button {
                class: "icon-button quiet",
                title: "The step before (,)",
                "aria-label": "The step before",
                disabled: descent.shown() == 0,
                onclick: move |_| tracer.step_descent(-1),
                IconGlyph { icon: Icon::StepBack }
            }
            button {
                class: "icon-button quiet",
                title: "The next step (.)",
                "aria-label": "The next step",
                disabled: at_end,
                onclick: move |_| tracer.step_descent(1),
                IconGlyph { icon: Icon::StepForward }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A root and a leaf below it, three steps each.
    fn two_nodes() -> Descent {
        let (root, leaf) = (NodeAt::new(1, 0), NodeAt::new(0, 3));
        Descent {
            searches: vec![
                NodeSearch {
                    at: root,
                    phases: vec![1, 2, 4],
                },
                NodeSearch {
                    at: leaf,
                    phases: vec![2, 3, 4],
                },
            ],
            shown: 0,
            playing_to: Some(leaf),
        }
    }

    #[test]
    fn steps_count_through_each_row_in_turn() {
        let mut descent = two_nodes();
        assert_eq!(descent.total(), 6);
        assert_eq!((descent.phase(0), descent.current()), (0, None));
        descent.step_by(2);
        assert_eq!(descent.phase(0), 2);
        descent.step_by(2);
        assert_eq!((descent.phase(1), descent.current()), (2, Some((1, 0))));
        descent.step_by(1);
        assert_eq!(descent.phase(1), 3);
        descent.step_by(9);
        assert_eq!((descent.phase(0), descent.current()), (4, Some((1, 2))));
        assert!(descent.at_end());
    }

    #[test]
    fn the_node_follows_the_steps_and_playing_stops_at_its_goal() {
        let mut descent = two_nodes();
        let (root, leaf) = (NodeAt::new(1, 0), NodeAt::new(0, 3));
        assert_eq!(descent.node(), Some(root));
        descent.play_to(root);
        for _ in 0..3 {
            assert!(descent.playing());
            descent.play_on();
        }
        assert!(!descent.playing());
        assert_eq!((descent.shown(), descent.node()), (3, Some(root)));
        assert_eq!(descent.step_in(root), Some(2));
        descent.toggle_playing();
        descent.play_on();
        assert_eq!(descent.node(), Some(leaf));
        assert_eq!(descent.step_in(root), None);
        descent.step_by(9);
        descent.toggle_playing();
        assert_eq!((descent.shown(), descent.node()), (0, Some(root)));
    }
}
