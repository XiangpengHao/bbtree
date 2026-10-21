//! The figure of a node search: binary search against the count, over the
//! same eight inline keys and the same target, each row a compare's result
//! per slot (1: the key is smaller). Binary search compares one slot, then
//! the one that compare chose, each waiting for the last; the count
//! compares every slot at once, and its ones are the child to follow. Both
//! reach the same child, marked across the rows. Eight slots and small
//! numbers: the idea of the count, not a node of a tree.

use crate::svg::{Element, Motion, Svg};

/// A node's inline keys, the last slot empty, and a target to count
/// against.
pub(crate) struct KeyCount {
    slots: [Option<u16>; 8],
    target: u16,
}

impl KeyCount {
    const LEFT: f64 = 112.0;
    const CELL: f64 = 50.0;
    const GAP: f64 = 6.0;
    const ROW: f64 = 28.0;

    pub(crate) fn new() -> Self {
        Self {
            slots: [
                Some(3),
                Some(11),
                Some(27),
                Some(40),
                Some(58),
                Some(71),
                Some(90),
                None,
            ],
            target: 45,
        }
    }

    /// Whether slot `slot` counts: an empty one holds the largest value,
    /// so never does.
    fn is_smaller(&self, slot: usize) -> bool {
        self.slots[slot].is_some_and(|key| key < self.target)
    }

    /// The child to follow.
    fn count(&self) -> usize {
        (0..self.slots.len())
            .filter(|&slot| self.is_smaller(slot))
            .count()
    }

    /// The slots a binary search probes, in order, to the same child.
    fn binary_probes(&self) -> Vec<usize> {
        let (mut low, mut high) = (0, self.slots.len());
        let mut probes = Vec::new();
        while low < high {
            let mid = (low + high) / 2;
            probes.push(mid);
            if self.is_smaller(mid) {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        probes
    }

    fn cell_x(&self, slot: usize) -> f64 {
        Self::LEFT + slot as f64 * (Self::CELL + Self::GAP)
    }

    fn cell_center(&self, slot: usize) -> f64 {
        self.cell_x(slot) + Self::CELL / 2.0
    }

    /// A cell of the row at `y`: its fill class and its text.
    fn cell(&self, slot: usize, y: f64, class: &str, text: &str) -> Element {
        let x = self.cell_x(slot);
        Element::group()
            .class(class)
            .child(Element::rect(x, y, Self::CELL, Self::ROW).attr("rx", 4))
            .child(Element::text(x + Self::CELL / 2.0, y + 19.0, text).class("data middle"))
    }

    /// Slot `slot`'s compare with the target: its fill class and its bit.
    fn compare(&self, slot: usize) -> (&'static str, &'static str) {
        if self.is_smaller(slot) {
            ("smaller", "1")
        } else {
            ("larger", "0")
        }
    }

    /// An arrow over the row at `y`, from one probe to the one it chose.
    fn arrow(&self, from: usize, to: usize, y: f64, class: &str) -> Element {
        // It leaves its probe off center, toward where it goes, clear of
        // the arrow that came in.
        let x2 = self.cell_center(to);
        let x1 = self.cell_center(from) + 8.0 * (x2 - self.cell_center(from)).signum();
        let rise = 12.0 + (x2 - x1).abs() / 8.0;
        let (tip, middle) = (y - 3.0, (x1 + x2) / 2.0);
        Element::group()
            .class(class)
            .child(
                Element::path(&format!(
                    "M {x1} {y} Q {middle} {} {x2} {}",
                    y - 2.0 * rise,
                    tip - 6.0
                ))
                .class("arrow"),
            )
            .child(
                Element::path(&format!(
                    "M {} {} L {} {} L {x2} {tip} Z",
                    x2 - 4.0,
                    tip - 7.0,
                    x2 + 4.0,
                    tip - 7.0
                ))
                .class("arrow-head"),
            )
    }

    pub(crate) fn svg(&self) -> Svg {
        const KEYS: f64 = 40.0;
        const BINARY: f64 = 104.0;
        const COUNT: f64 = 162.0;
        let probes = self.binary_probes();
        let steps = probes.len();
        let count = self.count();
        let mut motion = Motion::looping(steps as f64, 0.9, 3.5);
        let mut svg = Svg::new(
            720.0,
            232.0,
            "Binary search against the count, over the same inline keys and the same target",
        );
        svg.add(Element::text(24.0, 24.0, &format!("looking up {}", self.target)).class("strong"));
        for (label, y) in [
            ("inline keys", KEYS),
            ("binary search", BINARY),
            ("count", COUNT),
        ] {
            svg.add(Element::text(24.0, y + 19.0, label).class("label"));
        }

        // The answer: the edge between the last smaller key and the first
        // that isn't, which both searches reach.
        let edge = self.cell_x(count) - Self::GAP / 2.0;
        motion.appear("answer", 0.6);
        svg.add(
            Element::group()
                .class("answer")
                .child(Element::line(edge, KEYS - 6.0, edge, COUNT + Self::ROW + 6.0).class("edge"))
                .child(
                    Element::rect(edge - 32.0, 6.0, 64.0, 22.0)
                        .attr("rx", 11)
                        .class("edge-label"),
                )
                .child(
                    Element::text(edge, 21.5, &format!("child {count}"))
                        .class("strong middle on-marker"),
                ),
        );

        for (slot, key) in self.slots.iter().enumerate() {
            let text = key.map_or("max".to_string(), |key| key.to_string());
            let class = if key.is_some() { "key" } else { "empty" };
            svg.add(self.cell(slot, KEYS, class, &text));
            let (side, bit) = self.compare(slot);
            svg.add(self.cell(slot, COUNT, &format!("{side} mask"), bit));
        }
        motion.appear("mask", 0.0);

        const ORDINALS: [&str; 4] = ["1st", "2nd", "3rd", "4th"];
        for (step, &slot) in probes.iter().enumerate() {
            let class = format!("probe-{step}");
            motion.appear(&class, step as f64);
            let (side, bit) = self.compare(slot);
            svg.add(self.cell(slot, BINARY, &format!("{side} {class}"), bit));
            svg.add(
                Element::text(
                    self.cell_center(slot),
                    BINARY + Self::ROW + 14.0,
                    ORDINALS[step],
                )
                .class(&format!("label middle {class}")),
            );
            if step > 0 {
                svg.add(self.arrow(probes[step - 1], slot, BINARY, &class));
            }
        }

        let result_x = self.cell_x(self.slots.len()) + 6.0;
        motion.appear("binary-done", steps as f64 - 0.4);
        svg.add(
            Element::text(
                result_x,
                BINARY + 19.0,
                &format!("{steps} compares in turn"),
            )
            .class("strong binary-done"),
        );
        motion.appear("count-done", 0.6);
        svg.add(
            Element::text(
                result_x,
                COUNT + 19.0,
                &format!("{} compares at once", self.slots.len()),
            )
            .class("strong count-done"),
        );
        svg.add(
            Element::text(
                24.0,
                COUNT + Self::ROW + 32.0,
                &format!(
                    "1: the key is smaller than {}. An empty slot holds the largest possible key, so it never is.",
                    self.target
                ),
            )
            .class("note"),
        );
        svg.style(
            ".key rect { fill: var(--compare-soft); } \
             .empty rect { fill: var(--panel); stroke: var(--rule-strong); } \
             .empty text { fill: var(--muted); } \
             .smaller rect { fill: var(--slot-smaller); } \
             .smaller text { fill: var(--panel); } \
             .larger rect { fill: var(--slot-larger); } \
             .arrow { fill: none; stroke: var(--graphite); stroke-width: 1.5; } \
             .arrow-head { fill: var(--graphite); } \
             .edge { stroke: var(--marker-strong); stroke-width: 2.5; } \
             .edge-label { fill: var(--marker); stroke: var(--marker-strong); stroke-width: 1.5; }",
        );
        svg.style(motion.css());
        svg
    }
}
