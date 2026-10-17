//! The post's second card: the maps threads share, as threads are added,
//! at 10M entries, on 96 cores of AWS's c8a, a thread a core, not on the
//! desktop the other cards ran on: throughput, all threads together, a
//! line a map. Gets
//! (half of them misses; bbtree's batched ones a line of their own),
//! inserts, updates, and mixes of 10% updates with 90% lookups or 90%
//! scans; a kind of key at a time.
//!
//! The lines are drawn for reading, not decoration: thread counts to
//! scale from zero, so that a map whose throughput grows with its threads
//! draws a straight line from the corner; bbtree's lines highlighted, the
//! others dark; DashMap, a hash map, for reference, faint and dotted, as a
//! different kind of map; each line named at its end, so there is no
//! legend to match colors against; and a crosshair that reads every line
//! at the thread count under the pointer.

use std::rc::Rc;

use dioxus::prelude::*;

use crate::chart_scale::{FrameWidth, LinearScale, SmoothLine};
use crate::measurements::{MeasuredKeys, MeasuredMap, MeasuredOperation, Measurements};
use crate::repository_links::RepositoryLinks;
use crate::spotlight::{Hint, Spotlight};

/// The workloads the card offers.
const WORKLOADS: [(MeasuredOperation, &str); 5] = [
    (MeasuredOperation::Get, "Get"),
    (MeasuredOperation::Insert, "Insert"),
    (MeasuredOperation::Update, "Update"),
    (MeasuredOperation::Mixed, "90/10 get/update"),
    (MeasuredOperation::ScanMixed, "90/10 scan/update"),
];

/// One map's line: throughput by thread count, of its calls one key at a
/// time or through its batch API.
#[derive(Clone, PartialEq, Debug)]
struct ConcurrentLine {
    map: MeasuredMap,
    batched: bool,
    points: Vec<(u32, f64)>,
}

impl ConcurrentLine {
    fn label(&self) -> String {
        if self.batched {
            format!("{} (batched)", self.map.label())
        } else if !self.map.is_ordered() {
            format!("{} (unordered, hashtable)", self.map.label())
        } else {
            self.map.label().to_string()
        }
    }

    /// The class of its marks: bbtree's highlighted, a hash map's faint.
    fn series_class(&self) -> &'static str {
        if self.map.is_ours() {
            "chart-series ours"
        } else if !self.map.is_ordered() {
            "chart-series unordered"
        } else {
            "chart-series"
        }
    }

    /// The lines of `operation` with `keys`: a map each, and bbtree's
    /// batched one where it has a batch API for it.
    fn of(
        measurements: &Measurements,
        keys: MeasuredKeys,
        operation: MeasuredOperation,
    ) -> Vec<Self> {
        let mut lines: Vec<Self> = (MeasuredMap::CONCURRENT.into_iter())
            .map(|map| Self {
                map,
                batched: false,
                points: measurements.concurrent_mops(map, keys, operation),
            })
            .collect();
        if let Some(batch) = operation.batched() {
            lines.insert(
                1,
                Self {
                    map: MeasuredMap::BBTree,
                    batched: true,
                    points: measurements.concurrent_mops(MeasuredMap::BBTree, keys, batch),
                },
            );
        }
        lines.retain(|line| !line.points.is_empty());
        lines
    }
}

#[component]
pub(crate) fn ConcurrentChart(spotlight: Spotlight) -> Element {
    let measurements = use_hook(|| Rc::new(Measurements::of_epyc_9r45()));
    let mut keys = use_signal(|| MeasuredKeys::WikiTitles); // SCREENSHOT-TEMP
    let mut operation = use_signal(|| MeasuredOperation::Get);
    let frame = FrameWidth::use_new();
    let machine = measurements.machine();
    let label = (WORKLOADS.iter())
        .find(|(choice, _)| *choice == operation())
        .map_or("", |(_, label)| label);
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
            div { class: "segmented", role: "group", "aria-label": "Workload",
                for (choice, label) in WORKLOADS {
                    button {
                        class: "segment-button",
                        "aria-pressed": if operation() == choice { "true" } else { "false" },
                        onclick: move |_| operation.set(choice),
                        {label}
                    }
                }
            }
        }
        div { class: "chart-frame", onresize: move |resized| frame.measure(resized),
            if frame.get() > 0.0 {
                // Keyed by what it shows, so that new lines draw anew.
                for (shown_keys, shown_operation) in std::iter::once((keys(), operation())) {
                    ThroughputLines {
                        key: "{shown_keys:?}-{shown_operation:?}",
                        lines: ConcurrentLine::of(&measurements, shown_keys, shown_operation),
                        title: format!("{label} · {}", shown_keys.label()),
                        width: frame.get(),
                        spotlight,
                    }
                }
            }
        }
        p { class: "chart-note",
            "All threads together, 10M entries; higher is better. A thread a core. "
            match operation() {
                MeasuredOperation::Get => "Get: half hits, half misses; batched: get_many_copied, 64 keys a call. ",
                MeasuredOperation::Mixed => "90% lookups, 10% updates. ",
                MeasuredOperation::ScanMixed => "90% scans of 100 entries, 10% updates. ",
                _ => "",
            }
            "{machine.name} ({machine.detail})."
            RepositoryLinks {
                links: (MeasuredMap::CONCURRENT.into_iter())
                    .filter_map(MeasuredMap::repository_link)
                    .collect::<Vec<_>>(),
            }
        }
    }
}

/// A name at a line's end, moved off its neighbors'.
#[derive(Clone, PartialEq, Debug)]
struct EndLabel {
    map: MeasuredMap,
    name: String,
    value: f64,
    /// Where the line ends, and where its name goes.
    end_y: f64,
    y: f64,
}

impl EndLabel {
    /// The labels of lines ending at `ends`, each as near its line's end
    /// as `gap` from its neighbors and the band `(top, bottom)` allow.
    fn placed(
        ends: Vec<(MeasuredMap, String, f64, f64)>,
        gap: f64,
        top: f64,
        bottom: f64,
    ) -> Vec<Self> {
        let mut labels: Vec<Self> = (ends.into_iter())
            .map(|(map, name, value, end_y)| Self {
                map,
                name,
                value,
                end_y,
                y: end_y,
            })
            .collect();
        labels.sort_by(|a, b| a.end_y.total_cmp(&b.end_y));
        // Down from the top, each at least `gap` below the one above; then
        // up from the bottom, so the last stays in the band.
        let mut floor = top;
        for label in &mut labels {
            label.y = label.y.max(floor);
            floor = label.y + gap;
        }
        let mut ceiling = bottom;
        for label in labels.iter_mut().rev() {
            label.y = label.y.min(ceiling);
            ceiling = label.y - gap;
        }
        labels
    }
}

/// Throughput by thread count, a line a map, `width` wide: each line named
/// at its end, or, narrow, in a key under the plot.
#[component]
fn ThroughputLines(
    lines: Vec<ConcurrentLine>,
    title: String,
    width: f64,
    spotlight: Spotlight,
) -> Element {
    /// An end name's average character, at its 12.5px.
    const NAME_CHARACTER: f64 = 6.6;
    let narrow = FrameWidth::is_narrow(width);
    let height = if narrow { 300.0 } else { 380.0 };
    // Room for the longest name at the lines' ends, past its value.
    let longest_name = (lines.iter())
        .map(|line| line.label().chars().count())
        .max();
    let right = if narrow {
        16.0
    } else {
        (66.0 + longest_name.unwrap_or(0) as f64 * NAME_CHARACTER).max(196.0)
    };
    const LEFT: f64 = 44.0;
    const TOP: f64 = 30.0;
    const BOTTOM: f64 = 40.0;
    let thread_counts: Vec<u32> = {
        let mut counts: Vec<u32> = (lines.iter())
            .flat_map(|line| line.points.iter().map(|(threads, _)| *threads))
            .collect();
        counts.sort_unstable();
        counts.dedup();
        counts
    };
    // Thread counts to scale, from zero.
    let most = thread_counts.last().copied().unwrap_or(1).max(1);
    let x_of =
        move |threads: u32| LEFT + f64::from(threads) / f64::from(most) * (width - LEFT - right);
    // A band a thread count, from halfway to the count before it to
    // halfway to the one after: the pointer finds the count, not the line.
    let bands: Vec<(u32, f64, f64)> = (thread_counts.iter().enumerate())
        .map(|(index, threads)| {
            let x = x_of(*threads);
            let left = (index.checked_sub(1))
                .map_or(LEFT, |before| (x_of(thread_counts[before]) + x) / 2.0);
            let right = (thread_counts.get(index + 1))
                .map_or(width - right + 8.0, |after| (x + x_of(*after)) / 2.0);
            (*threads, left, right)
        })
        .collect();
    let greatest = (lines.iter())
        .flat_map(|line| line.points.iter().map(|(_, mops)| *mops))
        .fold(0.0, f64::max);
    let y = LinearScale::fitting(greatest, (height - BOTTOM, TOP));
    let ends: Vec<(MeasuredMap, String, f64, f64)> = (lines.iter())
        .filter_map(|line| {
            let (_, mops) = line.points.last()?;
            Some((line.map, line.label(), *mops, y.at(*mops)))
        })
        .collect();
    let label_class = |map: MeasuredMap| {
        if map.is_ours() {
            "end-label ours lands"
        } else if !map.is_ordered() {
            "end-label unordered lands"
        } else {
            "end-label lands"
        }
    };
    let key_class = |map: MeasuredMap| {
        if map.is_ours() {
            "line-key ours"
        } else if !map.is_ordered() {
            "line-key unordered"
        } else {
            "line-key"
        }
    };
    let labels = EndLabel::placed(ends, 17.0, TOP, height - BOTTOM);
    let mut crosshair = use_signal(|| None::<u32>);
    let mut spotlight = spotlight;
    let readings = lines.clone();
    let reading = Rc::new(move |threads: u32| {
        let mut values: Vec<(String, f64)> = (readings.iter())
            .filter_map(|line| {
                let (_, mops) = line.points.iter().find(|(count, _)| *count == threads)?;
                Some((line.label(), *mops))
            })
            .collect();
        values.sort_by(|a, b| b.1.total_cmp(&a.1));
        let body = (values.iter())
            .map(|(label, mops)| format!("{mops:>6.1}   {label}"))
            .collect::<Vec<_>>()
            .join("\n");
        (format!("{threads} threads · M ops/s"), body)
    });
    rsx! {
        figure { class: "chart-panel wide line-chart",
            figcaption { class: "chart-title",
                strong { {title.clone()} }
                span { class: "chart-subtitle", "million operations a second, higher is better" }
            }
            svg {
                class: "chart-svg",
                width: "{width}",
                height: "{height}",
                view_box: "0 0 {width} {height}",
                role: "img",
                "aria-label": "{title}: throughput by thread count",
                onmouseleave: move |_| {
                    crosshair.set(None);
                    spotlight.clear();
                },
                for tick in y.ticks() {
                    line {
                        class: if tick == 0.0 { "chart-axis" } else { "chart-grid" },
                        x1: LEFT,
                        x2: width - right + 8.0,
                        y1: y.at(tick),
                        y2: y.at(tick),
                    }
                    text { class: "chart-tick", x: LEFT - 10.0, y: y.at(tick) + 4.0, text_anchor: "end", "{tick}" }
                }
                text { class: "chart-tick", x: 0, y: TOP - 14.0, "M ops/s" }
                for threads in thread_counts.iter().copied() {
                    text {
                        class: if crosshair() == Some(threads) { "chart-tick strong" } else { "chart-tick" },
                        x: x_of(threads),
                        y: height - BOTTOM + 22.0,
                        text_anchor: "middle",
                        "{threads}"
                    }
                }
                text { class: "chart-tick", x: LEFT + (width - LEFT - right) / 2.0, y: height - 2.0, text_anchor: "middle", "threads" }
                if let Some(threads) = crosshair() {
                    line {
                        class: "chart-crosshair",
                        x1: x_of(threads),
                        x2: x_of(threads),
                        y1: TOP - 6.0,
                        y2: height - BOTTOM,
                    }
                }
                // The others first, so bbtree's lines lie on top.
                for line in lines.iter().filter(|line| !line.map.is_ours()).chain(lines.iter().filter(|line| line.map.is_ours())) {
                    {
                        let smooth = SmoothLine::through(
                            (line.points.iter())
                                .map(|(threads, mops)| (x_of(*threads), y.at(*mops)))
                                .collect(),
                        );
                        let at = crosshair().and_then(|threads| {
                            line.points.iter().find(|(count, _)| *count == threads).copied()
                        });
                        rsx! {
                            g {
                                class: line.series_class(),
                                if line.map.is_ours() {
                                    path { class: "chart-highlight draws", path_length: 1, d: smooth.path() }
                                }
                                if line.map.is_ordered() {
                                    path { class: "chart-line draws", path_length: 1, style: "stroke:{line.map.line_color()}", d: smooth.path() }
                                } else {
                                    // Dotted, its dashes in pixels: it fades in
                                    // as the others finish drawing.
                                    path { class: "chart-line lands", style: "stroke:{line.map.line_color()}", d: smooth.path() }
                                }
                                for (threads, mops) in line.points.iter().copied() {
                                    circle {
                                        class: "chart-point lands",
                                        style: "fill:{line.map.fill_color()}",
                                        cx: x_of(threads),
                                        cy: y.at(mops),
                                        r: 3,
                                    }
                                }
                                if let Some((threads, mops)) = at {
                                    circle {
                                        class: "chart-dot",
                                        style: "fill:{line.map.fill_color()}",
                                        cx: x_of(threads),
                                        cy: y.at(mops),
                                        r: 5,
                                    }
                                }
                            }
                        }
                    }
                }
                for label in labels.iter().filter(|_| !narrow) {
                    {
                        let end_x = width - right;
                        rsx! {
                            g { class: label_class(label.map),
                                circle {
                                    class: "chart-dot",
                                    style: "fill:{label.map.fill_color()}",
                                    cx: end_x,
                                    cy: label.end_y,
                                    r: 4,
                                }
                                if (label.y - label.end_y).abs() > 1.0 {
                                    path {
                                        class: "end-leader",
                                        d: "M{end_x + 6.0:.1},{label.end_y:.1}L{end_x + 14.0:.1},{label.y:.1}",
                                    }
                                }
                                text { class: "end-value", x: end_x + 18.0, y: label.y + 4.0,
                                    "{label.value:.1}"
                                }
                                text { class: "end-name", x: end_x + 58.0, y: label.y + 4.0,
                                    {label.name.clone()}
                                }
                            }
                        }
                    }
                }
                for (threads, left, right) in bands.iter().copied() {
                    {
                        let reading = reading.clone();
                        rsx! {
                            rect {
                                class: "chart-hit",
                                x: left,
                                y: TOP - 6.0,
                                width: right - left,
                                height: height - BOTTOM - TOP + 6.0,
                                onmouseenter: move |evt| {
                                    crosshair.set(Some(threads));
                                    let point = evt.client_coordinates();
                                    let (title, body) = reading(threads);
                                    spotlight.show_hint(Hint::new((point.x, point.y), title, body));
                                },
                                onmousemove: move |evt| {
                                    let point = evt.client_coordinates();
                                    spotlight.move_hint((point.x, point.y));
                                },
                            }
                        }
                    }
                }
            }
            if narrow {
                ul { class: "line-keys",
                    for label in labels.iter() {
                        li { class: key_class(label.map),
                            span { class: "line-key-swatch", style: "--swatch:{label.map.line_color()}" }
                            strong { "{label.value:.1}" }
                            " {label.name}"
                        }
                    }
                }
            }
            div { class: "sr-only",
                table {
                    caption { "{title}: millions of operations a second" }
                    thead {
                        tr {
                            th { scope: "col", "map" }
                            for threads in thread_counts.iter() {
                                th { scope: "col", "{threads} threads" }
                            }
                        }
                    }
                    tbody {
                        for line in lines.iter() {
                            tr {
                                th { scope: "row", {line.label()} }
                                for (_, mops) in line.points.iter() {
                                    td { "{mops:.1}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
