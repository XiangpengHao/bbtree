//! The built tree in one line: its size, its levels, and its pages.

use dioxus::prelude::*;

use crate::concepts::{Concept, Term};
use crate::inspected_tree::LoadedTree;
use crate::spotlight::Spotlight;

#[component]
pub(crate) fn TreeSummary(tree: LoadedTree, spotlight: Spotlight) -> Element {
    let shape = tree.shape();
    let levels = shape.level_count();
    let megabytes = shape.page_bytes() as f64 / (1024.0 * 1024.0);
    rsx! {
        p { class: "tree-summary data",
            span { "{levels} levels" }
            span { "{megabytes:.1} MiB" }
            Term { concept: Concept::Wasm32, spotlight, "wasm32 layouts" }
        }
    }
}
