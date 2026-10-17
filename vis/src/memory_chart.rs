//! The post's memory card: the heap each map holds for 10M entries, its
//! keys' own allocations included, counted by the allocator: a bar a map,
//! solid to what it holds once built, lighter on to the most it held at
//! once while the entries went in. A kind of key at a time.

use std::rc::Rc;

use dioxus::prelude::*;

use crate::chart_scale::{FrameWidth, LinearScale};
use crate::measurements::{MapMemory, MeasuredKeys, MeasuredMap, Measurements};
use crate::repository_links::RepositoryLinks;
use crate::spotlight::{Hint, Spotlight};

/// Bytes as megabytes, to three digits at most.
struct Megabytes(f64);

impl std::fmt::Display for Megabytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let megabytes = self.0 / 1e6;
        if megabytes >= 1000.0 {
            write!(f, "{:.2} GB", megabytes / 1e3)
        } else {
            write!(f, "{megabytes:.0} MB")
        }
    }
}

#[component]
pub(crate) fn MemoryChart(measurements: Rc<Measurements>, spotlight: Spotlight) -> Element {
    let mut keys = use_signal(|| MeasuredKeys::U64);
    let frame = FrameWidth::use_new();
    let machine = measurements.machine();
    let bars: Vec<(MeasuredMap, MapMemory)> = (MeasuredMap::MEMORY.into_iter())
        .filter_map(|map| Some((map, measurements.memory(map, keys())?)))
        .collect();
    let peaked = bars.iter().any(|(_, memory)| memory.peaked());
    rsx! {
        div { class: "chart-controls",
            div { class: "segmented", role: "group", "aria-label": "Keys",
                for choice in MeasuredKeys::ALL {
                    button {
                        class: "segment-button",
                        "aria-pressed": if keys() == choice { "true" } else { "false" },
                        title: choice.detail(),
                        onclick: move |_| keys.set(choice),
                        {choice.label()}
                    }
                }
            }
        }
        div { class: "chart-frame", onresize: move |resized| frame.measure(resized),
            if frame.get() > 0.0 {
                // Keyed by the keys, so that new ones grow their bars anew.
                for shown in std::iter::once(keys()) {
                    MemoryBars {
                        key: "{shown:?}",
                        bars: bars.clone(),
                        title: format!("Memory · {}", shown.label()),
                        width: frame.get(),
                        spotlight,
                    }
                }
            }
        }
        p { class: "chart-note",
            "Heap held for 10M entries, keys' own allocations included; lower is better. Counted by the allocator, the bytes asked for. "
            if peaked {
                "The lighter bar: the most held at once while the entries went in. "
            } else {
                "While the entries went in, no map held more than it holds once built. "
            }
            "{machine.name}."
            RepositoryLinks {
                links: (MeasuredMap::MEMORY.into_iter())
                    .filter_map(MeasuredMap::repository_link)
                    .collect::<Vec<_>>(),
            }
        }
    }
}

/// A bar a map, `width` wide: what it holds, then on to its peak.
#[component]
fn MemoryBars(
    bars: Vec<(MeasuredMap, MapMemory)>,
    title: String,
    width: f64,
    spotlight: Spotlight,
) -> Element {
    let narrow = FrameWidth::is_narrow(width);
    let label = if narrow { 124.0 } else { 160.0 };
    const ROW: f64 = 40.0;
    const BAR: f64 = 18.0;
    const TOP: f64 = 4.0;
    const AXIS: f64 = 24.0;
    let height = TOP + ROW * bars.len() as f64 + AXIS;
    let greatest = (bars.iter())
        .map(|(_, memory)| memory.peak_bytes.max(memory.built_bytes))
        .fold(0.0, f64::max);
    let x = LinearScale::fitting(greatest, (label, width - if narrow { 72.0 } else { 160.0 }));
    let mut spotlight = spotlight;
    rsx! {
        figure { class: "chart-panel wide",
            div {
                svg {
                    class: "chart-svg",
                    width: "{width}",
                    height: "{height}",
                    view_box: "0 0 {width} {height}",
                    role: "img",
                    "aria-label": title.clone(),
                    for tick in x.ticks_apart(64.0) {
                        line {
                            class: if tick == 0.0 { "chart-axis" } else { "chart-grid" },
                            x1: x.at(tick),
                            x2: x.at(tick),
                            y1: TOP,
                            y2: height - AXIS + 4.0,
                        }
                        text { class: "chart-tick", x: x.at(tick), y: height - 6.0, text_anchor: "middle",
                            "{Megabytes(tick)}"
                        }
                    }
                    for (index, (map, memory)) in bars.iter().copied().enumerate() {
                        {
                            let y = TOP + ROW * index as f64 + (ROW - BAR) / 2.0;
                            let peak = memory.peak_bytes.max(memory.built_bytes);
                            let title = format!("{} · {}", Megabytes(memory.built_bytes), map.label());
                            let body = format!(
                                "{} once built; {} at the most while the entries went in",
                                Megabytes(memory.built_bytes),
                                Megabytes(peak)
                            );
                            rsx! {
                                g {
                                    class: if map.is_ours() { "chart-row ours" } else { "chart-row" },
                                    onmouseenter: move |evt| {
                                        let point = evt.client_coordinates();
                                        spotlight.show_hint(Hint::new((point.x, point.y), title.clone(), body.clone()));
                                    },
                                    onmousemove: move |evt| {
                                        let point = evt.client_coordinates();
                                        spotlight.move_hint((point.x, point.y));
                                    },
                                    onmouseleave: move |_| spotlight.clear(),
                                    rect {
                                        class: "chart-hit",
                                        x: 0,
                                        y: y - (ROW - BAR) / 2.0,
                                        width: width,
                                        height: ROW,
                                    }
                                    text { class: "chart-row-label", x: label - 12.0, y: y + BAR - 4.0, text_anchor: "end",
                                        {map.label()}
                                    }
                                    path { class: "peak-bar grow-x", style: "fill:{map.fill_color()};--grow:{x.growth_ms(peak)}ms", d: x.bar_path(peak, y, BAR) }
                                    path { class: "chart-bar grow-x", style: "fill:{map.fill_color()};--grow:{x.growth_ms(memory.built_bytes)}ms", d: x.bar_path(memory.built_bytes, y, BAR) }
                                    text { class: "chart-value lands", style: "--grow:{x.growth_ms(peak)}ms", x: x.at(peak) + 10.0, y: y + BAR - 4.0,
                                        tspan { class: "memory-built", "{Megabytes(memory.built_bytes)}" }
                                        if memory.peaked() {
                                            tspan { class: "memory-peak", "  peak {Megabytes(peak)}" }
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
                    caption { {title.clone()} }
                    thead {
                        tr {
                            th { scope: "col", "map" }
                            th { scope: "col", "once built" }
                            th { scope: "col", "peak while inserting" }
                        }
                    }
                    tbody {
                        for (map, memory) in bars.iter() {
                            tr {
                                th { scope: "row", {map.label()} }
                                td { "{Megabytes(memory.built_bytes)}" }
                                td { "{Megabytes(memory.peak_bytes)}" }
                            }
                        }
                    }
                }
            }
        }
    }
}
