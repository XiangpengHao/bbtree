//! The post's figure of node layout (doc 14, Figure 3): one lookup's leaf,
//! a cell a 64-byte line, named by what the lines hold, the lines the
//! lookup read lighting up in the order its search read them. Read from a
//! real tree: bbtree's own page map and its trace of the lookup
//! (`memory_map.rs` marks the same reads in the explorer). The reader looks
//! up another key of the tree and watches its reads; pointing at a line
//! says what it holds.
//!
//! Only the leaf: this page runs bbtree built for wasm32, whose 4-byte
//! pointers give internal nodes another layout than a 64-bit machine's,
//! while a leaf of `u64` keys and values is the same on both.

use std::ops::Range;

use bbtree::inspect::SlotContents;
use dioxus::prelude::*;

use crate::element_ids::ElementIds;
use crate::figure::{Figure, FigureFrame, TreeStore};
use crate::icons::{Icon, IconGlyph};
use crate::inspected_tree::LoadedTree;
use crate::memory_map::{PageLines, PageMarks};
use crate::shared_link::ExplorerLink;
use crate::timeline::{Timeline, TimelineControls};
use crate::workload::{InsertOrder, KeyKind, Workload};

/// A lookup's leaf as the lookup read it, a line at a time.
#[derive(Clone, PartialEq)]
struct LeafLines {
    key: String,
    bytes: usize,
    /// What each line mostly holds, by the post's name for it.
    labels: Vec<&'static str>,
    /// When the lookup read each line: the step of the figure's timeline,
    /// lines read together on one step.
    read_at: Vec<Option<usize>>,
    steps: usize,
}

impl LeafLines {
    const LINE: usize = 64;

    /// The leaf the lookup of `key` in `tree` ended in.
    fn of_lookup(tree: &LoadedTree, key: &str) -> Option<Self> {
        let trace = tree.trace(key).ok()?;
        let step = trace.steps().last()?;
        let page = tree.page_map(step.at());
        let labels = PageLines::of(&page)
            .holds()
            .iter()
            .map(|holds| holds.map_or("", Self::label))
            .collect();
        let order = PageMarks::of_step(step, true).line_read_order(&page);
        let mut ranks: Vec<usize> = order.iter().flatten().copied().collect();
        ranks.sort_unstable();
        ranks.dedup();
        let read_at = order
            .iter()
            .map(|rank| rank.and_then(|rank| ranks.iter().position(|seen| *seen == rank)))
            .collect();
        Some(Self {
            key: key.to_string(),
            bytes: page.size(),
            labels,
            read_at,
            steps: ranks.len().max(1),
        })
    }

    /// What the post calls what a line holds: its words, not the
    /// explorer's.
    fn label(contents: SlotContents) -> &'static str {
        match contents {
            SlotContents::Header | SlotContents::RootArea => "header",
            SlotContents::InlineKeys => "prefix keys",
            SlotContents::HashTags => "hash tags",
            SlotContents::Keys
            | SlotContents::HeadKey
            | SlotContents::Head
            | SlotContents::Values => "keys and values",
            SlotContents::Edges => "child pointers",
        }
    }

    fn lines_read(&self) -> usize {
        self.read_at.iter().flatten().count()
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

    /// Line `line` in words: what it holds, where, and whether it was read.
    fn describe(&self, line: usize) -> String {
        let start = line * Self::LINE;
        let read = if self.read_at[line].is_some() {
            "read"
        } else {
            "not read"
        };
        let holds = match self.labels[line] {
            "" => "padding",
            label => label,
        };
        format!(
            "line {line}: {holds}, bytes {start}–{}, {read}",
            start + Self::LINE - 1
        )
    }

    /// A step a read.
    fn timeline(&self) -> Timeline {
        Timeline::of_steps(self.steps, 700.0)
    }
}

#[component]
pub(crate) fn CacheLinesFigure(trees: TreeStore) -> Element {
    let workload = Workload::new(KeyKind::Integers, 1_000_000, InsertOrder::Random);
    let figure = Figure::use_new(ExplorerLink::lookup(workload, "851191", Some(0)), trees);
    // The key looked up: the link's, until the reader asks for another.
    let mut key = use_signal(|| "851191".to_string());
    let mut seed = use_signal(|| 1u64);
    let tree = figure.tree();
    let leaf = tree
        .as_ref()
        .and_then(|tree| LeafLines::of_lookup(tree, &key()));
    rsx! {
        FigureFrame {
            figure,
            room: (190, 230),
            caption: rsx! {
                "The leaf of one "
                code { "u64" }
                " lookup, a cell a 64-byte line, lit in the order the lookup reads them: the prefix keys, then the one line that holds the key and its value. Look up another key: still three lines. An internal node reads the same way, its prefix keys, then one child pointer."
            },
            if let (Some(tree), Some(leaf)) = (tree, leaf) {
                div { class: "diagram",
                    div { class: "figure-toolbar",
                        p { class: "figure-readout",
                            "lookup of "
                            span { class: "data", "{leaf.key}" }
                        }
                        button {
                            class: "button",
                            onclick: move |_| {
                                let next = seed() + 1;
                                seed.set(next);
                                if let Some(other) = tree.random_key(next) {
                                    key.set(other);
                                }
                            },
                            IconGlyph { icon: Icon::Shuffle }
                            "Another key"
                        }
                    }
                    // Keyed by the key, so that another lookup reads anew.
                    for shown in std::iter::once(leaf) {
                        LeafReads { key: "{shown.key}", leaf: shown }
                    }
                }
            }
        }
    }
}

/// The leaf's lines lighting up as the lookup reads them, and what the
/// line pointed at holds.
#[component]
fn LeafReads(leaf: LeafLines) -> Element {
    let element = ElementIds::of_current().id("leaf-reads");
    let now = use_signal(|| None::<usize>);
    let playing = use_signal(|| true);
    let mut pointed = use_signal(|| None::<usize>);
    let timeline = leaf.timeline();
    let (run_at, run_timeline, controls_at) = (element.clone(), timeline.clone(), element.clone());
    use_future(move || {
        let (timeline, element) = (run_timeline.clone(), run_at.clone());
        async move { timeline.run(&element, now, playing).await }
    });
    let readout = match pointed() {
        Some(line) => leaf.describe(line),
        None => format!(
            "leaf, {} KiB: {} of {} lines read",
            leaf.bytes / 1024,
            leaf.lines_read(),
            leaf.read_at.len()
        ),
    };
    rsx! {
        div { id: element, class: "timeline leaf-reads",
            p { class: "cache-lines-name", {readout} }
            div {
                class: "cache-lines-grid",
                style: "--lines:{leaf.read_at.len()}",
                onmouseleave: move |_| pointed.set(None),
                for (line , read_at) in leaf.read_at.iter().enumerate() {
                    button {
                        key: "line-{line}",
                        class: if read_at.is_some() { "cache-line read" } else { "cache-line" },
                        style: if let Some(step) = read_at { "--s:{step}" } else { "" },
                        "aria-label": leaf.describe(line),
                        onmouseenter: move |_| pointed.set(Some(line)),
                        onfocus: move |_| pointed.set(Some(line)),
                        onclick: move |_| pointed.set(Some(line)),
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
            TimelineControls {
                element: controls_at,
                timeline,
                now,
                playing,
                label: "The lookup's reads, one at a time",
            }
        }
    }
}
