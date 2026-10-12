//! One node of the lookup's path in full: what the search did there, in
//! words; its page, line by line; and its slots, one row each.

use bbtree::InlineKeyStrategy;
use bbtree::inspect::{NodeView, PageMap, TraceOutcome};
use dioxus::prelude::*;

use crate::count_text::Group;
use crate::inspected_tree::LoadedTree;
use crate::memory_map::{MemoryMap, PageMarks, PageResult};
use crate::trace::{TraceStep, Tracer};
use crate::tree_shape::NodeAt;

/// What the inspector shows of a node: owned, so it needs no borrow of the
/// map.
#[derive(Clone, PartialEq)]
pub(crate) struct NodeDetail {
    at: NodeAt,
    id: String,
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
pub(crate) struct KeyRow {
    /// The bytes every key of the node shares, which its search skips.
    pub(crate) shared: String,
    pub(crate) rest: String,
    pub(crate) inline_key: Option<String>,
    pub(crate) length_tag: Option<u16>,
    pub(crate) value: Option<String>,
}

impl KeyRow {
    fn text(&self) -> String {
        format!("{}{}", self.shared, self.rest)
    }
}

impl NodeDetail {
    pub(crate) fn read<K, S: InlineKeyStrategy<K>>(
        at: NodeAt,
        node: &NodeView<'_, K, u64, S>,
        keys: Vec<KeyRow>,
        child_lens: Vec<usize>,
        head: Option<String>,
    ) -> Self {
        Self {
            at,
            id: node.id().to_string(),
            len: node.len(),
            capacity: node.capacity(),
            min_len: node.min_len(),
            prefix_len: node.prefix_len(),
            local_prefix_len: node.local_prefix_len(),
            inline_key_width: node.inline_key_width(),
            page: node.page_map(),
            head,
            keys,
            child_lens,
        }
    }

    fn is_leaf(&self) -> bool {
        self.at.level() == 0
    }

    /// The bytes the node's search skips: its local prefix if it keeps one,
    /// else what its fences share.
    fn skipped(&self) -> usize {
        self.local_prefix_len.unwrap_or(self.prefix_len)
    }

    fn key_text(&self, index: usize) -> String {
        self.keys.get(index).map(KeyRow::text).unwrap_or_default()
    }

    /// What the lookup did here, in sentences.
    fn explain(&self, step: &TraceStep) -> Vec<String> {
        let mut said = Vec::new();
        let searched = !matches!(
            step.outcome(),
            TraceOutcome::ToHead { .. } | TraceOutcome::Head
        );
        if searched {
            if self.skipped() > 0 {
                let which = if self.local_prefix_len.is_some() {
                    "its own keys share, a longer prefix than its fences (D33)"
                } else {
                    "its fences share, and so every key in it (D26)"
                };
                said.push(format!(
                    "The search skips the first {} bytes, which {which}.",
                    self.skipped()
                ));
            }
            match (step.inline_key(), self.inline_key_width, step.tied()) {
                (Some(window), Some(width), Some(tied)) => {
                    let start = if self.skipped() > 0 { "next" } else { "first" };
                    let matched = match tied.len() {
                        0 => "none equals it, so the inline keys alone place the key".to_string(),
                        1 => format!(
                            "only slot {}'s equals it, so only that key is compared in full",
                            tied.start
                        ),
                        n => format!(
                            "slots {}–{} equal it, so those {n} keys are compared in full",
                            tied.start,
                            tied.end - 1
                        ),
                    };
                    said.push(format!(
                        "It compares the key's {start} {width} bytes, {window}, with all {} inline keys at once (D18): {matched}.",
                        self.len
                    ));
                }
                _ => said.push(
                    "These keys have no inline keys, so the search compares full keys.".to_string(),
                ),
            }
        }
        said.push(match step.outcome() {
            TraceOutcome::Found { index } if self.is_leaf() => format!(
                "Slot {index} holds the key: found, with its value, {}, in the same entry.",
                self.keys[index].value.clone().unwrap_or_default()
            ),
            TraceOutcome::Found { index } => format!(
                "Slot {index} holds the key: a separator (D17), whose value is the head of the leftmost leaf to its right, so the lookup goes on down edge {}.",
                index + 1
            ),
            TraceOutcome::Descend { edge } => {
                let between = match (edge.checked_sub(1), edge < self.len) {
                    (Some(left), true) => format!(
                        "between slot {left}, {}, and slot {edge}, {}",
                        self.key_text(left),
                        self.key_text(edge)
                    ),
                    (None, _) => format!("before slot 0, {}", self.key_text(0)),
                    (Some(left), false) => format!("after slot {left}, {}", self.key_text(left)),
                };
                format!("The key falls {between}, so the lookup goes down edge {edge}.")
            }
            TraceOutcome::Absent { insert_at } => format!(
                "No key here equals it: the map doesn't hold it. Inserting it would put it at slot {insert_at}."
            ),
            TraceOutcome::ToHead { edge } => format!(
                "Not searched: on the way to the separator's value, the lookup takes edge {edge}, the first."
            ),
            TraceOutcome::Head => format!(
                "Not searched: this leaf's head is the separator's value, {}.",
                self.head.clone().unwrap_or_default()
            ),
        });
        said
    }

    /// What the lookup read of this node's page.
    fn marks(&self, step: Option<&TraceStep>) -> PageMarks {
        let Some(step) = step else {
            return PageMarks::default();
        };
        PageMarks {
            candidates: step.tied().unwrap_or(0..0),
            result: match step.outcome() {
                TraceOutcome::Found { index } => Some(PageResult::Key(index)),
                TraceOutcome::Descend { edge } | TraceOutcome::ToHead { edge } => {
                    Some(PageResult::Edge(edge))
                }
                TraceOutcome::Head => Some(PageResult::Head),
                TraceOutcome::Absent { .. } => None,
            },
            read_inline_keys: step.inline_key().is_some(),
        }
    }
}

/// The inspected node, under the path.
#[component]
pub(crate) fn NodeInspector(
    tree: LoadedTree,
    at: NodeAt,
    step: Option<TraceStep>,
    tracer: Tracer,
) -> Element {
    let detail = tree.detail(at);
    let shape = tree.shape();
    let level = shape.level(at.level());
    let title = shape.level_name(at.level());
    let fill = detail.len as f64 / detail.capacity.max(1) as f64;
    let sentences = step
        .as_ref()
        .map(|step| detail.explain(step))
        .unwrap_or_default();
    let marks = detail.marks(step.as_ref());
    let keys: Vec<String> = detail.keys.iter().map(KeyRow::text).collect();
    let lines = detail.page.size().div_ceil(64);
    rsx! {
        section { class: "grid min-h-0 flex-1 grid-cols-[minmax(0,auto)_minmax(0,1fr)] border-t border-[var(--rule)] bg-[var(--panel)]",
            div { class: "flex min-h-0 flex-col gap-x-7 gap-y-4 overflow-y-auto border-r border-[var(--rule)] px-6 py-4 2xl:flex-row",
                div { class: "flex shrink-0 flex-col gap-3",
                header { class: "flex items-baseline justify-between gap-4",
                    h2 { class: "display text-[13px]",
                        "{title} "
                        span { class: "data text-[12px] font-normal tracking-normal text-[var(--graphite)] normal-case",
                            "{Group(at.index() + 1)} of {Group(level.node_count())}"
                        }
                    }
                    span { class: "data text-[11px] text-[var(--muted)]", "page {detail.id}" }
                }
                MemoryMap { page: detail.page.clone(), marks, keys }
                }
                div { class: "flex w-full max-w-[400px] flex-col gap-4",
                if !sentences.is_empty() {
                    div { class: "flex flex-col gap-1.5 text-[13.5px] leading-[1.55] text-[var(--ink)]",
                        for sentence in sentences {
                            p { {sentence} }
                        }
                    }
                }
                dl { class: "facts",
                    div {
                        dt { "keys" }
                        dd { "{detail.len} of {detail.capacity} · {fill * 100.0:.0}%" }
                    }
                    div {
                        dt { "minimum (D22)" }
                        dd { "{detail.min_len} keys" }
                    }
                    div {
                        dt { "fence prefix" }
                        dd { "{detail.prefix_len} B" }
                    }
                    if let Some(local) = detail.local_prefix_len {
                        div {
                            dt { "search prefix" }
                            dd { "{local} B" }
                        }
                    }
                    div {
                        dt { "inline keys" }
                        dd {
                            {detail.inline_key_width.map(|width| format!("{width} B each")).unwrap_or_else(|| "none".to_string())}
                        }
                    }
                    if detail.is_leaf() {
                        div {
                            dt { "head" }
                            dd { {detail.head.clone().unwrap_or_else(|| "none, leftmost leaf".to_string())} }
                        }
                    }
                    div {
                        dt { "page" }
                        dd { "{Group(detail.page.size())} B · {lines} lines" }
                    }
                }
                }
            }
            SlotTable { tree: tree.clone(), detail, step, tracer }
        }
    }
}

/// The node's slots, one row each, with the lookup's marked.
#[component]
fn SlotTable(
    tree: LoadedTree,
    detail: NodeDetail,
    step: Option<TraceStep>,
    tracer: Tracer,
) -> Element {
    let has_inline = detail.inline_key_width.is_some();
    let has_tags = detail.keys.iter().any(|row| row.length_tag.is_some());
    let tied = step.as_ref().and_then(TraceStep::tied).unwrap_or(0..0);
    let found = step.as_ref().and_then(TraceStep::found);
    let edge_taken = step.as_ref().and_then(TraceStep::edge_taken);
    // The row the lookup ended on, kept in view: a key's, or the row whose
    // child it went down to (the first child's row is the one before key 0).
    let marked_row = match (found, edge_taken) {
        (Some(index), _) => Some(format!("slot-row-{index}")),
        (None, Some(0)) => Some("slot-row-edge".to_string()),
        (None, Some(edge)) if !detail.is_leaf() => Some(format!("slot-row-{}", edge - 1)),
        _ => None,
    };
    use_effect(use_reactive!(|marked_row| {
        if let Some(row) = marked_row {
            document::eval(&format!(
                "document.getElementById('{row}')?.scrollIntoView({{block: 'center'}})"
            ));
        }
    }));
    let shape = tree.shape();
    let children =
        (!detail.is_leaf()).then(|| shape.level(detail.at.level()).children(detail.at.index()));
    let child_at = move |edge: usize| {
        children
            .clone()
            .map(|children| NodeAt::new(detail.at.level() - 1, children.start + edge))
    };
    let columns = 3 + usize::from(has_inline) + usize::from(has_tags);
    rsx! {
        div { class: "min-h-0 overflow-y-auto",
            table { class: "slots data w-full table-fixed text-[12px]",
                thead {
                    tr {
                        th { class: "w-[76px]", "slot" }
                        th { "key" }
                        if has_inline {
                            th { class: if detail.inline_key_width.unwrap_or(0) > 4 { "w-[188px]" } else { "w-[104px]" }, "inline key" }
                        }
                        if has_tags {
                            th { class: "w-16", "length" }
                        }
                        th { class: "w-32",
                            if detail.is_leaf() { "value" } else { "child right of it" }
                        }
                    }
                }
                tbody {
                    if detail.is_leaf() {
                        tr { class: if step.as_ref().is_some_and(|step| step.outcome() == TraceOutcome::Head) { "row-found" } else { "" },
                            td { class: "text-[var(--muted)]", "head" }
                            td { class: "truncate text-[var(--graphite)]", colspan: columns - 2,
                                "the value of the separator just left of this leaf"
                            }
                            td { {detail.head.clone().unwrap_or_else(|| "—".to_string())} }
                        }
                    } else {
                        tr {
                            id: "slot-row-edge",
                            class: if edge_taken == Some(0) { "row-found" } else { "" },
                            td { class: "text-[var(--muted)]", "edge 0" }
                            td { class: "truncate text-[var(--graphite)]", colspan: columns - 2, "every key below the first" }
                            td {
                                ChildLink { tree: tree.clone(), target: child_at(0), keys: detail.child_lens.first().copied().unwrap_or(0), tracer }
                            }
                        }
                    }
                    for (index, row) in detail.keys.iter().enumerate() {
                        tr {
                            id: "slot-row-{index}",
                            class: if found == Some(index) || edge_taken == Some(index + 1) && !detail.is_leaf() { "row-found" } else if tied.contains(&index) { "row-candidate" } else { "" },
                            td { class: "text-[var(--muted)]", "{index}" }
                            td { class: "truncate", title: row.text(),
                                if !row.shared.is_empty() {
                                    span { class: "text-[var(--muted)]", "…" }
                                }
                                {row.rest.clone()}
                            }
                            if has_inline {
                                td { class: "whitespace-nowrap text-[var(--graphite)]", {row.inline_key.clone().unwrap_or_default()} }
                            }
                            if has_tags {
                                td { class: "text-[var(--graphite)]", {row.length_tag.map(|tag| tag.to_string()).unwrap_or_default()} }
                            }
                            td {
                                match &row.value {
                                    Some(value) => rsx! { {value.clone()} },
                                    None => rsx! {
                                        ChildLink { tree: tree.clone(), target: child_at(index + 1), keys: detail.child_lens.get(index + 1).copied().unwrap_or(0), tracer }
                                    },
                                }
                            }
                        }
                    }
                    for index in detail.len..detail.capacity {
                        tr { class: "row-empty",
                            td { "{index}" }
                            td { colspan: columns - 1, "room" }
                        }
                    }
                }
            }
        }
    }
}

/// A link that looks up the first key under a child, so the path goes
/// through it.
#[component]
fn ChildLink(tree: LoadedTree, target: Option<NodeAt>, keys: usize, tracer: Tracer) -> Element {
    let mut tracer = tracer;
    rsx! {
        button {
            class: "link",
            title: "Look up the first key under this child",
            onclick: move |_| {
                if let Some(target) = target {
                    tracer.run_into(&tree, target);
                }
            },
            "{keys} keys ↓"
        }
    }
}
