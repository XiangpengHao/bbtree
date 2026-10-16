//! The batch's keys, editable, each beside its searches and prefetches in
//! the order they run: a search is a block, and from each prefetch to that
//! key's next search the line is pending while the other keys search. The
//! prefetches stack up across the rows: the keys' lines are fetched
//! together, where one key alone fetches one at a time.
//!
//! The rows draw themselves as the batch plays: what hasn't happened yet is
//! faint, and the playhead is the board's `--t`. Click a row to pause
//! there; edit a key and press Enter to look it up in the batch.

use std::rc::Rc;

use dioxus::prelude::*;

use crate::batch_run::{BatchKeys, BatchRun};
use crate::batch_view::{Batcher, Playback};
use crate::concepts::{Concept, Term};
use crate::icons::{Icon, IconGlyph};
use crate::inspected_tree::LoadedTree;
use crate::spotlight::Spotlight;
use crate::trace::Tracer;

/// The rows' geometry, in pixels.
#[derive(Clone, Copy, PartialEq)]
struct RowsLayout {
    /// Pixels a search.
    scale: f64,
    /// Where the first search starts, past the keys.
    left: f64,
}

impl RowsLayout {
    const ROW_HEIGHT: f64 = 32.0;
    const BAR_HEIGHT: f64 = 14.0;
    const GAP: f64 = 16.0;

    fn of(run: &BatchRun, width: f64, keys_width: f64) -> Self {
        Self {
            scale: ((width - keys_width - Self::GAP - 4.0) / run.duration().max(1.0)).max(1.0),
            left: keys_width + Self::GAP,
        }
    }

    fn x(&self, time: f64) -> f64 {
        self.left + time * self.scale
    }

    fn bar_y(lane: usize) -> f64 {
        lane as f64 * Self::ROW_HEIGHT + (Self::ROW_HEIGHT - Self::BAR_HEIGHT) / 2.0
    }
}

/// The keys, each beside its searches and prefetches.
#[component]
pub(crate) fn KeyRows(
    tree: LoadedTree,
    run: BatchRun,
    keys: BatchKeys,
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
    let layout = (width > 0.0).then(|| RowsLayout::of(&run, width, keys_width));
    let lit = pointed();
    let mut batcher = batcher;
    let height = run.lanes().len() as f64 * RowsLayout::ROW_HEIGHT;
    rsx! {
        section {
            class: "batch-keys",
            "aria-label": "The keys, each beside its searches and prefetches",
            div { class: "batch-section-head",
                h2 { class: "batch-heading",
                    "Keys, with their searches and "
                    Term { concept: Concept::Prefetch, spotlight, "prefetches" }
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
                            Playback::seek((x - layout.left) / layout.scale);
                        },
                        for lane in 0..run.lanes().len() {
                            line {
                                class: "row-rule",
                                x1: layout.left,
                                x2: width - 4.0,
                                y1: RowsLayout::bar_y(lane) + RowsLayout::BAR_HEIGHT / 2.0,
                                y2: RowsLayout::bar_y(lane) + RowsLayout::BAR_HEIGHT / 2.0,
                            }
                        }
                        for (index , poll) in run.polls().iter().enumerate() {
                            {
                                let lane = poll.lane();
                                let start = run.start(index);
                                let y = RowsLayout::bar_y(lane);
                                let middle = y + RowsLayout::BAR_HEIGHT / 2.0;
                                let lit_class = if lit == Some(lane) { " lit" } else { "" };
                                let pending = poll
                                    .fetched()
                                    .and(run.read_at(index))
                                    .filter(|read| *read > start + 1.0);
                                rsx! {
                                    if let Some(read) = pending {
                                        line {
                                            class: "prefetch-ghost",
                                            x1: layout.x(start + 1.0),
                                            x2: layout.x(read),
                                            y1: middle,
                                            y2: middle,
                                        }
                                        line {
                                            class: "prefetch{lit_class}",
                                            style: "--s:{run.fetched_at(index)};--d:{read - run.fetched_at(index)};",
                                            x1: layout.x(start + 1.0),
                                            x2: layout.x(read),
                                            y1: middle,
                                            y2: middle,
                                        }
                                    }
                                    rect {
                                        class: "search{lit_class}",
                                        style: "--s:{start};",
                                        x: layout.x(start) + 0.5,
                                        y,
                                        rx: 2,
                                        width: (layout.scale - 1.0).max(1.0),
                                        height: RowsLayout::BAR_HEIGHT,
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
                for (lane , key) in keys.keys().iter().enumerate() {
                    KeyField {
                        key: "{lane}",
                        lane,
                        key_text: key.clone(),
                        tree: tree.clone(),
                        lit: lit == Some(lane),
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
                        span { class: "legend-mark search" }
                        "search"
                    }
                    span { class: "legend-item",
                        span { class: "legend-mark prefetch" }
                        "prefetch, pending until the key's next search"
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
            style: "transform:translate(0,{lane as f64 * RowsLayout::ROW_HEIGHT}px);",
            onmouseenter: move |_| pointed.set(Some(lane)),
            onmouseleave: move |_| pointed.set(None),
            button {
                class: "key-chip data",
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
