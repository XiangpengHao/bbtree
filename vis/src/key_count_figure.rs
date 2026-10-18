//! The post's figure of a node search (doc 14, Figure 2): the count against
//! binary search, over the same slots and the same target. The count
//! compares every slot at once and has the child after one step; binary
//! search probes one slot, then the next, each waiting for the last. Both
//! play on one timeline, a step a unit; the reader moves the target and
//! they race again. Eight slots and small numbers, drawn plain: the idea of
//! the count, not a node of a tree (the explorer's way down shows real
//! nodes' counts).

use dioxus::prelude::*;

use crate::element_ids::ElementIds;
use crate::how_it_works::Diagram;
use crate::timeline::{Timeline, TimelineControls};

/// A node's slots, the last one empty, and a target to count against.
#[derive(Clone, Copy, PartialEq)]
struct CountExample {
    slots: [Option<u16>; 8],
    target: u16,
}

impl CountExample {
    const SLOTS: [Option<u16>; 8] = [
        Some(3),
        Some(11),
        Some(27),
        Some(40),
        Some(58),
        Some(71),
        Some(90),
        None,
    ];

    fn with_target(target: u16) -> Self {
        Self {
            slots: Self::SLOTS,
            target,
        }
    }

    /// Whether slot `slot` counts: an empty one holds the largest value,
    /// so never does.
    fn is_smaller(&self, slot: usize) -> bool {
        self.slots[slot].is_some_and(|key| key < self.target)
    }

    /// The child to follow.
    fn count(&self) -> usize {
        (0..self.slots.len())
            .filter(|&slot| self.is_smaller(slot))
            .count()
    }

    /// The slots a binary search probes, in order, to the same child.
    fn binary_probes(&self) -> Vec<usize> {
        let (mut low, mut high) = (0, self.slots.len());
        let mut probes = Vec::new();
        while low < high {
            let mid = (low + high) / 2;
            probes.push(mid);
            if self.is_smaller(mid) {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        probes
    }

    /// A step a unit: the count's one, then binary search's last.
    fn timeline(&self) -> Timeline {
        Timeline::of_steps(self.binary_probes().len().max(1), 800.0)
    }
}

#[component]
pub(crate) fn KeyCountFigure() -> Element {
    let mut target = use_signal(|| 45u16);
    let example = CountExample::with_target(target());
    rsx! {
        Diagram {
            caption: rsx! {
                "Move the target. The count compares every slot at once and has the child after one step; binary search probes one slot at a time, each probe waiting for the last: {example.binary_probes().len()} steps here, and 6 in a full node of 49 children."
            },
            div { class: "diagram",
                label { class: "count-target",
                    span { "target" }
                    input {
                        r#type: "range",
                        min: "0",
                        max: "99",
                        value: "{target}",
                        oninput: move |evt| {
                            if let Ok(value) = evt.value().parse() {
                                target.set(value);
                            }
                        },
                    }
                    output { class: "data", "{target}" }
                }
                // Keyed by the target, so that a new one races anew.
                for shown in std::iter::once(example) {
                    CountRace { key: "{shown.target}", example: shown }
                }
            }
        }
    }
}

/// The slots, the count's row and binary search's, on one timeline.
#[component]
fn CountRace(example: CountExample) -> Element {
    let element = ElementIds::of_current().id("count-race");
    let now = use_signal(|| None::<usize>);
    let playing = use_signal(|| true);
    let timeline = example.timeline();
    let (run_at, run_timeline, controls_at) = (element.clone(), timeline.clone(), element.clone());
    use_future(move || {
        let (timeline, element) = (run_timeline.clone(), run_at.clone());
        async move { timeline.run(&element, now, playing).await }
    });
    let count = example.count();
    let probes = example.binary_probes();
    let steps = probes.len();
    rsx! {
        div { id: element, class: "timeline count-race",
            div { class: "key-count data", style: "--steps:{steps}",
                span { class: "key-count-label", "prefix keys" }
                for (slot , key) in example.slots.iter().enumerate() {
                    span {
                        key: "key-{slot}",
                        class: if key.is_some() { "key-count-cell" } else { "key-count-cell empty" },
                        match key {
                            Some(key) => rsx! { "{key}" },
                            None => rsx! { "max" },
                        }
                    }
                }
                span { class: "key-count-label", "count" }
                for slot in 0..example.slots.len() {
                    span {
                        key: "mask-{slot}",
                        class: if example.is_smaller(slot) { "key-count-cell mask smaller" } else { "key-count-cell mask larger" },
                        if example.is_smaller(slot) {
                            "1"
                        } else {
                            "0"
                        }
                    }
                }
                span { class: "key-count-result count-done",
                    "child {count}, in 1 step"
                }
                span { class: "key-count-label", "binary search" }
                for slot in 0..example.slots.len() {
                    match probes.iter().position(|probe| *probe == slot) {
                        Some(step) => rsx! {
                            span {
                                key: "probe-{slot}",
                                class: if example.is_smaller(slot) { "key-count-cell probe smaller" } else { "key-count-cell probe larger" },
                                style: "--s:{step}",
                                "{step + 1}"
                            }
                        },
                        None => rsx! {
                            span { key: "probe-{slot}", class: "key-count-cell unprobed" }
                        },
                    }
                }
                span { class: "key-count-result binary-done",
                    "child {count}, in {steps} steps"
                }
            }
            TimelineControls {
                element: controls_at,
                timeline,
                now,
                playing,
                label: "The race, step by step",
            }
        }
    }
}
