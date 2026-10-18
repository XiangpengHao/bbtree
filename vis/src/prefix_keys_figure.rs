//! The post's figure of prefix keys (doc 14, Figure 1): four keys whose
//! first 8 bytes tie, and an 8-byte window that slides past the bytes they
//! share, to bytes that tell them apart: why a node skips what its keys
//! share. Real neighbors from the benchmarks' key sets (Wikipedia titles
//! with underscores for spaces, as stored); every byte shown is the keys'
//! own. The slide plays once the reader sees it; the reader can put the
//! window back, and pick another key set.

use dioxus::prelude::*;

use crate::element_ids::ElementIds;
use crate::how_it_works::Diagram;
use crate::timeline::Timeline;

/// Four neighbors of one key set, and the windows of them a search
/// compares.
#[derive(Clone, Copy, PartialEq)]
struct SharedPrefixExample {
    name: &'static str,
    keys: [&'static str; 4],
}

impl SharedPrefixExample {
    /// The bytes of a key a string node keeps as its prefix key.
    const WIDTH: usize = 8;

    const ALL: [Self; 3] = [
        Self {
            name: "Wikipedia titles",
            keys: [
                "List_of_bridges_in_Afghanistan",
                "List_of_bridges_in_Alabama",
                "List_of_bridges_in_Alaska",
                "List_of_bridges_in_Albania",
            ],
        },
        Self {
            name: "Linux paths",
            keys: [
                "drivers/net/ethernet/intel/e1000e/80003es2lan.c",
                "drivers/net/ethernet/intel/e1000e/82571.c",
                "drivers/net/ethernet/intel/e1000e/Makefile",
                "drivers/net/ethernet/intel/e1000e/defines.h",
            ],
        },
        Self {
            name: "URLs",
            keys: [
                "https://www.aproposdecriture.com/tag/victor-hugo",
                "https://www.aproposdecriture.com/tag/word",
                "https://www.aproposdecriture.com/tag/youtube",
                "https://www.aproposdecriture.com/tag/zola",
            ],
        },
    ];

    /// How many leading bytes every key shares.
    fn shared_len(&self) -> usize {
        let first = self.keys[0].as_bytes();
        self.keys[1..]
            .iter()
            .map(|key| {
                first
                    .iter()
                    .zip(key.as_bytes())
                    .take_while(|(a, b)| a == b)
                    .count()
            })
            .min()
            .unwrap_or(first.len())
    }

    /// `key`'s `WIDTH` bytes from `offset`, a dot for each zero byte that
    /// pads a window past the key's end.
    fn window(key: &str, offset: usize) -> String {
        (offset..offset + Self::WIDTH)
            .map(|at| key.as_bytes().get(at).map_or('·', |&byte| byte as char))
            .collect()
    }

    /// The two moments the figure plays: the window at the keys' start,
    /// held long enough to see the tie, then its slide past what they
    /// share.
    fn timeline() -> Timeline {
        Timeline::of_steps(2, 900.0).with_slow_start(1.0, 1000.0)
    }
}

#[component]
pub(crate) fn PrefixKeysFigure() -> Element {
    let chosen = use_signal(|| 0usize);
    let example = SharedPrefixExample::ALL[chosen()];
    rsx! {
        Diagram {
            caption: rsx! {
                "Four neighboring {example.name}. Their first 8 bytes are the same, so a plain prefix decides nothing. They share their first {example.shared_len()} bytes: a node whose keys all share them skips them, and keeps the next 8 bytes, which tell the keys apart."
            },
            div { class: "diagram",
                // Keyed by the key set, so that another set plays anew.
                for shown in std::iter::once(example) {
                    WindowSlide { key: "{shown.name}", example: shown, chosen }
                }
            }
        }
    }
}

/// The keys, the window sliding along them, what it holds, and what that
/// decides: played on one timeline, `--t` from 0 to 2; and the switches of
/// the window and of the key set, `chosen`.
#[component]
fn WindowSlide(example: SharedPrefixExample, chosen: Signal<usize>) -> Element {
    let element = ElementIds::of_current().id("window-slide");
    let now = use_signal(|| None::<usize>);
    let playing = use_signal(|| true);
    let timeline = SharedPrefixExample::timeline();
    let run_at = element.clone();
    use_future(move || {
        let (timeline, element) = (timeline.clone(), run_at.clone());
        async move { timeline.run(&element, now, playing).await }
    });
    let shared = example.shared_len();
    let skipped = now() == Some(1);
    let mut chosen = chosen;
    let (first_at, skip_at) = (element.clone(), element.clone());
    rsx! {
        div {
            id: element,
            class: "timeline window-slide",
            style: "--shared:{shared}",
            div { class: "figure-toolbar",
            div { class: "segmented", role: "group", "aria-label": "Key set",
                for (index , choice) in SharedPrefixExample::ALL.iter().enumerate() {
                    button {
                        key: "{choice.name}",
                        class: "segment-button",
                        "aria-pressed": if chosen() == index { "true" } else { "false" },
                        onclick: move |_| chosen.set(index),
                        {choice.name}
                    }
                }
            }
            div { class: "segmented", role: "group", "aria-label": "Window",
                button {
                    class: "segment-button",
                    "aria-pressed": if skipped { "false" } else { "true" },
                    onclick: move |_| Timeline::seek(&first_at, 0.5),
                    "first 8 bytes"
                }
                button {
                    class: "segment-button",
                    "aria-pressed": if skipped { "true" } else { "false" },
                    onclick: move |_| Timeline::seek(&skip_at, 1.5),
                    "past the shared {shared}"
                }
            }
            }
            div { class: "window-keys",
                for key in example.keys {
                    div { key: "{key}", class: "window-row",
                        span { class: "window-key data",
                            span { class: "window-shared", {&key[..shared]} }
                            {&key[shared..]}
                            span { class: "window-frame", "aria-hidden": "true" }
                        }
                        span { class: "window-bytes data",
                            span { class: "byte-chip tied", {SharedPrefixExample::window(key, 0)} }
                            span { class: "byte-chip apart", {SharedPrefixExample::window(key, shared)} }
                        }
                    }
                }
            }
            p { class: "window-verdict",
                span { class: "verdict tied", "All four tie: compare the full keys, from the heap." }
                span { class: "verdict apart", "All four differ: decided in a register, no heap read." }
            }
        }
    }
}
