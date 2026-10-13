//! Keys for the common moves, anywhere but in a text field: `/` to type a
//! key, `R` for a random one, `[` and `]` for the leaves beside the last
//! one, and the arrows up and down the path.

use dioxus::prelude::*;

use crate::inspected_tree::LoadedTree;
use crate::scenarios::Scenario;
use crate::trace::Tracer;

/// Listens for the keys on the whole page; draws nothing.
#[component]
pub(crate) fn Shortcuts(tree: Signal<Option<LoadedTree>>, tracer: Tracer) -> Element {
    let mut tracer = tracer;
    use_future(move || async move {
        // The page forwards the keys it doesn't leave to the browser: none
        // while typing, and the arrows only while the page itself doesn't
        // scroll, as it does on narrow screens.
        let mut keys = document::eval(
            r#"
            document.addEventListener('keydown', (event) => {
                if (event.defaultPrevented || event.metaKey || event.ctrlKey || event.altKey) return;
                const target = event.target;
                if (target.closest?.('input, select, textarea, [contenteditable]')) {
                    if (event.key === 'Escape') target.blur();
                    return;
                }
                const arrow = event.key === 'ArrowUp' || event.key === 'ArrowDown';
                const pageScrolls = document.documentElement.scrollHeight > window.innerHeight + 1;
                if (arrow && (pageScrolls || target.closest?.('.slot-scroll'))) return;
                if (!['/', 'r', 'R', '[', ']', 'ArrowUp', 'ArrowDown'].includes(event.key)) return;
                event.preventDefault();
                if (event.key === '/') {
                    const input = document.getElementById('lookup');
                    input?.focus();
                    input?.select();
                    return;
                }
                dioxus.send(event.key);
            });
            "#,
        );
        while let Ok(key) = keys.recv::<String>().await {
            let Some(tree) = tree.peek().clone() else {
                continue;
            };
            match key.as_str() {
                "r" | "R" => tracer.run_scenario(&tree, Scenario::Random),
                "[" => tracer.run_beside(&tree, -1),
                "]" => tracer.run_beside(&tree, 1),
                "ArrowUp" => tracer.inspect_along(-1),
                "ArrowDown" => tracer.inspect_along(1),
                _ => {}
            }
        }
    });
    rsx! {}
}
