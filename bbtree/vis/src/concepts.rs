//! The ideas a lookup in bbtree rests on, in plain words for a reader who
//! knows B-trees but not bbtree, and the term that names one where a panel
//! uses it: pointing at the term defines it and lights up what it names.

use std::rc::Rc;

use dioxus::prelude::*;

use crate::spotlight::Spotlight;

/// One idea of bbtree's, named where the panels use it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Concept {
    InlineKey,
    Tie,
    FencePrefix,
    SharedPrefix,
    Separator,
    Head,
    HashTag,
    Child,
    Page,
    Wasm32,
    Prefetch,
}

impl Concept {
    /// The idea's name, as the definition's title.
    pub(crate) fn title(self) -> &'static str {
        match self {
            Concept::InlineKey => "Inline keys",
            Concept::Tie => "Tied keys",
            Concept::FencePrefix => "Fence prefix",
            Concept::SharedPrefix => "Shared prefix",
            Concept::Separator => "Separator",
            Concept::Head => "Head",
            Concept::HashTag => "Hash tags",
            Concept::Child => "Children",
            Concept::Page => "Page",
            Concept::Wasm32 => "wasm32 layouts",
            Concept::Prefetch => "Prefetch",
        }
    }

    /// The idea, for someone who knows B-trees.
    pub(crate) fn definition(self) -> &'static str {
        match self {
            Concept::InlineKey => {
                "A short summary of each key: its next few bytes past the node's prefix, as many as the Fences line says. Summaries sort as their keys do, so the search compares the key's own summary with all of the node's and counts how many are smaller: that count is where the key goes. Only keys whose summary is equal, the ties, need a full compare. On x86-64 the count is a few vector compares over the whole array, where a B-tree would binary search full keys."
            }
            Concept::Tie => {
                "Keys whose inline key equals the searched key's. Only they can equal it, so only they are searched with full key compares; every other key is already placed by its summary."
            }
            Concept::FencePrefix => {
                "Every key of a node lies between the two separators that bound it in its ancestors, its fences, so it starts with the bytes they share. The node records how many, and its inline keys start after them: deeper nodes summarize the bytes that still differ."
            }
            Concept::SharedPrefix => {
                "The bytes every key of the map shares, up to 64, which the map keeps in its root's page, after the root node. Nodes along the tree's borders, where a fence is missing, take them as their prefix, as the root does. A lookup checks the key against them once, before the root: a key without them sorts before or after every key, and reads no node."
            }
            Concept::Separator => {
                "A key in an internal node that divides two children. bbtree moves a key up rather than copying it, so every key is stored exactly once: a separator's value stays below, as the head of the leaf to its right."
            }
            Concept::Head => {
                "Every leaf but the leftmost holds one extra value: the value of the separator just left of it. A lookup that finds its key as a separator goes on down to that leaf to read it."
            }
            Concept::HashTag => {
                "Leaves keep a byte per key in spare padding, its hash tag: equal keys have equal tags. A string's tag is its length, up to 255. When several inline keys tie, matching tags narrow the candidates before any key is read from the heap."
            }
            Concept::Child => {
                "An internal node's child pointers. Child i holds the keys between key i − 1 and key i. A pointer to a leaf has its lowest bit set, so a lookup knows a child's kind before reading it."
            }
            Concept::Page => {
                "A node's memory: one allocation, aligned to its size, holding its slot arrays where its layout puts them. The root's page is larger: room for a node of either kind, then the root area, which keeps the tree's shared prefix. Whatever no array covers is padding."
            }
            Concept::Wasm32 => {
                "This page runs bbtree compiled for wasm32, where a pointer takes 4 bytes: internal nodes hold more keys than on a 64-bit machine, and string leaves have no room for hash tags. Leaves of fixed-size keys and values are the same. Without vector compares, it counts the smaller inline keys by a binary search over them instead, to the same count; the cache lines drawn are those of the x86-64 count."
            }
            Concept::Prefetch => {
                "Asking memory for a line before it is needed, without waiting for it. Each key prefetches the line its search reads next, and the other keys search while it comes, so their waits for memory overlap instead of adding up. Batches are the only code in bbtree that prefetches."
            }
        }
    }

    /// The design decision behind the idea, as `design/03-decisions.md` numbers
    /// it.
    pub(crate) fn decision(self) -> Option<&'static str> {
        match self {
            Concept::InlineKey => Some("D18"),
            Concept::Tie => Some("D18"),
            Concept::FencePrefix => Some("D26"),
            Concept::SharedPrefix => Some("D50"),
            Concept::Separator | Concept::Head => Some("D17"),
            Concept::HashTag => Some("D36"),
            Concept::Page => Some("D15"),
            Concept::Child => Some("D32"),
            Concept::Prefetch => Some("D53"),
            Concept::Wasm32 => None,
        }
    }
}

/// A term in running text: pointing at it or focusing it shows its
/// definition, and lights up in the panels what it names.
#[component]
pub(crate) fn Term(concept: Concept, spotlight: Spotlight, children: Element) -> Element {
    let mut mounted: Signal<Option<Rc<MountedData>>> = use_signal(|| None);
    let mut spotlight = spotlight;
    rsx! {
        button {
            class: "term",
            r#type: "button",
            "aria-describedby": "hint",
            onmounted: move |evt| mounted.set(Some(evt.data())),
            onmouseenter: move |evt| {
                let point = evt.client_coordinates();
                spotlight.point_at_concept(concept, (point.x, point.y));
            },
            onmousemove: move |evt| {
                let point = evt.client_coordinates();
                spotlight.move_hint((point.x, point.y));
            },
            onmouseleave: move |_| spotlight.clear(),
            onfocus: move |_| {
                if let Some(element) = mounted.peek().clone() {
                    spawn(async move {
                        if let Ok(rect) = element.get_client_rect().await {
                            let anchor = (rect.min_x() + rect.width() / 2.0, rect.min_y());
                            spotlight.point_at_concept(concept, anchor);
                        }
                    });
                }
            },
            onblur: move |_| spotlight.clear(),
            {children}
        }
    }
}
