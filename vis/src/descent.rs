//! A lookup's way down, played a search step at a time: each node's search
//! is the steps the inspector lists (skip the shared bytes, count the
//! smaller inline keys, search the tied keys, the outcome), root first.
//! The page shows the way down up to a number of steps, which the tracer
//! keeps: the path's cards and the key draw what those steps did, and CSS
//! transitions carry them from one step to the next. Playing adds a step
//! at a pace a reader can follow; the controls pause, or step either way.

use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;

use crate::icons::{Icon, IconGlyph};
use crate::inspected_tree::LoadedTree;
use crate::node_inspector::SearchStep;
use crate::trace::{Trace, Tracer};

/// Each node's search steps on the way down, by the part of the search
/// each is: 1 skips the prefix, 2 compares, 3 searches the ties, 4 is the
/// outcome (`SearchStep::phase`).
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct DescentPlan {
    /// By row of the path, root first.
    rows: Vec<Vec<u8>>,
}

impl DescentPlan {
    /// How long a step lasts while playing.
    const STEP_MS: u32 = 700;

    pub(crate) fn of(tree: &LoadedTree, trace: &Trace) -> Self {
        let shape = tree.shape();
        let rows = trace
            .steps()
            .iter()
            .map(|step| {
                let at = step.at();
                let prefix_len = shape.level(at.level()).prefix_len(at.index());
                SearchStep::for_step(step, prefix_len, false)
                    .iter()
                    .map(SearchStep::phase)
                    .collect()
            })
            .collect();
        Self { rows }
    }

    /// How many steps the way down takes.
    pub(crate) fn total(&self) -> usize {
        self.rows.iter().map(Vec::len).sum()
    }

    /// How far the search at row `row` has gone once `shown` steps are
    /// shown: the part of its last step shown, 0 before its first.
    pub(crate) fn phase(&self, row: usize, shown: usize) -> u8 {
        let before: usize = self.rows[..row].iter().map(Vec::len).sum();
        let done = shown.saturating_sub(before).min(self.rows[row].len());
        done.checked_sub(1).map_or(0, |last| self.rows[row][last])
    }

    /// The step shown last: its row and its index among the row's steps.
    pub(crate) fn current(&self, shown: usize) -> Option<(usize, usize)> {
        let mut left = shown.checked_sub(1)?;
        for (row, steps) in self.rows.iter().enumerate() {
            if left < steps.len() {
                return Some((row, left));
            }
            left -= steps.len();
        }
        None
    }
}

/// Play, pause and step the way down, as the batch's controls play a
/// batch. Keyed by the lookup, so each lookup's timer starts afresh.
#[component]
pub(crate) fn DescentControls(total: usize, tracer: Tracer) -> Element {
    let mut tracer = tracer;
    use_future(move || async move {
        let mut waited = 0;
        loop {
            TimeoutFuture::new(100).await;
            if !tracer.playing_now() {
                waited = 0;
                continue;
            }
            waited += 100;
            if waited < DescentPlan::STEP_MS {
                continue;
            }
            waited = 0;
            let shown = tracer.shown_now();
            if shown < total {
                tracer.show_steps(shown + 1);
            }
            if shown + 1 >= total {
                tracer.set_playing(false);
            }
        }
    });
    let playing = tracer.playing();
    let shown = tracer.shown();
    let at_end = shown >= total;
    rsx! {
        div {
            class: "batch-controls",
            role: "group",
            "aria-label": "Play the lookup",
            button {
                class: "icon-button",
                title: if playing { "Pause" } else if at_end { "Play again" } else { "Play" },
                "aria-label": if playing { "Pause" } else if at_end { "Play again" } else { "Play" },
                onclick: move |_| {
                    if playing {
                        tracer.set_playing(false);
                    } else {
                        if at_end {
                            tracer.show_steps(0);
                        }
                        tracer.set_playing(true);
                    }
                },
                IconGlyph { icon: if playing { Icon::Pause } else if at_end { Icon::Replay } else { Icon::Play } }
            }
            button {
                class: "icon-button quiet",
                title: "The step before",
                "aria-label": "The step before",
                disabled: shown == 0,
                onclick: move |_| {
                    tracer.set_playing(false);
                    tracer.show_steps(shown.saturating_sub(1));
                },
                IconGlyph { icon: Icon::StepBack }
            }
            button {
                class: "icon-button quiet",
                title: "The next step",
                "aria-label": "The next step",
                disabled: at_end,
                onclick: move |_| {
                    tracer.set_playing(false);
                    tracer.show_steps((shown + 1).min(total));
                },
                IconGlyph { icon: Icon::StepForward }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_count_through_each_row_in_turn() {
        let plan = DescentPlan {
            rows: vec![vec![1, 2, 4], vec![2, 3, 4]],
        };
        assert_eq!(plan.total(), 6);
        assert_eq!(plan.phase(0, 0), 0);
        assert_eq!(plan.phase(0, 2), 2);
        assert_eq!(plan.phase(0, 6), 4);
        assert_eq!(plan.phase(1, 3), 0);
        assert_eq!(plan.phase(1, 5), 3);
        assert_eq!(plan.current(0), None);
        assert_eq!(plan.current(4), Some((1, 0)));
        assert_eq!(plan.current(6), Some((1, 2)));
    }
}
