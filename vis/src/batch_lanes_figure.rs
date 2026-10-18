//! The post's figure of a batch (doc 14, Figure 4): four keys looked up
//! together on one thread, a row a key, played search by search: each
//! search lands as it runs, and from each prefetch to that key's next
//! search the line fills on its way, while the other keys search. Read from
//! bbtree's own run of the batch (`TreeView::trace_many`, `batch_run.rs`)
//! and played as the explorer plays it (`BatchRun::timeline`): time counted
//! in searches, since the trace says nothing of when a line arrives.
//! Pointing at a row lights its key.

use std::ops::Range;

use dioxus::prelude::*;

use crate::batch_run::BatchRun;
use crate::element_ids::ElementIds;
use crate::figure::{Figure, FigureFrame, TreeStore};
use crate::shared_link::ExplorerLink;
use crate::timeline::TimelineControls;
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

    /// How many lines are on their way while search `search` runs.
    fn on_their_way_during(&self, search: usize) -> usize {
        let during = search as f64 + 0.5;
        (0..self.run.polls().len())
            .filter(|&index| {
                self.on_its_way(index)
                    .is_some_and(|way| way.contains(&during))
            })
            .count()
    }

    /// The most lines on their way at once, over the batch.
    fn most_on_their_way(&self) -> usize {
        (0..self.run.polls().len())
            .map(|search| self.on_their_way_during(search))
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
            room: (250, 280),
            caption: rsx! {
                "Four "
                code { "u64" }
                " keys looked up together on one thread, a row each, time counted in searches. After each search, a key prefetches the next node it reads; while that line is on its way, the other keys search. "
                if most > 1 {
                    "Up to {most} lines are on their way at once; one key alone has one."
                }
            },
            if let Some(rows) = rows {
                BatchLanes { rows }
            }
        }
    }
}

/// The rows, played on the batch's timeline.
#[component]
fn BatchLanes(rows: LaneRows) -> Element {
    let element = ElementIds::of_current().id("batch-lanes");
    let now = use_signal(|| None::<usize>);
    let playing = use_signal(|| true);
    let mut pointed = use_signal(|| None::<usize>);
    let timeline = rows.run.timeline();
    let (run_at, run_timeline, controls_at) = (element.clone(), timeline.clone(), element.clone());
    use_future(move || {
        let (timeline, element) = (run_timeline.clone(), run_at.clone());
        async move { timeline.run(&element, now, playing).await }
    });
    let in_flight = now().map_or(0, |search| rows.on_their_way_during(search));
    let total = rows.run.duration();
    rsx! {
        div {
            id: element,
            class: if pointed().is_some() { "diagram timeline batch-lanes pointing" } else { "diagram timeline batch-lanes" },
            style: "--total:{total}",
            p { class: "batch-lanes-flight",
                "lines on their way: "
                span { class: "flight-count data", "{in_flight}" }
            }
            div { class: "batch-lanes-rows", onmouseleave: move |_| pointed.set(None),
                for (lane , key) in rows.keys.iter().enumerate() {
                    div {
                        key: "{lane}",
                        class: if pointed() == Some(lane) { "batch-lanes-row lit" } else { "batch-lanes-row" },
                        onmouseenter: move |_| pointed.set(Some(lane)),
                        span { class: "batch-lanes-key data", {key.as_str()} }
                        svg {
                            class: "batch-lanes-plot",
                            view_box: "0 0 {total} 1",
                            preserve_aspect_ratio: "none",
                            "aria-hidden": "true",
                            for index in rows.polls_of(lane) {
                                g { key: "{index}",
                                    if let Some(way) = rows.on_its_way(index) {
                                        rect {
                                            class: "batch-lanes-track",
                                            x: way.start,
                                            y: 0.4,
                                            width: way.end - way.start,
                                            height: 0.2,
                                        }
                                        rect {
                                            class: "batch-lanes-way",
                                            style: "--s:{way.start};--d:{way.end - way.start}",
                                            x: way.start,
                                            y: 0.4,
                                            width: way.end - way.start,
                                            height: 0.2,
                                        }
                                    }
                                    rect {
                                        class: "batch-lanes-search",
                                        style: "--s:{rows.run.start(index)}",
                                        x: rows.run.start(index) + 0.08,
                                        y: 0.1,
                                        width: 0.84,
                                        height: 0.8,
                                    }
                                }
                            }
                        }
                    }
                }
                div { class: "batch-lanes-playhead", "aria-hidden": "true" }
            }
            p { class: "batch-lanes-legend",
                span { class: "legend-mark search" }
                "search"
                span { class: "legend-mark prefetch" }
                "line on its way, from the key's prefetch to its next search"
            }
            TimelineControls {
                element: controls_at,
                timeline,
                now,
                playing,
                label: "The batch, search by search",
            }
        }
    }
}
