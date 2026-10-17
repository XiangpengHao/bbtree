//! The post's figure of a batch (doc 14, Figure 4): four keys looked up
//! together on one thread, a row a key, each search a block and, from each
//! prefetch to that key's next search, the line on its way. Read from
//! bbtree's own run of the batch (`TreeView::trace_many`, `batch_run.rs`),
//! and drawn whole, time counted in searches as the explorer counts it:
//! the trace says nothing of when a line arrives.

use std::ops::Range;

use dioxus::prelude::*;

use crate::batch_run::BatchRun;
use crate::figure::{Figure, FigureFrame, TreeStore};
use crate::shared_link::ExplorerLink;
use crate::workload::{InsertOrder, KeyKind, Workload};

/// A batch's run, as rows of searches and lines on their way.
#[derive(Clone, PartialEq)]
struct LaneRows {
    run: BatchRun,
    keys: Vec<String>,
}

impl LaneRows {
    /// When the line poll `index` fetched is on its way: from the end of
    /// the search that fetched it to its key's next search, which reads it;
    /// `None` if nothing came between.
    fn on_its_way(&self, index: usize) -> Option<Range<f64>> {
        let start = self.run.start(index);
        self.run.polls()[index]
            .fetched()
            .and(self.run.read_at(index))
            .filter(|read| *read > start + 1.0)
            .map(|read| start + 1.0..read)
    }

    /// The most lines on their way at once, over the batch.
    fn most_on_their_way(&self) -> usize {
        let polls = self.run.polls().len();
        (0..polls)
            .map(|search| {
                let during = search as f64 + 0.5;
                (0..polls)
                    .filter(|&index| {
                        self.on_its_way(index)
                            .is_some_and(|way| way.contains(&during))
                    })
                    .count()
            })
            .max()
            .unwrap_or(0)
    }

    /// The searches of lane `lane`, by poll.
    fn polls_of(&self, lane: usize) -> impl Iterator<Item = usize> + '_ {
        (0..self.run.polls().len()).filter(move |&index| self.run.polls()[index].lane() == lane)
    }
}

#[component]
pub(crate) fn BatchLanesFigure(trees: TreeStore) -> Element {
    let workload = Workload::new(KeyKind::Integers, 1_000_000, InsertOrder::Random);
    let link = ExplorerLink::batch(workload, ["131072", "500000", "851191", "999000"]);
    let figure = Figure::use_new(link, trees);
    let rows = figure.tree().and_then(|tree| {
        let keys = figure.batcher().keys_for(&tree).keys().to_vec();
        let run = tree.trace_many(&keys).ok()?;
        Some(LaneRows { run, keys })
    });
    let most = rows.as_ref().map_or(0, LaneRows::most_on_their_way);
    rsx! {
        FigureFrame {
            figure,
            room: (210, 230),
            caption: rsx! {
                "Four "
                code { "u64" }
                " keys looked up together on one thread, a row each, time counted in searches. After each search, a key prefetches the next node it reads; while that line is on its way, the other keys search. "
                if most > 1 {
                    "Up to {most} lines are on their way at once; one key alone has one."
                }
            },
            if let Some(rows) = rows {
                div { class: "diagram batch-lanes",
                    for (lane , key) in rows.keys.iter().enumerate() {
                        span { key: "key-{lane}", class: "batch-lanes-key data", {key.as_str()} }
                        svg {
                            class: "batch-lanes-row",
                            view_box: "0 0 {rows.run.duration()} 1",
                            preserve_aspect_ratio: "none",
                            "aria-hidden": "true",
                            for index in rows.polls_of(lane) {
                                g { key: "{index}",
                                    if let Some(way) = rows.on_its_way(index) {
                                        rect {
                                            class: "batch-lanes-way",
                                            x: way.start,
                                            y: 0.4,
                                            width: way.end - way.start,
                                            height: 0.2,
                                        }
                                    }
                                    rect {
                                        class: "batch-lanes-search",
                                        x: rows.run.start(index) + 0.08,
                                        y: 0.1,
                                        width: 0.84,
                                        height: 0.8,
                                    }
                                }
                            }
                        }
                    }
                    p { class: "batch-lanes-legend",
                        span { class: "legend-mark search" }
                        "search"
                        span { class: "legend-mark prefetch" }
                        "line on its way, from the key's prefetch to its next search"
                    }
                }
            }
        }
    }
}
