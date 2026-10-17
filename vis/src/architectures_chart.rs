//! The post's architectures card: gets of UUIDs on one thread, bbtree
//! against std's BTreeMap, on every machine and instruction set the post
//! runs on: two bars a machine, captioned with its instruction set, the
//! time of each get in nanoseconds. WebAssembly waits for Rust to build wasm32 with its SIMD by
//! default: without it bbtree's gets of fixed-size keys are slower than
//! std's (doc 14).

use dioxus::prelude::*;

use crate::baseline_bars::{BarScale, BaselineBars, BaselineGroup, BaselinePair};
use crate::measurements::{Machine, MeasuredKeys, MeasuredMap, MeasuredOperation, Measurements};
use crate::spotlight::Spotlight;

/// A machine the card covers.
#[derive(Clone, Copy, PartialEq, Debug)]
struct ArchitectureMachine {
    name: &'static str,
    /// Its instruction set, and its core where it has one to name.
    caption: &'static str,
    /// Its `throughput.csv`, the single-thread cells of
    /// `bench/tools/blog-runs.sh arch` merged by `blog-collect.sh`, once it
    /// has run: add it here with `include_str!`.
    throughput: &'static str,
}

impl ArchitectureMachine {
    const ALL: [Self; 5] = [
        Self {
            name: "Ryzen 9 9900X",
            caption: "x86-64 · Zen 5",
            throughput: include_str!("../data/9900x-20261016/throughput.csv"),
        },
        Self {
            name: "Core i5-11400",
            caption: "x86-64 · Rocket Lake",
            throughput: include_str!("../data/pittsburgh-20261016/throughput.csv"),
        },
        Self {
            name: "Apple M1 Pro",
            caption: "AArch64 · Firestorm",
            throughput: include_str!("../data/m1-20261016/throughput.csv"),
        },
        Self {
            name: "AWS C8i",
            caption: "x86-64 · Xeon 6975P-C",
            throughput: include_str!("../data/aws-c8i-20261016/throughput.csv"),
        },
        Self {
            name: "AWS Graviton5",
            caption: "AArch64 · Neoverse V3",
            throughput: include_str!("../data/aws-c9g-20261016/throughput.csv"),
        },
    ];

    /// Its bar of the chart, if it has run.
    fn group(self) -> Option<BaselineGroup> {
        let machine = Machine {
            name: self.name,
            detail: self.caption,
        };
        let measurements = Measurements::of_single_thread(machine, self.throughput);
        let keys = MeasuredKeys::Uuid;
        let get = |map| measurements.single_thread_nanos(map, keys, MeasuredOperation::Get);
        Some(BaselineGroup {
            name: self.name.to_string(),
            caption: self.caption.to_string(),
            pairs: vec![BaselinePair {
                // The machine's name, under it, says which bar it is.
                workload: String::new(),
                detail: format!("{}: gets, half of which miss, 10M entries", keys.detail()),
                baseline: "std BTreeMap".to_string(),
                baseline_nanos: get(MeasuredMap::StdBTree)?,
                ours: "bbtree".to_string(),
                ours_nanos: get(MeasuredMap::BBTree)?,
            }],
        })
    }
}

#[component]
pub(crate) fn ArchitecturesChart(spotlight: Spotlight) -> Element {
    let groups: Vec<BaselineGroup> = (ArchitectureMachine::ALL.into_iter())
        .filter_map(ArchitectureMachine::group)
        .collect();
    rsx! {
        div { class: "chart-keys",
            span { class: "chart-key",
                span { class: "chart-key-swatch", style: "background:var(--theirs-1)" }
                "std BTreeMap"
            }
            span { class: "chart-key",
                span { class: "chart-key-swatch ours" }
                strong { "bbtree" }
            }
        }
        BaselineBars {
            groups,
            scale: BarScale::Nanos,
            label: "Time per get of UUID keys on one thread, by machine",
            spotlight,
        }
        p { class: "chart-note",
            "Mean time per get on one thread (lower is better): 10M random version-4 UUIDs, half of the lookups miss, the same cells on every machine (bench/tools/blog-runs.sh arch), each machine's bench built for its CPU. AWS: c8i.large and c9g.large, spot, in us-west-2."
        }
    }
}
