//! The figure of `u64` keys as bytes: four keys of one leaf of a real
//! tree, big-endian, split where the leaf splits them: the prefix its
//! separators share, which the leaf skips, the inline key it keeps of
//! each, and the rest, which it compares only on a tie. Every number is
//! the leaf's own, read through bbtree's inspect views.

use bbtree::unsync::BBTreeMap;

use crate::svg::{Element, Svg};

/// A leaf's first keys and how the leaf splits each.
pub(crate) struct IntegerLeaf {
    keys: Vec<u64>,
    prefix_len: usize,
    inline_width: usize,
}

impl IntegerLeaf {
    const SHOWN: usize = 4;
    const BYTE: f64 = 56.0;
    const LEFT: f64 = 200.0;

    /// The leaf the lookup of `key` in `map` ends in.
    pub(crate) fn of_lookup(map: &BBTreeMap<u64, u64>, key: u64) -> Self {
        let view = map.inspect();
        let trace = view.trace(&key);
        let leaf = trace
            .last()
            .expect("a lookup reads at least the root")
            .node();
        assert!(leaf.is_leaf());
        let width = leaf.inline_key_width().expect("u64 keys have inline keys");
        let keys: Vec<u64> = (0..Self::SHOWN.min(leaf.len()))
            .map(|index| *leaf.key(index))
            .collect();
        // The leaf's inline keys are the keys' bytes past its prefix.
        for (index, key) in keys.iter().enumerate() {
            let window = leaf
                .inline_key(index)
                .expect("u64 keys have inline keys")
                .to_be_bytes();
            let bytes = key.to_be_bytes();
            let kept = (leaf.prefix_len() + width).min(8);
            assert_eq!(
                window[..kept - leaf.prefix_len()],
                bytes[leaf.prefix_len()..kept]
            );
        }
        Self {
            keys,
            prefix_len: leaf.prefix_len(),
            inline_width: width,
        }
    }

    fn byte_x(&self, byte: usize) -> f64 {
        Self::LEFT + byte as f64 * Self::BYTE
    }

    pub(crate) fn svg(&self) -> Svg {
        let height = 96.0 + self.keys.len() as f64 * 34.0;
        let mut svg = Svg::new(
            720.0,
            height,
            "Four u64 keys of one leaf as bytes: the prefix the leaf skips, the 2-byte inline key it keeps, the rest",
        );
        svg.style(
            ".byte { fill: var(--panel); stroke: var(--rule-strong); } \
             .prefix { fill: var(--well); stroke: var(--rule); } \
             .inline { fill: var(--compare-soft); stroke: var(--compare); stroke-width: 1.5; } \
             .prefix-text { fill: var(--muted); } \
             .brace { stroke: var(--graphite); fill: none; stroke-width: 1.2; }",
        );
        let kept_end = (self.prefix_len + self.inline_width).min(8);
        let parts = [
            (
                0,
                self.prefix_len,
                format!("prefix: {} bytes", self.prefix_len),
            ),
            (
                self.prefix_len,
                kept_end,
                format!("inline key: {} bytes", self.inline_width),
            ),
            (kept_end, 8, "rest: compared only on a tie".to_string()),
        ];
        for (start, end, label) in parts {
            if start == end {
                continue;
            }
            let (left, right) = (self.byte_x(start) + 2.0, self.byte_x(end) - 6.0);
            svg.add(Element::path(&format!("M{left} 40 V34 H{right} V40")).class("brace"));
            svg.add(Element::text((left + right) / 2.0, 26.0, &label).class("label middle"));
        }
        svg.add(Element::text(24.0, 66.0, "key (big-endian bytes)").class("label"));
        for (row, key) in self.keys.iter().enumerate() {
            let y = 52.0 + row as f64 * 34.0;
            svg.add(Element::text(24.0, y + 40.0, &format!("{key}")).class("data"));
            for (byte, value) in key.to_be_bytes().iter().enumerate() {
                let x = self.byte_x(byte);
                let (cell, text) = if byte < self.prefix_len {
                    ("prefix", "data prefix-text")
                } else if byte < kept_end {
                    ("inline", "data")
                } else {
                    ("byte", "data")
                };
                svg.add(
                    Element::rect(x, y + 22.0, Self::BYTE - 6.0, 26.0)
                        .attr("rx", 3)
                        .class(cell),
                );
                svg.add(
                    Element::text(
                        x + (Self::BYTE - 6.0) / 2.0,
                        y + 40.0,
                        &format!("{value:02x}"),
                    )
                    .class(&format!("{text} middle")),
                );
            }
        }
        svg
    }
}
