//! The lookup in one line above its path: where it ended, how many nodes
//! and cache lines it read, what the path's marks mean, and a way to watch
//! it go down again.

use dioxus::prelude::*;

use crate::concepts::{Concept, Term};
use crate::icons::{Icon, IconGlyph};
use crate::inspected_tree::LoadedTree;
use crate::spotlight::Spotlight;
use crate::trace::{Trace, Tracer};

#[component]
pub(crate) fn PathSummary(
    tree: LoadedTree,
    trace: Trace,
    tracer: Tracer,
    spotlight: Spotlight,
) -> Element {
    let outcome = trace.outcome_text(tree.shape());
    let nodes = trace.steps().len();
    let (lines, maybe) = trace.lines_read(&tree);
    let mut tracer = tracer;
    rsx! {
        div { class: "path-summary",
            div { class: "summary-lead", role: "status",
                p { class: "summary-outcome",
                    code { class: "summary-key data", {trace.query().to_string()} }
                    " "
                    {outcome}
                }
                p { class: "summary-cost",
                    span { class: "cost-figure data", "{nodes}" }
                    " nodes, "
                    span { class: "cost-figure data",
                        if maybe > 0 { "{lines}–{lines + maybe}" } else { "{lines}" }
                    }
                    " "
                    Term { concept: Concept::CacheLine, spotlight, "cache lines" }
                    " read"
                }
            }
            div { class: "path-legend", "aria-label": "What the path's marks mean",
                span { class: "legend-item",
                    span { class: "ruler-swatch slot-key" }
                    "key"
                }
                span { class: "legend-item",
                    span { class: "ruler-swatch slot-room" }
                    "empty"
                }
                span { class: "legend-item",
                    span { class: "ruler-swatch slot-tied" }
                    Term { concept: Concept::Tie, spotlight, "tied" }
                }
                span { class: "legend-item",
                    span { class: "ruler-swatch slot-found" }
                    "found"
                }
                span { class: "legend-item",
                    span { class: "notch-swatch" }
                    Term { concept: Concept::Child, spotlight, "child taken" }
                }
            }
            button {
                class: "button button-quiet",
                title: "Watch the lookup go down again",
                onclick: move |_| tracer.replay(),
                IconGlyph { icon: Icon::Replay }
                "Replay"
            }
        }
    }
}
