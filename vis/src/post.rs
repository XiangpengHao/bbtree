//! The post (doc/14-blogpost.md): what bbtree is, how fast, on one
//! thread, on many, and in real systems (`performance_cards.rs`), and how
//! it is made, in figures drawn by the explorer's own panels
//! (`figure.rs`). The figures build their trees as the reader comes to
//! them, sharing the trees of one workload.

use std::rc::Rc;

use dioxus::prelude::*;

use crate::batch_view::BatchView;
use crate::figure::{Figure, FigureFrame, TreeStore};
use crate::measurements::{MeasuredMap, MeasuredOperation, Measurements};
use crate::node_inspector::NodeInspector;
use crate::path_summary::PathSummary;
use crate::path_tree::PathTree;
use crate::performance_cards::PerformanceCards;
use crate::shared_link::ExplorerLink;
use crate::site::{Credit, SiteHeader, SitePage};
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
        document::Title { "The fastest ordered map" }
        div { class: "post-page",
            SiteHeader { current: SitePage::Post }
            article { class: "post",
                header { class: "post-head",
                    h1 { class: "post-title", "The fastest ordered map" }
                }
                p {
                    strong { "bbtree" }
                    " is an ordered map for Rust that many threads share. For one thread, "
                    code { "bbtree::unsync::BBTreeMap" }
                    " is the same tree, which takes "
                    code { "std::collections::BTreeMap" }
                    "'s place without a change to the code around it: the same API, the same behavior. Both have no dependencies, and need no "
                    code { "std" }
                    ", only an allocator."
                }
                blockquote { class: "post-quote",
                    p { "[Jim Gray on B-trees: the quote and its source to confirm.]" }
                }
                p {
                    "B-trees hold the indexes of nearly every database. bbtree builds its tree the way those databases do, for memory instead of disk: nodes are pages, a lookup reads a few cache lines of each, and many lookups can share one thread's wait for memory."
                }
                h2 { class: "post-section", "How fast" }
                p {
                    "Against std's B-tree, the skip list concurrent Rust code reaches for, and the other concurrent ordered maps on crates.io, with 10M keys of four kinds: integers, UUIDs, Wikipedia's titles and JSON objects' keys; then in real systems, end to end."
                }
                HeadlineResult { measurements: measurements.clone() }
                PerformanceCards { measurements: measurements.clone(), spotlight: charts }
                h2 { class: "post-section", "How it is made" }
                h3 { class: "post-subsection", "Compare a few bytes, read a few lines" }
                p {
                    "A lookup in a B-tree is a chain of cache misses, one node after another, and each node's search reads more of it. bbtree keeps, beside each node's keys, a few bytes of each, its "
                    em { "inline key" }
                    ": the bytes after those that all of the node's keys share. The search compares the looked-up key's inline key with all of them at once, in a few vector instructions and no branches, and counts the smaller ones; that count is where the key goes. Only keys whose inline keys tie are compared in full."
                }
                p {
                    "What the node's keys share comes from where the node sits: the separators either side of it in its parent, its "
                    em { "fences" }
                    ", bound every key below. The deeper the node, the closer its fences, the more bytes they share, and the further into the key its inline keys reach."
                }
                WayDownFigure { trees, hint }
                p {
                    "In the leaf, all of that is a few lines of its page: the header, the inline keys, compared all at once, and the one key they tie with, compared in full. The rest of the page stays where it is."
                }
                LeafFigure { trees, hint }
                h3 { class: "post-subsection", "Many lookups, one thread" }
                p {
                    "Each node a lookup reads is a cache miss the processor waits for. Lookups of other keys don't depend on it, and could run while it waits. "
                    code { "get_many" }
                    " runs them that way: each key is a small state machine that searches the node it holds, asks for the line it reads next, and steps aside; the others search while that line comes. All on one thread, with no locks or atomics."
                }
                BatchFigure { trees, hint }
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

/// The post's result in one sentence, from the measurements: bbtree's gets
/// against std's on one thread, one key a call and batched, and the
/// systems end to end with bbtree.
#[component]
fn HeadlineResult(measurements: Rc<Measurements>) -> Element {
    let single = measurements.single_thread_speedups(
        (MeasuredMap::BBTree, MeasuredOperation::Get),
        (MeasuredMap::StdBTree, MeasuredOperation::Get),
    );
    let batched = measurements.single_thread_speedups(
        (MeasuredMap::BBTree, MeasuredOperation::GetBatch),
        (MeasuredMap::StdBTree, MeasuredOperation::Get),
    );
    let systems = measurements.integrated_systems();
    let end_to_end = measurements.integration_speedups();
    let (Some(single), Some(batched), Some(end_to_end)) = (single, batched, end_to_end) else {
        return rsx! {};
    };
    let named = match systems.split_last() {
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        None => return rsx! {},
    };
    rsx! {
        p {
            "On one thread, bbtree's gets are "
            strong { "{single} faster" }
            " than std's BTreeMap's across the four kinds of keys, {batched} batched. With bbtree in place of their skip list, {named} run their own benchmarks "
            strong { "{end_to_end} faster" }
            "."
        }
    }
}
