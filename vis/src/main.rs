//! Look keys up in a bbtree, in the browser, and watch how it finds them:
//! the path from the root, and each node's page, cache line by cache line,
//! drawn from bbtree's own read-only views of its nodes
//! (`BBTreeMap::inspect`), as bbtree stores them (`doc/10-visualization.md`).

mod concepts;
mod count_text;
mod icons;
mod inspected_tree;
mod memory_map;
mod node_inspector;
mod path_summary;
mod path_tree;
mod scenarios;
mod shortcuts;
mod slot_table;
mod spotlight;
mod trace;
mod tree_shape;
mod tree_summary;
mod workload;

use dioxus::prelude::*;
use web_time::{SystemTime, UNIX_EPOCH};

use crate::concepts::Concept;
use crate::inspected_tree::LoadedTree;
use crate::node_inspector::NodeInspector;
use crate::path_summary::PathSummary;
use crate::path_tree::PathTree;
use crate::shortcuts::Shortcuts;
use crate::spotlight::{HintLayer, Spotlight};
use crate::trace::{LookupBar, Trace, Tracer};
use crate::tree_shape::NodeAt;
use crate::tree_summary::TreeSummary;
use crate::workload::WorkloadPicker;

const MAIN_CSS: Asset = asset!("/assets/main.css");
const TAILWIND_CSS: Asset = asset!("/assets/tailwind.css");
const FONTS: &str = "https://fonts.googleapis.com/css2?family=IBM+Plex+Mono:wght@400;500&family=IBM+Plex+Sans:wght@400;500;600&family=Martian+Mono:wdth,wght@112.5,500;112.5,600&display=swap";

fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    let tree = use_signal(|| None::<LoadedTree>);
    let trace = use_signal(|| None::<Trace>);
    let inspected = use_signal(|| None::<NodeAt>);
    let error = use_signal(|| None::<String>);
    let seed = use_signal(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(1, |since| since.as_nanos() as u64 | 1)
    });
    let replay = use_signal(|| 0u64);
    let tracer = Tracer::new(trace, inspected, error, seed, replay);
    let spotlight = Spotlight::new(
        use_signal(|| None),
        use_signal(|| None),
        use_signal(|| None),
    );
    let loaded = tree.read().clone();
    let current = tracer.trace();
    let at = tracer.inspected();
    let replayed = tracer.replay_count();
    rsx! {
        document::Link { rel: "preconnect", href: "https://fonts.googleapis.com" }
        document::Link { rel: "stylesheet", href: FONTS }
        document::Link { rel: "stylesheet", href: MAIN_CSS }
        document::Link { rel: "stylesheet", href: TAILWIND_CSS }
        div { class: "app",
            header { class: "app-header",
                div { class: "brand",
                    h1 { class: "wordmark", "bbtree" }
                    p { class: "tagline",
                        "A B+-tree that compares short key summaries all at once. "
                        a { class: "tagline-link", href: Concept::decisions_link(), target: "_blank", rel: "noopener", "Design notes" }
                    }
                }
                WorkloadPicker { tree, tracer }
                if let Some(loaded) = loaded.clone() {
                    TreeSummary { tree: loaded, spotlight }
                }
            }
            if let Some(loaded) = loaded {
                LookupBar { tree: loaded.clone(), tracer, spotlight }
                main { class: "app-main",
                    if let Some(current) = current {
                        PathSummary { tree: loaded.clone(), trace: current.clone(), tracer, spotlight }
                        PathTree {
                            tree: loaded.clone(),
                            trace: current.clone(),
                            inspected: at,
                            replay: replayed,
                            tracer,
                            spotlight,
                        }
                        if let Some(at) = at {
                            NodeInspector {
                                key: "{at.level()}-{at.index()}",
                                tree: loaded,
                                at,
                                trace: current,
                                replay: replayed,
                                tracer,
                                spotlight,
                            }
                        }
                    } else {
                        p { class: "empty-state", "Look up a key to see the path bbtree takes to it." }
                    }
                }
            } else {
                div { class: "loading-state", role: "status",
                    div { class: "skeleton-path", "aria-hidden": "true",
                        for row in 0..4 {
                            span { class: "skeleton-card", style: "--row:{row};" }
                        }
                    }
                    p { "Building the tree…" }
                }
            }
            HintLayer { spotlight }
            Shortcuts { tree, tracer }
        }
    }
}
