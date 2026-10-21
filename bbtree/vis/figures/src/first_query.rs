//! The figure of "Case 2: bbtree-mmap": the time from a restart to a map's
//! first answer, its 10M `u64` entries on disk. std's `BTreeMap` reads
//! them back and rebuilds itself; a `BBTreeMapFile` maps its file and
//! looks the key up where it lies. From bbtree-mmap's `first_query`
//! example on the Ryzen 9 9900X (`vis/data/9900x-20261021/first-query.csv`):
//! a pair of bars with the file in the page cache, and a pair with it on
//! disk. The bars play the restarts ten times slower than they ran, from
//! one start: std's reads, then rebuilds; bbtree-mmap's answers at once.

use std::fs;
use std::io;
use std::path::Path;

use crate::svg::{Element, Motion, Svg};

/// When a restart had loaded its file, built its map and answered, in
/// microseconds from its start.
#[derive(Clone, Copy)]
struct Restart {
    loaded: f64,
    built: f64,
    answered: f64,
}

/// One state of the page cache, and each map's restart in it.
struct PageCache {
    id: &'static str,
    title: &'static str,
    std: Restart,
    mmap: Restart,
}

pub(crate) struct FirstQuery {
    caches: Vec<PageCache>,
}

impl FirstQuery {
    /// The states of the page cache, as the example names them and as the
    /// post does, in the post's order.
    const CACHES: [(&'static str, &'static str); 2] = [
        ("warm", "File in the page cache"),
        ("cold", "File on disk, cold"),
    ];
    const LEFT: f64 = 170.0;
    const RIGHT: f64 = 600.0;
    const TOP: f64 = 40.0;
    const HEADING: f64 = 30.0;
    const ROW: f64 = 26.0;
    const BAR: f64 = 18.0;
    /// The thinnest bar drawn, so that an answer in microseconds shows.
    const THINNEST: f64 = 3.0;
    /// Seconds the figure takes to play a measured second.
    const SLOWDOWN: f64 = 10.0;

    pub(crate) fn read(csv: &Path) -> io::Result<Self> {
        let text = fs::read_to_string(csv)?;
        let mut lines = text.lines();
        let header: Vec<&str> = lines.next().unwrap_or("").split(',').collect();
        let column = |name: &str| {
            (header.iter().position(|field| *field == name))
                .ok_or_else(|| io::Error::other(format!("{} lacks {name}", csv.display())))
        };
        let (map, cache) = (column("map")?, column("page_cache")?);
        let (load, build, answer) = (
            column("load_us")?,
            column("build_us")?,
            column("first_answer_us")?,
        );
        let rows: Vec<Vec<&str>> = lines.map(|line| line.split(',').collect()).collect();
        let restart = |bench_map: &str, bench_cache: &str| {
            let row = (rows.iter())
                .find(|row| row[map] == bench_map && row[cache] == bench_cache)
                .ok_or_else(|| {
                    io::Error::other(format!("{} lacks {bench_map} {bench_cache}", csv.display()))
                })?;
            let micros = |index: usize| row[index].parse::<f64>().map_err(io::Error::other);
            let loaded = micros(load)?;
            Ok::<_, io::Error>(Restart {
                loaded,
                built: loaded + micros(build)?,
                answered: micros(answer)?,
            })
        };
        let caches = (Self::CACHES.iter())
            .map(|&(id, title)| {
                Ok(PageCache {
                    id,
                    title,
                    std: restart("std-btree", id)?,
                    mmap: restart("bbtree-mmap", id)?,
                })
            })
            .collect::<io::Result<_>>()?;
        Ok(Self { caches })
    }

    fn longest(&self) -> f64 {
        (self.caches.iter())
            .map(|cache| cache.std.answered.max(cache.mmap.answered))
            .fold(0.0, f64::max)
    }

    fn x(&self, micros: f64) -> f64 {
        Self::LEFT + micros / self.longest() * (Self::RIGHT - Self::LEFT)
    }

    /// `micros` as the figure prints a time: microseconds under one
    /// millisecond, milliseconds from there.
    fn time_text(micros: f64) -> String {
        if micros < 1000.0 {
            format!("{micros:.0} µs")
        } else {
            format!("{:.1} ms", micros / 1000.0)
        }
    }

    /// `ratio` to two significant digits, its thousands grouped: one run
    /// measures no finer.
    fn speedup_text(ratio: f64) -> String {
        let unit = 10_f64.powi(ratio.log10().floor() as i32 - 1).max(1.0);
        let digits = ((ratio / unit).round() * unit).to_string();
        let mut grouped = String::new();
        for (index, digit) in digits.chars().enumerate() {
            if index > 0 && (digits.len() - index).is_multiple_of(3) {
                grouped.push(',');
            }
            grouped.push(digit);
        }
        format!("{grouped}× sooner")
    }

    pub(crate) fn svg(&self) -> Svg {
        let height = Self::TOP + self.caches.len() as f64 * (Self::HEADING + 2.0 * Self::ROW) + 4.0;
        let mut svg = Svg::new(
            720.0,
            height,
            "Time from a restart to the first answer: std's BTreeMap rebuilt, bbtree-mmap mapped",
        );
        // A step is the longest restart, slowed down: the figure acts the
        // restarts out at one speed, and holds while the last answer fades
        // in.
        let at = |micros: f64| micros / self.longest();
        let mut motion = Motion::once(1.2, self.longest() / 1e6 * Self::SLOWDOWN);
        svg.style(
            ".bar { transform-box: fill-box; transform-origin: left center; } \
             .read { fill: var(--theirs-3); } \
             .rebuilt { fill: var(--theirs-1); } \
             .ours { fill: var(--marker); stroke: var(--marker-strong); stroke-width: 1.5; } \
             .in-bar { fill: var(--panel); font-size: 12px; }",
        );
        svg.add(
            Element::text(700.0, 20.0, "time from the restart to the first answer")
                .class("label end"),
        );
        let mut y = Self::TOP;
        for cache in &self.caches {
            svg.add(Element::text(24.0, y + 18.0, cache.title).class("strong"));
            y += Self::HEADING;

            let std = cache.std;
            svg.add(Element::text(40.0, y + 13.5, "std BTreeMap").class("note"));
            for (name, from, to, text) in [
                ("read", 0.0, std.loaded, "read"),
                ("rebuilt", std.loaded, std.built, "rebuild"),
            ] {
                let grows = format!("grows-{name}-{}", cache.id);
                motion.animate(
                    &grows,
                    &[
                        (0.0, "transform: scaleX(0)".into()),
                        (at(from), "transform: scaleX(0)".into()),
                        (at(to), "transform: scaleX(1)".into()),
                    ],
                );
                let (left, right) = (self.x(from), self.x(to));
                svg.add(
                    Element::rect(left, y, right - left, Self::BAR)
                        .class(&format!("bar {grows} {name}")),
                );
                // In the bar where it fits, in the panel's color: hidden
                // until the bar grows under it.
                if right - left > text.len() as f64 * 7.0 + 12.0 {
                    svg.add(Element::text(left + 6.0, y + 13.0, text).class("in-bar"));
                }
            }
            let answered = format!("answered-std-{}", cache.id);
            motion.appear(&answered, at(std.answered));
            svg.add(
                Element::text(
                    self.x(std.answered) + 6.0,
                    y + 13.5,
                    &Self::time_text(std.answered),
                )
                .class(&format!("data {answered}")),
            );
            y += Self::ROW;

            let mmap = cache.mmap;
            let answered = format!("answered-mmap-{}", cache.id);
            motion.appear(&answered, at(mmap.answered));
            svg.add(Element::text(40.0, y + 13.5, "bbtree-mmap").class("note strong"));
            let width = (self.x(mmap.answered) - Self::LEFT).max(Self::THINNEST);
            svg.add(
                Element::rect(Self::LEFT, y, width, Self::BAR)
                    .attr("rx", 1)
                    .class(&format!("ours {answered}")),
            );
            svg.add(
                Element::text(
                    Self::LEFT + width + 6.0,
                    y + 13.5,
                    &Self::time_text(mmap.answered),
                )
                .class(&format!("data {answered}")),
            );
            svg.add(
                Element::text(
                    700.0,
                    y + 13.5,
                    &Self::speedup_text(std.answered / mmap.answered),
                )
                .class(&format!("note end {answered}")),
            );
            y += Self::ROW;
        }
        svg.style(motion.css());
        svg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_each_maps_restart_in_each_cache() {
        let csv = std::env::temp_dir().join(format!("first-query-{}.csv", std::process::id()));
        fs::write(
            &csv,
            "map,page_cache,entries,file_mb,load_us,build_us,first_answer_us\n\
             std-btree,cold,10,1.0,60.0,40.0,101.0\n\
             bbtree-mmap,cold,10,1.0,2.0,0.0,3.0\n\
             std-btree,warm,10,1.0,10.0,35.0,45.5\n\
             bbtree-mmap,warm,10,1.0,0.5,0.0,0.75\n",
        )
        .unwrap();
        let chart = FirstQuery::read(&csv).unwrap();
        fs::remove_file(&csv).unwrap();
        let ids: Vec<&str> = chart.caches.iter().map(|cache| cache.id).collect();
        assert_eq!(ids, ["warm", "cold"]);
        let warm = &chart.caches[0];
        assert_eq!(
            (warm.std.loaded, warm.std.built, warm.std.answered),
            (10.0, 45.0, 45.5)
        );
        assert_eq!(chart.caches[1].mmap.answered, 3.0);
    }

    #[test]
    fn prints_speedups_to_two_digits() {
        assert_eq!(FirstQuery::speedup_text(2467.2), "2,500× sooner");
        assert_eq!(FirstQuery::speedup_text(164.4), "160× sooner");
        assert_eq!(FirstQuery::speedup_text(7.3), "7× sooner");
    }
}
