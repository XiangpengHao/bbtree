//! The thought experiment of "Minimize cache misses": binary search and
//! linear search over the same 64 sorted `u64`s, eight to a 64-byte cache
//! line, played a cache miss at a time. Binary search halves what is left
//! with each miss; linear search rules out one line.

use std::cmp::Ordering;
use std::ops::Range;

use crate::svg::{Element, Motion, Svg};

/// A sorted array of `len` items and the index of the one searched for.
pub(crate) struct SearchSpace {
    len: usize,
    target: usize,
}

/// One cache miss of a search: the line it brought in, the items the
/// search compared there, and what was left to search after them.
struct Miss {
    line: usize,
    compared: Vec<usize>,
    left: Range<usize>,
}

/// Which search a row plays.
#[derive(Clone, Copy)]
enum Search {
    Binary,
    Linear,
}

impl Search {
    fn name(self) -> &'static str {
        match self {
            Self::Binary => "binary search",
            Self::Linear => "linear search",
        }
    }

    fn slug(self) -> &'static str {
        match self {
            Self::Binary => "binary",
            Self::Linear => "linear",
        }
    }
}

impl SearchSpace {
    const PER_LINE: usize = 8;
    const CELL: f64 = 6.0;
    const CELL_GAP: f64 = 1.0;
    const LINE_GAP: f64 = 6.0;
    const LEFT: f64 = 204.0;
    const STEP: f64 = 1.0;

    pub(crate) fn new() -> Self {
        Self {
            len: 64,
            target: 45,
        }
    }

    /// The misses of `search`, in order.
    fn misses(&self, search: Search) -> Vec<Miss> {
        match search {
            Search::Binary => self.binary_misses(),
            Search::Linear => self.linear_misses(),
        }
    }

    /// A probe in a line already brought in is compared within the latest
    /// miss: it costs no other.
    fn binary_misses(&self) -> Vec<Miss> {
        let mut misses: Vec<Miss> = Vec::new();
        let (mut low, mut high) = (0, self.len);
        loop {
            let probe = (low + high) / 2;
            let line = probe / Self::PER_LINE;
            if !misses.iter().any(|miss| miss.line == line) {
                misses.push(Miss {
                    line,
                    compared: Vec::new(),
                    left: low..high,
                });
            }
            match probe.cmp(&self.target) {
                Ordering::Less => low = probe + 1,
                Ordering::Greater => high = probe,
                Ordering::Equal => (low, high) = (probe, probe + 1),
            }
            let miss = misses.last_mut().expect("a miss for every probe");
            miss.compared.push(probe);
            miss.left = low..high;
            if probe == self.target {
                return misses;
            }
        }
    }

    fn linear_misses(&self) -> Vec<Miss> {
        (0..=self.target / Self::PER_LINE)
            .map(|line| {
                let start = line * Self::PER_LINE;
                let end = (start + Self::PER_LINE).min(self.target + 1);
                let left = if end > self.target {
                    self.target..self.target + 1
                } else {
                    end..self.len
                };
                Miss {
                    line,
                    compared: (start..end).collect(),
                    left,
                }
            })
            .collect()
    }

    fn cell_x(&self, item: usize) -> f64 {
        let line = item / Self::PER_LINE;
        Self::LEFT + item as f64 * (Self::CELL + Self::CELL_GAP) + line as f64 * Self::LINE_GAP
    }

    fn line_x(&self, line: usize) -> Range<f64> {
        let first = line * Self::PER_LINE;
        let last = first + Self::PER_LINE - 1;
        self.cell_x(first) - 2.0..self.cell_x(last) + Self::CELL + 2.0
    }

    pub(crate) fn svg(&self) -> Svg {
        let searches = [Search::Binary, Search::Linear];
        let steps = searches
            .iter()
            .map(|&search| self.misses(search).len())
            .max()
            .unwrap_or(1);
        let mut motion = Motion::looping(steps as f64, Self::STEP, 3.5);
        let mut svg = Svg::new(
            720.0,
            236.0,
            "Binary search and linear search over 64 sorted u64s, counted in cache misses",
        );
        svg.style(
            ".item { fill: var(--slot-filled); } \
             .ruled-out { fill: var(--rule); } \
             .fetched { fill: var(--marker); stroke: var(--marker-strong); stroke-width: 1.5; } \
             .compared { fill: var(--ink); } \
             .target { fill: none; stroke: var(--ink); stroke-width: 1.5; } \
             .line-index { fill: var(--muted); font-size: 11px; } \
             .count { font-size: 13px; font-weight: 600; }",
        );
        for line in 0..self.len / Self::PER_LINE {
            let span = self.line_x(line);
            svg.add(
                Element::text((span.start + span.end) / 2.0, 22.0, &format!("line {line}"))
                    .class("line-index middle"),
            );
        }
        svg.add(Element::text(Self::LEFT - 14.0, 22.0, "64-byte lines").class("label end"));
        for (row, &search) in searches.iter().enumerate() {
            let top = 44.0 + row as f64 * 92.0;
            svg.add(self.row(search, top, &mut motion));
        }
        svg.add(
            Element::text(
                16.0,
                svg.height() - 14.0,
                "Each step is one cache miss: one 64-byte line, 8 u64s, brought in from memory.",
            )
            .class("note"),
        );
        svg.style(motion.css());
        svg
    }

    /// One search's row at `top`: its name and count of misses, the items,
    /// and each miss's line, comparisons and ruling out, in turn.
    fn row(&self, search: Search, top: f64, motion: &mut Motion) -> Element {
        let misses = self.misses(search);
        let slug = search.slug();
        let mut row =
            Element::group().child(Element::text(16.0, top + 18.0, search.name()).class("strong"));
        // The count so far, one text a step: the last stays.
        for step in 0..misses.len() {
            let class = format!("{slug}-count-{step}");
            let last = step + 1 == misses.len();
            if last {
                motion.appear(&class, step as f64);
            } else {
                motion.between(&class, step as f64, (step + 1) as f64);
            }
            let count = step + 1;
            let words = if count == 1 { "miss" } else { "misses" };
            let found = if last { ", found" } else { "" };
            row = row.child(
                Element::text(16.0, top + 36.0, &format!("{count} cache {words}{found}"))
                    .class(&format!("count {class}")),
            );
        }
        // Each line, lit from the step it is fetched.
        for (step, miss) in misses.iter().enumerate() {
            let span = self.line_x(miss.line);
            let class = format!("{slug}-fetch-{step}");
            motion.appear(&class, step as f64);
            row = row.child(
                Element::rect(span.start, top + 4.0, span.end - span.start, 30.0)
                    .attr("rx", 3)
                    .class(&format!("fetched {class}")),
            );
        }
        // Items, each ruled out at the step after which it is no longer
        // left, and the comparisons made in each step.
        let ruled_out_at = |item: usize| misses.iter().position(|miss| !miss.left.contains(&item));
        for item in 0..self.len {
            let x = self.cell_x(item);
            row = row.child(Element::rect(x, top + 9.0, Self::CELL, 20.0).class("item"));
            if let Some(step) = ruled_out_at(item) {
                let class = format!("{slug}-out-{step}");
                row = row.child(
                    Element::rect(x, top + 9.0, Self::CELL, 20.0)
                        .class(&format!("ruled-out {class}")),
                );
            }
        }
        for (step, miss) in misses.iter().enumerate() {
            let class = format!("{slug}-out-{step}");
            motion.appear(&class, step as f64 + 0.5);
            let compared = format!("{slug}-compared-{step}");
            motion.appear(&compared, step as f64 + 0.2);
            for &item in &miss.compared {
                let x = self.cell_x(item) + Self::CELL / 2.0;
                row = row.child(
                    Element::path(&format!("M{} {} l-3.5 -6 h7 z", x, top + 2.0))
                        .class(&format!("compared {compared}")),
                );
            }
        }
        let x = self.cell_x(self.target);
        row.child(
            Element::rect(x - 1.5, top + 7.5, Self::CELL + 3.0, 23.0)
                .attr("rx", 2)
                .class("target"),
        )
        .child(Element::text(x + Self::CELL / 2.0, top + 48.0, "target").class("label middle"))
    }
}
