//! Keys for the common moves, anywhere but in a text field: `/` to search,
//! `R` for a random key, `[` and `]` for the leaves beside the last one,
//! the arrows up and down the path, and left and right back and forward
//! through the lookups. Space plays or pauses the way down, or the batch,
//! and `,` and `.` step it back or on. `B` switches between one key and a
//! batch; in a batch, `R` picks new keys.

use dioxus::prelude::*;

use crate::batch_view::{Batcher, Playback, PlaybackCommand};
use crate::inspected_tree::LoadedTree;
use crate::scenarios::Scenario;
use crate::shared_link::HistoryStep;
use crate::trace::Tracer;

/// Listens for the keys on the whole page; draws nothing.
#[component]
pub(crate) fn Shortcuts(
    tree: Signal<Option<LoadedTree>>,
    tracer: Tracer,
    batcher: Batcher,
    search_open: Signal<bool>,
) -> Element {
    let mut tracer = tracer;
    let mut batcher = batcher;
    let mut search_open = search_open;
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
                const vertical = event.key === 'ArrowUp' || event.key === 'ArrowDown';
                const pageScrolls = document.documentElement.scrollHeight > window.innerHeight + 1;
                if (vertical && pageScrolls) return;
                if (event.key.startsWith('Arrow') && target.closest?.('.slot-scroll, .fence-scroll, [role=dialog], [role=tablist]')) return;
                // Space presses a focused control.
                if (event.key === ' ' && target.closest?.('button, a, [role=tab]')) return;
                const keys = ['/', 'r', 'R', '[', ']', 'ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight', ' ', ',', '.', 'b', 'B'];
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
            let batch = batcher.is_shown();
            match key.as_str() {
                "b" | "B" if batch => {
                    batcher.close();
                    if tracer.trace().is_none() {
                        tracer.start(&tree, None);
                    }
                }
                "b" | "B" => batcher.start(),
                "r" | "R" if batch => batcher.reseed(),
                " " if batch => Playback::send(PlaybackCommand::Toggle),
                "," if batch => Playback::send(PlaybackCommand::Back),
                "." if batch => Playback::send(PlaybackCommand::Forward),
                _ if batch => {}
                " " => tracer.toggle_descent(),
                "," => tracer.step_descent(-1),
                "." => tracer.step_descent(1),
                "/" => search_open.set(true),
                "r" | "R" => tracer.run_scenario(&tree, Scenario::Random),
                "[" => tracer.run_beside(&tree, -1),
                "]" => tracer.run_beside(&tree, 1),
                "ArrowUp" => tracer.inspect_along(-1),
                "ArrowDown" => tracer.inspect_along(1),
                "ArrowLeft" => HistoryStep::Back.take(),
                "ArrowRight" => HistoryStep::Forward.take(),
                _ => {}
            }
        }
    });
    rsx! {}
}
