//! The batch view: four keys looked up together, played out search by
//! search, from bbtree's own run of `get_many` (`batch_run.rs`). On the
//! left the keys, editable, each beside its searches and prefetches in the
//! order they run (`batch_timeline.rs`), and the nodes they read, with each
//! key where it is (`batch_tree.rs`); on the right the code every key runs,
//! with each key at the line it stopped at, and the search running in a
//! sentence (`lane_program.rs`).
//!
//! The moment is one number, the time on the batch's axis, in searches,
//! which a loop in the browser advances each frame and writes to the board
//! as `--t`: prefetched lines fill and the rows draw themselves in CSS,
//! without a redraw. The panels redraw only when another search starts.
//! Paused or stepped, the moment is just after a search, which every view
//! shows as run.

use dioxus::prelude::*;

use crate::batch_run::{BatchKeys, BatchRun};
use crate::batch_timeline::KeyRows;
use crate::batch_tree::BatchTree;
use crate::concepts::{Concept, Term};
use crate::element_ids::ElementIds;
use crate::icons::{Icon, IconGlyph};
use crate::inspected_tree::LoadedTree;
use crate::lane_program::{LaneProgram, PollCaption};
use crate::spotlight::Spotlight;
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
        Playback::send(&self.board_id(), command);
    }

    /// Pauses the batch just after the poll running at `time`.
    pub(crate) fn seek(&self, time: f64) {
        Playback::seek(&self.board_id(), time);
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
    // The search running, or last run; and whether the batch plays.
    let now = use_signal(|| None::<usize>);
    let playing = use_signal(|| true);
    // The key under the pointer, lit in every view.
    let pointed = use_signal(|| None::<usize>);
    let playback = Playback::of(&run);
    use_future(move || {
        let playback = playback.clone();
        async move { playback.run(&batcher.board_id(), now, playing).await }
    });
    rsx! {
        section {
            id: batcher.board_id(),
            class: "batch-board",
            "aria-label": "Four keys looked up together, search by search",
            BatchHeadline {
                run: run.clone(),
                now,
                playing,
                batcher,
                spotlight,
            }
            KeyRows {
                tree: tree.clone(),
                run: run.clone(),
                keys,
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
                    title: "A search back (,)",
                    "aria-label": "A search back",
                    onclick: move |_| batcher.play(PlaybackCommand::Back),
                    IconGlyph { icon: Icon::StepBack }
                }
                button {
                    class: "icon-button quiet",
                    title: "The next search (.)",
                    "aria-label": "The next search",
                    onclick: move |_| batcher.play(PlaybackCommand::Forward),
                    IconGlyph { icon: Icon::StepForward }
                }
            }
            div { class: "batch-lead",
                p { class: "batch-sentence",
                    strong { "{lanes} keys, searched together on one thread." }
                    " Each "
                    Term { concept: Concept::Prefetch, spotlight, "prefetches" }
                    " the line it needs next, and the others search while it comes."
                }
                if !run.batches() {
                    p { class: "batch-note",
                        code { "get_many" }
                        " searches the keys of a map this small one at a time; these are the searches it runs on larger maps."
                    }
                }
            }
        }
    }
}

/// What the controls ask the loop that plays the batch.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PlaybackCommand {
    /// Play, or pause; at the end, play from the start.
    Toggle,
    /// Pause after the poll before.
    Back,
    /// Pause after the next poll.
    Forward,
}

/// The loop in the browser that advances the batch's time and writes it to
/// the board, and tells the panels when another poll starts.
#[derive(Clone, PartialEq)]
struct Playback {
    /// Where each poll starts on the time axis.
    starts: Vec<f64>,
    /// Where the second round starts: the first plays slowly.
    first_round: f64,
    total: f64,
    /// How long a poll lasts on screen.
    step_ms: f64,
}

impl Playback {
    /// How long a poll of the first round lasts on screen: slow enough to
    /// follow each lane's first poll.
    const FIRST_ROUND_MS: f64 = 560.0;

    fn of(run: &BatchRun) -> Self {
        let polls = run.polls().len();
        let lanes = run.lanes().len();
        Self {
            starts: (0..polls).map(|poll| run.start(poll)).collect(),
            first_round: if lanes < polls {
                run.start(lanes)
            } else {
                run.duration()
            },
            total: run.duration(),
            // About eight seconds a batch, but no poll a blur or a crawl.
            step_ms: (8000.0 / run.duration().max(1.0)).clamp(70.0, 260.0),
        }
    }

    /// Plays the batch from the start on the board with the id `board`,
    /// telling `now` each poll that starts and `playing` whether it plays,
    /// until the board goes. Paused, time rests just after a poll; with
    /// reduced motion, it jumps from one poll to the next.
    async fn run(self, board: &str, now: Signal<Option<usize>>, playing: Signal<bool>) {
        let (mut now, mut playing) = (now, playing);
        let mut heard = document::eval(
            r#"
            const [board_id, starts, first_round, total, step_ms, first_ms] = await dioxus.recv();
            const board = document.getElementById(board_id);
            if (!board) return;
            const reduced = matchMedia('(prefers-reduced-motion: reduce)').matches;
            // Just before a poll ends: run, its fetch gone out.
            const RAN = 0.999;
            let t = -1, playing = true, last = null, hold = 0, told = null;
            const pollAt = (time) => {
                let low = 0, high = starts.length;
                while (low < high) {
                    const mid = (low + high) >> 1;
                    if (starts[mid] <= time) low = mid + 1; else high = mid;
                }
                return low - 1;
            };
            const after = (index) => index < 0 ? -1 : Math.min(starts[Math.min(index, starts.length - 1)] + RAN, total);
            board.addEventListener('batch-playback', (event) => {
                const command = event.detail;
                const index = pollAt(t);
                if (command === 'toggle') {
                    if (t >= total) t = -1;
                    else if (playing) t = after(index);
                    playing = !playing;
                } else if (command === 'back') {
                    playing = false;
                    t = after(index - 1);
                } else if (command === 'forward') {
                    playing = false;
                    t = after(index + 1);
                } else if (typeof command === 'number') {
                    playing = false;
                    t = after(pollAt(command));
                }
                last = null;
            });
            const frame = (time) => {
                if (!board.isConnected) return;
                if (playing && last !== null) {
                    if (reduced) {
                        if (time >= hold) {
                            t = after(pollAt(t) + 1);
                            hold = time + step_ms * 4;
                        }
                    } else {
                        t = Math.min(total, t + (time - last) / (t < first_round ? first_ms : step_ms));
                    }
                }
                if (t >= total || (reduced && pollAt(t) === starts.length - 1)) playing = false;
                last = time;
                board.style.setProperty('--t', t.toFixed(3));
                const index = pollAt(t);
                const state = index + ':' + playing;
                if (state !== told) {
                    told = state;
                    dioxus.send([index, playing]);
                }
                requestAnimationFrame(frame);
            };
            requestAnimationFrame(frame);
            "#,
        );
        let _ = heard.send((
            board,
            &self.starts,
            self.first_round,
            self.total,
            self.step_ms,
            Self::FIRST_ROUND_MS,
        ));
        while let Ok((index, is_playing)) = heard.recv::<(i64, bool)>().await {
            let index = usize::try_from(index).ok();
            if *now.peek() != index {
                now.set(index);
            }
            if *playing.peek() != is_playing {
                playing.set(is_playing);
            }
        }
    }

    /// Asks the loop of the board with the id `board` to play, pause or
    /// step.
    fn send(board: &str, command: PlaybackCommand) {
        let detail = match command {
            PlaybackCommand::Toggle => "'toggle'",
            PlaybackCommand::Back => "'back'",
            PlaybackCommand::Forward => "'forward'",
        };
        Self::dispatch(board, detail);
    }

    /// Pauses the batch of the board with the id `board` just after the
    /// poll running at `time`.
    fn seek(board: &str, time: f64) {
        Self::dispatch(board, &format!("{time:.3}"));
    }

    fn dispatch(board: &str, detail: &str) {
        document::eval(&format!(
            "document.getElementById('{board}')?.dispatchEvent(new CustomEvent('batch-playback', {{ detail: {detail} }}));"
        ));
    }
}
