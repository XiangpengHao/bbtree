//! The two Results charts of "How it works": what one lookup costs std's
//! `BTreeMap` and bbtree, in cache lines brought in from memory and in
//! instructions, a pair of bars a kind of key. From bbtree-eval's
//! `bbtree-bench --perf` run on the Ryzen 9 9900X, gets that hit, 10M
//! entries, `u64` values (`vis/data/9900x-20261018/lookups.csv`).

use std::fs;
use std::io;
use std::path::Path;

use crate::svg::{Element, Motion, Svg};

/// What a chart counts per lookup.
#[derive(Clone, Copy)]
pub(crate) enum LookupCount {
    LinesFromMemory,
    Instructions,
}

impl LookupCount {
    fn column(self) -> &'static str {
        match self {
            Self::LinesFromMemory => "lines_from_memory_per_op",
            Self::Instructions => "instructions_per_op",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::LinesFromMemory => {
                "Cache lines from memory per lookup, std's BTreeMap and bbtree"
            }
            Self::Instructions => "Instructions per lookup, std's BTreeMap and bbtree",
        }
    }

    fn unit(self) -> &'static str {
        match self {
            Self::LinesFromMemory => "cache lines from memory per lookup",
            Self::Instructions => "instructions per lookup",
        }
    }

    /// The value as the chart shows it: to a tenth of a line, to a whole
    /// instruction.
    fn shown(self, value: f64) -> f64 {
        match self {
            Self::LinesFromMemory => (value * 10.0).round() / 10.0,
            Self::Instructions => value.round(),
        }
    }

    fn format(self, value: f64) -> String {
        match self {
            Self::LinesFromMemory => format!("{:.1}", self.shown(value)),
            Self::Instructions => format!("{:.0}", self.shown(value)),
        }
    }
}

/// The counts of each kind of key, std's and bbtree's.
pub(crate) struct LookupCounts {
    count: LookupCount,
    rows: Vec<(&'static str, f64, f64)>,
}

impl LookupCounts {
    /// The kinds of key, as the bench names them and as the post does.
    const KEYS: [(&'static str, &'static str); 4] = [
        ("u64", "u64"),
        ("hash32", "32-byte hashes"),
        ("str-json", "JSON keys"),
        ("wiki-titles", "Wikipedia titles"),
    ];
    const LEFT: f64 = 150.0;
    const RIGHT: f64 = 560.0;
    const BAR: f64 = 18.0;
    const ROW: f64 = 56.0;
    const TOP: f64 = 40.0;

    pub(crate) fn read(csv: &Path, count: LookupCount) -> io::Result<Self> {
        let text = fs::read_to_string(csv)?;
        let mut lines = text.lines();
        let header: Vec<&str> = lines.next().unwrap_or("").split(',').collect();
        let column = |name: &str| header.iter().position(|field| *field == name);
        let (map, key, value) = (column("map"), column("key"), column(count.column()));
        let (Some(map), Some(key), Some(value)) = (map, key, value) else {
            return Err(io::Error::other(format!(
                "{} lacks map, key or {}",
                csv.display(),
                count.column()
            )));
        };
        let rows: Vec<Vec<&str>> = lines.map(|line| line.split(',').collect()).collect();
        let of = |bench_key: &str, bench_map: &str| {
            rows.iter()
                .find(|row| row[key] == bench_key && row[map] == bench_map)
                .and_then(|row| row[value].parse::<f64>().ok())
        };
        let rows = Self::KEYS
            .iter()
            .filter_map(|&(bench_key, name)| {
                Some((name, of(bench_key, "std-btree")?, of(bench_key, "bbtree")?))
            })
            .collect();
        Ok(Self { count, rows })
    }

    fn top_value(&self) -> f64 {
        (self.rows.iter())
            .map(|&(_, std, ours)| std.max(ours))
            .fold(0.0, f64::max)
    }

    fn width(&self, value: f64) -> f64 {
        value / self.top_value() * (Self::RIGHT - Self::LEFT)
    }

    pub(crate) fn svg(&self) -> Svg {
        let height = Self::TOP + self.rows.len() as f64 * Self::ROW;
        let mut svg = Svg::new(720.0, height, self.count.title());
        let mut motion = Motion::once(1.0, 0.9);
        motion.animate(
            "grows",
            &[
                (0.0, "transform: scaleX(0)".into()),
                (1.0, "transform: scaleX(1)".into()),
            ],
        );
        motion.appear("lands", 0.95);
        svg.style(
            ".bar { transform-box: fill-box; transform-origin: left center; } \
             .theirs { fill: var(--theirs-1); } \
             .ours { fill: var(--marker); stroke: var(--marker-strong); stroke-width: 1.5; } \
             .swatch-label { fill: var(--graphite); }",
        );
        svg.add(
            Element::rect(Self::LEFT, 10.0, 12.0, 12.0)
                .attr("rx", 2)
                .class("theirs"),
        );
        svg.add(Element::text(Self::LEFT + 18.0, 20.0, "std BTreeMap").class("swatch-label"));
        svg.add(
            Element::rect(Self::LEFT + 130.0, 10.0, 12.0, 12.0)
                .attr("rx", 2)
                .class("ours"),
        );
        svg.add(Element::text(Self::LEFT + 148.0, 20.0, "bbtree").class("swatch-label strong"));
        svg.add(Element::text(700.0, 20.0, self.count.unit()).class("label end"));
        for (index, &(name, std, ours)) in self.rows.iter().enumerate() {
            let y = Self::TOP + index as f64 * Self::ROW;
            svg.add(Element::text(24.0, y + Self::BAR + 6.0, name).class("strong"));
            for (offset, value, class) in [(0.0, std, "theirs"), (Self::BAR + 2.0, ours, "ours")] {
                let width = self.width(value);
                svg.add(
                    Element::rect(Self::LEFT, y + offset, width, Self::BAR)
                        .attr("rx", 2)
                        .class(&format!("bar grows {class}")),
                );
                svg.add(
                    Element::text(
                        Self::LEFT + width + 6.0,
                        y + offset + 13.5,
                        &self.count.format(value),
                    )
                    .class("data lands"),
                );
            }
            // From the values as shown, so that the reader's own sum agrees.
            let (std, ours) = (self.count.shown(std), self.count.shown(ours));
            let fewer = if ours <= std {
                format!("{:.1}% fewer", (std - ours) / std * 100.0)
            } else {
                format!("{:.1}% more", (ours - std) / std * 100.0)
            };
            svg.add(Element::text(700.0, y + Self::BAR + 6.0, &fewer).class("note end lands"));
        }
        svg.style(motion.css());
        svg
    }
}
