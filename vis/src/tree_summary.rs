//! The built tree in one line: its size, its height, and its pages.

use dioxus::prelude::*;

use crate::count_text::Group;
use crate::inspected_tree::LoadedTree;

/// Pointers take 4 bytes in the wasm32 build this page runs, 8 on 64-bit
/// targets (`doc/10-visualization.md`).
const LAYOUT_NOTE: &str = "Layouts as compiled for wasm32, where a pointer takes 4 bytes: internal nodes hold more keys than on a 64-bit target, and String leaves differ (no length tags fit). Leaves of fixed-size keys and values match.";

#[component]
pub(crate) fn TreeSummary(tree: LoadedTree) -> Element {
    let shape = tree.shape();
    let height = shape.height().unwrap_or(0);
    let leaves = shape.leaf_count();
    let internal = shape.node_count() - leaves;
    let megabytes = shape.page_bytes() as f64 / (1024.0 * 1024.0);
    rsx! {
        p { class: "data ml-auto flex flex-wrap items-center gap-x-4 text-[12px] text-[var(--graphite)]",
            span { class: "text-[var(--ink)]", "{Group(shape.entries())} {tree.key_kind().label()} keys" }
            span { "{height + 1} levels" }
            span { "{Group(leaves)} leaves, {Group(internal)} internal" }
            span { "{megabytes:.1} MiB of pages" }
            span { class: "chip cursor-help", title: LAYOUT_NOTE, "wasm32 layouts" }
        }
    }
}
