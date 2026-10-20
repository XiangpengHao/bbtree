//! The post's end-to-end card: real systems, before and after one of their
//! maps became bbtree, on their own benchmarks, on one thread: with more,
//! these systems are bound by their own locks and counters. A group a
//! system, a cluster a workload: bbtree's time as a share of the time as
//! shipped.

use std::rc::Rc;

use dioxus::prelude::*;

use crate::baseline_bars::{BarScale, BaselineBars, BaselineGroup, BaselinePair};
use crate::measurements::Measurements;
use crate::repository_links::RepositoryLinks;
use crate::spotlight::Spotlight;

#[component]
pub(crate) fn EndToEndChart(measurements: Rc<Measurements>, spotlight: Spotlight) -> Element {
    let systems = measurements.integrated_systems();
    if systems.is_empty() {
        return rsx! {
            p { class: "chart-note", "The integrations are still running." }
        };
    }
    let mut links: Vec<(String, &'static str)> = Vec::new();
    let groups: Vec<BaselineGroup> = (systems.into_iter())
        .map(|system| {
            let rows = measurements.integration_rows(&system);
            if let Some(url) = rows.first().and_then(|row| row.repository()) {
                links.push((system.clone(), url));
            }
            BaselineGroup {
                caption: String::new(),
                pairs: (rows.into_iter())
                    .map(|row| BaselinePair {
                        workload: row.workload,
                        detail: row.detail,
                        baseline: row.before,
                        baseline_nanos: row.before_nanos,
                        ours: "bbtree".to_string(),
                        ours_nanos: row.after_nanos,
                    })
                    .collect(),
                name: system,
            }
        })
        .collect();
    let machine = measurements.machine();
    rsx! {
        div { class: "chart-keys",
            span { class: "chart-key",
                span { class: "chart-key-swatch", style: "background:var(--theirs-1)" }
                "as shipped, 100%"
            }
            span { class: "chart-key",
                span { class: "chart-key-swatch ours" }
                strong { "with bbtree" }
            }
        }
        BaselineBars {
            groups,
            scale: BarScale::Relative,
            label: "Time per operation with bbtree, relative to as shipped",
            heading: (
                "Time per operation, relative to as shipped".to_string(),
                "one thread; lower is better; the speedup over each bright bar".to_string(),
            ),
            spotlight,
        }
        p { class: "chart-note",
            "Each system's own benchmark, one thread, wall time per operation: the dark bar as shipped (100%), the bright one inside it with bbtree. Only the map changed, and where a system writes a batch, the call that hands it to the map whole (insert_many; Lance's, update_or_insert_many): crossbeam's SkipMap, or Lance's own arena skip list. More threads hit the systems' own locks first. Not shown, slower or unchanged: SlateDB's memtable_seek, 0.91–0.99× (its iterators read three rows ahead of a seek, which the adapter clones out); Tonbo's Transaction::get, 0.86× (it copies every row per call); Lance's lookups of absent keys, 1.00–1.04× (they plan a DataFusion query). {machine.name}."
            RepositoryLinks { links }
        }
    }
}
