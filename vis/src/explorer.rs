//! The explorer: look keys up in a bbtree of the reader's choosing and
//! watch how it finds them: the path from the root, and each node's page,
//! cache line by cache line, drawn from bbtree's own read-only views of its
//! nodes (`unsync::BBTreeMap::inspect`), as bbtree stores them; or many
//! keys at once, as `get_many` interleaves their lookups. One screen, its
//! state in its address (`doc/10-visualization.md`).

use dioxus::prelude::*;
use web_time::{SystemTime, UNIX_EPOCH};

use crate::batch_view::{BatchView, Batcher, ModeSwitch};
use crate::node_inspector::{InspectorTab, NodeInspector};
use crate::path_summary::PathSummary;
use crate::path_tree::PathTree;
use crate::shared_link::SharedLink;
use crate::shortcuts::Shortcuts;
use crate::site::{Credit, SiteHeader, SitePage};
use crate::spotlight::{HintLayer, PointerHint, Spotlight};
use crate::trace::{LookupMenu, Tracer};
use crate::tree_summary::TreeSummary;
use crate::workload::{Builder, WorkloadMenu};

#[component]
pub(crate) fn Explorer() -> Element {
    let seed = use_signal(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(1, |since| since.as_nanos() as u64 | 1)
    });
    let tracer = Tracer::use_new(seed);
    let batcher = Batcher::use_new(seed);
    let builder = Builder::use_new();
    let hint = PointerHint::use_new();
    let spotlight = Spotlight::use_new(hint);
    let search_open = use_signal(|| false);
    let tab = use_signal(|| InspectorTab::Page);
    let loaded = builder.tree();
    let current = tracer.trace();
    let at = tracer.inspected();
    let replayed = tracer.replay_count();
    let descent = tracer.descent();
    let batch = batcher.is_shown();
    rsx! {
        document::Title { "bbtree: how a lookup finds its key" }
        div { class: "app",
            SiteHeader { current: SitePage::Explorer,
                ModeSwitch { batcher, tracer, tree: loaded.clone() }
                if let Some(loaded) = loaded.clone().filter(|_| !batch) {
                    LookupMenu { tree: loaded, tracer, open: search_open }
                }
                WorkloadMenu { builder, tracer }
                if let Some(loaded) = loaded.clone() {
                    TreeSummary { tree: loaded, spotlight }
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
                        Credit {}
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
            HintLayer { hint }
            Shortcuts {
                builder,
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
