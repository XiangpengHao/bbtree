//! A node's fences: the separators just left and right of the way down to
//! it, which bound every key it can hold. The bytes they share, every key
//! between them shares: the node's prefix, which its search skips and its
//! inline keys start after (`doc/05-fence-key.md`). The strip draws the
//! fences, the node's first and last keys and the key looked up as bytes,
//! a column per byte, so the shared columns line up.

use bbtree::inspect::TraceOutcome;
use dioxus::prelude::*;

use crate::concepts::{Concept, Term};
use crate::inspected_tree::LoadedTree;
use crate::node_inspector::NodeDetail;
use crate::spotlight::{Hint, NodePart, Spotlight};
use crate::trace::{TraceStep, Tracer};
use crate::tree_shape::NodeAt;
use crate::workload::KeyKind;

/// A key as the strip draws it: its text, and its bytes as bbtree orders
/// and windows it.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct KeyBytes {
    text: String,
    bytes: Vec<u8>,
}

impl KeyBytes {
    pub(crate) fn new(text: String, bytes: Vec<u8>) -> Self {
        Self { text, bytes }
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }
}

/// A fence: a separator of an ancestor, at slot `slot` of the node at
/// `owner`.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct Fence {
    key: KeyBytes,
    owner: NodeAt,
    slot: usize,
}

impl Fence {
    pub(crate) fn new(key: KeyBytes, owner: NodeAt, slot: usize) -> Self {
        Self { key, owner, slot }
    }
}

/// What bounds a node, the low fence and the high one, `None` on the
/// tree's left or right border; with the node's own first and last keys.
#[derive(Clone, PartialEq, Debug, Default)]
pub(crate) struct NodeFences {
    low: Option<Fence>,
    high: Option<Fence>,
    first: Option<KeyBytes>,
    last: Option<KeyBytes>,
}

impl NodeFences {
    /// Whether the node is on the tree's left or right border, where a
    /// fence is missing.
    pub(crate) fn lacks_a_fence(&self) -> bool {
        self.low.is_none() || self.high.is_none()
    }

    pub(crate) fn new(
        low: Option<Fence>,
        high: Option<Fence>,
        first: Option<KeyBytes>,
        last: Option<KeyBytes>,
    ) -> Self {
        Self {
            low,
            high,
            first,
            last,
        }
    }
}

/// A row of the strip, in key order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum StripRow {
    LowFence,
    FirstKey,
    LookedUp,
    LastKey,
    HighFence,
}

impl StripRow {
    fn label(self) -> &'static str {
        match self {
            StripRow::LowFence => "low fence",
            StripRow::FirstKey => "first key",
            StripRow::LookedUp => "looked up",
            StripRow::LastKey => "last key",
            StripRow::HighFence => "high fence",
        }
    }

    /// The rows for a node of `len` keys, with the key looked up where
    /// the lookup placed it, at `place`, if it passed through.
    fn in_order(place: Option<usize>, len: usize) -> Vec<StripRow> {
        let mut rows = vec![StripRow::LowFence, StripRow::FirstKey];
        if len > 1 {
            rows.push(StripRow::LastKey);
        }
        rows.push(StripRow::HighFence);
        if let Some(place) = place {
            let index = if place == 0 {
                1
            } else if place >= len {
                rows.len() - 1
            } else {
                2
            };
            rows.insert(index, StripRow::LookedUp);
        }
        rows
    }
}

/// Which part of a key a byte is in, at a node.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ByteRegion {
    /// Shared with the fences, or on the tree's border, with every key.
    Prefix,
    /// The inline key's bytes.
    Window,
    Rest,
}

impl ByteRegion {
    fn of(index: usize, detail: &NodeDetail) -> Self {
        let prefix = detail.prefix_len();
        if index < prefix {
            ByteRegion::Prefix
        } else if index < prefix + detail.inline_key_width().unwrap_or(0) {
            ByteRegion::Window
        } else {
            ByteRegion::Rest
        }
    }

    fn class(self) -> &'static str {
        match self {
            ByteRegion::Prefix => "byte in-prefix",
            ByteRegion::Window => "byte in-window",
            ByteRegion::Rest => "byte",
        }
    }
}

/// The node's fences and keys as bytes, and what follows from them.
#[component]
pub(crate) fn FenceStrip(
    tree: LoadedTree,
    detail: NodeDetail,
    /// The lookup's step at this node, if it passed through.
    step: Option<TraceStep>,
    /// The key looked up, if it passed through.
    looked_up: Option<KeyBytes>,
    tracer: Tracer,
    spotlight: Spotlight,
) -> Element {
    let kind = tree.key_kind();
    let fences = detail.fences().clone();
    let place = step.as_ref().map(|step| match step.outcome() {
        TraceOutcome::Descend { edge } | TraceOutcome::ToHead { edge } => edge,
        TraceOutcome::Found { index } => index,
        TraceOutcome::Absent { insert_at } => insert_at,
        TraceOutcome::Head => 0,
    });
    let rows = StripRow::in_order(place.filter(|_| looked_up.is_some()), detail.len());
    let key_of = |row: StripRow| -> Option<KeyBytes> {
        match row {
            StripRow::LowFence => fences.low.as_ref().map(|fence| fence.key.clone()),
            StripRow::FirstKey => fences.first.clone(),
            StripRow::LookedUp => looked_up.clone(),
            StripRow::LastKey => fences.last.clone(),
            StripRow::HighFence => fences.high.as_ref().map(|fence| fence.key.clone()),
        }
    };
    let longest = rows
        .iter()
        .filter_map(|&row| key_of(row).map(|key| key.bytes.len()))
        .max()
        .unwrap_or(0);
    let columns = longest.min(if kind.is_text() { 48 } else { 16 });
    let lit = match spotlight.concept() {
        Some(Concept::FencePrefix) => " lit-prefix",
        Some(Concept::SharedPrefix) if fences.lacks_a_fence() => " lit-prefix",
        Some(Concept::InlineKey) => " lit-window",
        _ => "",
    };
    let cell = if kind.is_text() { "8px" } else { "21px" };
    let prefix = detail.prefix_len();
    let width = detail.inline_key_width().unwrap_or(0);
    let on_border = fences.lacks_a_fence();
    rsx! {
        section { class: "fence-section", "aria-label": "The node's fences and prefix",
            h3 { class: "panel-heading",
                Term { concept: Concept::FencePrefix, spotlight, "Fences" }
                span { class: "panel-heading-note data",
                    "prefix {prefix} B"
                    if on_border {
                        ", every key's"
                    }
                    if width > 0 {
                        " · inline keys {width} B"
                    }
                }
            }
            div { class: "fence-scroll",
                div {
                    class: "fence-strip{lit}",
                    style: "--columns:{columns};--cell:{cell};",
                    // Brackets over the columns each part takes.
                    span { class: "strip-label" }
                    if prefix > 0 {
                        span {
                            class: "strip-bracket bracket-prefix",
                            style: "grid-column:2 / span {prefix.min(columns)};",
                            "prefix"
                        }
                    }
                    if width > 0 && prefix < columns {
                        span {
                            class: "strip-bracket bracket-window",
                            style: "grid-column:{prefix + 2} / span {width.min(columns - prefix)};",
                            "inline key"
                        }
                    }
                    for row in rows {
                        StripRowCells {
                            key: "{row:?}",
                            row,
                            key_bytes: key_of(row),
                            fence: match row {
                                StripRow::LowFence => fences.low.as_ref().map(|fence| (fence.owner, fence.slot)),
                                StripRow::HighFence => fences.high.as_ref().map(|fence| (fence.owner, fence.slot)),
                                _ => None,
                            },
                            detail: detail.clone(),
                            columns,
                            kind,
                            tree: tree.clone(),
                            tracer,
                            spotlight,
                        }
                    }
                }
            }
        }
    }
}

/// One row of the strip: its label, a cell per byte, and the key's text
/// where the bytes don't read as it.
#[component]
fn StripRowCells(
    row: StripRow,
    key_bytes: Option<KeyBytes>,
    /// Where a fence is a separator: its node and slot.
    fence: Option<(NodeAt, usize)>,
    detail: NodeDetail,
    columns: usize,
    kind: KeyKind,
    tree: LoadedTree,
    tracer: Tracer,
    spotlight: Spotlight,
) -> Element {
    let mut tracer = tracer;
    let mut spotlight = spotlight;
    let mark = if row == StripRow::LookedUp {
        " looked-up"
    } else {
        ""
    };
    let label = match fence {
        Some((owner, slot)) => {
            let side = if row == StripRow::LowFence {
                "left"
            } else {
                "right"
            };
            let name = tree.shape().node_name(owner);
            let hint = format!(
                "The separator at slot {slot} of {name}: the nearest key {side} of the way down to this node. Click to inspect {name}."
            );
            rsx! {
                button {
                    class: "strip-label strip-fence",
                    onmouseenter: move |evt| {
                        let point = evt.client_coordinates();
                        spotlight.point_at_part(owner, NodePart::Entry(slot), Some(Hint::new((point.x, point.y), row.label(), hint.clone())));
                    },
                    onmouseleave: move |_| spotlight.clear(),
                    onclick: move |_| {
                        spotlight.clear();
                        tracer.inspect(owner);
                    },
                    {row.label()}
                }
            }
        }
        None => rsx! {
            span { class: "strip-label{mark}", {row.label()} }
        },
    };
    let Some(key) = key_bytes else {
        let side = if row == StripRow::LowFence {
            "left"
        } else {
            "right"
        };
        let shared = tree.shared_prefix_len();
        let border = format!(
            "none: the tree's {side} border, where the prefix is the {shared} B every key shares"
        );
        return rsx! {
            {label}
            span { class: "strip-none", style: "grid-column:2 / span {columns.max(1)};", {border} }
            span { class: "strip-text" }
        };
    };
    let cut = key.bytes.len() > columns;
    rsx! {
        {label}
        for index in 0..columns {
            span {
                class: "{ByteRegion::of(index, &detail).class()}{mark}",
                match key.bytes.get(index) {
                    Some(_) if cut && index + 1 == columns => "…".to_string(),
                    Some(&byte) if kind.is_text() && byte.is_ascii_graphic() => char::from(byte).to_string(),
                    Some(_) if kind.is_text() => "·".to_string(),
                    Some(&byte) => format!("{byte:02x}"),
                    None => String::new(),
                }
            }
        }
        span { class: "strip-text data",
            if kind == KeyKind::Integers {
                {key.text().to_string()}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use bbtree::BBTreeMap;

    use super::*;
    use crate::inspected_tree::{InspectedMap, InspectedTree, VisKey};

    fn common_prefix(a: &[u8], b: &[u8]) -> usize {
        a.iter().zip(b).take_while(|(a, b)| a == b).count()
    }

    /// The hex of `bytes` from `start`, `width` of them, zeros past their
    /// end, as the inspector writes inline keys.
    fn window_hex(bytes: &[u8], start: usize, width: usize) -> String {
        (start..start + width)
            .map(|index| format!("{:02x}", bytes.get(index).copied().unwrap_or(0)))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// At nodes across every level: the prefix bbtree keeps is what the
    /// strip's fences share, or on a border, what every key shares, and the
    /// inline keys are the bytes the strip marks as them.
    fn check<K: VisKey>(map: BBTreeMap<K, u64>, kind: KeyKind) {
        let tree = InspectedMap::new(map, kind);
        let shape = tree.shape();
        for level in 0..shape.level_count() {
            let nodes = shape.level(level).node_count();
            for index in [0, nodes / 3, nodes / 2, nodes - 1] {
                let at = NodeAt::new(level, index);
                let detail = tree.detail(at);
                let fences = detail.fences();
                let shared = match (&fences.low, &fences.high) {
                    (Some(low), Some(high)) => common_prefix(&low.key.bytes, &high.key.bytes),
                    _ => tree.shared_prefix_len(),
                };
                assert_eq!(detail.prefix_len(), shared, "{at:?}");
                let width = detail.inline_key_width().unwrap();
                let first = fences.first.as_ref().unwrap();
                assert_eq!(
                    detail.keys()[0].inline_key.as_deref(),
                    Some(window_hex(&first.bytes, detail.prefix_len(), width).as_str()),
                    "{at:?}"
                );
            }
        }
    }

    /// `count` keys of `key_at`, inserted in a scrambled order.
    fn scrambled<K: VisKey>(count: u64, key_at: impl Fn(u64) -> K) -> BBTreeMap<K, u64> {
        let mut map = BBTreeMap::new();
        for index in 0..count {
            let index = index * 7919 % count;
            map.insert(key_at(index), index);
        }
        map
    }

    #[test]
    fn prefixes_are_what_the_fences_share() {
        check(scrambled(200_000, |index| index * 3), KeyKind::Integers);
        check(
            scrambled(50_000, |index| {
                format!("https://shop.example.com/users/{index:09}")
            }),
            KeyKind::Urls,
        );
        check(
            scrambled(100_000, |index| {
                let mut id = [0; 16];
                id[8..].copy_from_slice(&index.to_be_bytes());
                id
            }),
            KeyKind::Ids,
        );
    }
}
