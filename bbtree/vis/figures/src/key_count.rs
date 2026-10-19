//! The figure of a node search: the count against binary search, over the
//! same eight inline keys and the same target. The count compares every
//! slot at once and has the child after one step; binary search probes one
//! slot, then the next, each waiting for the last. Eight slots and small
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

    /// A cell of the row at `y`: its fill class and its text.
    fn cell(&self, slot: usize, y: f64, class: &str, text: &str) -> Element {
        let x = self.cell_x(slot);
        Element::group()
            .class(class)
            .child(Element::rect(x, y, Self::CELL, 28.0).attr("rx", 4))
            .child(Element::text(x + Self::CELL / 2.0, y + 19.0, text).class("data middle"))
    }

    pub(crate) fn svg(&self) -> Svg {
        let probes = self.binary_probes();
        let steps = probes.len();
        let count = self.count();
        let mut motion = Motion::looping(steps as f64, 0.9, 3.5);
        let mut svg = Svg::new(
            720.0,
            176.0,
            "Counting the inline keys smaller than the target, against binary search",
        );
        svg.add(Element::text(24.0, 22.0, &format!("target {}", self.target)).class("strong"));
        let rows = [
            ("inline keys", 34.0),
            ("count", 76.0),
            ("binary search", 118.0),
        ];
        for (label, y) in rows {
            svg.add(Element::text(24.0, y + 19.0, label).class("label"));
        }
        let result_x = self.cell_x(self.slots.len()) + 6.0;
        for (slot, key) in self.slots.iter().enumerate() {
            let text = key.map_or("max".to_string(), |key| key.to_string());
            let class = if key.is_some() { "key" } else { "empty" };
            svg.add(self.cell(slot, 34.0, class, &text));
            let (side, bit) = if self.is_smaller(slot) {
                ("smaller", "1")
            } else {
                ("larger", "0")
            };
            svg.add(self.cell(slot, 76.0, &format!("{side} mask"), bit));
            if let Some(step) = probes.iter().position(|probe| *probe == slot) {
                let class = format!("probe-{step}");
                motion.appear(&class, step as f64);
                svg.add(self.cell(
                    slot,
                    118.0,
                    &format!("{side} {class}"),
                    &format!("{}", step + 1),
                ));
            }
        }
        motion.appear("mask", 0.0);
        motion.appear("count-done", 0.6);
        motion.appear("binary-done", steps as f64 - 0.4);
        svg.add(
            Element::text(result_x, 95.0, &format!("child {count}, in 1 step"))
                .class("strong count-done"),
        );
        let words = if steps == 1 { "step" } else { "steps" };
        svg.add(
            Element::text(
                result_x,
                137.0,
                &format!("child {count}, in {steps} {words}"),
            )
            .class("strong binary-done"),
        );
        svg.add(
            Element::text(
                24.0,
                168.0,
                "An empty slot holds the largest value, so it never counts.",
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
             .probe-0 text, .probe-1 text, .probe-2 text, .probe-3 text { font-weight: 600; }",
        );
        svg.style(motion.css());
        svg
    }
}
