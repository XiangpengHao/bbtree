//! The cards the post leads with, a tab each: one thread, threads
//! sharing a map, real systems end to end, memory, and other machines and
//! instruction sets. The reader picks one; nothing turns by itself. The
//! charts' motion waits until the cards are first in view.

use std::rc::Rc;

use dioxus::prelude::*;

use crate::architectures_chart::ArchitecturesChart;
use crate::concurrent_chart::ConcurrentChart;
use crate::element_ids::ElementIds;
use crate::end_to_end_chart::EndToEndChart;
use crate::measurements::Measurements;
use crate::memory_chart::MemoryChart;
use crate::single_thread_chart::SingleThreadChart;
use crate::spotlight::Spotlight;

/// One of the cards.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PerformanceCard {
    SingleThread,
    Concurrent,
    EndToEnd,
    Memory,
    Architectures,
}

impl PerformanceCard {
    const ALL: [Self; 5] = [
        Self::SingleThread,
        Self::Concurrent,
        Self::EndToEnd,
        Self::Memory,
        Self::Architectures,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::SingleThread => "Single thread",
            Self::Concurrent => "Concurrent",
            Self::EndToEnd => "End to End",
            Self::Memory => "Memory",
            Self::Architectures => "Architectures",
        }
    }

    fn slug(self) -> &'static str {
        match self {
            Self::SingleThread => "single-thread",
            Self::Concurrent => "concurrent",
            Self::EndToEnd => "end-to-end",
            Self::Memory => "memory",
            Self::Architectures => "architectures",
        }
    }
}

#[component]
pub(crate) fn PerformanceCards(measurements: Rc<Measurements>, spotlight: Spotlight) -> Element {
    let mut shown = use_signal(|| PerformanceCard::SingleThread);
    // The cards have been in view: their charts' motion may play.
    let mut seen = use_signal(|| false);
    let ids = ElementIds::of_current();
    let mut charts = spotlight;
    let mut pick = move |card: PerformanceCard| {
        charts.clear();
        shown.set(card);
    };
    rsx! {
        section {
            id: ids.id("performance"),
            class: if seen() { "performance-cards seen" } else { "performance-cards" },
            "aria-label": "Performance",
            onmounted: move |_| {
                let id = ids.id("performance");
                spawn(async move {
                    let mut heard = document::eval(
                        r#"
                        const cards = document.getElementById(await dioxus.recv());
                        if (!cards) { dioxus.send(true); return; }
                        const observer = new IntersectionObserver((entries) => {
                            if (entries.some((entry) => entry.isIntersecting)) {
                                observer.disconnect();
                                dioxus.send(true);
                            }
                        }, { threshold: 0.2 });
                        observer.observe(cards);
                        "#,
                    );
                    let _ = heard.send(id);
                    // In view, or the observer failed: either way, play.
                    let _ = heard.recv::<bool>().await;
                    seen.set(true);
                });
            },
            div { class: "card-tabs", role: "tablist", "aria-label": "Performance",
                for card in PerformanceCard::ALL {
                    button {
                        id: ids.id(&format!("tab-{}", card.slug())),
                        class: "card-tab",
                        role: "tab",
                        "aria-selected": if shown() == card { "true" } else { "false" },
                        "aria-controls": ids.id(&format!("card-{}", card.slug())),
                        tabindex: if shown() == card { "0" } else { "-1" },
                        onclick: move |_| pick(card),
                        onkeydown: move |evt| {
                            let delta = match evt.key() {
                                Key::ArrowRight => 1,
                                Key::ArrowLeft => PerformanceCard::ALL.len() - 1,
                                _ => return,
                            };
                            evt.prevent_default();
                            let index = PerformanceCard::ALL.iter().position(|other| *other == card).unwrap_or(0);
                            let next = PerformanceCard::ALL[(index + delta) % PerformanceCard::ALL.len()];
                            pick(next);
                            document::eval(&format!(
                                "document.getElementById('{}')?.focus()",
                                ids.id(&format!("tab-{}", next.slug()))
                            ));
                        },
                        {card.label()}
                    }
                }
            }
            div {
                id: ids.id(&format!("card-{}", shown().slug())),
                class: "card-body",
                role: "tabpanel",
                "aria-labelledby": ids.id(&format!("tab-{}", shown().slug())),
                match shown() {
                    PerformanceCard::SingleThread => rsx! {
                        SingleThreadChart { measurements: measurements.clone(), spotlight }
                    },
                    PerformanceCard::Concurrent => rsx! {
                        ConcurrentChart { measurements: measurements.clone(), spotlight }
                    },
                    PerformanceCard::EndToEnd => rsx! {
                        EndToEndChart { measurements: measurements.clone(), spotlight }
                    },
                    PerformanceCard::Memory => rsx! {
                        MemoryChart { measurements: measurements.clone(), spotlight }
                    },
                    PerformanceCard::Architectures => rsx! {
                        ArchitecturesChart { spotlight }
                    },
                }
            }
        }
    }
}
