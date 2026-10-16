//! Look keys up in a bbtree, in the browser, and watch how it finds them:
//! the path from the root, and each node's page, cache line by cache line,
//! drawn from bbtree's own read-only views of its nodes
//! (`BBTreeMap::inspect`), as bbtree stores them; or many keys at once, as
//! `get_many` interleaves their lookups (`doc/10-visualization.md`).

mod batch_run;
mod batch_timeline;
mod batch_tree;
mod batch_view;
mod concepts;
mod count_text;
mod descent;
mod fences;
mod icons;
mod inspected_tree;
mod lane_program;
mod memory_map;
mod node_inspector;
mod path_summary;
mod path_tree;
mod popover;
mod query_key;
mod scenarios;
mod shared_link;
mod shortcuts;
mod slot_table;
mod spotlight;
mod trace;
mod tree_shape;
mod tree_summary;
mod workload;

use dioxus::prelude::*;
use web_time::{SystemTime, UNIX_EPOCH};

use crate::batch_view::{BatchView, Batcher, ModeSwitch};
use crate::descent::Descent;
use crate::icons::{Icon, IconGlyph};
use crate::inspected_tree::LoadedTree;
use crate::node_inspector::{InspectorTab, NodeInspector};
use crate::path_summary::PathSummary;
use crate::path_tree::PathTree;
use crate::shared_link::SharedLink;
use crate::shortcuts::Shortcuts;
use crate::spotlight::{HintLayer, Spotlight};
use crate::trace::{LookupMenu, Trace, Tracer};
use crate::tree_shape::NodeAt;
use crate::tree_summary::TreeSummary;
use crate::workload::{Builder, Workload, WorkloadMenu};

const MAIN_CSS: Asset = asset!("/assets/main.css");
const REPOSITORY: &str = "https://github.com/XiangpengHao/bbtree";
const AUTHOR_PAGE: &str = "https://xiangpeng.systems";
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
    let tracer = Tracer::new(
        trace,
        inspected,
        error,
        seed,
        replay,
        use_signal(Descent::default),
    );
    let batcher = Batcher::new(use_signal(|| false), use_signal(|| None), seed);
    let builder = Builder::new(
        tree,
        use_signal(Workload::first),
        use_signal(|| None),
        use_signal(|| None),
    );
    let spotlight = Spotlight::new(
        use_signal(|| None),
        use_signal(|| None),
        use_signal(|| None),
        use_signal(|| false),
        use_signal(|| None),
    );
    let search_open = use_signal(|| false);
    let tab = use_signal(|| InspectorTab::Page);
    let loaded = tree.read().clone();
    let current = tracer.trace();
    let at = tracer.inspected();
    let replayed = tracer.replay_count();
    let descent = tracer.descent();
    let batch = batcher.is_shown();
    rsx! {
        document::Link { rel: "preconnect", href: "https://fonts.googleapis.com" }
        document::Link { rel: "stylesheet", href: FONTS }
        document::Link { rel: "stylesheet", href: MAIN_CSS }
        document::Link { rel: "stylesheet", href: TAILWIND_CSS }
        div { class: "app",
            header { class: "toolbar",
                h1 { class: "wordmark", "bbtree" }
                ModeSwitch { batcher, tracer, tree: loaded.clone() }
                if let Some(loaded) = loaded.clone().filter(|_| !batch) {
                    LookupMenu { tree: loaded, tracer, open: search_open }
                }
                WorkloadMenu { builder, tracer }
                if let Some(loaded) = loaded.clone() {
                    TreeSummary { tree: loaded, spotlight }
                }
                div { class: "toolbar-end",
                    a {
                        class: "toolbar-icon-link",
                        href: REPOSITORY,
                        target: "_blank",
                        rel: "noopener",
                        title: "bbtree on GitHub",
                        "aria-label": "bbtree on GitHub",
                        IconGlyph { icon: Icon::GitHub }
                    }
                }
            }
            if let Some(loaded) = loaded.clone().filter(|_| batch) {
                main { class: "workspace batch",
                    BatchView {
                        tree: loaded,
                        batcher,
                        tracer,
                        spotlight,
                    }
                }
            } else if let (Some(loaded), Some(current)) = (loaded.clone(), current) {
                main { class: "workspace",
                    div { class: "path-column",
                        PathSummary {
                            tree: loaded.clone(),
                            trace: current.clone(),
                            descent: descent.clone(),
                            tracer,
                            replay: replayed,
                        }
                        PathTree {
                            tree: loaded.clone(),
                            trace: current.clone(),
                            inspected: at,
                            replay: replayed,
                            descent: descent.clone(),
                            tracer,
                            spotlight,
                        }
                        footer { class: "credit",
                            "Created with care by "
                            a { href: AUTHOR_PAGE, target: "_blank", rel: "noopener", "Xiangpeng Hao" }
                        }
                    }
                    if let Some(at) = at {
                        NodeInspector {
                            key: "{at.level()}-{at.index()}",
                            tree: loaded.clone(),
                            at,
                            trace: current.clone(),
                            replay: replayed,
                            descent,
                            tab,
                            tracer,
                            spotlight,
                        }
                    }
                }
            } else if loaded.is_some() {
                p { class: "empty-state", "Look up a key to see the path bbtree takes to it." }
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
            Shortcuts {
                tree,
                tracer,
                batcher,
                search_open,
            }
            SharedLink {
                builder,
                tracer,
                batcher,
                tab,
            }
        }
    }
}
