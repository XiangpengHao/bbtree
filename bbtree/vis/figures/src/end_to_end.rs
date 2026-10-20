//! The end-to-end results again, inside "What it can do?": real systems,
//! before and after one of their maps became bbtree, on their own
//! benchmarks, on one thread, the runs the post's End to End card shows
//! (`vis/data/9900x-20261017/integrations.csv`). A row a workload, grouped
//! by system: the time as shipped, dark, as 100%, bbtree's inside it,
//! bright, as a share of it, and the speedup at the row's end.

use std::fs;
use std::io;
use std::path::Path;

use crate::svg::{Element, Motion, Svg};

/// One workload of a system: its time per operation as shipped, and with
/// bbtree.
struct Workload {
    name: String,
    shipped_nanos: f64,
    bbtree_nanos: f64,
}

/// A system's workloads, and the map it ships.
struct System {
    name: String,
    shipped_map: String,
    workloads: Vec<Workload>,
}

pub(crate) struct EndToEnd {
    systems: Vec<System>,
}

impl EndToEnd {
    const WIDTH: f64 = 720.0;
    const LEFT: f64 = 170.0;
    const RIGHT: f64 = 620.0;
    const TOP: f64 = 44.0;
    const HEADING: f64 = 30.0;
    const ROW: f64 = 24.0;
    const BAR: f64 = 16.0;

    /// The systems' one-thread rows, in the order of the CSV.
    pub(crate) fn read(csv: &Path) -> io::Result<Self> {
        let text = fs::read_to_string(csv)?;
        let mut systems: Vec<System> = Vec::new();
        for line in text.lines().skip(1) {
            let fields: Vec<&str> = line.split(',').collect();
            // One thread only, as the card: with more, the systems are
            // bound by their own locks.
            let [system, workload, _, "1", shipped_map, shipped, _, bbtree] = fields[..] else {
                continue;
            };
            let (Ok(shipped_nanos), Ok(bbtree_nanos)) = (shipped.parse(), bbtree.parse()) else {
                continue;
            };
            if systems.last().is_none_or(|last| last.name != system) {
                systems.push(System {
                    name: system.to_string(),
                    shipped_map: shipped_map.to_string(),
                    workloads: Vec::new(),
                });
            }
            let last = systems.last_mut().expect("pushed above");
            last.workloads.push(Workload {
                name: workload.to_string(),
                shipped_nanos,
                bbtree_nanos,
            });
        }
        Ok(Self { systems })
    }

    fn height(&self) -> f64 {
        let rows: usize = self
            .systems
            .iter()
            .map(|system| system.workloads.len())
            .sum();
        Self::TOP + self.systems.len() as f64 * Self::HEADING + rows as f64 * Self::ROW + 8.0
    }

    pub(crate) fn svg(&self) -> Svg {
        let full = Self::RIGHT - Self::LEFT;
        let mut svg = Svg::new(
            Self::WIDTH,
            self.height(),
            "Time per operation of real systems with bbtree, relative to as shipped",
        );
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
             .shipped { fill: var(--theirs-1); } \
             .ours { fill: var(--marker); stroke: var(--marker-strong); stroke-width: 1.5; } \
             .swatch-label { fill: var(--graphite); }",
        );
        svg.add(
            Element::rect(Self::LEFT, 10.0, 12.0, 12.0)
                .attr("rx", 2)
                .class("shipped"),
        );
        svg.add(Element::text(Self::LEFT + 18.0, 20.0, "as shipped, 100%").class("swatch-label"));
        svg.add(
            Element::rect(Self::LEFT + 150.0, 10.0, 12.0, 12.0)
                .attr("rx", 2)
                .class("ours"),
        );
        svg.add(
            Element::text(Self::LEFT + 168.0, 20.0, "with bbtree").class("swatch-label strong"),
        );
        svg.add(Element::text(700.0, 20.0, "time per operation, one thread").class("label end"));
        let mut y = Self::TOP;
        for system in &self.systems {
            svg.add(Element::text(24.0, y + 18.0, &system.name).class("strong"));
            svg.add(
                Element::text(
                    Self::LEFT,
                    y + 18.0,
                    &format!("ships {}", system.shipped_map),
                )
                .class("label"),
            );
            y += Self::HEADING;
            for workload in &system.workloads {
                let share = workload.bbtree_nanos / workload.shipped_nanos;
                svg.add(Element::text(40.0, y + 12.5, &workload.name).class("note"));
                svg.add(
                    Element::rect(Self::LEFT, y, full, Self::BAR)
                        .attr("rx", 2)
                        .class("shipped"),
                );
                svg.add(
                    Element::rect(Self::LEFT, y + 3.0, full * share.min(1.0), Self::BAR - 6.0)
                        .attr("rx", 2)
                        .class("bar grows ours"),
                );
                let speedup = workload.shipped_nanos / workload.bbtree_nanos;
                svg.add(
                    Element::text(700.0, y + 12.5, &format!("{speedup:.2}×"))
                        .class("data end lands"),
                );
                y += Self::ROW;
            }
        }
        svg.style(motion.css());
        svg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_each_systems_one_thread_rows_in_order() {
        let csv = std::env::temp_dir().join(format!("end-to-end-{}.csv", std::process::id()));
        fs::write(
            &csv,
            "system,workload,detail,threads,before,before_ns,after,after_ns\n\
             fjall,Lookup,get,1,crossbeam SkipMap,900,bbtree,300\n\
             fjall,Lookup,get,8,crossbeam SkipMap,100,bbtree,50\n\
             fjall,Insert,insert,1,crossbeam SkipMap,600,bbtree,400\n\
             Lance,Lookup,get,1,arena skiplist,2000,bbtree,1000\n",
        )
        .unwrap();
        let chart = EndToEnd::read(&csv).unwrap();
        fs::remove_file(&csv).unwrap();
        let names: Vec<(&str, usize)> = (chart.systems.iter())
            .map(|system| (system.name.as_str(), system.workloads.len()))
            .collect();
        assert_eq!(names, [("fjall", 2), ("Lance", 1)]);
        assert_eq!(chart.systems[1].shipped_map, "arena skiplist");
    }
}
