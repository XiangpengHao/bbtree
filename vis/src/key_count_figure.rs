//! The post's figure of a node search (doc 14, Figure 2): the target
//! against every slot at once, and the count of the smaller keys, which is
//! the child to follow. Eight slots and small numbers, drawn plain: the
//! idea of the count, not a node of a tree (the explorer's way down shows
//! real nodes' counts).

use dioxus::prelude::*;

use crate::how_it_works::Diagram;

/// A node's slots, the last one empty, and a target to count against.
struct CountExample {
    slots: [Option<u16>; 8],
    target: u16,
}

impl CountExample {
    const EIGHT_SLOTS: Self = Self {
        slots: [
            Some(3),
            Some(11),
            Some(27),
            Some(40),
            Some(58),
            Some(71),
            Some(90),
            None,
        ],
        target: 45,
    };

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

    /// How many steps a binary search over the slots takes, each waiting
    /// for the last.
    fn binary_search_steps(&self) -> u32 {
        self.slots.len().ilog2()
    }
}

#[component]
pub(crate) fn KeyCountFigure() -> Element {
    let example = CountExample::EIGHT_SLOTS;
    let count = example.count();
    rsx! {
        Diagram {
            caption: rsx! {
                "One compare checks every slot against the target; counting the ones gives the child to follow. Binary search would take {example.binary_search_steps()} steps here, each waiting for the last, and 6 in a full node of 49 children."
            },
            div { class: "key-count data",
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
                span { class: "key-count-label", "target" }
                for slot in 0..example.slots.len() {
                    span { key: "target-{slot}", class: "key-count-cell target", "{example.target}" }
                }
                span { class: "key-count-label", "key < target" }
                for slot in 0..example.slots.len() {
                    span {
                        key: "mask-{slot}",
                        class: if example.is_smaller(slot) { "key-count-cell smaller" } else { "key-count-cell larger" },
                        if example.is_smaller(slot) {
                            "1"
                        } else {
                            "0"
                        }
                    }
                }
                span { class: "key-count-result", "{count} ones: go to child {count}" }
            }
        }
    }
}
