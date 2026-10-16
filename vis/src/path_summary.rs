//! Above the path: the controls that play the lookup's way down, a level
//! at a time, and the key, walking down with it; and the path's legend.

use dioxus::prelude::*;

use crate::concepts::{Concept, Term};
use crate::icons::{Icon, IconGlyph};
use crate::inspected_tree::LoadedTree;
use crate::query_key::QueryKey;
use crate::spotlight::Spotlight;
use crate::trace::{Trace, Tracer};

#[component]
pub(crate) fn PathSummary(
    tree: LoadedTree,
    trace: Trace,
    tracer: Tracer,
    /// Changes with every lookup and replay, so the key's walk plays again.
    replay: u64,
) -> Element {
    let outcome = trace.outcome_text(tree.shape());
    rsx! {
        div { class: "path-summary",
            DescentControls { tracer }
            QueryKey { tree: tree.clone(), trace: trace.clone(), replay }
            p { class: "sr-only", role: "status", {outcome} }
        }
    }
}

/// What the descent controls ask the page's animations.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DescentCommand {
    /// Play, or pause; at the end, play again.
    Toggle,
    /// Pause on the level above.
    Back,
    /// Pause on the level below.
    Forward,
}

impl DescentCommand {
    fn send(self) {
        let name = match self {
            DescentCommand::Toggle => "toggle",
            DescentCommand::Back => "back",
            DescentCommand::Forward => "forward",
        };
        document::eval(&format!(
            "document.dispatchEvent(new CustomEvent('descent-playback', {{ detail: '{name}' }}));"
        ));
    }
}

/// Play, pause and step the lookup's way down, as the batch's controls play
/// a batch. The way down is the path's and the key's CSS animations, each
/// a level's (`Trace::LEVEL_MS` apart); stepping shows every level down to
/// one done and none below it begun.
#[component]
fn DescentControls(tracer: Tracer) -> Element {
    let mut playing = use_signal(|| true);
    let mut finished = use_signal(|| false);
    let mut tracer = tracer;
    use_future(move || async move {
        let mut heard = document::eval(
            r#"
            const L = await dioxus.recv();
            // The way down's animations: the path's and the key's, not the
            // inspector's.
            const list = () => [...document.querySelectorAll('.path-tree, .query-key')]
                .flatMap((root) => root.getAnimations({ subtree: true }))
                .filter((a) => a instanceof CSSAnimation);
            const timing = (a) => a.effect.getTiming();
            const end = (a) => (timing(a).delay || 0) + (Number(timing(a).duration) || 0);
            const isWalk = (a) => a.animationName.startsWith('key-walk');
            const levels = () => Math.max(1, document.querySelectorAll('.path-card').length);
            // The key's walk spans the whole way down from its start: its
            // clock is the descent's. Without one, the animations under way.
            const elapsed = () => {
                const anims = list();
                const walk = anims.find(isWalk);
                if (walk) return walk.currentTime || 0;
                const live = anims.filter((a) => a.playState !== 'finished');
                return live.length ? Math.max(...live.map((a) => a.currentTime || 0)) : levels() * L;
            };
            let level = null;
            const show = (k) => {
                for (const a of list()) {
                    a.pause();
                    a.currentTime = isWalk(a)
                        ? Math.min(end(a), k * L + 100)
                        : Math.floor((timing(a).delay || 0) / L) <= k ? end(a) : 0;
                }
            };
            document.addEventListener('descent-playback', (event) => {
                const anims = list();
                const command = event.detail;
                if (command === 'toggle') {
                    if (anims.some((a) => a.playState === 'running')) {
                        anims.forEach((a) => a.pause());
                        level = Math.min(levels() - 1, Math.floor(elapsed() / L));
                    } else if (level === null) {
                        dioxus.send('replay');
                    } else {
                        const from = (level + 1) * L;
                        anims.forEach((a) => { a.currentTime = from; a.play(); });
                        level = null;
                    }
                } else {
                    const from = level ?? Math.min(levels() - 1, Math.floor(elapsed() / L));
                    level = command === 'forward' ? Math.min(levels() - 1, from + 1) : Math.max(0, from - 1);
                    show(level);
                }
            });
            // A new lookup starts its animations afresh.
            let told = '';
            setInterval(() => {
                const anims = list();
                const running = anims.some((a) => a.playState === 'running');
                const walk = anims.find(isWalk);
                if (walk && walk.playState === 'running' && walk.currentTime < L) level = null;
                const done = !running && level === null;
                const state = running + ':' + done;
                if (state !== told) {
                    told = state;
                    dioxus.send(state);
                }
            }, 150);
            "#,
        );
        let _ = heard.send(Trace::LEVEL_MS);
        while let Ok(message) = heard.recv::<String>().await {
            match message.as_str() {
                "replay" => tracer.replay(),
                state => {
                    let (running, done) = state.split_once(':').unwrap_or(("false", "true"));
                    playing.set(running == "true");
                    finished.set(done == "true");
                }
            }
        }
    });
    rsx! {
        div { class: "batch-controls", role: "group", "aria-label": "Play the lookup",
            button {
                class: "icon-button",
                title: if playing() { "Pause" } else if finished() { "Play again" } else { "Play" },
                "aria-label": if playing() { "Pause" } else if finished() { "Play again" } else { "Play" },
                onclick: move |_| DescentCommand::Toggle.send(),
                IconGlyph { icon: if playing() { Icon::Pause } else if finished() { Icon::Replay } else { Icon::Play } }
            }
            button {
                class: "icon-button quiet",
                title: "A level up",
                "aria-label": "A level up",
                onclick: move |_| DescentCommand::Back.send(),
                IconGlyph { icon: Icon::StepBack }
            }
            button {
                class: "icon-button quiet",
                title: "A level down",
                "aria-label": "A level down",
                onclick: move |_| DescentCommand::Forward.send(),
                IconGlyph { icon: Icon::StepForward }
            }
        }
    }
}

/// What the path's marks mean, in its top corner.
#[component]
pub(crate) fn PathLegend(spotlight: Spotlight) -> Element {
    rsx! {
        div { class: "path-legend", "aria-label": "What the path's marks mean",
            span { class: "legend-item",
                span { class: "ruler-swatch slot-smaller" }
                "smaller key"
            }
            span { class: "legend-item",
                span { class: "ruler-swatch slot-larger" }
                "larger key"
            }
            span { class: "legend-item",
                span { class: "ruler-swatch slot-tied" }
                Term { concept: Concept::Tie, spotlight, "tied" }
            }
            span { class: "legend-item",
                span { class: "ruler-swatch slot-found" }
                "found"
            }
            span { class: "legend-item",
                span { class: "ruler-swatch slot-room" }
                "empty slot"
            }
            span { class: "legend-item",
                span { class: "notch-swatch" }
                Term { concept: Concept::Child, spotlight, "child taken" }
            }
        }
    }
}
