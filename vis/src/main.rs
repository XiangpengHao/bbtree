//! Browse a bbtree in the browser: a map built here, drawn from bbtree's own
//! read-only views of its nodes (`BBTreeMap::inspect`), level by level and
//! page by page, as bbtree stores them (`doc/10-visualization.md`).

mod inspected_tree;
mod node_detail;
mod page_bar;
mod tree_canvas;
mod tree_shape;
mod tree_summary;
mod workload;

use dioxus::prelude::*;

use crate::inspected_tree::LoadedTree;
use crate::node_detail::NodeDetailPanel;
use crate::tree_canvas::{ColorBy, ColorPicker, TreeCanvas};
use crate::tree_shape::NodeAt;
use crate::tree_summary::TreeSummary;
use crate::workload::WorkloadPicker;

const MAIN_CSS: Asset = asset!("/assets/main.css");
const TAILWIND_CSS: Asset = asset!("/assets/tailwind.css");

fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    let tree = use_signal(|| None::<LoadedTree>);
    let selected = use_signal(|| None::<NodeAt>);
    let focus = use_signal(|| None::<NodeAt>);
    let color_by = use_signal(|| ColorBy::Fill);
    let loaded = tree.read().clone();
    rsx! {
        document::Link { rel: "stylesheet", href: MAIN_CSS }
        document::Link { rel: "stylesheet", href: TAILWIND_CSS }
        div { class: "flex h-screen flex-col",
            header { class: "flex flex-wrap items-center gap-x-6 gap-y-2 border-b border-[var(--grid)] bg-[var(--surface)] px-4 py-2.5",
                h1 { class: "text-[15px] font-semibold tracking-tight", "bbtree" }
                WorkloadPicker { tree, selected }
                ColorPicker { color_by }
                if let Some(loaded) = loaded.clone() {
                    TreeSummary { tree: loaded }
                }
            }
            main { class: "flex min-h-0 flex-1",
                TreeCanvas { tree, selected, focus, color_by: color_by() }
                if let (Some(loaded), Some(at)) = (loaded, selected()) {
                    NodeDetailPanel { tree: loaded, at, selected, focus }
                }
            }
        }
    }
}
