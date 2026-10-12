//! Look keys up in a bbtree, in the browser, and watch how it finds them:
//! the path from the root, and each node's page, cache line by cache line,
//! drawn from bbtree's own read-only views of its nodes
//! (`BBTreeMap::inspect`), as bbtree stores them (`doc/10-visualization.md`).

mod count_text;
mod inspected_tree;
mod memory_map;
mod node_inspector;
mod path_tree;
mod trace;
mod tree_shape;
mod tree_summary;
mod workload;

use dioxus::prelude::*;
use web_time::{SystemTime, UNIX_EPOCH};

use crate::inspected_tree::LoadedTree;
use crate::node_inspector::NodeInspector;
use crate::path_tree::PathTree;
use crate::trace::{Trace, TraceBar, Tracer};
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
    let tracer = Tracer::new(trace, inspected, error, seed);
    let loaded = tree.read().clone();
    let current = tracer.trace();
    let at = tracer.inspected();
    rsx! {
        document::Link { rel: "preconnect", href: "https://fonts.googleapis.com" }
        document::Link { rel: "stylesheet", href: FONTS }
        document::Link { rel: "stylesheet", href: MAIN_CSS }
        document::Link { rel: "stylesheet", href: TAILWIND_CSS }
        div { class: "flex h-screen flex-col",
            header { class: "flex flex-wrap items-center gap-x-6 gap-y-2 border-b border-[var(--rule)] bg-[var(--panel)] px-5 py-2.5",
                h1 { class: "wordmark", "bbtree" }
                WorkloadPicker { tree, tracer }
                if let Some(loaded) = loaded.clone() {
                    TreeSummary { tree: loaded }
                }
            }
            if let Some(loaded) = loaded {
                TraceBar { tree: loaded.clone(), tracer }
                main { class: "flex min-h-0 flex-1 flex-col",
                    if let Some(current) = current {
                        PathTree { tree: loaded.clone(), trace: current.clone(), inspected: at, tracer }
                        if let Some(at) = at {
                            NodeInspector { tree: loaded, at, step: current.step_at(at).cloned(), tracer }
                        }
                    } else {
                        p { class: "m-auto text-[14px] text-[var(--graphite)]",
                            "Look up a key to see the path bbtree takes to it."
                        }
                    }
                }
            } else {
                p { class: "m-auto text-[14px] text-[var(--graphite)]", "Building the tree…" }
            }
        }
    }
}
