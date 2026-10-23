//! The figure of a batch: four `u64` keys looked up together, a task each,
//! on one core, turn by turn as bbtree's own run of the batch takes them
//! (`TreeView::trace_many`). The core is the axis. A task comes in, reads
//! the line it prefetched, steps down the tree, prefetches the next line
//! and goes out, and the next task comes in. Under the core, a row a task:
//! from the end of its turn to its next, its line is on its way while the
//! core runs the others. Time is counted in turns, since the trace says
//! nothing of when a line arrives.

use std::ops::Range;

use bbtree::unsync::BBTreeMap;

use crate::svg::{Element, Motion, Svg};

/// A batch's turns on the core, in the executor's order: each the task
/// that ran, and whether it ended by fetching a line rather than its
/// lookup.
pub(crate) struct BatchLanes {
    tasks: usize,
    turns: Vec<(usize, bool)>,
}

impl BatchLanes {
    const PLOT_LEFT: f64 = 96.0;
    const PLOT_RIGHT: f64 = 680.0;
    const CORE_TOP: f64 = 44.0;
    const CORE_HEIGHT: f64 = 28.0;
    /// The middle of the first task's row.
    const ROWS_TOP: f64 = 100.0;
    const ROW: f64 = 26.0;
    /// How far into a turn its task's connectors meet the core, in pixels.
    const INSET: f64 = 4.0;
    /// When, in a turn, its task prefetches and goes out.
    const OUT_AT: f64 = 0.8;
    /// How faint a connector is but while its turn runs.
    const CONNECTOR_REST: f64 = 0.1;

    pub(crate) fn of_lookups(map: &BBTreeMap<u64, u64>, keys: [u64; 4]) -> Self {
        let view = map.inspect();
        let trace = view.trace_many(keys.each_ref());
        assert!(trace.batches(), "a map this size runs its batches in lanes");
        let turns = trace
            .polls()
            .iter()
            .map(|poll| (poll.lane(), poll.fetch().is_some()))
            .collect();
        Self {
            tasks: keys.len(),
            turns,
        }
    }

    /// The turn after `index` of the same task.
    fn next_turn(&self, index: usize) -> Option<usize> {
        let task = self.turns[index].0;
        (index + 1..self.turns.len()).find(|&later| self.turns[later].0 == task)
    }

    /// When turn `index`'s line is on its way: from the end of the turn
    /// that fetched it to its task's next turn, which reads it; `None` if
    /// the turn ended its task or nothing came between.
    fn on_its_way(&self, index: usize) -> Option<Range<usize>> {
        let next = self.next_turn(index)?;
        (self.turns[index].1 && next > index + 1).then_some(index + 1..next)
    }

    /// How many lines are on their way during turn `turn`.
    fn on_their_way_during(&self, turn: usize) -> usize {
        (0..turn)
            .filter(|&index| {
                self.on_its_way(index)
                    .is_some_and(|way| way.contains(&turn))
            })
            .count()
    }

    fn unit(&self) -> f64 {
        (Self::PLOT_RIGHT - Self::PLOT_LEFT) / self.turns.len() as f64
    }

    fn x(&self, at: f64) -> f64 {
        Self::PLOT_LEFT + at * self.unit()
    }

    fn core_bottom() -> f64 {
        Self::CORE_TOP + Self::CORE_HEIGHT
    }

    fn row_middle(task: usize) -> f64 {
        Self::ROWS_TOP + task as f64 * Self::ROW
    }

    pub(crate) fn svg(&self) -> Svg {
        let total = self.turns.len() as f64;
        let rows_bottom = Self::row_middle(self.tasks - 1) + Self::ROW / 2.0;
        let mut svg = Svg::new(
            720.0,
            rows_bottom + 62.0,
            "Four u64 keys looked up together, a task each, on one core: the tasks take turns, and while a task's line is on its way, the core runs the others",
        );
        let mut motion = Motion::looping(total, 0.32, 3.5);
        let unit = self.unit();
        svg.add(Element::text(24.0, 24.0, "One core runs the four tasks in turn").class("strong"));
        self.add_counter(&mut svg, &mut motion);
        svg.add(Element::text(24.0, Self::CORE_TOP + 19.0, "core").class("strong"));
        svg.add(
            Element::rect(
                Self::PLOT_LEFT,
                Self::CORE_TOP,
                Self::PLOT_RIGHT - Self::PLOT_LEFT,
                Self::CORE_HEIGHT,
            )
            .attr("rx", 4)
            .class("core"),
        );
        for task in 0..self.tasks {
            let y = Self::row_middle(task);
            // Lit while the task is on the core.
            let class = format!("on-core-{task}");
            let turns: Vec<Range<f64>> = (self.turns.iter().enumerate())
                .filter(|(_, turn)| turn.0 == task)
                .map(|(index, _)| index as f64..index as f64 + 1.0)
                .collect();
            motion.lit_during(&class, &turns, 0.0);
            svg.add(
                Element::rect(16.0, y - 11.0, 64.0, 22.0)
                    .attr("rx", 4)
                    .class(&format!("on-core {class}")),
            );
            svg.add(Element::text(24.0, y + 4.5, &format!("task {}", task + 1)).class("note"));
            svg.add(Element::line(Self::PLOT_LEFT, y, Self::PLOT_RIGHT, y).class("baseline"));
        }
        for (index, &(task, fetched)) in self.turns.iter().enumerate() {
            let at = index as f64;
            let y = Self::row_middle(task);
            let (left, right) = (self.x(at) + Self::INSET, self.x(at + 1.0) - Self::INSET);
            // In: the task comes up from its row onto the core. Its
            // connectors are faint but while the turn runs.
            let class = format!("in-{index}");
            motion.lit_during(&class, &[at..at + 1.0], Self::CONNECTOR_REST);
            svg.add(
                Element::line(left, y, left, Self::core_bottom())
                    .class(&format!("connector {class}")),
            );
            // Out: done, or with the next line prefetched, back to its row.
            let class = format!("out-{index}");
            motion.lit_during(&class, &[at + Self::OUT_AT..at + 1.6], Self::CONNECTOR_REST);
            svg.add(
                Element::line(right, Self::core_bottom(), right, y)
                    .class(&format!("connector {class}")),
            );
            if !fetched {
                let class = format!("done-{index}");
                motion.appear(&class, at + Self::OUT_AT);
                svg.add(
                    Element::path(&format!("M{} {y} l3.5 3.5 l7 -7", right + 3.0))
                        .class(&format!("done {class}")),
                );
            }
            if let Some(way) = self.on_its_way(index) {
                let (start, end) = (right, self.x(way.end as f64) + Self::INSET);
                let class = format!("way-{index}");
                motion.animate(
                    &class,
                    &[
                        (0.0, "transform: scaleX(0)".into()),
                        (at + Self::OUT_AT, "transform: scaleX(0)".into()),
                        (way.end as f64, "transform: scaleX(1)".into()),
                    ],
                );
                svg.add(Element::rect(start, y - 2.5, end - start, 5.0).class("track"));
                svg.add(
                    Element::rect(start, y - 2.5, end - start, 5.0).class(&format!("way {class}")),
                );
            }
            // The turn itself, on the core, over its connectors' ends.
            let class = format!("turn-{index}");
            motion.animate(
                &class,
                &[
                    (0.0, "transform: scaleX(0)".into()),
                    (at, "transform: scaleX(0)".into()),
                    (at + Self::OUT_AT, "transform: scaleX(1)".into()),
                ],
            );
            svg.add(
                Element::rect(
                    self.x(at) + 1.0,
                    Self::CORE_TOP + 3.0,
                    unit - 2.0,
                    Self::CORE_HEIGHT - 6.0,
                )
                .attr("rx", 2)
                .class(&format!("turn {class}")),
            );
            let class = format!("number-{index}");
            motion.appear(&class, at + 0.2);
            svg.add(
                Element::text(
                    self.x(at + 0.5),
                    Self::CORE_TOP + 18.5,
                    &format!("{}", task + 1),
                )
                .class(&format!("turn-number middle {class}")),
            );
        }
        let distance = total * unit;
        motion.animate(
            "playhead",
            &[
                (0.0, "transform: translateX(0px)".into()),
                (total, format!("transform: translateX({distance}px)")),
            ],
        );
        svg.add(
            Element::line(
                Self::PLOT_LEFT,
                Self::CORE_TOP - 6.0,
                Self::PLOT_LEFT,
                rows_bottom,
            )
            .class("playhead-line playhead"),
        );
        self.add_legend(&mut svg, rows_bottom + 30.0);
        svg.style(
            ".core { fill: var(--well); } \
             .on-core { fill: var(--marker-soft); } \
             .turn { fill: var(--ink); transform-box: fill-box; transform-origin: left center; } \
             text.turn-number { fill: var(--panel); font-size: 11px; font-weight: 600; } \
             .connector { stroke: var(--graphite); stroke-width: 1.25; } \
             .track { fill: var(--marker); opacity: 0.25; } \
             .way { fill: var(--marker-strong); transform-box: fill-box; transform-origin: left center; } \
             .done { fill: none; stroke: var(--ok); stroke-width: 2; stroke-linecap: round; stroke-linejoin: round; } \
             .baseline { stroke: var(--rule); } \
             .playhead-line { stroke: var(--link); stroke-width: 1.5; }",
        );
        svg.style(motion.css());
        svg
    }

    /// How many lines are on their way, turn by turn, and once the batch
    /// is done, the most at once.
    fn add_counter(&self, svg: &mut Svg, motion: &mut Motion) {
        let mut turn = 0;
        while turn < self.turns.len() {
            let count = self.on_their_way_during(turn);
            let until = (turn..self.turns.len())
                .find(|&later| self.on_their_way_during(later) != count)
                .unwrap_or(self.turns.len());
            let class = format!("count-{turn}");
            motion.between(&class, turn as f64, until as f64);
            let lines = if count == 1 { "line" } else { "lines" };
            svg.add(
                Element::text(700.0, 24.0, &format!("{count} {lines} on their way"))
                    .class(&format!("label end {class}")),
            );
            turn = until;
        }
        let most = (0..self.turns.len())
            .map(|turn| self.on_their_way_during(turn))
            .max()
            .unwrap_or(0);
        motion.appear("count-most", self.turns.len() as f64);
        svg.add(
            Element::text(
                700.0,
                24.0,
                &format!("up to {most} lines on their way at once"),
            )
            .class("label end count-most"),
        );
    }

    fn add_legend(&self, svg: &mut Svg, y: f64) {
        let x = Self::PLOT_LEFT;
        svg.add(
            Element::rect(x, y - 11.0, 12.0, 14.0)
                .attr("rx", 2)
                .class("turn"),
        );
        svg.add(
            Element::text(
                x + 18.0,
                y,
                "a turn: the task comes in, reads its line, steps down, prefetches the next line, goes out",
            )
            .class("label"),
        );
        let y = y + 20.0;
        svg.add(Element::rect(x, y - 6.5, 24.0, 5.0).class("way"));
        svg.add(Element::text(x + 30.0, y, "its line on its way").class("label"));
        svg.add(Element::path(&format!("M{} {} l3.5 3.5 l7 -7", x + 152.0, y - 4.0)).class("done"));
        svg.add(Element::text(x + 168.0, y, "its key looked up").class("label"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_the_lines_on_their_way_while_each_turn_runs() {
        // Four tasks in turn, then the ones not done, as the executor
        // drops each task once its key is looked up.
        let lanes = BatchLanes {
            tasks: 4,
            turns: vec![
                (0, true),
                (1, true),
                (2, true),
                (3, true),
                (0, false),
                (1, true),
                (2, false),
                (3, true),
                (1, false),
                (3, false),
            ],
        };
        let counts: Vec<usize> = (0..lanes.turns.len())
            .map(|turn| lanes.on_their_way_during(turn))
            .collect();
        assert_eq!(counts, [0, 1, 2, 3, 3, 2, 2, 1, 1, 0]);
    }
}
