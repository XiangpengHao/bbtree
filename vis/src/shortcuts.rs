//! Keys for the common moves, anywhere but in a text field: `/` to search,
//! `R` for a random key, `[` and `]` for the leaves beside the last one,
//! the arrows up and down the path, and left and right through the guide.

use dioxus::prelude::*;

use crate::guide::Guide;
use crate::inspected_tree::LoadedTree;
use crate::scenarios::Scenario;
use crate::trace::Tracer;

/// Listens for the keys on the whole page; draws nothing.
#[component]
pub(crate) fn Shortcuts(
    tree: Signal<Option<LoadedTree>>,
    tracer: Tracer,
    search_open: Signal<bool>,
    guide: Guide,
) -> Element {
    let mut tracer = tracer;
    let mut search_open = search_open;
    let mut guide = guide;
    use_future(move || async move {
        // The page forwards the keys it doesn't leave to the browser: none
        // while typing, and the arrows only while the page itself doesn't
        // scroll, as it does on narrow screens.
        let mut keys = document::eval(
            r#"
            document.addEventListener('keydown', (event) => {
                if (event.defaultPrevented || event.metaKey || event.ctrlKey || event.altKey) return;
                const target = event.target;
                if (target.closest?.('input, select, textarea, [contenteditable]')) return;
                const arrow = event.key.startsWith('Arrow');
                const pageScrolls = document.documentElement.scrollHeight > window.innerHeight + 1;
                if (arrow && (pageScrolls || target.closest?.('.slot-scroll, [role=dialog]'))) return;
                const keys = ['/', 'r', 'R', '[', ']', 'ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight'];
                if (!keys.includes(event.key)) return;
                event.preventDefault();
                dioxus.send(event.key);
            });
            "#,
        );
        while let Ok(key) = keys.recv::<String>().await {
            let Some(tree) = tree.peek().clone() else {
                continue;
            };
            let trace = tracer.trace();
            match key.as_str() {
                "/" => search_open.set(true),
                "r" | "R" => tracer.run_scenario(&tree, Scenario::Random),
                "[" => tracer.run_beside(&tree, -1),
                "]" => tracer.run_beside(&tree, 1),
                "ArrowUp" => tracer.inspect_along(-1),
                "ArrowDown" => tracer.inspect_along(1),
                "ArrowLeft" | "ArrowRight" => {
                    if let Some(trace) = trace {
                        guide.go(if key == "ArrowLeft" { -1 } else { 1 }, &trace);
                    }
                }
                _ => {}
            }
        }
    });
    rsx! {}
}
