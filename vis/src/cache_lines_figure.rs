//! The post's figure of node layout (doc 14, Figure 3): one lookup's leaf,
//! a cell a 64-byte line, named by what the lines hold, the lines the
//! lookup read lit. Read from a real tree: bbtree's own page map and its
//! trace of the lookup (`memory_map.rs` marks the same reads in the
//! explorer). Only the leaf: this page runs bbtree built for wasm32, whose
//! 4-byte pointers give internal nodes another layout than a 64-bit
//! machine's, while a leaf of `u64` keys and values is the same on both.

use std::ops::Range;

use bbtree::inspect::SlotContents;
use dioxus::prelude::*;

use crate::figure::{Figure, FigureFrame, TreeStore};
use crate::inspected_tree::LoadedTree;
use crate::memory_map::{PageLines, PageMarks};
use crate::shared_link::ExplorerLink;
use crate::trace::Trace;
use crate::workload::{InsertOrder, KeyKind, Workload};

/// A lookup's leaf as the lookup read it, a line at a time.
#[derive(Clone, PartialEq)]
struct LeafLines {
    bytes: usize,
    /// What each line mostly holds, by the post's name for it.
    labels: Vec<&'static str>,
    read: Vec<bool>,
}

impl LeafLines {
    /// The leaf `trace` ended in.
    fn of_lookup(tree: &LoadedTree, trace: &Trace) -> Option<Self> {
        let step = trace.steps().last()?;
        let page = tree.page_map(step.at());
        let labels = PageLines::of(&page)
            .holds()
            .iter()
            .map(|holds| holds.map_or("", Self::label))
            .collect();
        Some(Self {
            bytes: page.size(),
            labels,
            read: PageMarks::of_step(step, true).lines_surely_read(&page),
        })
    }

    /// What the post calls what a line holds: its words, not the
    /// explorer's.
    fn label(contents: SlotContents) -> &'static str {
        match contents {
            SlotContents::Header | SlotContents::RootArea => "header",
            SlotContents::InlineKeys => "prefix keys",
            SlotContents::LengthTags => "length tags",
            SlotContents::Keys
            | SlotContents::HeadKey
            | SlotContents::Head
            | SlotContents::Values => "keys and values",
            SlotContents::Edges => "child pointers",
        }
    }

    fn lines_read(&self) -> usize {
        self.read.iter().filter(|read| **read).count()
    }

    /// Runs of lines that hold the same thing, each with its name.
    fn spans(&self) -> Vec<(&'static str, Range<usize>)> {
        let mut spans: Vec<(&'static str, Range<usize>)> = Vec::new();
        for (line, label) in self.labels.iter().enumerate() {
            match spans.last_mut() {
                Some((last, run)) if last == label => run.end = line + 1,
                _ => spans.push((label, line..line + 1)),
            }
        }
        spans
    }
}

#[component]
pub(crate) fn CacheLinesFigure(trees: TreeStore) -> Element {
    let workload = Workload::new(KeyKind::Integers, 1_000_000, InsertOrder::Random);
    let figure = Figure::use_new(ExplorerLink::lookup(workload, "851191", Some(0)), trees);
    let leaf = figure
        .tree()
        .zip(figure.tracer().trace())
        .and_then(|(tree, trace)| LeafLines::of_lookup(&tree, &trace));
    rsx! {
        FigureFrame {
            figure,
            room: (130, 150),
            caption: rsx! {
                "The leaf of one "
                code { "u64" }
                " lookup, a cell a 64-byte line. The lookup reads the lines lit: the prefix keys, then the one line that holds the key and its value. An internal node reads the same way: its prefix keys, then one child pointer."
            },
            if let Some(leaf) = leaf {
                div { class: "diagram",
                    p { class: "cache-lines-name",
                        strong { "leaf, {leaf.bytes / 1024} KiB" }
                        ": {leaf.lines_read()} of {leaf.read.len()} lines read"
                    }
                    div {
                        class: "cache-lines-grid",
                        style: "--lines:{leaf.read.len()}",
                        for (line , read) in leaf.read.iter().enumerate() {
                            span {
                                key: "line-{line}",
                                class: if *read { "cache-line read" } else { "cache-line" },
                            }
                        }
                        for (label , run) in leaf.spans() {
                            span {
                                key: "span-{run.start}",
                                class: "cache-lines-span",
                                style: "grid-column:{run.start + 1} / {run.end + 1}",
                                {label}
                            }
                        }
                    }
                }
            }
        }
    }
}
