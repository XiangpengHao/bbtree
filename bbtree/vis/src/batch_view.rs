//! The batch view: four keys looked up together on one core, played out
//! turn by turn, from bbtree's own run of `get_many` (`batch_run.rs`). On
//! the left the core, with each key's turns on it, and the keys under it,
//! editable, each with its lines on their way (`batch_timeline.rs`); and
//! the nodes they read, with each key where it is (`batch_tree.rs`); on the
//! right the code every key runs, with each key at the line it stopped at,
//! and the turn running in a sentence (`lane_program.rs`).
//!
//! The moment is one number, the time on the batch's axis, in turns, which
//! a loop in the browser (`timeline.rs`) advances each frame and writes to
//! the board as `--t`: prefetched lines fill and the rows draw themselves
//! in CSS, without a redraw. The panels redraw only when another turn
//! starts. Paused or stepped, the moment is just after a turn, which every
//! view shows as run.

use dioxus::prelude::*;

use crate::batch_run::{BatchKeys, BatchRun};
use crate::batch_timeline::CoreTurns;
use crate::batch_tree::BatchTree;
use crate::concepts::{Concept, Term};
use crate::element_ids::ElementIds;
use crate::icons::{Icon, IconGlyph};
use crate::inspected_tree::LoadedTree;
use crate::lane_program::{LaneProgram, PollCaption};
use crate::spotlight::Spotlight;
use crate::timeline::{PlaybackCommand, Timeline};
use crate::trace::Tracer;

/// Whether a view shows a batch, its keys, and what changes them and plays
/// them: owned by the view (the explorer, or one of the post's figures),
/// handed to each of its panels.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Batcher {
    shown: Signal<bool>,
    /// Keys chosen by hand or by a link, and the tree they were chosen on;
    /// a link's keys go to whatever tree is built. Other trees get keys
    /// picked by `seed`.
    chosen: Signal<Option<(BatchKeys, Option<LoadedTree>)>>,
    seed: Signal<u64>,
    /// The view's, for its board's id.
    ids: ElementIds,
}

impl Batcher {
    /// A view's batcher, showing one key until asked, picking random keys
    /// by `seed`: a hook, for the view's component.
    pub(crate) fn use_new(seed: Signal<u64>) -> Self {
        Self {
            shown: use_signal(|| false),
            chosen: use_signal(|| None),
            seed,
            ids: ElementIds::of_current(),
        }
    }

    /// Whether the page shows a batch rather than one key.
    pub(crate) fn is_shown(&self) -> bool {
        *self.shown.read()
    }

    /// The keys the batch looks up on `tree`.
    pub(crate) fn keys_for(&self, tree: &LoadedTree) -> BatchKeys {
        match &*self.chosen.read() {
            Some((keys, on)) if on.as_ref().is_none_or(|on| on == tree) => keys.clone(),
            _ => BatchKeys::random(tree, *self.seed.read()),
        }
    }

    /// Shows the batch of a link's keys.
    pub(crate) fn open(&mut self, keys: BatchKeys) {
        self.chosen.set(Some((keys, None)));
        self.shown.set(true);
    }

    pub(crate) fn start(&mut self) {
        self.shown.set(true);
    }

    /// Goes back to one key.
    pub(crate) fn close(&mut self) {
        self.shown.set(false);
    }

    /// Four other keys, at random.
    pub(crate) fn reseed(&mut self) {
        let mut state = *self.seed.peek() | 1;
        // xorshift64*, as the single key's random lookups.
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        self.seed.set(state.wrapping_mul(0x2545_F491_4F6C_DD1D));
        self.chosen.set(None);
    }

    /// Looks up `key` as key `index` of the batch on `tree`.
    pub(crate) fn set_key(&mut self, index: usize, key: String, tree: &LoadedTree) {
        let keys = {
            let chosen = self.chosen.peek();
            match &*chosen {
                Some((keys, on)) if on.as_ref().is_none_or(|on| on == tree) => keys.clone(),
                _ => BatchKeys::random(tree, *self.seed.peek()),
            }
        };
        self.chosen
            .set(Some((keys.with_key(index, key), Some(tree.clone()))));
    }

    /// Asks the loop that plays the batch to play, pause or step.
    pub(crate) fn play(&self, command: PlaybackCommand) {
        Timeline::send(&self.board_id(), command);
    }

    /// Pauses the batch just after the poll running at `time`.
    pub(crate) fn seek(&self, time: f64) {
        Timeline::seek(&self.board_id(), time);
    }

    /// The id of the board that plays the batch, which its loop writes its
    /// time to.
    fn board_id(&self) -> String {
        self.ids.id("batch-board")
    }
}

/// The toolbar's switch between one key and a batch.
#[component]
pub(crate) fn ModeSwitch(batcher: Batcher, tracer: Tracer, tree: Option<LoadedTree>) -> Element {
    let batch = batcher.is_shown();
    let mut batcher = batcher;
    let mut tracer = tracer;
    rsx! {
        div {
            class: "mode-switch",
            role: "group",
            "aria-label": "What to look up",
            button {
                class: "mode-option",
                "aria-pressed": if batch { "false" } else { "true" },
                title: "One key, node by node (B toggles)",
                onclick: move |_| {
                    batcher.close();
                    if tracer.trace().is_none()
                        && let Some(tree) = &tree
                    {
                        tracer.start(tree, None);
                    }
                },
                "One key"
            }
            button {
                class: "mode-option",
                "aria-pressed": if batch { "true" } else { "false" },
                title: "Four keys together, as get_many looks them up (B toggles)",
                onclick: move |_| batcher.start(),
                "Batch"
            }
        }
    }
}

/// One batch lookup, played out.
#[component]
pub(crate) fn BatchView(
    tree: LoadedTree,
    batcher: Batcher,
    tracer: Tracer,
    spotlight: Spotlight,
) -> Element {
    let keys = batcher.keys_for(&tree);
    let run = use_memo(use_reactive!(|tree, keys| tree.trace_many(keys.keys())));
    let run = run.read().clone();
    match run {
        Ok(run) => rsx! {
            BatchBoard {
                key: "{keys.keys().join(\"\\n\")}",
                tree,
                run,
                keys,
                batcher,
                tracer,
                spotlight,
            }
        },
        Err(error) => rsx! {
            p { class: "empty-state", {error} }
        },
    }
}

/// The batch's views and the controls that play it.
#[component]
fn BatchBoard(
    tree: LoadedTree,
    run: BatchRun,
    keys: BatchKeys,
    batcher: Batcher,
    tracer: Tracer,
    spotlight: Spotlight,
) -> Element {
    // The turn running, or last run; and whether the batch plays.
    let now = use_signal(|| None::<usize>);
    let playing = use_signal(|| true);
    // The key under the pointer, lit in every view.
    let pointed = use_signal(|| None::<usize>);
    let playback = run.timeline();
    use_future(move || {
        let playback = playback.clone();
        async move { playback.run(&batcher.board_id(), now, playing).await }
    });
    rsx! {
        section {
            id: batcher.board_id(),
            class: "batch-board",
            "aria-label": "Four keys looked up together, turn by turn",
            BatchHeadline {
                run: run.clone(),
                now,
                playing,
                batcher,
                spotlight,
            }
            CoreTurns {
                tree: tree.clone(),
                run: run.clone(),
                keys,
                now,
                pointed,
                batcher,
                tracer,
                spotlight,
            }
            BatchTree {
                tree: tree.clone(),
                run: run.clone(),
                now,
                pointed,
                batcher,
                tracer,
            }
            aside { class: "batch-code",
                LaneProgram { run: run.clone(), now, pointed }
                PollCaption {
                    tree,
                    run,
                    now,
                    playing,
                }
            }
        }
    }
}

/// The batch in a sentence, and the controls that play it.
#[component]
fn BatchHeadline(
    run: BatchRun,
    now: Signal<Option<usize>>,
    playing: Signal<bool>,
    batcher: Batcher,
    spotlight: Spotlight,
) -> Element {
    let lanes = run.lanes().len();
    let at_end = now().is_some_and(|now| now + 1 == run.polls().len()) && !playing();
    rsx! {
        div { class: "batch-headline",
            div {
                class: "batch-controls",
                role: "group",
                "aria-label": "Play the batch",
                button {
                    class: "icon-button",
                    title: if playing() { "Pause (Space)" } else { "Play (Space)" },
                    "aria-label": if playing() { "Pause" } else if at_end { "Play again" } else { "Play" },
                    onclick: move |_| batcher.play(PlaybackCommand::Toggle),
                    IconGlyph { icon: if playing() { Icon::Pause } else if at_end { Icon::Replay } else { Icon::Play } }
                }
                button {
                    class: "icon-button quiet",
                    title: "A turn back (,)",
                    "aria-label": "A turn back",
                    onclick: move |_| batcher.play(PlaybackCommand::Back),
                    IconGlyph { icon: Icon::StepBack }
                }
                button {
                    class: "icon-button quiet",
                    title: "The next turn (.)",
                    "aria-label": "The next turn",
                    onclick: move |_| batcher.play(PlaybackCommand::Forward),
                    IconGlyph { icon: Icon::StepForward }
                }
            }
            div { class: "batch-lead",
                p { class: "batch-sentence",
                    strong { "{lanes} keys, looked up together on one core." }
                    " They take turns on it: in its turn, a key "
                    Term { concept: Concept::Prefetch, spotlight, "prefetches" }
                    " the line it needs next and goes out, and the others run while it comes."
                }
                if !run.batches() {
                    p { class: "batch-note",
                        code { "get_many" }
                        " looks the keys of a map this small up one at a time; these are the turns it runs on larger maps."
                    }
                }
            }
        }
    }
}
