//! The two figures of `String` keys in a node, over the same eight
//! Wikipedia titles and the same lookup. Without inline keys, the node
//! holds only each `String`'s pointer, length and capacity, so every
//! comparison of a binary search follows a pointer to the heap. With
//! inline keys, the first 8 bytes of every key sit in one line of the
//! node: one compare of all of them leaves one candidate, and only it is
//! read from the heap.

use crate::svg::{Element, Motion, Svg};

/// Eight sorted keys of a node, as Wikipedia stores its titles, and the
/// one looked up.
pub(crate) struct StringNode {
    titles: [&'static str; 8],
    target: usize,
    inline_keys: bool,
}

/// Where a step of the lookup leads: the comparison's outcome.
#[derive(Clone, Copy)]
enum Outcome {
    Smaller,
    Equal,
    Larger,
}

impl Outcome {
    fn class(self) -> &'static str {
        match self {
            Self::Smaller => "smaller",
            Self::Equal => "equal",
            Self::Larger => "larger",
        }
    }
}

impl StringNode {
    const TITLES: [&'static str; 8] = [
        "Ada_Lovelace",
        "Alan_Turing",
        "Barbara_Liskov",
        "Donald_Knuth",
        "Edsger_W._Dijkstra",
        "Grace_Hopper",
        "Jim_Gray",
        "John_von_Neumann",
    ];
    /// Where each key's bytes lie on the heap: a slot of the heap's grid,
    /// so that neighbors in the node land apart, as allocations do.
    const HEAP_SLOTS: [(usize, usize); 8] = [
        (2, 0),
        (0, 1),
        (3, 1),
        (1, 0),
        (0, 0),
        (2, 1),
        (3, 0),
        (1, 1),
    ];
    const INLINE_WIDTH: usize = 8;
    /// The steps of the lookup with inline keys: compare them all, read
    /// the candidate's `String`, read its bytes.
    const INLINE_STEPS: usize = 3;
    const LEFT: f64 = 24.0;
    const SLOT: f64 = 84.0;
    const COLUMN: f64 = 8.1;

    pub(crate) fn plain() -> Self {
        Self {
            titles: Self::TITLES,
            target: 5,
            inline_keys: false,
        }
    }

    pub(crate) fn with_inline_keys() -> Self {
        Self {
            inline_keys: true,
            ..Self::plain()
        }
    }

    fn outcome(&self, slot: usize) -> Outcome {
        match self.titles[slot].cmp(self.titles[self.target]) {
            std::cmp::Ordering::Less => Outcome::Smaller,
            std::cmp::Ordering::Equal => Outcome::Equal,
            std::cmp::Ordering::Greater => Outcome::Larger,
        }
    }

    /// The keys a binary search over the full keys compares, in order.
    fn binary_probes(&self) -> Vec<usize> {
        let (mut low, mut high) = (0, self.titles.len());
        let mut probes = Vec::new();
        loop {
            let probe = (low + high) / 2;
            probes.push(probe);
            match self.outcome(probe) {
                Outcome::Smaller => low = probe + 1,
                Outcome::Larger => high = probe,
                Outcome::Equal => return probes,
            }
        }
    }

    fn slot_x(&self, slot: usize) -> f64 {
        Self::LEFT + slot as f64 * Self::SLOT
    }

    fn heap_box(&self, slot: usize) -> (f64, f64, f64) {
        let (column, row) = Self::HEAP_SLOTS[slot];
        let width = self.titles[slot].len() as f64 * Self::COLUMN + 16.0;
        let x = Self::LEFT + column as f64 * 172.0 + if row == 1 { 30.0 } else { 0.0 };
        let y = self.heap_top() + 30.0 + row as f64 * 40.0;
        (x, y, width)
    }

    fn node_top(&self) -> f64 {
        40.0
    }

    fn headers_top(&self) -> f64 {
        if self.inline_keys {
            112.0
        } else {
            self.node_top() + 12.0
        }
    }

    fn heap_top(&self) -> f64 {
        self.headers_top() + 92.0
    }

    pub(crate) fn svg(&self) -> Svg {
        let height = self.heap_top() + 130.0;
        let title = if self.inline_keys {
            "A node of String keys with 8-byte inline keys: one compare of all of them, one heap read"
        } else {
            "A node of String keys: each comparison of a binary search reads the heap"
        };
        let mut svg = Svg::new(720.0, height, title);
        svg.style(
            ".node { fill: var(--paper); stroke: var(--rule-strong); } \
             .header { fill: var(--panel); stroke: var(--rule-strong); } \
             .heap { fill: var(--panel); stroke: var(--rule-strong); } \
             .pointer { stroke: var(--rule-strong); stroke-width: 1; fill: none; } \
             .lit { fill: var(--marker); stroke: var(--marker-strong); stroke-width: 1.5; } \
             .lit-pointer { stroke: var(--marker-strong); stroke-width: 2; fill: none; } \
             .inline { fill: var(--compare-soft); stroke: var(--compare); } \
             .smaller { fill: var(--slot-smaller); } \
             .larger { fill: var(--slot-larger); } \
             .equal { fill: var(--marker); stroke: var(--marker-strong); stroke-width: 1.5; } \
             text.on-smaller { fill: var(--panel); } \
             .count { font-size: 13px; font-weight: 600; }",
        );
        let steps = if self.inline_keys {
            Self::INLINE_STEPS
        } else {
            self.binary_probes().len()
        };
        let mut motion = Motion::looping(steps as f64, 1.1, 3.5);
        svg.add(self.node(&mut motion));
        svg.add(self.heap(&mut motion));
        svg.style(motion.css());
        svg
    }

    fn node(&self, motion: &mut Motion) -> Element {
        let top = self.node_top();
        let bottom = self.headers_top() + 56.0;
        let target = self.titles[self.target];
        let mut node = Element::group()
            .child(Element::text(Self::LEFT, 22.0, &format!("looking up {target}")).class("strong"))
            .child(
                Element::rect(Self::LEFT - 10.0, top, 8.0 * Self::SLOT + 4.0, bottom - top)
                    .attr("rx", 6)
                    .class("node"),
            );
        if self.inline_keys {
            node = node.child(self.inline_row(motion));
        }
        let headers = self.headers_top();
        node = node.child(
            Element::text(
                Self::LEFT,
                headers - 4.0,
                "Strings: pointer, length, capacity, 24 bytes each",
            )
            .class("label"),
        );
        for slot in 0..self.titles.len() {
            let x = self.slot_x(slot);
            node = node
                .child(
                    Element::rect(x, headers + 2.0, Self::SLOT - 8.0, 26.0)
                        .attr("rx", 3)
                        .class("header"),
                )
                .child(
                    Element::text(
                        x + 8.0,
                        headers + 20.0,
                        &format!("ptr · {}", self.titles[slot].len()),
                    )
                    .class("data label"),
                );
        }
        for (step, slot) in self.lit_headers() {
            let class = format!("header-lit-{step}");
            motion.appear(&class, step as f64);
            let x = self.slot_x(slot);
            node = node
                .child(
                    Element::rect(x, headers + 2.0, Self::SLOT - 8.0, 26.0)
                        .attr("rx", 3)
                        .class(&format!("lit {class}")),
                )
                .child(
                    Element::text(
                        x + 8.0,
                        headers + 20.0,
                        &format!("ptr · {}", self.titles[slot].len()),
                    )
                    .class(&format!("data on-marker {class}")),
                );
        }
        node
    }

    /// The `String`s the lookup reads, with the step it reads each.
    fn lit_headers(&self) -> Vec<(usize, usize)> {
        if self.inline_keys {
            vec![(1, self.target)]
        } else {
            self.binary_probes().into_iter().enumerate().collect()
        }
    }

    /// The node's inline keys, one 64-byte line, compared all at once in
    /// the first step.
    fn inline_row(&self, motion: &mut Motion) -> Element {
        let top = self.node_top() + 26.0;
        let target = &self.titles[self.target][..Self::INLINE_WIDTH];
        motion.appear("compare", 0.0);
        let mut row = Element::group().child(
            Element::text(
                Self::LEFT,
                top - 6.0,
                &format!("inline keys: the first 8 bytes of each, 64 bytes, one cache line · compared with \"{target}\""),
            )
            .class("label"),
        );
        for slot in 0..self.titles.len() {
            let x = self.slot_x(slot);
            let outcome = self.outcome(slot);
            let text = &self.titles[slot][..Self::INLINE_WIDTH];
            let text_class = match outcome {
                Outcome::Smaller => "on-smaller compare",
                Outcome::Equal => "on-marker compare",
                Outcome::Larger => "compare",
            };
            row = row
                .child(
                    Element::rect(x, top, Self::SLOT - 8.0, 26.0)
                        .attr("rx", 3)
                        .class("inline"),
                )
                .child(Element::columns(x + 6.0, top + 18.0, text, Self::COLUMN))
                .child(
                    Element::rect(x, top, Self::SLOT - 8.0, 26.0)
                        .attr("rx", 3)
                        .class(&format!("{} compare", outcome.class())),
                )
                .child(
                    Element::columns(x + 6.0, top + 18.0, text, Self::COLUMN)
                        .class(&format!("data {text_class}")),
                );
        }
        row
    }

    fn heap(&self, motion: &mut Motion) -> Element {
        let top = self.heap_top();
        let headers = self.headers_top();
        let mut heap = Element::group().child(
            Element::text(
                Self::LEFT,
                top + 14.0,
                "the heap: each key's bytes, wherever they were allocated",
            )
            .class("label"),
        );
        for slot in 0..self.titles.len() {
            let (x, y, width) = self.heap_box(slot);
            let from = self.slot_x(slot) + (Self::SLOT - 8.0) / 2.0;
            heap = heap
                .child(
                    Element::path(&Self::pointer(from, headers + 28.0, x + width / 2.0, y))
                        .class("pointer"),
                )
                .child(Element::rect(x, y, width, 26.0).attr("rx", 3).class("heap"))
                .child(Element::columns(
                    x + 8.0,
                    y + 18.0,
                    self.titles[slot],
                    Self::COLUMN,
                ));
        }
        let reads = self.lit_headers();
        for (index, &(step, slot)) in reads.iter().enumerate() {
            let at = if self.inline_keys {
                step as f64 + 1.0
            } else {
                step as f64 + 0.4
            };
            let class = format!("heap-lit-{index}");
            motion.appear(&class, at);
            let (x, y, width) = self.heap_box(slot);
            let from = self.slot_x(slot) + (Self::SLOT - 8.0) / 2.0;
            heap = heap
                .child(
                    Element::path(&Self::pointer(from, headers + 28.0, x + width / 2.0, y))
                        .class(&format!("lit-pointer {class}")),
                )
                .child(
                    Element::rect(x, y, width, 26.0)
                        .attr("rx", 3)
                        .class(&format!("lit {class}")),
                )
                .child(
                    Element::columns(x + 8.0, y + 18.0, self.titles[slot], Self::COLUMN)
                        .class(&format!("data on-marker {class}")),
                );
            let last = index + 1 == reads.len();
            let count_class = format!("heap-count-{index}");
            if last {
                motion.appear(&count_class, at);
            } else {
                motion.between(&count_class, at, reads[index + 1].0 as f64 + 0.4);
            }
            let count = index + 1;
            let words = if count == 1 {
                "heap read"
            } else {
                "heap reads"
            };
            heap = heap.child(
                Element::text(720.0 - Self::LEFT, top + 14.0, &format!("{count} {words}"))
                    .class(&format!("count end {count_class}")),
            );
        }
        heap
    }

    /// A pointer from a `String` down to its bytes.
    fn pointer(x1: f64, y1: f64, x2: f64, y2: f64) -> String {
        let middle = (y1 + y2) / 2.0;
        format!("M{x1} {y1} C{x1} {middle} {x2} {middle} {x2} {y2}")
    }
}
