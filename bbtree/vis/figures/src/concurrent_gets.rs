//! The concurrent results again, inside "Minimize cache coherence
//! traffic": gets of `u64` keys, half of them misses, as threads are
//! added, on AWS's c8a.metal-24xl, the runs the post's Concurrent card
//! shows (`vis/data/aws-c8a-20261017`). A line a map, thread counts to
//! scale from zero, each line named at its end; bbtree's lines in the
//! site's highlighter, DashMap, a hash map, faint and dotted.

use std::fs;
use std::io;
use std::path::Path;

use crate::svg::{Element, Motion, Svg};

/// One map's line: its name, its look, and throughput by thread count.
struct ThroughputLine {
    name: &'static str,
    class: &'static str,
    points: Vec<(f64, f64)>,
}

pub(crate) struct ConcurrentGets {
    lines: Vec<ThroughputLine>,
}

impl ConcurrentGets {
    const LEFT: f64 = 64.0;
    const RIGHT: f64 = 470.0;
    const TOP: f64 = 28.0;
    const BOTTOM: f64 = 300.0;
    const MAX_THREADS: f64 = 96.0;
    /// The maps drawn, bottom of the stack first, with their names and
    /// looks: the CSV's map and workload, and the post's words.
    const MAPS: [(&'static str, &'static str, &'static str, &'static str); 7] = [
        ("dashmap", "get-half", "DashMap (hashtable)", "unordered"),
        (
            "std-btree-rwlock",
            "get-half",
            "RwLock<BTreeMap>",
            "theirs-1",
        ),
        (
            "crossbeam-skipmap",
            "get-half",
            "crossbeam SkipMap",
            "theirs-4",
        ),
        ("concurrent-map", "get-half", "concurrent-map", "theirs-3"),
        ("scc-treeindex", "get-half", "scc TreeIndex", "theirs-2"),
        (
            "bbtree",
            "get-half-batch",
            "bbtree (batched)",
            "ours batched",
        ),
        ("bbtree", "get-half", "bbtree", "ours"),
    ];

    /// The lines of `u64` gets from the c8a's throughput CSV.
    pub(crate) fn read(csv: &Path) -> io::Result<Self> {
        let text = fs::read_to_string(csv)?;
        let rows: Vec<Vec<&str>> = text
            .lines()
            .skip(1)
            .map(|line| line.split(',').collect())
            .collect();
        let lines = Self::MAPS
            .iter()
            .map(|&(map, workload, name, class)| {
                let mut points: Vec<(f64, f64)> = rows
                    .iter()
                    .filter(|row| row[1] == map && row[2] == "u64" && row[3] == workload)
                    .map(|row| (row[4].parse().unwrap_or(0.0), row[7].parse().unwrap_or(0.0)))
                    .collect();
                points.sort_by(|a, b| a.0.total_cmp(&b.0));
                ThroughputLine {
                    name,
                    class,
                    points,
                }
            })
            .filter(|line| !line.points.is_empty())
            .collect();
        Ok(Self { lines })
    }

    fn top_value(&self) -> f64 {
        let most = (self.lines.iter().flat_map(|line| &line.points))
            .map(|point| point.1)
            .fold(0.0, f64::max);
        (most / 200.0).ceil() * 200.0
    }

    fn x(&self, threads: f64) -> f64 {
        Self::LEFT + threads / Self::MAX_THREADS * (Self::RIGHT - Self::LEFT)
    }

    fn y(&self, value: f64) -> f64 {
        Self::BOTTOM - value / self.top_value() * (Self::BOTTOM - Self::TOP)
    }

    pub(crate) fn svg(&self) -> Svg {
        let mut svg = Svg::new(
            720.0,
            340.0,
            "Gets of u64 keys at 10M entries as threads are added, on 96 cores: a line a map",
        );
        let mut motion = Motion::once(1.0, 1.4);
        motion.animate(
            "draws",
            &[
                (0.0, "stroke-dashoffset: 1".into()),
                (1.0, "stroke-dashoffset: 0".into()),
            ],
        );
        motion.appear("lands", 0.9);
        svg.style(
            ".grid { stroke: var(--rule); } \
             .axis { stroke: var(--rule-strong); } \
             .series { fill: none; stroke-width: 2; stroke-linejoin: round; stroke-dasharray: 1; } \
             .theirs-1 { stroke: var(--theirs-1); } .theirs-2 { stroke: var(--theirs-2); } \
             .theirs-3 { stroke: var(--theirs-3); } .theirs-4 { stroke: var(--theirs-4); } \
             .ours { stroke: var(--marker-strong); stroke-width: 3; } \
             .ours.batched { stroke: var(--marker); stroke-width: 2.5; } \
             .rim { stroke: var(--marker-strong); stroke-width: 5; } \
             .dotted { fill: none; stroke-dasharray: 2 4; stroke-linecap: round; } \
             .unordered { stroke: var(--theirs-4); stroke-width: 1.5; } \
             .unordered-label { font-style: italic; fill: var(--muted); } \
             .ours-label { font-weight: 600; fill: var(--marker-ink); }",
        );
        let top = self.top_value();
        let mut value = 0.0;
        while value <= top {
            let y = self.y(value);
            svg.add(Element::line(Self::LEFT, y, Self::RIGHT, y).class("grid"));
            svg.add(
                Element::text(Self::LEFT - 8.0, y + 4.0, &format!("{value}"))
                    .class("label end data"),
            );
            value += 200.0;
        }
        svg.add(Element::text(Self::LEFT - 8.0, Self::TOP - 12.0, "M gets/s").class("label end"));
        for threads in [1.0, 24.0, 48.0, 72.0, 96.0] {
            let x = self.x(threads);
            svg.add(Element::line(x, Self::BOTTOM, x, Self::BOTTOM + 4.0).class("axis"));
            svg.add(
                Element::text(x, Self::BOTTOM + 18.0, &format!("{threads}"))
                    .class("label middle data"),
            );
        }
        svg.add(Element::line(Self::LEFT, Self::BOTTOM, Self::RIGHT, Self::BOTTOM).class("axis"));
        svg.add(
            Element::text(
                (Self::LEFT + Self::RIGHT) / 2.0,
                Self::BOTTOM + 34.0,
                "threads, a core each",
            )
            .class("label middle"),
        );
        for line in &self.lines {
            let d: String = (line.points.iter().enumerate())
                .map(|(index, &(threads, value))| {
                    let command = if index == 0 { 'M' } else { 'L' };
                    format!("{command}{:.1} {:.1} ", self.x(threads), self.y(value))
                })
                .collect();
            // A line draws itself from the left, as threads are added; the
            // dotted one, whose dashes the drawing would undo, fades in.
            if line.class == "ours batched" {
                // The highlighter alone is too pale a line on the panel:
                // rimmed in its dark gold, as bbtree's bars are.
                svg.add(
                    Element::path(d.trim_end())
                        .attr("pathLength", 1)
                        .class("series draws rim"),
                );
            }
            let path = Element::path(d.trim_end());
            svg.add(if line.class == "unordered" {
                path.class("dotted unordered lands")
            } else {
                path.attr("pathLength", 1)
                    .class(&format!("series draws {}", line.class))
            });
        }
        for (name, class, y) in self.end_labels() {
            svg.add(
                Element::text(Self::RIGHT + 10.0, y + 4.0, &name).class(&format!("lands {class}")),
            );
        }
        svg.style(motion.css());
        svg
    }

    /// Each line's name and last value at its end, moved off its
    /// neighbors' so none overlap.
    fn end_labels(&self) -> Vec<(String, &'static str, f64)> {
        const GAP: f64 = 15.0;
        let mut labels: Vec<(String, &'static str, f64)> = (self.lines.iter())
            .filter_map(|line| {
                let &(_, value) = line.points.last()?;
                let class = match line.class {
                    "unordered" => "unordered-label",
                    "ours" | "ours batched" => "ours-label",
                    _ => "label",
                };
                Some((format!("{} {value:.0}", line.name), class, self.y(value)))
            })
            .collect();
        labels.sort_by(|a, b| a.2.total_cmp(&b.2));
        let mut floor = Self::TOP;
        for label in &mut labels {
            label.2 = label.2.max(floor);
            floor = label.2 + GAP;
        }
        let mut ceiling = Self::BOTTOM;
        for label in labels.iter_mut().rev() {
            label.2 = label.2.min(ceiling);
            ceiling = label.2 - GAP;
        }
        labels
    }
}
