//! The built tree in one line: its size, its height, and its pages.

use dioxus::prelude::*;

use crate::inspected_tree::LoadedTree;
use crate::tree_canvas::Group;

/// Pointers take 4 bytes in the wasm32 build this page runs, 8 on 64-bit
/// targets: internal nodes here hold more keys, and heap-string leaves
/// differ, than in a native build (`doc/10-visualization.md`).
const LAYOUT_NOTE: &str = "Layouts as compiled for wasm32, where a pointer takes 4 bytes: internal nodes hold more keys than on a 64-bit target, and String leaves differ. Fixed-size leaves match.";

#[component]
pub(crate) fn TreeSummary(tree: LoadedTree) -> Element {
    let shape = tree.shape();
    let height = shape.height().unwrap_or(0);
    let leaves = shape.leaf_count();
    let internal = shape.node_count() - leaves;
    let megabytes = shape.page_bytes() as f64 / (1024.0 * 1024.0);
    let pages = match shape.internal_page_size() {
        Some(internal) => format!("{} B leaves, {internal} B internal", shape.leaf_page_size()),
        None => format!("{} B leaf", shape.leaf_page_size()),
    };
    rsx! {
        p { class: "tabular ml-auto flex flex-wrap items-center gap-x-3 text-[12px] text-[var(--ink-2)]",
            span { class: "font-medium text-[var(--ink)]", "{Group(shape.entries())} entries" }
            span { "{tree.key_kind().label()}" }
            span { "height {height}" }
            span { "{Group(leaves)} leaves · {Group(internal)} internal" }
            span { "{megabytes:.1} MiB of pages ({pages})" }
            span {
                class: "cursor-help rounded border border-[var(--grid)] px-1.5 text-[11px] text-[var(--muted)]",
                title: LAYOUT_NOTE,
                "wasm32 layouts"
            }
        }
    }
}
