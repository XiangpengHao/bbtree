//! The post: a column of prose, and figures wider than it, each drawn by
//! the explorer's own panels (`figure.rs`). The figures build their trees
//! as the reader comes to them, sharing the trees of one workload.

use dioxus::prelude::*;

use crate::figure::{Figure, FigureFrame, TreeStore};
use crate::path_summary::PathSummary;
use crate::path_tree::PathTree;
use crate::shared_link::ExplorerLink;
use crate::site::{Credit, SiteHeader, SitePage};
use crate::spotlight::{HintLayer, PointerHint};
use crate::workload::{InsertOrder, KeyKind, Workload};

#[component]
pub(crate) fn Post() -> Element {
    let trees = TreeStore::use_new();
    let hint = PointerHint::use_new();
    rsx! {
        document::Title { "How bbtree finds a key" }
        div { class: "post-page",
            SiteHeader { current: SitePage::Post }
            article { class: "post",
                header { class: "post-head",
                    p { class: "display post-kicker", "Draft" }
                    h1 { class: "post-title", "How bbtree finds a key" }
                }
                p {
                    "The post's text goes here. The figure below is drawn by the explorer's own panels, from bbtree's own trace of one lookup: the key 851191, in a map of a million "
                    code { "u64" }
                    " keys inserted in random order."
                }
                WayDownFigure { trees, hint }
                p {
                    "At each node on the way down, the search skips the bytes the node's keys share, counts the inline keys smaller than the key's, compares any ties in full, and goes down the edge the count names."
                }
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
            caption: rsx! {
                "The lookup of 851191 among a million "
                code { "u64" }
                " keys. Play it again, or step it, with the buttons above the key; point at a slot for its key."
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
