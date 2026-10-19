//! The figure of a batch: four `u64` keys looked up together on one
//! thread, a row a key, played search by search. Each search lands as it
//! runs; from the prefetch a search ends with to that key's next search,
//! the line is on its way, while the other keys search. Read from bbtree's
//! own run of the batch (`TreeView::trace_many`), in the executor's order;
//! time is counted in searches, since the trace says nothing of when a
//! line arrives.

use std::ops::Range;

use bbtree::unsync::BBTreeMap;

use crate::svg::{Element, Motion, Svg};

/// A batch's searches, in the executor's order: each the lane that ran,
/// and whether it ended by fetching a line.
pub(crate) struct BatchLanes {
    keys: [u64; 4],
    polls: Vec<(usize, bool)>,
}

impl BatchLanes {
    const PLOT_LEFT: f64 = 196.0;
    const PLOT_RIGHT: f64 = 700.0;
    const ROW: f64 = 34.0;
    const TOP: f64 = 48.0;

    pub(crate) fn of_lookups(map: &BBTreeMap<u64, u64>, keys: [u64; 4]) -> Self {
        let view = map.inspect();
        let trace = view.trace_many(keys.each_ref());
        assert!(trace.batches(), "a map this size runs its batches in lanes");
        let polls = trace
            .polls()
            .iter()
            .map(|poll| (poll.lane(), poll.fetch().is_some()))
            .collect();
        Self { keys, polls }
    }

    /// When poll `index`'s line is on its way: from the end of the search
    /// that fetched it to its key's next search, which reads it; `None` if
    /// nothing came between.
    fn on_its_way(&self, index: usize) -> Option<Range<f64>> {
        let (lane, fetched) = self.polls[index];
        let next = (index + 1..self.polls.len()).find(|&later| self.polls[later].0 == lane)?;
        let start = index as f64 + 1.0;
        (fetched && next as f64 > start).then_some(start..next as f64)
    }

    /// The most lines on their way at once, over the batch.
    fn most_on_their_way(&self) -> usize {
        (0..self.polls.len())
            .map(|search| {
                let during = search as f64 + 0.5;
                (0..self.polls.len())
                    .filter(|&index| {
                        self.on_its_way(index)
                            .is_some_and(|way| way.contains(&during))
                    })
                    .count()
            })
            .max()
            .unwrap_or(0)
    }

    fn unit(&self) -> f64 {
        (Self::PLOT_RIGHT - Self::PLOT_LEFT) / self.polls.len() as f64
    }

    fn x(&self, at: f64) -> f64 {
        Self::PLOT_LEFT + at * self.unit()
    }

    pub(crate) fn svg(&self) -> Svg {
        let total = self.polls.len() as f64;
        let rows_bottom = Self::TOP + self.keys.len() as f64 * Self::ROW;
        let mut svg = Svg::new(
            720.0,
            rows_bottom + 56.0,
            "Four u64 keys looked up together on one thread: while one key's line is on its way, the others search",
        );
        let mut motion = Motion::looping(total, 0.32, 3.5);
        let unit = self.unit();
        svg.add(
            Element::text(
                24.0,
                24.0,
                &format!(
                    "Up to {} lines on their way at once; one key alone has one.",
                    self.most_on_their_way()
                ),
            )
            .class("strong"),
        );
        for (lane, key) in self.keys.iter().enumerate() {
            let y = Self::TOP + lane as f64 * Self::ROW;
            svg.add(Element::text(24.0, y + 17.0, &format!("{key}")).class("data"));
            svg.add(
                Element::line(Self::PLOT_LEFT, y + 12.0, Self::PLOT_RIGHT, y + 12.0)
                    .class("baseline"),
            );
        }
        for (index, &(lane, _)) in self.polls.iter().enumerate() {
            let y = Self::TOP + lane as f64 * Self::ROW;
            if let Some(way) = self.on_its_way(index) {
                let class = format!("way-{index}");
                motion.animate(
                    &class,
                    &[
                        (0.0, "transform: scaleX(0)".into()),
                        (way.start, "transform: scaleX(0)".into()),
                        (way.end, "transform: scaleX(1)".into()),
                    ],
                );
                svg.add(
                    Element::rect(
                        self.x(way.start),
                        y + 9.0,
                        (way.end - way.start) * unit,
                        6.0,
                    )
                    .class("track"),
                );
                svg.add(
                    Element::rect(
                        self.x(way.start),
                        y + 9.0,
                        (way.end - way.start) * unit,
                        6.0,
                    )
                    .class(&format!("way {class}")),
                );
            }
            let class = format!("search-{index}");
            motion.appear(&class, index as f64);
            svg.add(
                Element::rect(self.x(index as f64) + 0.1 * unit, y, 0.8 * unit, 24.0)
                    .attr("rx", 2)
                    .class(&format!("search {class}")),
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
                Self::TOP - 8.0,
                Self::PLOT_LEFT,
                rows_bottom,
            )
            .class("playhead-line playhead"),
        );
        let legend_y = rows_bottom + 30.0;
        svg.add(
            Element::rect(Self::PLOT_LEFT, legend_y - 11.0, 12.0, 14.0)
                .attr("rx", 2)
                .class("search"),
        );
        svg.add(Element::text(Self::PLOT_LEFT + 18.0, legend_y, "search").class("label"));
        svg.add(Element::rect(Self::PLOT_LEFT + 80.0, legend_y - 7.0, 24.0, 6.0).class("way"));
        svg.add(
            Element::text(
                Self::PLOT_LEFT + 110.0,
                legend_y,
                "line on its way, from the key's prefetch to its next search",
            )
            .class("label"),
        );
        svg.style(
            ".search { fill: var(--ink); } \
             .track { fill: var(--marker); opacity: 0.25; } \
             .way { fill: var(--marker-strong); transform-box: fill-box; transform-origin: left center; } \
             .baseline { stroke: var(--rule); } \
             .playhead-line { stroke: var(--link); stroke-width: 1.5; }",
        );
        svg.style(motion.css());
        svg
    }
}
