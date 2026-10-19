//! The figure of `prefix_keys`: two separators in a parent, the keys of
//! the child between them, and an 8-byte window over each key. The keys
//! start with whatever the separators share, so the window slides past
//! those bytes, from where all keys tie to where they differ. Real
//! neighbors from the benchmarks' Wikipedia titles, as stored; the child's
//! first key is its left separator, as in bbtree, where a separator is the
//! first key of the node right of it.

use crate::svg::{Element, Motion, Svg};

/// A parent's two separators and the child's keys between them.
pub(crate) struct SeparatedKeys {
    left: &'static str,
    right: &'static str,
    keys: [&'static str; 3],
}

impl SeparatedKeys {
    const WIDTH: usize = 8;
    const COLUMN: f64 = 8.4;
    const KEYS_X: f64 = 150.0;

    pub(crate) fn new() -> Self {
        Self {
            left: "List_of_bridges_in_Afghanistan",
            right: "List_of_bridges_in_Albania",
            keys: [
                "List_of_bridges_in_Afghanistan",
                "List_of_bridges_in_Alabama",
                "List_of_bridges_in_Alaska",
            ],
        }
    }

    /// How many leading bytes the separators share: the child's prefix.
    fn shared_len(&self) -> usize {
        (self.left.bytes().zip(self.right.bytes()))
            .take_while(|(a, b)| a == b)
            .count()
    }

    /// `key`'s `WIDTH` bytes from `offset`, a dot for each zero byte that
    /// pads a window past the key's end.
    fn window(key: &str, offset: usize) -> String {
        (offset..offset + Self::WIDTH)
            .map(|at| key.as_bytes().get(at).map_or('·', |&byte| byte as char))
            .collect()
    }

    pub(crate) fn svg(&self) -> Svg {
        let shared = self.shared_len();
        debug_assert!(
            self.keys
                .iter()
                .all(|key| key.as_bytes()[..shared] == self.left.as_bytes()[..shared])
        );
        let mut svg = Svg::new(
            720.0,
            290.0,
            "The child's keys start with what its two separators share, so its inline keys start after those bytes",
        );
        let slide = shared as f64 * Self::COLUMN;
        // Two moments: the window over the first 8 bytes, then past the
        // shared ones.
        let mut motion = Motion::looping(2.0, 1.6, 3.5);
        motion.animate(
            "window",
            &[
                (0.0, "transform: translateX(0px)".into()),
                (1.0, "transform: translateX(0px)".into()),
                (1.5, format!("transform: translateX({slide}px)")),
            ],
        );
        motion.vanish("tied", 1.4);
        motion.appear("apart", 1.4);
        motion.appear("dimmed", 1.2);
        svg.style(&format!(
            ".node {{ fill: var(--paper); stroke: var(--rule-strong); }} \
             .shared {{ fill: var(--marker-soft); }} \
             .veil {{ fill: var(--panel); fill-opacity: 0.55; }} \
             .frame {{ fill: rgba(247, 164, 127, 0.22); stroke: var(--compare); stroke-width: 1.5; }} \
             .chip {{ fill: var(--well); }} \
             .chip-apart {{ fill: var(--compare-soft); stroke: var(--compare); }} \
             .dim {{ fill: var(--muted); }} \
             .edge {{ stroke: var(--rule-strong); stroke-width: 1.5; fill: none; }} \
             .verdict {{ font-size: 13px; font-weight: 600; }} \
             {}",
            motion.css()
        ));
        // The parent and its two separators.
        svg.add(
            Element::rect(16.0, 12.0, 688.0, 64.0)
                .attr("rx", 6)
                .class("node"),
        );
        svg.add(Element::text(28.0, 34.0, "parent").class("label"));
        svg.add(Element::text(28.0, 50.0, &format!("shared: {shared} bytes")).class("label"));
        let right_x = Self::KEYS_X + (self.left.len() as f64 + 4.0) * Self::COLUMN;
        for (x, separator) in [(Self::KEYS_X, self.left), (right_x, self.right)] {
            svg.add(
                Element::rect(x - 3.0, 30.0, shared as f64 * Self::COLUMN + 6.0, 26.0)
                    .attr("rx", 3)
                    .class("shared"),
            );
            svg.add(Element::columns(x, 48.0, separator, Self::COLUMN));
        }
        let edge_x = right_x - 2.0 * Self::COLUMN;
        svg.add(Element::path(&format!("M{edge_x} 76 V100")).class("edge"));
        // The child: its prefix length, its keys, and the window.
        svg.add(
            Element::rect(16.0, 100.0, 688.0, 136.0)
                .attr("rx", 6)
                .class("node"),
        );
        svg.add(Element::text(28.0, 122.0, "child").class("label"));
        svg.add(Element::text(28.0, 138.0, &format!("prefix: {shared} bytes")).class("label"));
        let chips_x = Self::KEYS_X + (self.keys[0].len() as f64 + 3.0) * Self::COLUMN;
        svg.add(Element::text(chips_x, 122.0, "its inline key").class("label"));
        for (row, key) in self.keys.iter().enumerate() {
            let y = 150.0 + row as f64 * 28.0;
            svg.add(
                Element::rect(
                    Self::KEYS_X - 3.0,
                    y - 18.0,
                    Self::WIDTH as f64 * Self::COLUMN + 6.0,
                    26.0,
                )
                .attr("rx", 4)
                .class("frame window"),
            );
            svg.add(Element::columns(Self::KEYS_X, y, key, Self::COLUMN));
            svg.add(
                Element::rect(
                    Self::KEYS_X - 1.0,
                    y - 15.0,
                    shared as f64 * Self::COLUMN + 2.0,
                    20.0,
                )
                .class("veil dimmed"),
            );
            svg.add(
                Element::rect(
                    chips_x,
                    y - 17.0,
                    Self::WIDTH as f64 * Self::COLUMN + 14.0,
                    24.0,
                )
                .attr("rx", 4)
                .class("chip tied"),
            );
            svg.add(
                Element::columns(chips_x + 7.0, y, &Self::window(key, 0), Self::COLUMN)
                    .class("tied"),
            );
            svg.add(
                Element::rect(
                    chips_x,
                    y - 17.0,
                    Self::WIDTH as f64 * Self::COLUMN + 14.0,
                    24.0,
                )
                .attr("rx", 4)
                .class("chip-apart apart"),
            );
            svg.add(
                Element::columns(chips_x + 7.0, y, &Self::window(key, shared), Self::COLUMN)
                    .class("apart"),
            );
        }
        svg.add(
            Element::text(
                28.0,
                264.0,
                "All three tie: compare the full keys, from the heap.",
            )
            .class("verdict note tied"),
        );
        svg.add(
            Element::text(
                28.0,
                264.0,
                "All three differ: decided by the inline keys, no heap read.",
            )
            .class("verdict apart"),
        );
        svg
    }
}
