//! The batch on its one core, turn by turn, in the order the executor runs
//! them: the core is the axis, a turn a block on it. A key comes in from
//! its row, reads the line it prefetched, steps down, prefetches the next
//! line and goes back out, and the next key comes in. Under the core, a row
//! a key, editable: from the end of its turn to its next, its line is on
//! its way while the core runs the others. The lines stack up across the
//! rows: the keys' lines are fetched together, where one key alone fetches
//! one at a time.
//!
//! The view draws itself as the batch plays: what hasn't happened yet is
//! faint, a turn's connectors darken while it runs, and the playhead is the
//! board's `--t`. Click the view to pause there; edit a key and press Enter
//! to look it up in the batch.

use std::rc::Rc;

use dioxus::prelude::*;

use crate::batch_run::{BatchKeys, BatchRun};
use crate::batch_view::Batcher;
use crate::concepts::{Concept, Term};
use crate::icons::{Icon, IconGlyph};
use crate::inspected_tree::LoadedTree;
use crate::spotlight::Spotlight;
use crate::trace::Tracer;

/// The core's and the rows' geometry, in pixels.
#[derive(Clone, Copy, PartialEq)]
struct TurnsLayout {
    /// Pixels a turn.
    scale: f64,
    /// Where the first turn starts, past the keys.
    left: f64,
}

impl TurnsLayout {
    const CORE_HEIGHT: f64 = 30.0;
    /// Between the core and the first key's row.
    const CORE_GAP: f64 = 12.0;
    const ROW_HEIGHT: f64 = 32.0;
    const GAP: f64 = 16.0;

    fn of(run: &BatchRun, width: f64, keys_width: f64) -> Self {
        Self {
            scale: ((width - keys_width - Self::GAP - 18.0) / run.duration().max(1.0)).max(1.0),
            left: keys_width + Self::GAP,
        }
    }

    fn x(&self, time: f64) -> f64 {
        self.left + time * self.scale
    }

    /// How far into a turn its key's connectors meet the core.
    fn inset(&self) -> f64 {
        (self.scale * 0.25).min(4.0)
    }

    /// Whether a turn is wide enough for its connectors to read as lines
    /// rather than a texture.
    fn shows_connectors(&self) -> bool {
        self.scale >= 10.0
    }

    /// Whether a turn is wide enough for its key's number.
    fn shows_numbers(&self) -> bool {
        self.scale >= 12.0
    }

    fn row_top(lane: usize) -> f64 {
        Self::CORE_HEIGHT + Self::CORE_GAP + lane as f64 * Self::ROW_HEIGHT
    }

    fn row_middle(lane: usize) -> f64 {
        Self::row_top(lane) + Self::ROW_HEIGHT / 2.0
    }
}

/// The core, and the keys' rows under it.
#[component]
pub(crate) fn CoreTurns(
    tree: LoadedTree,
    run: BatchRun,
    keys: BatchKeys,
    /// The turn running, or last run.
    now: Signal<Option<usize>>,
    pointed: Signal<Option<usize>>,
    batcher: Batcher,
    tracer: Tracer,
    spotlight: Spotlight,
) -> Element {
    // The panel's width and its left edge on the page.
    let mut frame = use_signal(|| (0.0f64, 0.0f64));
    let mounted: Signal<Option<Rc<MountedData>>> = use_signal(|| None);
    let measure = use_callback(move |_: ()| {
        if let Some(element) = mounted.peek().clone() {
            spawn(async move {
                if let Ok(rect) = element.get_client_rect().await {
                    let measured = (rect.width(), rect.min_x());
                    if *frame.peek() != measured {
                        frame.set(measured);
                    }
                }
            });
        }
    });
    // Why the key last typed names no key.
    let error = use_signal(|| None::<String>);
    let (width, page_left) = frame();
    // Room for the longest key, between bounds.
    let keys_width = if tree.key_kind().is_text() {
        400.0
    } else {
        200.0
    };
    let narrow = width > 0.0 && width < 640.0;
    let keys_width = if narrow {
        (width * 0.42).min(keys_width)
    } else {
        keys_width
    };
    let layout = (width > 0.0).then(|| TurnsLayout::of(&run, width, keys_width));
    let lit = pointed();
    // Each key's chip: on the core, looked up, or neither.
    let on_core = now().map(|turn| run.polls()[turn].lane());
    let states: Vec<&'static str> = run
        .moments(None, now())
        .iter()
        .enumerate()
        .map(|(lane, moment)| {
            if on_core == Some(lane) {
                " running"
            } else if moment.ended() {
                " ended"
            } else {
                ""
            }
        })
        .collect();
    let mut batcher = batcher;
    let height = TurnsLayout::row_top(run.lanes().len());
    rsx! {
        section {
            class: "batch-keys",
            "aria-label": "The keys' turns on one core, and their lines on their way",
            div { class: "batch-section-head",
                h2 { class: "batch-heading",
                    "Turns on one core, and "
                    Term { concept: Concept::Prefetch, spotlight, "prefetched" }
                    " lines on their way"
                }
                button {
                    class: "button",
                    title: "Four other keys (R)",
                    onclick: move |_| batcher.reseed(),
                    IconGlyph { icon: Icon::Shuffle }
                    "New keys"
                    kbd { class: "key-hint inline", "R" }
                }
            }
            div {
                class: "rows-canvas",
                style: "height:{height}px;--keys:{keys_width}px;",
                onmounted: move |evt| {
                    let mut mounted = mounted;
                    mounted.set(Some(evt.data()));
                    measure.call(());
                },
                onresize: move |_| measure.call(()),
                if let Some(layout) = layout {
                    svg {
                        class: "rows-plot",
                        width,
                        height,
                        style: "--scale:{layout.scale};--left:{layout.left}px;",
                        "aria-hidden": "true",
                        onclick: move |evt| {
                            let x = evt.client_coordinates().x - page_left;
                            batcher.seek((x - layout.left) / layout.scale);
                        },
                        rect {
                            class: "core-track",
                            x: layout.left,
                            y: 0,
                            width: layout.x(run.duration()) - layout.left,
                            height: TurnsLayout::CORE_HEIGHT,
                            rx: 4,
                        }
                        for lane in 0..run.lanes().len() {
                            line {
                                class: "row-rule",
                                x1: layout.left,
                                x2: layout.x(run.duration()),
                                y1: TurnsLayout::row_middle(lane),
                                y2: TurnsLayout::row_middle(lane),
                            }
                        }
                        for (index , poll) in run.polls().iter().enumerate() {
                            {
                                let lane = poll.lane();
                                let start = run.start(index);
                                let fetched_at = run.fetched_at(index);
                                let middle = TurnsLayout::row_middle(lane);
                                let inset = layout.inset();
                                let (into, out) = (layout.x(start) + inset, layout.x(start + 1.0) - inset);
                                let lit_class = if lit == Some(lane) { " lit" } else { "" };
                                let pending = poll
                                    .fetched()
                                    .and(run.read_at(index))
                                    .filter(|read| *read > start + 1.0);
                                rsx! {
                                    // In from the key's row, out to it.
                                    if layout.shows_connectors() {
                                        line {
                                            class: "turn-connector",
                                            style: "--from:{start};--until:{start + 1.0};",
                                            x1: into,
                                            x2: into,
                                            y1: middle,
                                            y2: TurnsLayout::CORE_HEIGHT,
                                        }
                                        line {
                                            class: "turn-connector",
                                            style: "--from:{fetched_at};--until:{fetched_at + 0.6};",
                                            x1: out,
                                            x2: out,
                                            y1: TurnsLayout::CORE_HEIGHT,
                                            y2: middle,
                                        }
                                    }
                                    if let Some(read) = pending {
                                        line {
                                            class: "prefetch-ghost",
                                            x1: out,
                                            x2: layout.x(read) + inset,
                                            y1: middle,
                                            y2: middle,
                                        }
                                        line {
                                            class: "prefetch{lit_class}",
                                            style: "--s:{fetched_at};--d:{read - fetched_at};",
                                            x1: out,
                                            x2: layout.x(read) + inset,
                                            y1: middle,
                                            y2: middle,
                                        }
                                    }
                                    if poll.fetched().is_none() {
                                        path {
                                            class: "turn-done",
                                            style: "--s:{fetched_at};",
                                            d: "M{out + 3.0} {middle} l3.5 3.5 l7 -7",
                                        }
                                    }
                                    rect {
                                        class: "turn-block{lit_class}",
                                        style: "--s:{start};",
                                        x: layout.x(start) + 0.5,
                                        y: 3,
                                        rx: 2,
                                        width: (layout.scale - 1.0).max(1.0),
                                        height: TurnsLayout::CORE_HEIGHT - 6.0,
                                    }
                                    if layout.shows_numbers() {
                                        text {
                                            class: "turn-number",
                                            style: "--s:{start};",
                                            x: layout.x(start + 0.5),
                                            y: TurnsLayout::CORE_HEIGHT / 2.0 + 4.0,
                                            "{lane}"
                                        }
                                    }
                                }
                            }
                        }
                        line {
                            class: "playhead",
                            x1: layout.left,
                            x2: layout.left,
                            y1: 0,
                            y2: height,
                        }
                    }
                }
                div { class: "core-label", "core" }
                for (lane , key) in keys.keys().iter().enumerate() {
                    KeyField {
                        key: "{lane}",
                        lane,
                        key_text: key.clone(),
                        tree: tree.clone(),
                        lit: lit == Some(lane),
                        state: states[lane],
                        pointed,
                        error,
                        batcher,
                        tracer,
                    }
                }
            }
            p { class: "batch-keys-foot", role: "alert",
                if let Some(error) = error() {
                    span { class: "key-error", "{error}. Try {tree.key_kind().example()}." }
                } else {
                    span { class: "legend-item",
                        span { class: "legend-mark turn" }
                        "a turn: the key comes in, reads its line, prefetches the next, goes out"
                    }
                    span { class: "legend-item",
                        span { class: "legend-mark prefetch" }
                        "its line on its way, until its next turn"
                    }
                    span { class: "keys-hint", "Edit a key and press Enter." }
                }
            }
        }
    }
}

/// One key of the batch: its number, which looks it up alone, and its text,
/// which takes another key on Enter.
#[component]
fn KeyField(
    lane: usize,
    key_text: String,
    tree: LoadedTree,
    lit: bool,
    /// " running" while the key is on the core, " ended" once looked up.
    state: &'static str,
    pointed: Signal<Option<usize>>,
    error: Signal<Option<String>>,
    batcher: Batcher,
    tracer: Tracer,
) -> Element {
    let mut draft = use_signal(|| key_text.clone());
    let mut invalid = use_signal(|| false);
    // A key changed elsewhere, by new keys or a link, shows here.
    use_effect(use_reactive!(|key_text| {
        draft.set(key_text);
        invalid.set(false);
    }));
    let (mut pointed, mut error, mut batcher, mut tracer) = (pointed, error, batcher, tracer);
    let commit_tree = tree.clone();
    let mut commit = move || {
        let text = draft.peek().trim().to_string();
        if text == *key_text {
            return;
        }
        match commit_tree.trace(&text) {
            Ok(_) => {
                invalid.set(false);
                error.set(None);
                batcher.set_key(lane, text, &commit_tree);
            }
            Err(why) => {
                invalid.set(true);
                error.set(Some(why));
            }
        }
    };
    let alone = tree.clone();
    rsx! {
        div {
            class: if lit { "key-field lit" } else { "key-field" },
            style: "transform:translate(0,{TurnsLayout::row_top(lane)}px);",
            onmouseenter: move |_| pointed.set(Some(lane)),
            onmouseleave: move |_| pointed.set(None),
            button {
                class: "key-chip data{state}",
                title: "Look key {lane} up alone",
                "aria-label": "Look key {lane} up alone",
                onclick: move |_| {
                    batcher.close();
                    tracer.run(&alone, &draft.peek());
                },
                "{lane}"
            }
            input {
                class: if invalid() { "key-input data invalid" } else { "key-input data" },
                "aria-label": "Key {lane}",
                "aria-invalid": if invalid() { "true" } else { "false" },
                spellcheck: false,
                autocomplete: "off",
                value: "{draft}",
                oninput: move |evt| draft.set(evt.value()),
                onfocus: move |_| pointed.set(Some(lane)),
                onkeydown: move |evt| {
                    if evt.key() == Key::Enter {
                        commit();
                    }
                },
                onblur: move |_| pointed.set(None),
            }
        }
    }
}
