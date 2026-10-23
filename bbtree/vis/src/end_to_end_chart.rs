//! The post's end-to-end card: real systems, before and after one of their
//! maps became bbtree, on their own benchmarks, on one thread: with more,
//! these systems are bound by their own locks and counters. Drawn as the
//! post's `end-to-end.svg` (`vis/figures/src/end_to_end.rs`) is: a row a
//! workload, grouped by system, the time as shipped a dark bar at 100%,
//! bbtree's a bright one inside it, the speedup at the row's end.

use std::rc::Rc;

use dioxus::prelude::*;

use crate::chart_scale::{FrameWidth, LinearScale, Nanos};
use crate::measurements::{IntegrationRow, Measurements};
use crate::repository_links::RepositoryLinks;
use crate::spotlight::{Hint, Spotlight};

/// A system's workloads, before and after, in the order measured.
#[derive(Clone, PartialEq, Debug)]
struct IntegratedSystem {
    name: String,
    rows: Vec<IntegrationRow>,
}

impl IntegratedSystem {
    /// How tall its name's row is, and each workload's.
    const HEADING: f64 = 30.0;
    const ROW: f64 = 24.0;

    /// Every system measured, in the order measured.
    fn all(measurements: &Measurements) -> Vec<Self> {
        (measurements.integrated_systems().into_iter())
            .map(|name| Self {
                rows: measurements.integration_rows(&name),
                name,
            })
            .collect()
    }

    /// The map the system ships, which bbtree replaced.
    fn shipped_map(&self) -> &str {
        self.rows.first().map_or("", |row| row.before.as_str())
    }

    fn repository_link(&self) -> Option<(String, &'static str)> {
        Some((self.name.clone(), self.rows.first()?.repository()?))
    }

    fn height(&self) -> f64 {
        Self::HEADING + self.rows.len() as f64 * Self::ROW
    }

    /// The top of its `index`th workload's row, below the system's own top.
    fn row_top(top: f64, index: usize) -> f64 {
        top + Self::HEADING + index as f64 * Self::ROW
    }
}

#[component]
pub(crate) fn EndToEndChart(measurements: Rc<Measurements>, spotlight: Spotlight) -> Element {
    let systems = IntegratedSystem::all(&measurements);
    if systems.is_empty() {
        return rsx! {
            p { class: "chart-note", "The integrations are still running." }
        };
    }
    let links: Vec<(String, &'static str)> = (systems.iter())
        .filter_map(IntegratedSystem::repository_link)
        .collect();
    let machine = measurements.machine();
    rsx! {
        div { class: "chart-keys",
            span { class: "chart-key",
                span { class: "chart-key-swatch", style: "background:var(--shipped)" }
                "as shipped, 100%"
            }
            span { class: "chart-key",
                span { class: "chart-key-swatch ours" }
                strong { "with bbtree" }
            }
        }
        SystemRows { systems, spotlight }
        p { class: "chart-note",
            "Each system's own benchmark, one thread, wall time per operation: the dark bar as shipped (100%), the bright one inside it with bbtree. Only the map changed, and where a system writes a batch, the call that hands it to the map whole (insert_many; Lance's, update_or_insert_many): crossbeam's SkipMap, or Lance's own arena skip list. More threads hit the systems' own locks first. Not shown, slower or unchanged: SlateDB's memtable_seek, 0.91–0.99× (its iterators read three rows ahead of a seek, which the adapter clones out); Tonbo's Transaction::get, 0.86× (it copies every row per call); Lance's lookups of absent keys, 1.00–1.04× (they plan a DataFusion query). {machine.name}."
            RepositoryLinks { links }
        }
    }
}

/// The systems' rows, drawn at the frame's width, a unit a pixel: a
/// system's name over its workloads, a bar a workload.
#[component]
fn SystemRows(systems: Vec<IntegratedSystem>, spotlight: Spotlight) -> Element {
    const BAR: f64 = 16.0;
    /// How far bbtree's bar sits inside the dark one, above and below.
    const INSET: f64 = 3.0;
    /// Room right of the bars for the speedup.
    const SPEEDUP: f64 = 56.0;
    let frame = FrameWidth::use_new();
    let width = frame.get();
    let bars_left = if FrameWidth::is_narrow(width) {
        112.0
    } else {
        170.0
    };
    // Each system's top, one under the other.
    let tops: Vec<f64> = (systems.iter())
        .scan(0.0, |next, system| {
            let top = *next;
            *next += system.height();
            Some(top)
        })
        .collect();
    let height: f64 = systems.iter().map(IntegratedSystem::height).sum();
    // The time as shipped is 1: every dark bar is full.
    let share = LinearScale::reaching(1.0, (bars_left, width - SPEEDUP));
    let mut spotlight = spotlight;
    rsx! {
        figure { class: "chart-panel wide",
            figcaption { class: "chart-title",
                strong { "Time per operation, relative to as shipped" }
                span { class: "chart-subtitle", "one thread; lower is better; the speedup at each row's end" }
            }
            div { class: "chart-frame", onresize: move |resized| frame.measure(resized),
                if width > 0.0 {
                    svg {
                        class: "chart-svg",
                        view_box: "0 0 {width} {height}",
                        role: "img",
                        "aria-label": "Time per operation with bbtree, relative to as shipped",
                        for (system, &system_top) in systems.iter().zip(&tops) {
                            text { class: "group-label", x: 0, y: system_top + 20.0, {system.name.clone()} }
                            text { class: "group-caption", x: bars_left, y: system_top + 20.0,
                                "ships {system.shipped_map()}"
                            }
                            for (index, row) in system.rows.iter().enumerate() {
                                {
                                    let row_top = IntegratedSystem::row_top(system_top, index);
                                    let relative = row.after_nanos / row.before_nanos;
                                    let speedup = row.before_nanos / row.after_nanos;
                                    let hint_title = format!("{} · {}: {speedup:.2}× faster", system.name, row.workload);
                                    let hint_body = format!(
                                        "{}\n{}: {}\nbbtree: {}",
                                        row.detail,
                                        row.before,
                                        Nanos(row.before_nanos),
                                        Nanos(row.after_nanos),
                                    );
                                    rsx! {
                                        g {
                                            class: "chart-row",
                                            onmouseenter: move |evt| {
                                                let point = evt.client_coordinates();
                                                spotlight.show_hint(Hint::new((point.x, point.y), hint_title.clone(), hint_body.clone()));
                                            },
                                            onmousemove: move |evt| {
                                                let point = evt.client_coordinates();
                                                spotlight.move_hint((point.x, point.y));
                                            },
                                            onmouseleave: move |_| spotlight.clear(),
                                            rect { class: "chart-hit", x: 0, y: row_top - (IntegratedSystem::ROW - BAR) / 2.0, width, height: IntegratedSystem::ROW }
                                            text { class: "chart-row-label", x: 14, y: row_top + BAR - 3.5, {row.workload.clone()} }
                                            path {
                                                class: "chart-bar",
                                                style: "fill:var(--shipped)",
                                                d: share.bar_path(1.0, row_top, BAR),
                                            }
                                            path {
                                                class: "chart-bar ours grow-x",
                                                style: "fill:var(--ours);--grow:{share.growth_ms(relative)}ms",
                                                d: share.bar_path(relative.min(1.0), row_top + INSET, BAR - 2.0 * INSET),
                                            }
                                            text {
                                                class: "row-speedup lands",
                                                style: "--grow:{share.growth_ms(relative)}ms",
                                                x: width,
                                                y: row_top + BAR - 3.5,
                                                text_anchor: "end",
                                                "{speedup:.2}×"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            div { class: "sr-only",
                table {
                    caption { "Time per operation with bbtree, relative to as shipped" }
                    thead {
                        tr {
                            th { scope: "col", "system" }
                            th { scope: "col", "workload" }
                            th { scope: "col", "as shipped" }
                            th { scope: "col", "with bbtree" }
                            th { scope: "col", "speedup" }
                        }
                    }
                    tbody {
                        for system in systems.iter() {
                            for row in system.rows.iter() {
                                tr {
                                    th { scope: "row", {system.name.clone()} }
                                    td { "{row.workload}: {row.detail}" }
                                    td { "{row.before}: {Nanos(row.before_nanos)}" }
                                    td { "{Nanos(row.after_nanos)}, {row.after_nanos / row.before_nanos * 100.0:.0}%" }
                                    td { "{row.before_nanos / row.after_nanos:.2}×" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
