//! The post (doc/14-blogpost.md, whose text it shows): what bbtree is, how
//! fast, on one thread, on many, and in real systems
//! (`performance_cards.rs`), and how it works, in figures drawn by the
//! explorer's own panels (`figure.rs`). The figures build their trees as
//! the reader comes to them, sharing the trees of one workload.

use std::rc::Rc;

use dioxus::prelude::*;

use crate::batch_view::BatchView;
use crate::figure::{Figure, FigureFrame, TreeStore};
use crate::measurements::Measurements;
use crate::node_inspector::NodeInspector;
use crate::path_summary::PathSummary;
use crate::path_tree::PathTree;
use crate::performance_cards::PerformanceCards;
use crate::shared_link::ExplorerLink;
use crate::site::{AUTHOR_PAGE, Credit, SiteHeader, SitePage};
use crate::spotlight::{HintLayer, PointerHint, Spotlight};
use crate::workload::{InsertOrder, KeyKind, Workload};

#[component]
pub(crate) fn Post() -> Element {
    let trees = TreeStore::use_new();
    let hint = PointerHint::use_new();
    // The charts' own spotlight, for their hints: they light no node.
    let charts = Spotlight::use_new(hint);
    let measurements = use_hook(|| Rc::new(Measurements::of_ryzen_9900x()));
    rsx! {
        document::Title { "bbtree: fastest ordered map in the world" }
        div { class: "post-page",
            SiteHeader { current: SitePage::Post }
            article { class: "post",
                header { class: "post-head",
                    h1 { class: "post-title", "bbtree: fastest ordered map in the world" }
                }
                blockquote { class: "post-quote",
                    p {
                        "B-trees are by far the most important access path structure in database and file systems."
                    }
                    footer {
                        "— "
                        a {
                            href: "https://en.wikipedia.org/wiki/Jim_Gray_(computer_scientist)",
                            target: "_blank",
                            rel: "noopener",
                            "Jim Gray"
                        }
                    }
                }
                p {
                    strong { "bbtree" }
                    " is by far the fastest btree (ordered map):"
                }
                ol {
                    li {
                        "5x faster than std's "
                        code { "BTreeMap" }
                        " at single thread, 7x faster than "
                        code { "crossbeam-skiplist" }
                        " at 12 threads."
                    }
                    li { "30% lower memory usage, 5x lower p99 latency." }
                    li {
                        "Zero dependencies, "
                        code { "no_std" }
                        " compatible."
                    }
                    li {
                        code { "unsync::BBTreeMap" }
                        " is a "
                        strong { "drop-in replacement" }
                        " for std's "
                        code { "BTreeMap" }
                        "/"
                        code { "BTreeSet" }
                        "."
                    }
                    li {
                        "Production-ready by formal verification, model checking, fuzzing, and lots of testing."
                    }
                    li {
                        "Brought to you by decades of database research and careful curation by "
                        a { href: AUTHOR_PAGE, target: "_blank", rel: "noopener", "Xiangpeng Hao" }
                        "."
                    }
                }
                PerformanceCards { measurements, spotlight: charts }
                h2 { class: "post-section", "How it works" }
                h3 { class: "post-subsection", "Prefix key to improve comparisons" }
                WayDownFigure { trees, hint }
                LeafFigure { trees, hint }
                h3 { class: "post-subsection", "AVX512/NEON SIMD to improve search speed" }
                h3 { class: "post-subsection", "Batching to improve throughput" }
                BatchFigure { trees, hint }
                h3 { class: "post-subsection", "Careful layouts to improve cache locality" }
                h3 { class: "post-subsection", "Optimistic latch coupling to improve concurrency" }
                h3 { class: "post-subsection", "Formal verification to ensure correctness" }
            }
            Credit {}
            HintLayer { hint }
        }
    }
}

/// One lookup's way down, a search step at a time: the key, and the path's
/// cards from the root to the leaf.
#[component]
fn WayDownFigure(trees: TreeStore, hint: PointerHint) -> Element {
    let workload = Workload::new(KeyKind::Integers, 1_000_000, InsertOrder::Random);
    let figure = Figure::use_new(ExplorerLink::lookup(workload, "851191", None), trees, hint);
    let tracer = figure.tracer();
    let replay = tracer.replay_count();
    let descent = tracer.descent();
    rsx! {
        FigureFrame {
            figure: figure.clone(),
            room: (560, 560),
            caption: rsx! {
                "The lookup of 851191 among a million "
                code { "u64" }
                " keys. Each card is a node on its way down: the bytes it skips, its inline keys flashing as the search compares them all, the smaller ones dark. Play it again, or step it, with the buttons above the key."
            },
            if let (Some(tree), Some(trace)) = (figure.tree(), tracer.trace()) {
                PathSummary {
                    tree: tree.clone(),
                    trace: trace.clone(),
                    descent: descent.clone(),
                    tracer,
                    replay,
                }
                PathTree {
                    tree,
                    trace,
                    inspected: tracer.inspected(),
                    replay,
                    descent,
                    tracer,
                    spotlight: figure.spotlight(),
                }
            }
        }
    }
}

/// The same lookup's leaf, in the inspector: its fences and the prefix they
/// share, the search's steps, and its page as cache lines, with the lines
/// the search read.
#[component]
fn LeafFigure(trees: TreeStore, hint: PointerHint) -> Element {
    let workload = Workload::new(KeyKind::Integers, 1_000_000, InsertOrder::Random);
    let figure = Figure::use_still(
        ExplorerLink::lookup(workload, "851191", Some(0)),
        trees,
        hint,
    );
    let tracer = figure.tracer();
    rsx! {
        FigureFrame {
            figure: figure.clone(),
            room: (780, 680),
            caption: rsx! {
                "The leaf that holds 851191: the bytes its fences share, which its search skips; its inline keys; and its 1 KiB page as 64-byte cache lines, the lines the lookup read dotted. Point at a slot to see its key."
            },
            if let (Some(tree), Some(trace), Some(at)) = (figure.tree(), tracer.trace(), tracer.inspected()) {
                NodeInspector {
                    key: "{at.level()}-{at.index()}",
                    tree,
                    at,
                    trace,
                    replay: tracer.replay_count(),
                    descent: tracer.descent(),
                    tab: figure.tab(),
                    tracer,
                    spotlight: figure.spotlight(),
                }
            }
        }
    }
}

/// Four lookups on one thread, as `get_many` runs them: the keys and their
/// searches and prefetches, the nodes they read, and the code they run.
#[component]
fn BatchFigure(trees: TreeStore, hint: PointerHint) -> Element {
    let workload = Workload::new(KeyKind::Integers, 1_000_000, InsertOrder::Random);
    let link = ExplorerLink::batch(workload, ["131072", "500000", "851191", "999000"]);
    let figure = Figure::use_new(link, trees, hint);
    rsx! {
        FigureFrame {
            figure: figure.clone(),
            room: (715, 1655),
            caption: rsx! {
                "Four keys looked up together on one thread. Each key's row shows its searches, and its prefetches stretching to the search that reads what they fetched; the nodes below hold a token for each key. Play, pause or step it with the buttons."
            },
            if let Some(tree) = figure.tree().filter(|_| figure.tracer().trace().is_some()) {
                BatchView {
                    tree,
                    batcher: figure.batcher(),
                    tracer: figure.tracer(),
                    spotlight: figure.spotlight(),
                }
            }
        }
    }
}
