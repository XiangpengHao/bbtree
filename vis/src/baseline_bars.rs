//! bbtree's times against a baseline's, a cluster each, drawn one of two
//! ways ([`BarScale`]): as shares, the baseline's time a dark bar at 100%
//! and bbtree's a bright one inside it, the speedup above, so that no
//! group's times compare with another's; or as nanoseconds, the two side
//! by side on one axis, each labelled with its time. Clusters gather in
//! groups (a system, a machine), each named under its clusters with a
//! caption. The groups flow into rows as even as they can be and as wide
//! as the frame: one or two rows on a desk, a group a row on a phone.

use dioxus::prelude::*;

use crate::chart_scale::{FrameWidth, LinearScale, Nanos};
use crate::spotlight::{Hint, Spotlight};

/// How a chart draws a cluster's two times.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum BarScale {
    /// One bar: the baseline's time as 100%, ours inside it as a share,
    /// the speedup above; each row of groups on an axis of its own.
    Relative,
    /// Two bars side by side, each labelled with its time; every row on
    /// one axis.
    Nanos,
}

/// One cluster: a workload of a group, the baseline's time and ours.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct BaselinePair {
    pub(crate) workload: String,
    /// What exactly ran, in a few words.
    pub(crate) detail: String,
    pub(crate) baseline: String,
    pub(crate) baseline_nanos: f64,
    pub(crate) ours: String,
    pub(crate) ours_nanos: f64,
}

impl BaselinePair {
    fn speedup(&self) -> f64 {
        self.baseline_nanos / self.ours_nanos
    }

    /// Our time as a share of the baseline's.
    fn relative_time(&self) -> f64 {
        self.ours_nanos / self.baseline_nanos
    }
}

/// A group of clusters: its name, a caption under it, its pairs.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct BaselineGroup {
    pub(crate) name: String,
    pub(crate) caption: String,
    pub(crate) pairs: Vec<BaselinePair>,
}

/// Where a cluster sits.
#[derive(Clone, PartialEq, Debug)]
struct Cluster {
    group: String,
    pair: BaselinePair,
    /// Its step's left edge.
    x: f64,
}

impl Cluster {
    /// Its workload's name in one line, or in two where one would be
    /// wider than `step`: split at the space or after the hyphen nearest
    /// its middle.
    fn label_lines(&self, step: f64) -> Vec<String> {
        /// About how wide a character of a cluster's label is.
        const CHARACTER: f64 = 6.6;
        let label = &self.pair.workload;
        if label.chars().count() as f64 * CHARACTER <= step - 6.0 {
            return vec![label.clone()];
        }
        let middle = label.len() / 2;
        let split = (label.char_indices())
            .filter(|(_, character)| matches!(character, ' ' | '-'))
            .min_by_key(|(at, _)| at.abs_diff(middle));
        match split {
            Some((at, ' ')) => vec![label[..at].to_string(), label[at + 1..].to_string()],
            Some((at, _)) => vec![label[..=at].to_string(), label[at + 1..].to_string()],
            None => vec![label.clone()],
        }
    }
}

/// A group, placed: its name and caption, where its clusters start and
/// end, and how many it has.
#[derive(Clone, PartialEq, Debug)]
struct PlacedGroup {
    name: String,
    caption: String,
    start: f64,
    end: f64,
    cluster_count: usize,
}

/// One row of groups, an SVG of its own, with its own axis.
#[derive(Clone, PartialEq, Debug, Default)]
struct GroupRow {
    clusters: Vec<Cluster>,
    groups: Vec<PlacedGroup>,
}

impl GroupRow {
    /// The top of its axis: the baseline's 1, or more where ours ran
    /// slower and reaches past it. A row's own, so that one slow row
    /// doesn't flatten the others.
    fn greatest(&self) -> f64 {
        (self.clusters.iter())
            .map(|cluster| cluster.pair.relative_time())
            .fold(1.0, f64::max)
    }

    /// How far below its usual place each group's name goes: a line down
    /// below two-line labels, up into their room where no cluster has one.
    fn name_drop(&self, step: f64) -> f64 {
        if (self.clusters.iter()).any(|cluster| cluster.label_lines(step).len() > 1) {
            13.0
        } else if (self.clusters.iter()).all(|cluster| cluster.pair.workload.is_empty()) {
            -18.0
        } else {
            0.0
        }
    }
}

/// How the chart is laid out at a width, in steps: a cluster takes one, its
/// bar half of it centered, its label the whole; a gap between groups
/// takes half of one. Groups split into as few rows as keep a step wide
/// enough for a two-word label, as evenly as they go, and the fullest row
/// fills the width.
#[derive(Clone, PartialEq, Debug)]
struct GroupLayout {
    width: f64,
    /// How many groups each row holds, in order.
    row_lengths: Vec<usize>,
    step: f64,
}

impl GroupLayout {
    const LEFT: f64 = 52.0;
    const TOP: f64 = 30.0;
    const BOTTOM: f64 = 93.0;
    /// The gap between groups, in steps.
    const GROUP_GAP: f64 = 0.5;
    /// The layout at `width` for groups of `cluster_counts` clusters each,
    /// drawn at `scale`.
    fn at(width: f64, cluster_counts: &[usize], scale: BarScale) -> Self {
        let (least_step, most_step) = Self::steps_for(scale);
        let room = Self::room(width);
        let total = Self::steps(cluster_counts);
        let mut layout = Self {
            width,
            row_lengths: vec![cluster_counts.len()],
            step: (room / total).min(most_step),
        };
        for row_count in 2..=cluster_counts.len() {
            if layout.step >= least_step {
                break;
            }
            let row_lengths = Self::split(cluster_counts, row_count);
            let mut start = 0;
            let widest = (row_lengths.iter())
                .map(|length| {
                    let row = &cluster_counts[start..start + length];
                    start += length;
                    Self::steps(row)
                })
                .fold(0.0, f64::max);
            layout = Self {
                width,
                row_lengths,
                step: (room / widest).min(most_step),
            };
        }
        layout
    }

    /// The narrowest and widest step at `scale`. One bar a cluster: close
    /// together, a step as narrow as a label of two short lines allows
    /// ("Index" over "lookup"). Two bars, each labelled with its time:
    /// room for both labels.
    fn steps_for(scale: BarScale) -> (f64, f64) {
        match scale {
            BarScale::Relative => (50.0, 64.0),
            BarScale::Nanos => (96.0, 140.0),
        }
    }

    /// The groups of `cluster_counts` in `row_count` rows, in order, each
    /// row closed once it holds its share of the steps.
    fn split(cluster_counts: &[usize], row_count: usize) -> Vec<usize> {
        let share = Self::steps(cluster_counts) / row_count as f64;
        let mut row_lengths = Vec::new();
        let mut row_start = 0;
        for index in 0..cluster_counts.len() {
            let rows_left = row_count - row_lengths.len();
            let groups_left = cluster_counts.len() - index;
            let full = index > row_start
                && (Self::steps(&cluster_counts[row_start..index]) >= share
                    || groups_left < rows_left);
            if full && rows_left > 1 {
                row_lengths.push(index - row_start);
                row_start = index;
            }
        }
        row_lengths.push(cluster_counts.len() - row_start);
        row_lengths
    }

    /// From the first group's left edge to the right edge.
    fn room(width: f64) -> f64 {
        width - 8.0 - (Self::LEFT + 8.0)
    }

    /// A row's width in steps: its clusters and the gaps between groups.
    fn steps(cluster_counts: &[usize]) -> f64 {
        let clusters: usize = cluster_counts.iter().sum();
        clusters as f64 + Self::GROUP_GAP * cluster_counts.len().saturating_sub(1) as f64
    }

    /// A bar's width: most of a step for one bar, a third for each of two.
    fn bar(&self, scale: BarScale) -> f64 {
        match scale {
            BarScale::Relative => self.step * 0.62,
            BarScale::Nanos => self.step / 3.0,
        }
    }

    fn height(&self) -> f64 {
        if FrameWidth::is_narrow(self.width) {
            316.0
        } else {
            376.0
        }
    }

    /// The groups placed in their rows.
    fn rows(&self, groups: Vec<BaselineGroup>) -> Vec<GroupRow> {
        let mut groups = groups.into_iter();
        (self.row_lengths.iter())
            .map(|length| {
                let mut row = GroupRow::default();
                let mut x = Self::LEFT + 8.0;
                for group in groups.by_ref().take(*length) {
                    let start = x;
                    let cluster_count = group.pairs.len();
                    for pair in group.pairs {
                        row.clusters.push(Cluster {
                            group: group.name.clone(),
                            pair,
                            x,
                        });
                        x += self.step;
                    }
                    row.groups.push(PlacedGroup {
                        name: group.name,
                        caption: group.caption,
                        start,
                        end: x,
                        cluster_count,
                    });
                    x += Self::GROUP_GAP * self.step;
                }
                row
            })
            .collect()
    }
}

/// The groups as pairs of bars, drawn at the frame's width.
#[component]
pub(crate) fn BaselineBars(
    groups: Vec<BaselineGroup>,
    scale: BarScale,
    /// What the chart shows, for screen readers.
    label: String,
    /// A title and its subtitle over the chart, if it has one.
    heading: Option<(String, String)>,
    spotlight: Spotlight,
) -> Element {
    let frame = FrameWidth::use_new();
    let width = frame.get();
    let counts: Vec<usize> = groups.iter().map(|group| group.pairs.len()).collect();
    let layout = GroupLayout::at(width, &counts, scale);
    let rows = layout.rows(groups);
    let clusters: Vec<Cluster> = (rows.iter())
        .flat_map(|row| row.clusters.iter().cloned())
        .collect();
    let height = layout.height();
    let (top, left) = (GroupLayout::TOP, GroupLayout::LEFT);
    let (step, bar) = (layout.step, layout.bar(scale));
    // In nanoseconds, every row on the axis that holds the longest time.
    let longest = (clusters.iter())
        .map(|cluster| cluster.pair.baseline_nanos.max(cluster.pair.ours_nanos))
        .fold(0.0, f64::max);
    // bbtree's bar, inside the baseline's, so that the baseline's shows
    // around it.
    let inset = (bar * 0.16).max(2.5);
    let mut spotlight = spotlight;
    rsx! {
        div { class: "chart-frame", onresize: move |resized| frame.measure(resized),
            if width > 0.0 {
                figure { class: "chart-panel wide",
                    if let Some((title, subtitle)) = heading {
                        figcaption { class: "chart-title",
                            strong { {title} }
                            span { class: "chart-subtitle", {subtitle} }
                        }
                    }
                    for row in rows.iter() {
                        {
                        let greatest = match scale {
                            BarScale::Relative => row.greatest(),
                            BarScale::Nanos => longest,
                        };
                        let y = LinearScale::fitting(greatest, (height - GroupLayout::BOTTOM, top));
                        let baseline = y.at(0.0);
                        let drop = row.name_drop(step);
                        rsx! {
                        svg {
                            class: "chart-svg",
                            width: "{width}",
                            height: "{height}",
                            view_box: "0 0 {width} {height}",
                            role: "img",
                            "aria-label": label.clone(),
                            for tick in y.ticks() {
                                line {
                                    class: if tick == 0.0 { "chart-axis" } else { "chart-grid" },
                                    x1: left,
                                    x2: width - 8.0,
                                    y1: y.at(tick),
                                    y2: y.at(tick),
                                }
                                text { class: "chart-tick", x: left - 10.0, y: y.at(tick) + 4.0, text_anchor: "end",
                                    match (scale, tick == 0.0) {
                                        (BarScale::Relative, _) => format!("{:.0}%", tick * 100.0),
                                        (BarScale::Nanos, true) => "0".to_string(),
                                        (BarScale::Nanos, false) => Nanos(tick).to_string(),
                                    }
                                }
                            }
                            for cluster in row.clusters.iter() {
                                {
                                    let measured = cluster.pair.clone();
                                    let speedup = measured.speedup();
                                    let relative = measured.relative_time();
                                    let center = cluster.x + step / 2.0;
                                    let hint_title = match scale {
                                        BarScale::Relative => format!("{} · {}: {speedup:.2}× faster", cluster.group, measured.workload),
                                        BarScale::Nanos => cluster.group.clone(),
                                    };
                                    let hint_body = format!(
                                        "{}\n{}: {}\n{}: {}",
                                        measured.detail,
                                        measured.baseline,
                                        Nanos(measured.baseline_nanos),
                                        measured.ours,
                                        Nanos(measured.ours_nanos),
                                    );
                                    // Two bars side by side, 4 apart, about the middle.
                                    let (theirs_x, ours_x) = (center - bar - 2.0, center + 2.0);
                                    // The speedup just over bbtree's bar, in the dark bar
                                    // around it, in its panel's color; over the dark bar,
                                    // in ink, where the room between them is too short.
                                    let over_ours = y.at(relative) - 6.0;
                                    let inside = over_ours - 12.0 >= y.at(1.0) + 2.0;
                                    let speedup_y = if inside { over_ours } else { y.at(relative.max(1.0)) - 8.0 };
                                    let speedup_class = match (inside, speedup >= 1.0) {
                                        (true, _) => "cluster-speedup inside",
                                        (false, true) => "cluster-speedup",
                                        (false, false) => "cluster-speedup slower",
                                    };
                                    rsx! {
                                        g { class: "chart-cluster",
                                            onmouseenter: move |evt| {
                                                let point = evt.client_coordinates();
                                                spotlight.show_hint(Hint::new((point.x, point.y), hint_title.clone(), hint_body.clone()));
                                            },
                                            onmousemove: move |evt| {
                                                let point = evt.client_coordinates();
                                                spotlight.move_hint((point.x, point.y));
                                            },
                                            onmouseleave: move |_| spotlight.clear(),
                                            rect {
                                                class: "chart-hit",
                                                x: cluster.x,
                                                y: top - 20.0,
                                                width: step,
                                                height: baseline - top + 20.0,
                                            }
                                            match scale {
                                                BarScale::Relative => rsx! {
                                                    path {
                                                        class: "grow-y",
                                                        style: "fill:var(--theirs-1);--grow:{y.growth_ms(1.0)}ms",
                                                        d: y.column_path(center - bar / 2.0, bar, 1.0),
                                                    }
                                                    path {
                                                        class: "chart-bar ours grow-y",
                                                        style: "fill:var(--ours);--grow:{y.growth_ms(relative)}ms",
                                                        d: y.inset_column_path(center - bar / 2.0, bar, relative, inset),
                                                    }
                                                    text {
                                                        class: "{speedup_class} lands",
                                                        style: "--grow:{y.growth_ms(relative)}ms",
                                                        x: center,
                                                        y: speedup_y,
                                                        text_anchor: "middle",
                                                        "{speedup:.1}×"
                                                    }
                                                },
                                                BarScale::Nanos => rsx! {
                                                    path {
                                                        class: "grow-y",
                                                        style: "fill:var(--theirs-1);--grow:{y.growth_ms(measured.baseline_nanos)}ms",
                                                        d: y.column_path(theirs_x, bar, measured.baseline_nanos),
                                                    }
                                                    text { class: "cluster-value lands", style: "--grow:{y.growth_ms(measured.baseline_nanos)}ms", x: theirs_x + bar / 2.0, y: y.at(measured.baseline_nanos) - 6.0, text_anchor: "middle",
                                                        "{Nanos(measured.baseline_nanos)}"
                                                    }
                                                    path {
                                                        class: "chart-bar ours grow-y",
                                                        style: "fill:var(--ours);--grow:{y.growth_ms(measured.ours_nanos)}ms",
                                                        d: y.column_path(ours_x, bar, measured.ours_nanos),
                                                    }
                                                    text { class: "cluster-value ours lands", style: "--grow:{y.growth_ms(measured.ours_nanos)}ms", x: ours_x + bar / 2.0, y: y.at(measured.ours_nanos) - 6.0, text_anchor: "middle",
                                                        "{Nanos(measured.ours_nanos)}"
                                                    }
                                                },
                                            }
                                            if !measured.workload.is_empty() {
                                                text { class: "cluster-label", x: center, y: baseline + 16.0, text_anchor: "middle",
                                                    for (index, line) in cluster.label_lines(step).into_iter().enumerate() {
                                                        tspan { x: center, dy: if index == 0 { 0.0 } else { 13.0 }, {line} }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            for group in row.groups.iter() {
                                if group.cluster_count > 1 {
                                    line { class: "group-rule", x1: group.start + 6.0, x2: group.end - 6.0, y1: baseline + drop + 30.0, y2: baseline + drop + 30.0 }
                                }
                                text { class: "group-label", x: (group.start + group.end) / 2.0, y: baseline + drop + 50.0, text_anchor: "middle",
                                    {group.name.clone()}
                                }
                                text { class: "group-caption", x: (group.start + group.end) / 2.0, y: baseline + drop + 66.0, text_anchor: "middle",
                                    {group.caption.clone()}
                                }
                            }
                        }
                        }
                        }
                    }
                    div { class: "sr-only",
                        table {
                            caption { {label.clone()} }
                            thead {
                                tr {
                                    th { scope: "col", "group" }
                                    th { scope: "col", "workload" }
                                    th { scope: "col", "baseline" }
                                    th { scope: "col", "ours" }
                                    th { scope: "col", "speedup" }
                                }
                            }
                            tbody {
                                for cluster in clusters.iter() {
                                    tr {
                                        th { scope: "row", {cluster.group.clone()} }
                                        td { "{cluster.pair.workload}: {cluster.pair.detail}" }
                                        td { "{cluster.pair.baseline}: {Nanos(cluster.pair.baseline_nanos)}" }
                                        td { "{cluster.pair.ours}: {Nanos(cluster.pair.ours_nanos)}, {cluster.pair.relative_time() * 100.0:.0}%" }
                                        td { "{cluster.pair.speedup():.2}×" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(name: &str, workloads: usize) -> BaselineGroup {
        let pair = |index: usize| BaselinePair {
            workload: format!("w{index}"),
            detail: String::new(),
            baseline: String::new(),
            baseline_nanos: 2.0,
            ours: String::new(),
            ours_nanos: 1.0,
        };
        BaselineGroup {
            name: name.to_string(),
            caption: String::new(),
            pairs: (0..workloads).map(pair).collect(),
        }
    }

    fn ends(rows: &[GroupRow]) -> Vec<f64> {
        (rows.iter())
            .map(|row| row.groups.last().unwrap().end)
            .collect()
    }

    #[test]
    fn groups_split_evenly_into_rows_that_fit() {
        let counts = [3, 4, 3, 3, 4];
        let groups = || (counts.iter().enumerate()).map(|(index, n)| group(&index.to_string(), *n));
        // A desk: one row of narrow steps.
        let desk = GroupLayout::at(1060.0, &counts, BarScale::Relative);
        assert_eq!(desk.row_lengths, vec![5]);
        let rows = desk.rows(groups().collect());
        let end = ends(&rows)[0];
        assert!(end <= 1060.0 - 8.0 + 0.5, "the row ends at {end}");
        // A phone: a group a row, none past the edge.
        let phone = GroupLayout::at(350.0, &counts, BarScale::Relative);
        assert_eq!(phone.row_lengths, vec![1; 5]);
        for end in ends(&phone.rows(groups().collect())) {
            assert!(end <= 350.0 - 8.0 + 0.5, "a group ends at {end}");
        }
        // Two bars a cluster want wider steps: five machines of one
        // cluster each fit a desk's row, a phone's two.
        let machines = [1; 5];
        assert_eq!(
            GroupLayout::at(1060.0, &machines, BarScale::Nanos).row_lengths,
            vec![5]
        );
        assert_eq!(
            GroupLayout::at(350.0, &machines, BarScale::Nanos)
                .row_lengths
                .len(),
            3
        );
        // A two-word label too wide for its step takes two lines.
        let cluster = &rows[0].clusters[0];
        let mut wide = cluster.clone();
        wide.pair.workload = "Wikipedia titles".to_string();
        assert_eq!(wide.label_lines(68.0), vec!["Wikipedia", "titles"]);
        assert_eq!(wide.label_lines(120.0), vec!["Wikipedia titles"]);
        wide.pair.workload = "Write-heavy".to_string();
        assert_eq!(wide.label_lines(68.0), vec!["Write-", "heavy"]);
    }
}
