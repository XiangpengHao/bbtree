//! One node in full, read from the node itself: its header, its page as it
//! is laid out, and each key with what bbtree keeps beside it.

use bbtree::Auto;
use bbtree::inspect::{NodeView, PageMap};
use dioxus::prelude::*;

use crate::inspected_tree::{LoadedTree, VisKey};
use crate::page_bar::{PageBar, PageLegend};
use crate::tree_shape::NodeAt;

/// What the panel shows of a node: owned, so the panel needs no borrow of
/// the map.
#[derive(Clone, PartialEq)]
pub(crate) struct NodeDetail {
    id: String,
    height: usize,
    len: usize,
    capacity: usize,
    min_len: usize,
    prefix_len: usize,
    local_prefix_len: Option<usize>,
    inline_key_width: Option<usize>,
    page: PageMap,
    head: Option<String>,
    keys: Vec<KeyRow>,
    /// Each child's key count, for an internal node.
    child_lens: Vec<usize>,
}

/// One key, as the node keeps it.
#[derive(Clone, PartialEq)]
struct KeyRow {
    /// The bytes every key of the node shares, which its searches skip.
    shared: String,
    rest: String,
    inline_key: Option<String>,
    length_tag: Option<u16>,
    value: Option<String>,
}

impl NodeDetail {
    pub(crate) fn read<K: VisKey>(node: NodeView<'_, K, u64, Auto>) -> Self {
        let skipped = node.local_prefix_len().unwrap_or(node.prefix_len());
        let inline_key_width = node.inline_key_width();
        let keys = (0..node.len())
            .map(|index| {
                let (shared, rest) = node.key(index).text_split_at(skipped);
                KeyRow {
                    shared,
                    rest,
                    inline_key: Option::zip(node.inline_key(index), inline_key_width)
                        .map(|(word, width)| Self::inline_key_hex(word, width)),
                    length_tag: node.length_tag(index),
                    value: node.value(index).map(u64::to_string),
                }
            })
            .collect();
        let child_lens = (0..=node.len())
            .filter_map(|edge| node.child(edge))
            .map(|child| child.len())
            .collect();
        Self {
            id: node.id().to_string(),
            height: node.height(),
            len: node.len(),
            capacity: node.capacity(),
            min_len: node.min_len(),
            prefix_len: node.prefix_len(),
            local_prefix_len: node.local_prefix_len(),
            inline_key_width,
            page: node.page_map(),
            head: node.head().map(u64::to_string),
            keys,
            child_lens,
        }
    }

    /// The bytes an inline key keeps, from its first, in hex.
    fn inline_key_hex(word: u64, width: usize) -> String {
        let kept = word >> (64 - 8 * width as u32);
        format!("{kept:0digits$x}", digits = 2 * width)
    }

    fn is_leaf(&self) -> bool {
        self.height == 0
    }
}

/// The selected node, beside the tree.
#[component]
pub(crate) fn NodeDetailPanel(
    tree: LoadedTree,
    at: NodeAt,
    selected: Signal<Option<NodeAt>>,
    focus: Signal<Option<NodeAt>>,
) -> Element {
    let detail = tree.node_detail(at);
    let shape = tree.shape();
    let height = shape.height().unwrap_or(0);
    let level = shape.level(at.level());
    let count = level.node_count();
    let title = if detail.is_leaf() {
        "Leaf".to_string()
    } else if at.level() == height {
        "Root".to_string()
    } else {
        format!("Level {}", at.level())
    };
    let parent =
        (at.level() < height).then(|| NodeAt::new(at.level() + 1, level.parent(at.index())));
    let previous = at
        .index()
        .checked_sub(1)
        .map(|index| NodeAt::new(at.level(), index));
    let next = (at.index() + 1 < count).then(|| NodeAt::new(at.level(), at.index() + 1));
    let first_child = (at.level() > 0).then(|| level.children(at.index()).start);
    let fill = detail.len as f64 / detail.capacity as f64;
    let mut selected_signal = selected;
    let mut focus_signal = focus;

    rsx! {
        aside { class: "flex w-[440px] shrink-0 flex-col border-l border-[var(--grid)] bg-[var(--surface)]",
            div { class: "flex items-start justify-between gap-3 border-b border-[var(--grid)] px-4 py-3",
                div {
                    h2 { class: "text-[15px] font-semibold",
                        "{title} "
                        span { class: "tabular font-normal text-[var(--ink-2)]",
                            "{at.index() + 1} of {count}"
                        }
                    }
                    p { class: "tabular text-[11px] text-[var(--muted)]", "page {detail.id}" }
                }
                button {
                    class: "rounded-md px-2 py-1 text-[var(--ink-2)] hover:bg-[var(--plane)]",
                    "aria-label": "Close",
                    onclick: move |_| selected_signal.set(None),
                    "✕"
                }
            }
            nav { class: "flex flex-wrap gap-1.5 border-b border-[var(--grid)] px-4 py-2",
                NavButton { label: "↑ parent", target: parent, selected }
                NavButton { label: "← previous", target: previous, selected }
                NavButton { label: "next →", target: next, selected }
                button {
                    class: "ml-auto rounded-md border border-[var(--grid)] px-2 py-1 text-[12px] hover:bg-[var(--plane)]",
                    onclick: move |_| focus_signal.set(Some(at)),
                    "zoom to node"
                }
            }
            div { class: "min-h-0 flex-1 overflow-y-auto",
                dl { class: "tabular grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 px-4 py-3 text-[13px]",
                    dt { class: "text-[var(--ink-2)]", "keys" }
                    dd { "{detail.len} of {detail.capacity} ({fill * 100.0:.0}% full; at least {detail.min_len} below the root)" }
                    dt { class: "text-[var(--ink-2)]", "fence prefix" }
                    dd { "{detail.prefix_len} bytes" }
                    if let Some(local) = detail.local_prefix_len {
                        dt { class: "text-[var(--ink-2)]", "search prefix" }
                        dd { "{local} bytes (D33)" }
                    }
                    dt { class: "text-[var(--ink-2)]", "inline keys" }
                    dd {
                        match detail.inline_key_width {
                            Some(width) => rsx! { "{width} bytes each (D18)" },
                            None => rsx! { "none" },
                        }
                    }
                    if detail.is_leaf() {
                        dt { class: "text-[var(--ink-2)]", "head" }
                        dd {
                            match &detail.head {
                                Some(head) => rsx! { {head.clone()} },
                                None => rsx! { "none: the leftmost leaf" },
                            }
                        }
                    }
                    dt { class: "text-[var(--ink-2)]", "page" }
                    dd { "{detail.page.size()} bytes" }
                }
                section { class: "px-4 pb-3",
                    h3 { class: "mb-1.5 text-[12px] font-medium text-[var(--ink-2)]", "Page layout" }
                    div { class: "overflow-hidden rounded",
                        PageBar { page: detail.page.clone(), height: 28.0 }
                    }
                    div { class: "tabular mb-2 mt-0.5 flex justify-between text-[10px] text-[var(--muted)]",
                        span { "0" }
                        span { "{detail.page.size()} B" }
                    }
                    PageLegend { page: detail.page.clone() }
                }
                KeyTable { detail: detail.clone(), first_child, at, selected }
            }
        }
    }
}

/// A button that selects `target`, or does nothing when there is none.
#[component]
fn NavButton(
    label: &'static str,
    target: Option<NodeAt>,
    selected: Signal<Option<NodeAt>>,
) -> Element {
    let mut selected = selected;
    rsx! {
        button {
            class: "rounded-md border border-[var(--grid)] px-2 py-1 text-[12px] hover:bg-[var(--plane)] disabled:opacity-40",
            disabled: target.is_none(),
            onclick: move |_| {
                if let Some(target) = target {
                    selected.set(Some(target));
                }
            },
            {label}
        }
    }
}

/// The node's keys, one row each; an internal node's rows also lead to
/// the children between them.
#[component]
fn KeyTable(
    detail: NodeDetail,
    first_child: Option<usize>,
    at: NodeAt,
    selected: Signal<Option<NodeAt>>,
) -> Element {
    let has_tags = detail.keys.iter().any(|row| row.length_tag.is_some());
    let has_inline = detail.inline_key_width.is_some();
    let child_at =
        move |edge: usize| first_child.map(|first| NodeAt::new(at.level() - 1, first + edge));
    rsx! {
        table { class: "tabular w-full border-t border-[var(--grid)] text-[12px]",
            thead { class: "sticky top-0 bg-[var(--surface)] text-left text-[11px] text-[var(--muted)]",
                tr {
                    th { class: "py-1.5 pl-4 pr-2 font-normal", "#" }
                    th { class: "px-2 font-normal", "key" }
                    if has_inline {
                        th { class: "whitespace-nowrap px-2 font-normal", "inline key" }
                    }
                    if has_tags {
                        th { class: "px-2 font-normal", "len" }
                    }
                    th { class: "whitespace-nowrap px-2 pr-4 font-normal",
                        if detail.is_leaf() { "value" } else { "right child" }
                    }
                }
            }
            tbody {
                if !detail.is_leaf() {
                    ChildRow { edge: 0, keys: detail.child_lens[0], target: child_at(0), selected }
                }
                for (index, row) in detail.keys.iter().enumerate() {
                    tr { class: "border-t border-[var(--grid)] align-top",
                        td { class: "py-1 pl-4 pr-2 text-[var(--muted)]", "{index}" }
                        td { class: "break-all px-2 font-mono text-[11.5px]",
                            span { class: "text-[var(--muted)]", "{row.shared}" }
                            "{row.rest}"
                        }
                        if has_inline {
                            td { class: "px-2 font-mono text-[11px] text-[var(--ink-2)]",
                                {row.inline_key.clone().unwrap_or_default()}
                            }
                        }
                        if has_tags {
                            td { class: "px-2 text-[var(--ink-2)]",
                                {row.length_tag.map(|tag| tag.to_string()).unwrap_or_default()}
                            }
                        }
                        td { class: "px-2 pr-4 text-[var(--ink-2)]",
                            match &row.value {
                                Some(value) => rsx! { {value.clone()} },
                                None => rsx! {
                                    ChildLink { edge: index + 1, keys: detail.child_lens[index + 1], target: child_at(index + 1), selected }
                                },
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The row for an internal node's first child, which no key precedes.
#[component]
fn ChildRow(
    edge: usize,
    keys: usize,
    target: Option<NodeAt>,
    selected: Signal<Option<NodeAt>>,
) -> Element {
    rsx! {
        tr { class: "border-t border-[var(--grid)]",
            td { class: "py-1 pl-4 pr-2 text-[var(--muted)]" }
            td { class: "px-2 text-[var(--muted)]", colspan: 3, "left of every key" }
            td { class: "px-2 pr-4",
                ChildLink { edge, keys, target, selected }
            }
        }
    }
}

/// A link to child `edge`, which holds `keys` keys.
#[component]
fn ChildLink(
    edge: usize,
    keys: usize,
    target: Option<NodeAt>,
    selected: Signal<Option<NodeAt>>,
) -> Element {
    let mut selected = selected;
    rsx! {
        button {
            class: "whitespace-nowrap text-[var(--accent)] hover:underline",
            title: "child {edge}",
            onclick: move |_| {
                if let Some(target) = target {
                    selected.set(Some(target));
                }
            },
            "{keys} keys →"
        }
    }
}
