//! A node's page drawn byte by byte: each slot array where the layout puts
//! it, the slots the node uses solid, the rest faint, and padding bare.

use std::ops::Range;

use bbtree::inspect::{PageMap, SlotContents};
use dioxus::prelude::*;

/// How the panels draw and name what a slot array holds.
pub(crate) trait SlotLook {
    /// The CSS class of the array's color.
    fn class(self) -> &'static str;
    fn label(self) -> &'static str;
}

impl SlotLook for SlotContents {
    fn class(self) -> &'static str {
        match self {
            SlotContents::Header => "slot-header",
            SlotContents::InlineKeys => "slot-inline",
            SlotContents::LengthTags => "slot-tags",
            SlotContents::Keys => "slot-keys",
            SlotContents::Head | SlotContents::Values | SlotContents::Edges => "slot-trailing",
        }
    }

    fn label(self) -> &'static str {
        match self {
            SlotContents::Header => "header",
            SlotContents::InlineKeys => "inline keys",
            SlotContents::LengthTags => "length tags",
            SlotContents::Keys => "keys",
            SlotContents::Head => "head",
            SlotContents::Values => "values",
            SlotContents::Edges => "children",
        }
    }
}

/// A stretch of a page that one array's slots take, all used or all empty.
#[derive(Clone, PartialEq, Debug)]
struct SlotRun {
    contents: SlotContents,
    used: bool,
    slots: Range<usize>,
    bytes: Range<usize>,
}

impl SlotRun {
    /// The runs of `page`'s slots: a dense array as its used slots and its
    /// empty ones, and an array whose slots alternate with another's, as
    /// keys and values do in paired entries (D29), slot by slot.
    fn of(page: &PageMap) -> Vec<SlotRun> {
        let mut runs = Vec::new();
        for array in page.arrays() {
            if array.slot_size() == 0 || array.capacity() == 0 {
                continue;
            }
            let run_of = |slots: Range<usize>, used: bool| SlotRun {
                contents: array.contents(),
                used,
                bytes: array.slot_bytes(slots.start).start..array.slot_bytes(slots.end - 1).end,
                slots,
            };
            if array.stride() == array.slot_size() {
                if array.used() > 0 {
                    runs.push(run_of(0..array.used(), true));
                }
                if array.used() < array.capacity() {
                    runs.push(run_of(array.used()..array.capacity(), false));
                }
            } else {
                runs.extend(
                    (0..array.capacity()).map(|slot| run_of(slot..slot + 1, slot < array.used())),
                );
            }
        }
        runs
    }

    fn class(&self) -> String {
        let faint = if self.used { "" } else { " slot-empty" };
        format!("{}{faint}", self.contents.class())
    }

    fn describe(&self) -> String {
        let slots = if self.slots.len() == 1 {
            format!("slot {}", self.slots.start)
        } else {
            format!("slots {}–{}", self.slots.start, self.slots.end - 1)
        };
        let state = if self.used { "used" } else { "empty" };
        format!(
            "{} {slots}, {state}: bytes {}–{}",
            self.contents.label(),
            self.bytes.start,
            self.bytes.end
        )
    }
}

/// `page`, across the width it is given, `height` pixels tall.
#[component]
pub(crate) fn PageBar(page: PageMap, height: f64) -> Element {
    let size = page.size();
    rsx! {
        svg {
            class: "block w-full",
            height,
            view_box: "0 0 {size} 1",
            preserve_aspect_ratio: "none",
            "role": "img",
            "aria-label": "Page layout, {size} bytes",
            rect { class: "page-track", x: 0, y: 0, width: size, height: 1 }
            for run in SlotRun::of(&page) {
                rect {
                    class: run.class(),
                    x: run.bytes.start,
                    y: 0,
                    width: run.bytes.len(),
                    height: 1,
                    title { {run.describe()} }
                }
            }
        }
    }
}

/// What each color of `page`'s bar holds.
#[component]
pub(crate) fn PageLegend(page: PageMap) -> Element {
    let has = |contents: &[SlotContents]| {
        page.arrays()
            .iter()
            .any(|array| contents.contains(&array.contents()))
    };
    let trailing = if has(&[SlotContents::Edges]) {
        "children"
    } else {
        "values · head"
    };
    let entries = [
        (SlotContents::Header, "header"),
        (SlotContents::InlineKeys, "inline keys"),
        (SlotContents::LengthTags, "length tags"),
        (SlotContents::Keys, "keys"),
        (SlotContents::Values, trailing),
    ]
    .into_iter()
    .filter(|(contents, _)| *contents == SlotContents::Values || has(&[*contents]));
    rsx! {
        ul { class: "flex flex-wrap gap-x-3 gap-y-1 text-[11px] text-[var(--ink-2)]",
            for (contents, label) in entries {
                li { class: "flex items-center gap-1.5",
                    span { class: "inline-block h-2.5 w-2.5 rounded-sm {contents.class()}" }
                    {label}
                }
            }
            li { class: "flex items-center gap-1.5",
                span { class: "inline-block h-2.5 w-2.5 rounded-sm slot-keys slot-empty" }
                "empty slot"
            }
            li { class: "flex items-center gap-1.5",
                span { class: "inline-block h-2.5 w-2.5 rounded-sm bg-[var(--grid)]" }
                "padding"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use bbtree::BBTreeMap;

    use super::*;

    #[test]
    fn runs_cover_every_slot_once() {
        let map: BBTreeMap<u64, u64> = (0..10_000).map(|key| (key * 3 % 10_000, key)).collect();
        let tree = map.inspect();
        let mut node = tree.root().unwrap();
        while let Some(child) = node.child(1) {
            node = child;
        }
        let page = node.page_map();
        let runs = SlotRun::of(&page);
        for array in page.arrays() {
            let covered: usize = runs
                .iter()
                .filter(|run| run.contents == array.contents())
                .map(|run| run.slots.len())
                .sum();
            assert_eq!(covered, array.capacity(), "{:?}", array.contents());
        }
        assert!(runs.iter().all(|run| run.bytes.end <= page.size()));
    }
}
