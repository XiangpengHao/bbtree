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
use crate::tab_list::{Tab, TabList};

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

    fn tab_id(self, ids: ElementIds) -> String {
        ids.id(&format!("tab-{}", self.slug()))
    }

    fn panel_id(self, ids: ElementIds) -> String {
        ids.id(&format!("card-{}", self.slug()))
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
            TabList {
                name: "Performance",
                class: "card-tabs",
                tab_class: "card-tab",
                tabs: PerformanceCard::ALL
                    .map(|card| Tab::new(card.tab_id(ids), card.panel_id(ids), rsx! { {card.label()} }))
                    .to_vec(),
                shown: PerformanceCard::ALL.iter().position(|card| *card == shown()).unwrap_or(0),
                onpick: move |index: usize| pick(PerformanceCard::ALL[index]),
            }
            div {
                id: shown().panel_id(ids),
                class: "card-body",
                role: "tabpanel",
                "aria-labelledby": shown().tab_id(ids),
                match shown() {
                    PerformanceCard::SingleThread => rsx! {
                        SingleThreadChart { measurements: measurements.clone(), spotlight }
                    },
                    PerformanceCard::Concurrent => rsx! {
                        ConcurrentChart { spotlight }
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
