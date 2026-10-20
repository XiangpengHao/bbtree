//! The post's first card: one thread, each map as a program without
//! threads would use it, at 10M entries, one panel a kind of key: the mean
//! time of an operation, operations back to back, a bar a map from zero.
//! On bbtree's row a second, hollow bar is the same work through its batch
//! API. std's hash map comes last, set apart: it keeps no order, and where
//! the work scans, its row says it can't.

use std::rc::Rc;

use dioxus::prelude::*;

use crate::chart_scale::{FrameWidth, LinearScale, Nanos};
use crate::measurements::{MeasuredKeys, MeasuredMap, MeasuredOperation, Measurements};
use crate::repository_links::RepositoryLinks;
use crate::spotlight::{Hint, Spotlight};

/// One row of a panel: a map, its mean time, and bbtree's batched one.
#[derive(Clone, Copy, PartialEq, Debug)]
struct PanelRow {
    map: MeasuredMap,
    /// None for the hash map where the work scans: it can't.
    nanos: Option<f64>,
    /// The same work through bbtree's batch API (`get_many_copied`, 64
    /// keys a call; `insert_many`, 16), where it has one.
    batched_nanos: Option<f64>,
}

impl PanelRow {
    /// The rows of the panel of `keys`: the maps measured, and the hash
    /// map where the work scans.
    fn of(
        measurements: &Measurements,
        keys: MeasuredKeys,
        operation: MeasuredOperation,
    ) -> Vec<Self> {
        (MeasuredMap::SINGLE_THREAD.into_iter())
            .filter_map(|map| {
                let nanos = measurements.single_thread_nanos(map, keys, operation);
                let cannot_scan = operation == MeasuredOperation::Scan && !map.is_ordered();
                if nanos.is_none() && !cannot_scan {
                    return None;
                }
                Some(Self {
                    map,
                    nanos,
                    batched_nanos: (operation.batched())
                        .filter(|_| map == MeasuredMap::BBTree)
                        .and_then(|batch| measurements.single_thread_nanos(map, keys, batch)),
                })
            })
            .collect()
    }
}

/// The card's chart: its controls, a panel a kind of key, and a note on
/// what was measured.
#[component]
pub(crate) fn SingleThreadChart(measurements: Rc<Measurements>, spotlight: Spotlight) -> Element {
    let mut operation = use_signal(|| MeasuredOperation::Get);
    let machine = measurements.machine();
    let panels: Vec<(MeasuredKeys, Vec<PanelRow>)> = (MeasuredKeys::ALL.into_iter())
        .map(|keys| (keys, PanelRow::of(&measurements, keys, operation())))
        .collect();
    // One scale for the four panels, so that the kinds of keys compare.
    let greatest = (panels.iter())
        .flat_map(|(_, rows)| rows.iter().filter_map(|row| row.nanos))
        .fold(0.0, f64::max);
    let links: Vec<(String, &'static str)> = (MeasuredMap::SINGLE_THREAD.into_iter())
        .filter_map(MeasuredMap::repository_link)
        .collect();
    rsx! {
        div { class: "chart-controls",
            div { class: "segmented", role: "group", "aria-label": "Operation",
                for (choice, label, title) in [
                    (MeasuredOperation::Get, "Get", "Lookups, half of which hit and half miss"),
                    (MeasuredOperation::Insert, "Insert", "Inserts of new keys into a growing map"),
                    (MeasuredOperation::Update, "Update", "A change to the value of a key the map holds"),
                    (MeasuredOperation::Scan, "Scan", "100 entries in key order from a key the map holds"),
                ] {
                    button {
                        class: "segment-button",
                        "aria-pressed": if operation() == choice { "true" } else { "false" },
                        title,
                        onclick: move |_| operation.set(choice),
                        {label}
                    }
                }
            }
            if operation().batched().is_some() {
                div { class: "chart-keys",
                    span { class: "chart-key",
                        span { class: "chart-key-swatch hollow" }
                        "batched"
                    }
                }
            }
        }
        // Keyed by the operation, so that a new one grows its bars anew.
        for shown in std::iter::once(operation()) {
            div { key: "{shown:?}", class: "panel-grid",
                for (keys, rows) in panels.clone() {
                    TimePanel { keys, rows, greatest, spotlight }
                }
            }
        }
        p { class: "chart-note",
            "Mean time per operation, one thread, 10M entries; lower is better. Get: half hits, half misses. Scan: 100 entries in order. Batched: get_many_copied, 64 keys a call; insert_many, 16. scc TreeIndex and concurrent-map are concurrent maps, used from one thread. std's HashMap keeps no order, so it can't scan. {machine.name} ({machine.detail})."
            RepositoryLinks { links }
        }
    }
}

/// One kind of key's rows, a bar a map from zero to the panels' `greatest`
/// time; bbtree's row holds two, its own above and its batched one,
/// hollow, below.
#[component]
fn TimePanel(
    keys: MeasuredKeys,
    rows: Vec<PanelRow>,
    greatest: f64,
    spotlight: Spotlight,
) -> Element {
    const ROW: f64 = 32.0;
    const BAR: f64 = 11.0;
    const TOP: f64 = 4.0;
    const AXIS: f64 = 26.0;
    // Drawn at the panel's width, a unit a pixel, so its text reads at the
    // size the page sets, on a phone as on a desk.
    let frame = FrameWidth::use_new();
    let width = frame.get();
    let label = if FrameWidth::is_narrow(width) {
        136.0
    } else {
        150.0
    };
    let height = TOP + ROW * rows.len() as f64 + AXIS;
    let times = LinearScale::reaching(greatest, (label, width - 56.0));
    let mut spotlight = spotlight;
    rsx! {
        figure { class: "chart-panel wide",
            figcaption { class: "panel-title",
                strong { {keys.label()} }
                span { class: "panel-detail", " · {keys.detail()}" }
            }
            div { class: "chart-frame", onresize: move |resized| frame.measure(resized),
            if width > 0.0 {
            svg {
                class: "chart-svg",
                view_box: "0 0 {width} {height}",
                role: "img",
                "aria-label": "{keys.label()}: mean time per operation by map",
                for tick in times.ticks() {
                    line {
                        class: if tick == 0.0 { "chart-axis" } else { "chart-grid" },
                        x1: times.at(tick),
                        x2: times.at(tick),
                        y1: TOP,
                        y2: height - AXIS + 4.0,
                    }
                    text {
                        class: "chart-tick",
                        x: times.at(tick),
                        y: height - 8.0,
                        text_anchor: "middle",
                        if tick == 0.0 { "0" } else { "{Nanos(tick)}" }
                    }
                }
                for (index, row) in rows.iter().copied().enumerate() {
                    {
                        let row_top = TOP + ROW * index as f64;
                        let middle = row_top + ROW / 2.0;
                        let ordered = row.map.is_ordered();
                        // bbtree's two bars sit either side of the middle; the
                        // hash map's rises to make room for its note.
                        let bar_top = match (row.batched_nanos, ordered) {
                            (Some(_), _) => middle - BAR - 1.0,
                            (None, true) => middle - BAR / 2.0,
                            (None, false) => middle - BAR / 2.0 - 6.0,
                        };
                        let hint_title = match row.nanos {
                            Some(nanos) => format!("{} · {}", Nanos(nanos), row.map.label()),
                            None => format!("{}: can't scan", row.map.label()),
                        };
                        let hint_body = match row.batched_nanos {
                            None if !ordered => format!(
                                "a hash table: it keeps no order, so it can't scan a range or find the next key; mean time per operation, {}",
                                keys.label()
                            ),
                            Some(batched) => format!("{} batched; mean time per operation, {}", Nanos(batched), keys.label()),
                            None => format!("mean time per operation, {}", keys.label()),
                        };
                        rsx! {
                            if !ordered {
                                line { class: "unordered-rule", x1: 0, x2: width - 8.0, y1: row_top, y2: row_top }
                            }
                            g {
                                class: match (row.map.is_ours(), ordered) {
                                    (true, _) => "chart-row ours",
                                    (false, true) => "chart-row",
                                    (false, false) => "chart-row unordered",
                                },
                                onmouseenter: move |evt| {
                                    let point = evt.client_coordinates();
                                    spotlight.show_hint(Hint::new((point.x, point.y), hint_title.clone(), hint_body.clone()));
                                },
                                onmousemove: move |evt| {
                                    let point = evt.client_coordinates();
                                    spotlight.move_hint((point.x, point.y));
                                },
                                onmouseleave: move |_| spotlight.clear(),
                                rect { class: "chart-hit", x: 0, y: row_top, width, height: ROW }
                                text {
                                    class: "chart-row-label",
                                    x: label - 12.0,
                                    y: if row.batched_nanos.is_some() || row.nanos.is_none() { middle + 4.0 } else { bar_top + BAR - 1.5 },
                                    text_anchor: "end",
                                    {row.map.label()}
                                }
                                if let Some(nanos) = row.nanos {
                                    if !ordered {
                                        text { class: "chart-row-note", x: label + 4.0, y: bar_top + BAR + 11.0,
                                            "unordered, no scans"
                                        }
                                    }
                                    path {
                                        class: if ordered { "chart-bar grow-x" } else { "chart-bar unordered grow-x" },
                                        style: "fill:{row.map.fill_color()};stroke:{row.map.line_color()};--grow:{times.growth_ms(nanos)}ms",
                                        d: times.bar_path(nanos, bar_top, BAR),
                                    }
                                    text { class: "chart-value lands", style: "--grow:{times.growth_ms(nanos)}ms", x: times.at(nanos) + 6.0, y: bar_top + BAR - 1.5,
                                        "{Nanos(nanos)}"
                                    }
                                } else {
                                    text { class: "chart-row-note", x: label + 4.0, y: middle + 4.0,
                                        "can't scan: it keeps no order"
                                    }
                                }
                                if let Some(batched) = row.batched_nanos {
                                    path {
                                        class: "chart-bar hollow grow-x",
                                        style: "stroke:{row.map.line_color()};--grow:{times.growth_ms(batched)}ms",
                                        d: times.bar_path(batched, middle + 1.0, BAR),
                                    }
                                    text { class: "chart-value lands", style: "--grow:{times.growth_ms(batched)}ms", x: times.at(batched) + 6.0, y: middle + BAR - 0.5,
                                        "{Nanos(batched)}"
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
                    caption { "{keys.label()}: mean time per operation" }
                    thead {
                        tr {
                            th { scope: "col", "map" }
                            th { scope: "col", "one key a call" }
                            th { scope: "col", "batched" }
                        }
                    }
                    tbody {
                        for row in rows.iter() {
                            tr {
                                th { scope: "row", {row.map.label()} }
                                td { {row.nanos.map(|nanos| Nanos(nanos).to_string()).unwrap_or_else(|| "can't scan".to_string())} }
                                td { {row.batched_nanos.map(|batched| Nanos(batched).to_string()).unwrap_or_default()} }
                            }
                        }
                    }
                }
            }
        }
    }
}
