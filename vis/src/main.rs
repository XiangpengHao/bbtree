//! bbtree in the browser, as a site of two pages from one build
//! (`site.rs`): the explorer, where you look keys up in a bbtree and watch
//! how it finds them, from bbtree's own read-only views of its nodes
//! (`explorer.rs`); and a post whose figures are the explorer's panels
//! (`post.rs`, `figure.rs`). See `doc/10-visualization.md`.

mod architectures_chart;
mod baseline_bars;
mod batch_run;
mod batch_timeline;
mod batch_tree;
mod batch_view;
mod chart_scale;
mod concepts;
mod concurrent_chart;
mod count_text;
mod descent;
mod element_ids;
mod end_to_end_chart;
mod explorer;
mod fences;
mod figure;
mod icons;
mod inspected_tree;
mod lane_program;
mod measurements;
mod memory_chart;
mod memory_map;
mod node_inspector;
mod path_summary;
mod path_tree;
mod performance_cards;
mod popover;
mod post;
mod query_key;
mod repository_links;
mod scenarios;
mod shared_link;
mod shortcuts;
mod single_thread_chart;
mod site;
mod slot_table;
mod spotlight;
mod trace;
mod tree_shape;
mod tree_summary;
mod workload;

use dioxus::prelude::*;

use crate::explorer::Explorer;
use crate::post::Post;
use crate::site::SitePage;

// The stylesheets go into the page's static head (`index.html`), so the
// page is styled from its first paint, while the wasm still loads; the
// fonts are linked there too.
const _: Asset = asset!(
    "/assets/main.css",
    AssetOptions::css().with_static_head(true)
);
const _: Asset = asset!(
    "/assets/tailwind.css",
    AssetOptions::css().with_static_head(true)
);

fn main() {
    dioxus::launch(App);
}

/// The page the address names.
#[component]
fn App() -> Element {
    match SitePage::current() {
        SitePage::Explorer => rsx! { Explorer {} },
        SitePage::Post => rsx! { Post {} },
    }
}
