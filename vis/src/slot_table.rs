//! A node's slots, one row each: the keys with what the node keeps for
//! them, and the values or children beside them. The rows the lookup read
//! are marked, the one it ended on is kept in view, and pointing at a row
//! lights up the same slot in the other panels.

use bbtree::inspect::TraceOutcome;
use dioxus::prelude::*;

use crate::count_text::Group;
use crate::inspected_tree::LoadedTree;
use crate::node_inspector::NodeDetail;
use crate::spotlight::{NodePart, Spotlight};
use crate::trace::{TraceStep, Tracer};
use crate::tree_shape::NodeAt;

/// What the lookup did with each row of the table.
struct RowMarks {
    tied: std::ops::Range<usize>,
    found: Option<usize>,
    /// The child the lookup went down to.
    edge: Option<usize>,
    head: bool,
    /// Where the key would go, if the map doesn't hold it.
    insert_at: Option<usize>,
}

impl RowMarks {
    fn of(step: Option<&TraceStep>) -> Self {
        Self {
            tied: step.and_then(TraceStep::tied).unwrap_or(0..0),
            found: step.and_then(TraceStep::found),
            edge: step.and_then(TraceStep::edge_taken),
            head: step.is_some_and(|step| step.outcome() == TraceOutcome::Head),
            insert_at: step.and_then(|step| match step.outcome() {
                TraceOutcome::Absent { insert_at } => Some(insert_at),
                _ => None,
            }),
        }
    }

    /// The row to keep in view: the key the lookup ended on, the child it
    /// went down to, the head it read, or where the key would go.
    fn row_in_view(&self) -> Option<String> {
        if let Some(index) = self.found {
            return Some(format!("slot-row-{index}"));
        }
        if self.head {
            return Some("slot-row-head".to_string());
        }
        match (self.edge, self.insert_at) {
            (Some(0), _) => Some("slot-row-first".to_string()),
            (Some(edge), _) => Some(format!("slot-row-{}", edge - 1)),
            (None, Some(insert_at)) => Some(format!("slot-row-{}", insert_at.saturating_sub(1))),
            (None, None) => None,
        }
    }

    fn key_class(&self, index: usize, pointed: bool) -> &'static str {
        match (
            self.found == Some(index),
            self.tied.contains(&index),
            pointed,
        ) {
            (true, _, true) => "row-found row-pointed",
            (true, _, false) => "row-found",
            (false, true, true) => "row-candidate row-pointed",
            (false, true, false) => "row-candidate",
            (false, false, true) => "row-pointed",
            (false, false, false) => "",
        }
    }
}

/// The node's slots, one row each, with the lookup's marked.
#[component]
pub(crate) fn SlotTable(
    tree: LoadedTree,
    detail: NodeDetail,
    step: Option<TraceStep>,
    replay: u64,
    tracer: Tracer,
    spotlight: Spotlight,
) -> Element {
    let at = detail.at();
    let leaf = detail.is_leaf();
    let has_inline = detail.inline_key_width().is_some();
    let has_tags = detail.keys().iter().any(|row| row.length_tag.is_some());
    let marks = RowMarks::of(step.as_ref());
    let in_view = marks.row_in_view();
    use_effect(use_reactive!(|in_view, replay| {
        let _ = replay;
        if let Some(row) = in_view {
            // Scrolls the table's own box only: scrolling the row into view
            // would also scroll the page's clipped panels.
            document::eval(&format!(
                "requestAnimationFrame(() => {{
                    const row = document.getElementById('{row}');
                    const box = row?.closest('.slot-scroll');
                    if (!row || !box) return;
                    const offset = row.getBoundingClientRect().top - box.getBoundingClientRect().top;
                    const motion = matchMedia('(prefers-reduced-motion: reduce)').matches ? 'auto' : 'smooth';
                    box.scrollTo({{ top: box.scrollTop + offset - box.clientHeight / 2 + row.clientHeight / 2, behavior: motion }});
                }})"
            ));
        }
    }));
    let pointed = spotlight.part_of(at);
    let columns = 3 + usize::from(has_inline) + usize::from(has_tags);
    let empty = detail.capacity() - detail.len();
    let shape = tree.shape();
    let child_at = move |edge: usize| (!leaf).then(|| shape.child(at, edge));
    let mut spotlight = spotlight;
    rsx! {
        div { class: "slot-scroll",
            table { class: "slots data",
                caption { class: "sr-only", "The node's slots, one row each" }
                thead {
                    tr {
                        th { class: "col-slot", scope: "col", "slot" }
                        th { scope: "col", "key" }
                        if has_inline {
                            th {
                                class: if detail.inline_key_width().unwrap_or(0) > 4 { "col-inline-wide" } else { "col-inline" },
                                scope: "col",
                                "inline key"
                            }
                        }
                        if has_tags {
                            th { class: "col-tag", scope: "col", "length" }
                        }
                        th { class: "col-trailing", scope: "col",
                            if leaf { "value" } else { "child to its right" }
                        }
                    }
                }
                tbody { key: "{replay}-{at.level()}-{at.index()}",
                    if leaf {
                        tr {
                            id: "slot-row-head",
                            class: match (marks.head, pointed == Some(NodePart::Head)) {
                                (true, _) => "row-found",
                                (false, true) => "row-pointed",
                                (false, false) => "",
                            },
                            onmouseenter: move |_| spotlight.light_part(at, NodePart::Head),
                            onmouseleave: move |_| spotlight.clear(),
                            td { class: "text-muted", "head" }
                            td { class: "text-graphite", colspan: columns - 2,
                                "the value of the separator just left of this leaf"
                            }
                            td { {detail.head().map(str::to_string).unwrap_or_else(|| "none: the leftmost leaf".to_string())} }
                        }
                    } else {
                        tr {
                            id: "slot-row-first",
                            class: if pointed == Some(NodePart::Child(0)) { "row-pointed" } else { "" },
                            onmouseenter: move |_| spotlight.light_part(at, NodePart::Child(0)),
                            onmouseleave: move |_| spotlight.clear(),
                            td { class: "text-muted", "—" }
                            td { class: "text-graphite", colspan: columns - 2, "every key below slot 0's" }
                            td { class: if marks.edge == Some(0) { "cell-taken" } else { "" },
                                ChildLink {
                                    tree: tree.clone(),
                                    target: child_at(0),
                                    keys: detail.child_len(0),
                                    taken: marks.edge == Some(0),
                                    tracer,
                                }
                            }
                        }
                    }
                    for (index, row) in detail.keys().iter().enumerate() {
                        if marks.insert_at == Some(index) {
                            tr { class: "row-insert", "aria-label": "Where the key would go",
                                td { colspan: columns, span { "the key would go here" } }
                            }
                        }
                        tr {
                            id: "slot-row-{index}",
                            class: marks.key_class(index, pointed == Some(NodePart::Entry(index))),
                            onmouseenter: move |_| spotlight.light_part(at, NodePart::Entry(index)),
                            onmouseleave: move |_| spotlight.clear(),
                            td { class: "text-muted", "{index}" }
                            td { class: "cell-key",
                                KeyButton { tree: tree.clone(), text: row.text(), shared: !row.shared.is_empty(), rest: row.rest.clone(), tracer }
                            }
                            if has_inline {
                                td { class: "whitespace-nowrap text-graphite", {row.inline_key.clone().unwrap_or_default()} }
                            }
                            if has_tags {
                                td { class: "text-graphite", {row.length_tag.map(|tag| tag.to_string()).unwrap_or_default()} }
                            }
                            match &row.value {
                                Some(value) => rsx! {
                                    td { {value.clone()} }
                                },
                                None => rsx! {
                                    td {
                                        class: if marks.edge == Some(index + 1) { "cell-taken" } else if pointed == Some(NodePart::Child(index + 1)) { "cell-pointed" } else { "" },
                                        onmouseenter: move |evt| {
                                            evt.stop_propagation();
                                            spotlight.light_part(at, NodePart::Child(index + 1));
                                        },
                                        ChildLink {
                                            tree: tree.clone(),
                                            target: child_at(index + 1),
                                            keys: detail.child_len(index + 1),
                                            taken: marks.edge == Some(index + 1),
                                            tracer,
                                        }
                                    }
                                },
                            }
                        }
                    }
                    if marks.insert_at == Some(detail.len()) {
                        tr { class: "row-insert", "aria-label": "Where the key would go",
                            td { colspan: columns, span { "the key would go here" } }
                        }
                    }
                    if empty > 0 {
                        tr { class: "row-empty",
                            td { colspan: columns,
                                "{Group(empty)} empty slots, room for inserts before the node splits"
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A key: click to look it up.
#[component]
fn KeyButton(
    tree: LoadedTree,
    text: String,
    shared: bool,
    rest: String,
    tracer: Tracer,
) -> Element {
    let mut tracer = tracer;
    let query = text.clone();
    rsx! {
        button {
            class: "key-button",
            title: "Look up {text}",
            onclick: move |_| tracer.run(&tree, &query),
            if shared {
                span { class: "text-muted", "…" }
            }
            {rest}
        }
    }
}

/// A child: how many keys it holds; click to look up its first key, so the
/// path goes through it.
#[component]
fn ChildLink(
    tree: LoadedTree,
    target: Option<NodeAt>,
    keys: usize,
    taken: bool,
    tracer: Tracer,
) -> Element {
    let mut tracer = tracer;
    rsx! {
        button {
            class: if taken { "link taken" } else { "link" },
            title: "Look up the first key under this child",
            onclick: move |_| {
                if let Some(target) = target {
                    tracer.run_into(&tree, target);
                }
            },
            "{Group(keys)} keys"
            if taken {
                span { class: "taken-note", " · taken" }
            }
        }
    }
}
