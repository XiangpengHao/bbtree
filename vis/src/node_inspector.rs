//! One node of the lookup's path in full: what its search did, step by
//! step; its page, line by line; and its slots, one row each. Pointing at
//! any of them lights up the same part in the others.

use std::ops::Range;

use bbtree::InlineKeyStrategy;
use bbtree::inspect::{NodeView, PageMap, SlotContents, TraceOutcome};
use dioxus::prelude::*;

use crate::concepts::{Concept, Term};
use crate::count_text::Group;
use crate::fences::{FenceStrip, NodeFences};
use crate::icons::{Icon, IconGlyph};
use crate::inspected_tree::LoadedTree;
use crate::memory_map::{MemoryMap, PageMarks};
use crate::slot_table::SlotTable;
use crate::spotlight::{NodePart, Spotlight};
use crate::trace::{Trace, TraceStep, Tracer};
use crate::tree_shape::NodeAt;
use crate::workload::KeyKind;

/// What the inspector shows of a node: owned, so it needs no borrow of the
/// map.
#[derive(Clone, PartialEq)]
pub(crate) struct NodeDetail {
    at: NodeAt,
    id: String,
    len: usize,
    capacity: usize,
    prefix_len: usize,
    local_prefix_len: Option<usize>,
    inline_key_width: Option<usize>,
    page: PageMap,
    head: Option<String>,
    keys: Vec<KeyRow>,
    /// Each child's key count, for an internal node.
    child_lens: Vec<usize>,
    fences: NodeFences,
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
    pub(crate) fn text(&self) -> String {
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
        fences: NodeFences,
    ) -> Self {
        Self {
            at,
            id: node.id().to_string(),
            len: node.len(),
            capacity: node.capacity(),
            prefix_len: node.prefix_len(),
            local_prefix_len: node.local_prefix_len(),
            inline_key_width: node.inline_key_width(),
            page: node.page_map(),
            head,
            keys,
            child_lens,
            fences,
        }
    }

    pub(crate) fn at(&self) -> NodeAt {
        self.at
    }

    pub(crate) fn page(&self) -> &PageMap {
        &self.page
    }

    pub(crate) fn is_leaf(&self) -> bool {
        self.at.level() == 0
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }

    pub(crate) fn keys(&self) -> &[KeyRow] {
        &self.keys
    }

    pub(crate) fn head(&self) -> Option<&str> {
        self.head.as_deref()
    }

    pub(crate) fn inline_key_width(&self) -> Option<usize> {
        self.inline_key_width
    }

    /// How many keys child `edge` holds, of an internal node.
    pub(crate) fn child_len(&self, edge: usize) -> usize {
        self.child_lens.get(edge).copied().unwrap_or(0)
    }

    /// How many leading bytes the node's fences share, and so its keys.
    pub(crate) fn prefix_len(&self) -> usize {
        self.prefix_len
    }

    /// The bytes the node's search skips: its local prefix if it keeps one,
    /// else what its fences share.
    pub(crate) fn skipped(&self) -> usize {
        self.local_prefix_len.unwrap_or(self.prefix_len)
    }

    pub(crate) fn fences(&self) -> &NodeFences {
        &self.fences
    }

    fn key_text(&self, index: usize) -> String {
        self.keys.get(index).map(KeyRow::text).unwrap_or_default()
    }

    /// What slot `slot` of an array of `contents` holds, `used` or not, and
    /// where, in a few words.
    pub(crate) fn describe_slot(
        &self,
        contents: SlotContents,
        slot: usize,
        used: bool,
        bytes: &Range<usize>,
    ) -> String {
        let place = format!(
            "bytes {}–{}, cache line {}",
            bytes.start,
            bytes.end - 1,
            bytes.start / 64
        );
        let row = self.keys.get(slot).filter(|_| used);
        let what = match contents {
            SlotContents::Header => "the node's length and prefix lengths".to_string(),
            _ if !used => "empty: room for an insert".to_string(),
            SlotContents::InlineKeys => format!(
                "{} — the summary of key {slot}, {}",
                row.and_then(|row| row.inline_key.clone())
                    .unwrap_or_default(),
                self.key_text(slot)
            ),
            SlotContents::LengthTags => format!(
                "length {} — of key {slot}",
                row.and_then(|row| row.length_tag).unwrap_or_default()
            ),
            SlotContents::Keys => self.key_text(slot),
            SlotContents::Values => format!(
                "{} — the value of key {slot}",
                row.and_then(|row| row.value.clone()).unwrap_or_default()
            ),
            SlotContents::Head => format!(
                "{} — the value of the separator just left of this leaf",
                self.head.clone().unwrap_or_default()
            ),
            SlotContents::Edges => format!("a child of {} keys", Group(self.child_len(slot))),
        };
        format!("{what} · {place}")
    }

    /// The node's search, step by step, as the trace says it ran.
    fn search_steps(&self, step: &TraceStep) -> Vec<SearchStep> {
        let mut steps = Vec::new();
        let searched = !matches!(
            step.outcome(),
            TraceOutcome::ToHead { .. } | TraceOutcome::Head
        );
        if searched {
            if self.skipped() > 0 {
                steps.push(SearchStep::SkipPrefix {
                    bytes: self.skipped(),
                    local: self.local_prefix_len.is_some(),
                });
            }
            match (step.inline_key(), self.inline_key_width, step.tied()) {
                (Some(window), Some(width), Some(tied)) => {
                    steps.push(SearchStep::CompareInlineKeys {
                        window: window.to_string(),
                        width,
                        tied: tied.clone(),
                    });
                    if !tied.is_empty() {
                        steps.push(SearchStep::SearchTies { tied });
                    }
                }
                _ => steps.push(SearchStep::CompareFullKeys),
            }
        }
        steps.push(SearchStep::Outcome(step.outcome()));
        steps
    }
}

/// One step of a node's search, in the order it runs.
#[derive(Clone, PartialEq, Debug)]
enum SearchStep {
    /// Skip the bytes every key of the node shares: what its fences share,
    /// or a longer local prefix.
    SkipPrefix { bytes: usize, local: bool },
    /// Compare the key's inline key with every one of the node's, counting
    /// those smaller and those equal: the smaller ones place the key.
    CompareInlineKeys {
        window: String,
        width: usize,
        tied: Range<usize>,
    },
    /// No inline keys: a binary search over the full keys.
    CompareFullKeys,
    /// Search the tied keys, comparing them in full.
    SearchTies { tied: Range<usize> },
    /// Where the node's part of the lookup ended.
    Outcome(TraceOutcome),
}

impl SearchStep {
    fn title(&self) -> &'static str {
        match self {
            SearchStep::SkipPrefix { .. } => "Skip the shared bytes",
            SearchStep::CompareInlineKeys { .. } => "Count the smaller inline keys",
            SearchStep::CompareFullKeys => "Binary search over full keys",
            SearchStep::SearchTies { tied } if tied.len() == 1 => "Check the one tied key",
            SearchStep::SearchTies { .. } => "Search the tied keys",
            SearchStep::Outcome(TraceOutcome::Found { .. }) => "Found",
            SearchStep::Outcome(TraceOutcome::Absent { .. }) => "Not in the map",
            SearchStep::Outcome(TraceOutcome::Descend { .. }) => "Go down",
            SearchStep::Outcome(TraceOutcome::ToHead { .. }) => "Pass through",
            SearchStep::Outcome(TraceOutcome::Head) => "Read the head",
        }
    }

    /// What the step came to, in a few words, beside its title.
    fn fact(&self, detail: &NodeDetail) -> String {
        match self {
            SearchStep::SkipPrefix {
                bytes,
                local: false,
            } => format!("{bytes} B the fences share"),
            SearchStep::SkipPrefix { bytes, local: true } => format!("{bytes} B the keys share"),
            SearchStep::CompareInlineKeys { window, tied, .. } => {
                format!("{window}: {} smaller, {} equal", tied.start, tied.len())
            }
            SearchStep::CompareFullKeys => "no inline keys".to_string(),
            SearchStep::SearchTies { tied } if tied.len() == 1 => format!("slot {}", tied.start),
            SearchStep::SearchTies { tied } => format!("slots {}–{}", tied.start, tied.end - 1),
            SearchStep::Outcome(TraceOutcome::Found { index }) if detail.is_leaf() => {
                format!("slot {index}")
            }
            SearchStep::Outcome(TraceOutcome::Found { index }) => {
                format!("separator, slot {index}")
            }
            SearchStep::Outcome(TraceOutcome::Absent { insert_at }) => {
                format!("would go at slot {insert_at}")
            }
            SearchStep::Outcome(TraceOutcome::Descend { edge })
            | SearchStep::Outcome(TraceOutcome::ToHead { edge }) => format!("child {edge}"),
            SearchStep::Outcome(TraceOutcome::Head) => "its head is the value".to_string(),
        }
    }

    /// What lights up while the reader points at this step.
    fn light(&self, at: NodeAt, spotlight: &mut Spotlight) {
        match self {
            SearchStep::SkipPrefix { local: true, .. } => {
                spotlight.light_concept(Some(Concept::SearchPrefix))
            }
            SearchStep::SkipPrefix { .. } => spotlight.light_concept(Some(Concept::FencePrefix)),
            SearchStep::CompareInlineKeys { .. } => {
                spotlight.light_concept(Some(Concept::InlineKey))
            }
            SearchStep::CompareFullKeys => spotlight.clear(),
            SearchStep::SearchTies { .. } => spotlight.light_concept(Some(Concept::Tie)),
            SearchStep::Outcome(outcome) => match *outcome {
                TraceOutcome::Found { index } => spotlight.light_part(at, NodePart::Entry(index)),
                TraceOutcome::Descend { edge } | TraceOutcome::ToHead { edge } => {
                    spotlight.light_part(at, NodePart::Child(edge))
                }
                TraceOutcome::Head => spotlight.light_part(at, NodePart::Head),
                TraceOutcome::Absent { .. } => spotlight.clear(),
            },
        }
    }
}

/// Which view of the inspected node's slots is shown.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum InspectorTab {
    /// The page, cache line by cache line.
    Page,
    /// The slots, one row each.
    Slots,
}

impl InspectorTab {
    const ALL: [InspectorTab; 2] = [InspectorTab::Page, InspectorTab::Slots];

    /// The tab's name in a link.
    pub(crate) fn slug(self) -> &'static str {
        match self {
            InspectorTab::Page => "page",
            InspectorTab::Slots => "slots",
        }
    }

    pub(crate) fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tab| tab.slug() == slug)
    }
}

/// The inspected node, beside the path: its search as steps, then its page
/// or its slots.
#[component]
pub(crate) fn NodeInspector(
    tree: LoadedTree,
    at: NodeAt,
    trace: Trace,
    replay: u64,
    tab: Signal<InspectorTab>,
    tracer: Tracer,
    spotlight: Spotlight,
) -> Element {
    let detail = tree.detail(at);
    let shape = tree.shape();
    let level = shape.level(at.level());
    let step = trace.step_at(at).cloned();
    let marks = step
        .as_ref()
        .map(|step| PageMarks::of_step(step, detail.is_leaf()))
        .unwrap_or_default();
    let steps = step
        .as_ref()
        .map(|step| detail.search_steps(step))
        .unwrap_or_default();
    let kind = tree.key_kind();
    let shown = tab();
    let mut tab = tab;
    let mut tracer = tracer;
    let tree_for_pick = tree.clone();
    let detail_for_pick = detail.clone();
    let on_pick = move |part: NodePart| match part {
        NodePart::Entry(index) => {
            tracer.run_in(&tree_for_pick, &detail_for_pick.key_text(index), at);
        }
        NodePart::Child(edge) => {
            tracer.run_into(&tree_for_pick, tree_for_pick.shape().child(at, edge))
        }
        NodePart::Head => {}
    };
    rsx! {
        section { class: "inspector", "aria-label": "The inspected node",
            header { class: "inspector-head",
                div { class: "inspector-title",
                    h2 { class: "display", {shape.level_name(at.level())} }
                    span { class: "data text-small text-graphite",
                        "{Group(at.index() + 1)} of {Group(level.node_count())}"
                    }
                    span { class: "data text-small text-graphite",
                        " · {detail.len} of {detail.capacity} keys"
                    }
                    span { class: "data text-small text-graphite",
                        " · {Group(detail.page.size())} B "
                        Term { concept: Concept::Page, spotlight, "page" }
                    }
                    span {
                        class: "data text-small text-graphite",
                        title: "The page's address, which tells this node apart",
                        " · {detail.id}"
                    }
                }
            }
            FenceStrip {
                tree: tree.clone(),
                detail: detail.clone(),
                step: step.clone(),
                looked_up: step.as_ref().map(|_| trace.query_bytes().clone()),
                tracer,
                spotlight,
            }
            if steps.is_empty() {
                p { class: "inspector-note text-ui text-graphite",
                    "The lookup didn't pass through this node. Click one of its keys or children to look up a key that does."
                }
            } else {
                SearchSteps {
                    key: "{replay}-{at.level()}-{at.index()}",
                    steps,
                    detail: detail.clone(),
                    kind,
                    spotlight,
                }
            }
            div { class: "tabs", role: "tablist", "aria-label": "Views of the node",
                button {
                    class: "tab",
                    role: "tab",
                    "aria-selected": if shown == InspectorTab::Page { "true" } else { "false" },
                    onclick: move |_| tab.set(InspectorTab::Page),
                    "Page"
                    span { class: "tab-note", "its memory, line by line" }
                }
                button {
                    class: "tab",
                    role: "tab",
                    "aria-selected": if shown == InspectorTab::Slots { "true" } else { "false" },
                    onclick: move |_| tab.set(InspectorTab::Slots),
                    "Slots"
                    span { class: "tab-note", "every key, one row each" }
                }
            }
            if shown == InspectorTab::Page {
                div { class: "tab-panel inspector-page", role: "tabpanel",
                    MemoryMap {
                        detail: detail.clone(),
                        marks,
                        replay,
                        spotlight,
                        on_pick,
                    }
                }
            } else {
                SlotTable {
                    tree: tree.clone(),
                    detail,
                    step,
                    replay,
                    tracer,
                    spotlight,
                }
            }
        }
    }
}

/// The node's search, a line a step; a step opens to say it in full.
#[component]
fn SearchSteps(
    steps: Vec<SearchStep>,
    detail: NodeDetail,
    kind: KeyKind,
    spotlight: Spotlight,
) -> Element {
    let mut opened = use_signal(|| None::<usize>);
    rsx! {
        section { class: "search-section", "aria-label": "The node's search",
            h3 { class: "panel-heading", "Search" }
            ol { class: "search-steps",
                for (index, search_step) in steps.into_iter().enumerate() {
                    SearchStepItem {
                        number: index + 1,
                        search_step,
                        detail: detail.clone(),
                        kind,
                        open: opened() == Some(index),
                        on_toggle: move |_| {
                            let open = opened() == Some(index);
                            opened.set(if open { None } else { Some(index) });
                        },
                        spotlight,
                    }
                }
            }
        }
    }
}

/// One step of the node's search: its title and what it came to, and,
/// opened, the step in words drawn from the trace. Pointing at it lights
/// up what it read.
#[component]
fn SearchStepItem(
    number: usize,
    search_step: SearchStep,
    detail: NodeDetail,
    kind: KeyKind,
    open: bool,
    on_toggle: EventHandler<()>,
    spotlight: Spotlight,
) -> Element {
    let at = detail.at();
    let hover_step = search_step.clone();
    let mut hover = spotlight;
    let mut leave = spotlight;
    let fact = search_step.fact(&detail);
    rsx! {
        li {
            class: if open { "search-step open" } else { "search-step" },
            style: "--step:{number};",
            onmouseenter: move |_| hover_step.light(at, &mut hover),
            onmouseleave: move |_| leave.clear(),
            button {
                class: "step-row",
                "aria-expanded": if open { "true" } else { "false" },
                onclick: move |_| on_toggle.call(()),
                span { class: "step-number", "aria-hidden": "true", "{number}" }
                span { class: "step-title", {search_step.title()} }
                span { class: "step-fact data", {fact} }
                span { class: "step-disclose", IconGlyph { icon: Icon::Disclose } }
            }
            if open {
                p { class: "step-body",
                    match search_step {
                        SearchStep::SkipPrefix { bytes, local: false } => rsx! {
                            "The key's first {bytes} bytes are skipped: every key here shares them, because the node's "
                            Term { concept: Concept::FencePrefix, spotlight, "fences" }
                            " do."
                            if kind == KeyKind::Integers {
                                " A u64 compares as its 8 bytes, most significant first."
                            }
                        },
                        SearchStep::SkipPrefix { bytes, local: true } => rsx! {
                            "The key's first {bytes} bytes are skipped: every key here shares them, a longer "
                            Term { concept: Concept::SearchPrefix, spotlight, "search prefix" }
                            " than its fences give."
                        },
                        SearchStep::CompareInlineKeys { window, width, tied } => rsx! {
                            if detail.skipped() > 0 { "Its next {width} bytes, " } else { "Its first {width} bytes, " }
                            code { class: "bytes", {window} }
                            ", are compared with all {detail.len()} of the node's "
                            Term { concept: Concept::InlineKey, spotlight, "inline keys" }
                            ", counting how many are smaller and how many equal. "
                            {CompareOutcome::new(&tied, detail.len()).to_string()}
                        },
                        SearchStep::CompareFullKeys => rsx! {
                            "This node keeps no inline keys, so its search compares full keys."
                        },
                        SearchStep::SearchTies { tied } => rsx! {
                            if tied.len() == 1 {
                                "Only slot {tied.start} can equal the key, so only it is compared in full."
                            } else {
                                "A binary search among the {tied.len()} "
                                Term { concept: Concept::Tie, spotlight, "tied keys" }
                                " compares a few of them in full, never the rest of the node."
                            }
                        },
                        SearchStep::Outcome(outcome) => rsx! {
                            OutcomeText { outcome, detail: detail.clone(), spotlight }
                        },
                    }
                }
            }
        }
    }
}

/// What a compare of inline keys counted, in a sentence: the keys whose
/// inline keys are smaller than the searched key's come first, so their
/// count is where the key goes, and only the equal ones, the run `tied`,
/// can be the key.
pub(crate) struct CompareOutcome<'a> {
    tied: &'a Range<usize>,
    len: usize,
}

impl<'a> CompareOutcome<'a> {
    /// The compare that left `tied` in a node of `len` keys.
    pub(crate) fn new(tied: &'a Range<usize>, len: usize) -> Self {
        Self { tied, len }
    }

    fn smaller(&self) -> String {
        match self.tied.start {
            0 => "None is smaller".to_string(),
            1 => "1 is smaller".to_string(),
            smaller => format!("{smaller} are smaller"),
        }
    }
}

impl std::fmt::Display for CompareOutcome<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tied = self.tied;
        let smaller = self.smaller();
        match tied.len() {
            0 if tied.start == 0 => write!(
                f,
                "None is smaller or equal, so the key goes before slot 0, with no full key compared."
            ),
            0 if tied.start == self.len => write!(
                f,
                "All {} are smaller, so the key goes after the last slot, with no full key compared.",
                self.len
            ),
            0 => write!(
                f,
                "{smaller} and none is equal, so the key goes between slots {} and {}, with no full key compared.",
                tied.start - 1,
                tied.start
            ),
            1 => write!(
                f,
                "{smaller} and only slot {}'s is equal, so only that key can be the one searched for.",
                tied.start
            ),
            n => write!(
                f,
                "{smaller} and {n} are equal, slots {}–{}, so only those keys can be the one searched for.",
                tied.start,
                tied.end - 1
            ),
        }
    }
}

/// Where the node's part of the lookup ended, in words.
#[component]
fn OutcomeText(outcome: TraceOutcome, detail: NodeDetail, spotlight: Spotlight) -> Element {
    match outcome {
        TraceOutcome::Found { index } if detail.is_leaf() => rsx! {
            "Slot {index} holds the key. Its value, "
            code { class: "bytes", {detail.keys()[index].value.clone().unwrap_or_default()} }
            ", sits right beside it, in the same entry."
        },
        TraceOutcome::Found { index } => rsx! {
            "Slot {index} holds the key, as a "
            Term { concept: Concept::Separator, spotlight, "separator" }
            ". bbtree stores every key once, so its value waits below, as the "
            Term { concept: Concept::Head, spotlight, "head" }
            " of the leaf to its right: the lookup goes on down child {index + 1}."
        },
        TraceOutcome::Descend { edge } => {
            let between = match (edge.checked_sub(1), edge < detail.len()) {
                (Some(left), true) => format!(
                    "between slot {left}, {}, and slot {edge}, {}",
                    detail.key_text(left),
                    detail.key_text(edge)
                ),
                (None, _) => format!("before slot 0, {}", detail.key_text(0)),
                (Some(left), false) => format!("after the last key, {}", detail.key_text(left)),
            };
            rsx! {
                "The key falls {between}, so the lookup goes down "
                Term { concept: Concept::Child, spotlight, "child {edge}" }
                "."
            }
        }
        TraceOutcome::Absent { insert_at } => rsx! {
            "No key here equals it, so the map doesn't hold it. Inserting it would put it at slot {insert_at}."
        },
        TraceOutcome::ToHead { edge } => rsx! {
            "Not searched: on the way to the separator's value, the lookup takes child {edge}, the first."
        },
        TraceOutcome::Head => rsx! {
            "Not searched: this leaf's "
            Term { concept: Concept::Head, spotlight, "head" }
            " is the separator's value, "
            code { class: "bytes", {detail.head().unwrap_or_default().to_string()} }
            "."
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compare_outcomes_read_as_sentences() {
        assert_eq!(
            CompareOutcome::new(&(35..35), 70).to_string(),
            "35 are smaller and none is equal, so the key goes between slots 34 and 35, with no full key compared."
        );
        assert_eq!(
            CompareOutcome::new(&(0..0), 70).to_string(),
            "None is smaller or equal, so the key goes before slot 0, with no full key compared."
        );
        assert_eq!(
            CompareOutcome::new(&(70..70), 70).to_string(),
            "All 70 are smaller, so the key goes after the last slot, with no full key compared."
        );
        assert_eq!(
            CompareOutcome::new(&(7..8), 70).to_string(),
            "7 are smaller and only slot 7's is equal, so only that key can be the one searched for."
        );
        assert_eq!(
            CompareOutcome::new(&(0..9), 9).to_string(),
            "None is smaller and 9 are equal, slots 0–8, so only those keys can be the one searched for."
        );
    }
}
