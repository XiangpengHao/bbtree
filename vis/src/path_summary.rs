//! Above the path: the controls that play the lookup's way down, a search
//! step at a time (`descent.rs`), and the key, walking down with it; and the
//! path's legend.

use dioxus::prelude::*;

use crate::concepts::{Concept, Term};
use crate::descent::{Descent, DescentControls};
use crate::inspected_tree::LoadedTree;
use crate::query_key::QueryKey;
use crate::spotlight::Spotlight;
use crate::trace::{Trace, Tracer};

#[component]
pub(crate) fn PathSummary(
    tree: LoadedTree,
    trace: Trace,
    descent: Descent,
    tracer: Tracer,
    /// Changes with every lookup.
    replay: u64,
) -> Element {
    let outcome = trace.outcome_text(tree.shape());
    rsx! {
        div { class: "path-summary",
            DescentControls { key: "{replay}", descent: descent.clone(), tracer }
            QueryKey { tree: tree.clone(), trace: trace.clone(), descent }
            p { class: "sr-only", role: "status", {outcome} }
        }
    }
}

/// What the path's marks mean, in its top corner.
#[component]
pub(crate) fn PathLegend(spotlight: Spotlight) -> Element {
    rsx! {
        div { class: "path-legend", "aria-label": "What the path's marks mean",
            span { class: "legend-item",
                span { class: "ruler-swatch slot-smaller" }
                "smaller key"
            }
            span { class: "legend-item",
                span { class: "ruler-swatch slot-larger" }
                "larger key"
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
                span { class: "ruler-swatch slot-room" }
                "empty slot"
            }
            span { class: "legend-item",
                span { class: "notch-swatch" }
                Term { concept: Concept::Child, spotlight, "child taken" }
            }
        }
    }
}
