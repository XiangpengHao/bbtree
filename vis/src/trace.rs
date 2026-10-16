//! A lookup, step by step, as bbtree's own search takes it
//! (`TreeView::trace`), kept as plain data for the panels; what starts one;
//! and the bar to start one from.

use std::cmp::Ordering;
use std::ops::Range;

use bbtree::inspect::TraceOutcome;
use dioxus::prelude::*;

use crate::fences::KeyBytes;
use crate::icons::{Icon, IconGlyph};
use crate::inspected_tree::LoadedTree;
use crate::popover::Popover;
use crate::scenarios::Scenario;
use crate::shared_link::LinkedLookup;
use crate::tree_shape::{NodeAt, TreeShape};

/// One lookup, from the root down.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct Trace {
    query: KeyBytes,
    steps: Vec<TraceStep>,
    /// Where the key sorts against every key of the map, if it lacks what
    /// they all share (D50): the lookup then reads no node.
    placed_by_shared_prefix: Option<Ordering>,
}

/// What a lookup did at one node.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct TraceStep {
    at: NodeAt,
    outcome: TraceOutcome,
    /// The searched key's inline key at the node's prefix, in hex, as many
    /// bytes as the node keeps.
    inline_key: Option<String>,
    /// How many bytes of each key the node keeps as its inline key, the
    /// bytes its search compares first; `None` where it keeps none.
    inline_width: Option<usize>,
    /// The keys whose inline keys tie with it: the only ones the search
    /// goes on to compare in full.
    tied: Option<Range<usize>>,
    edge_taken: Option<usize>,
}

impl TraceStep {
    pub(crate) fn new(
        at: NodeAt,
        outcome: TraceOutcome,
        inline_key: Option<String>,
        inline_width: Option<usize>,
        tied: Option<Range<usize>>,
        edge_taken: Option<usize>,
    ) -> Self {
        Self {
            at,
            outcome,
            inline_key,
            inline_width,
            tied,
            edge_taken,
        }
    }

    pub(crate) fn at(&self) -> NodeAt {
        self.at
    }

    pub(crate) fn outcome(&self) -> TraceOutcome {
        self.outcome
    }

    pub(crate) fn inline_key(&self) -> Option<&str> {
        self.inline_key.as_deref()
    }

    pub(crate) fn inline_width(&self) -> Option<usize> {
        self.inline_width
    }

    pub(crate) fn tied(&self) -> Option<Range<usize>> {
        self.tied.clone()
    }

    pub(crate) fn edge_taken(&self) -> Option<usize> {
        self.edge_taken
    }

    /// The key slot the lookup ended on here, if it found the key here.
    pub(crate) fn found(&self) -> Option<usize> {
        match self.outcome {
            TraceOutcome::Found { index } => Some(index),
            _ => None,
        }
    }
}

impl Trace {
    /// The pace of a lookup's way down, which the stylesheet's animations
    /// follow: each level starts this long after the one above, in the
    /// path's cards and in the key above them.
    pub(crate) const LEVEL_MS: u32 = 260;

    pub(crate) fn new(
        query: KeyBytes,
        steps: Vec<TraceStep>,
        placed_by_shared_prefix: Option<Ordering>,
    ) -> Self {
        Self {
            query,
            steps,
            placed_by_shared_prefix,
        }
    }

    pub(crate) fn query(&self) -> &str {
        self.query.text()
    }

    /// The key looked up, with its bytes.
    pub(crate) fn query_bytes(&self) -> &KeyBytes {
        &self.query
    }

    pub(crate) fn steps(&self) -> &[TraceStep] {
        &self.steps
    }

    pub(crate) fn step_at(&self, at: NodeAt) -> Option<&TraceStep> {
        self.steps.iter().find(|step| step.at == at)
    }

    /// Where the lookup ended, in a few words.
    pub(crate) fn outcome_brief(&self, shape: &TreeShape) -> String {
        if self.placed_by_shared_prefix.is_some() {
            return "not in the map".to_string();
        }
        let Some(last) = self.steps.last() else {
            return "the map is empty".to_string();
        };
        let place = shape.node_name(last.at);
        match last.outcome {
            TraceOutcome::Found { .. } => format!("found in {place}"),
            TraceOutcome::Absent { .. } => "not in the map".to_string(),
            TraceOutcome::Head => "found as a separator".to_string(),
            TraceOutcome::Descend { .. } | TraceOutcome::ToHead { .. } => {
                format!("stopped at {place}")
            }
        }
    }

    /// Where the lookup ended, in a sentence.
    pub(crate) fn outcome_text(&self, shape: &TreeShape) -> String {
        if let Some(order) = self.placed_by_shared_prefix {
            let side = if order == Ordering::Less {
                "before"
            } else {
                "after"
            };
            return format!(
                "Not in the map: it lacks the bytes every key shares, so it sorts {side} them all, and the lookup read no node."
            );
        }
        let Some(last) = self.steps.last() else {
            return "The map is empty.".to_string();
        };
        let place = shape.node_name(last.at);
        match last.outcome {
            TraceOutcome::Found { index } => format!("Found in {place}, slot {index}."),
            TraceOutcome::Absent { insert_at } => {
                format!("Not in the map: it would go into {place} at slot {insert_at}.")
            }
            TraceOutcome::Head => {
                let separator = self
                    .steps
                    .iter()
                    .find(|step| step.found().is_some())
                    .map(|step| shape.node_name(step.at))
                    .unwrap_or_default();
                format!("Found as a separator in {separator}; its value is the head of {place}.")
            }
            TraceOutcome::Descend { .. } | TraceOutcome::ToHead { .. } => {
                format!("Stopped at {place}.")
            }
        }
    }
}

/// What the panels use to start a lookup and say what to inspect: owned by
/// the app, handed to each panel.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Tracer {
    trace: Signal<Option<Trace>>,
    inspected: Signal<Option<NodeAt>>,
    error: Signal<Option<String>>,
    seed: Signal<u64>,
    /// Counts lookups and replays, so animations of the way down restart.
    replay: Signal<u64>,
}

impl Tracer {
    pub(crate) fn new(
        trace: Signal<Option<Trace>>,
        inspected: Signal<Option<NodeAt>>,
        error: Signal<Option<String>>,
        seed: Signal<u64>,
        replay: Signal<u64>,
    ) -> Self {
        Self {
            trace,
            inspected,
            error,
            seed,
            replay,
        }
    }

    pub(crate) fn trace(&self) -> Option<Trace> {
        self.trace.read().clone()
    }

    pub(crate) fn inspected(&self) -> Option<NodeAt> {
        *self.inspected.read()
    }

    pub(crate) fn error(&self) -> Option<String> {
        self.error.read().clone()
    }

    pub(crate) fn replay_count(&self) -> u64 {
        *self.replay.read()
    }

    /// Looks up the key `query` names, and inspects the root, where the
    /// lookup starts; says whether `query` named a key.
    pub(crate) fn run(&mut self, tree: &LoadedTree, query: &str) -> bool {
        match tree.trace(query) {
            Ok(trace) => {
                self.inspected.set(trace.steps().first().map(TraceStep::at));
                self.trace.set(Some(trace));
                self.error.set(None);
                self.replay();
                true
            }
            Err(error) => {
                self.error.set(Some(error));
                false
            }
        }
    }

    /// Looks up the key `query` names, picked in the node at `at`, which
    /// holds it, and inspects that node.
    pub(crate) fn run_in(&mut self, tree: &LoadedTree, query: &str, at: NodeAt) {
        if self.run(tree, query) {
            self.inspect(at);
        }
    }

    /// Looks up the key a link names and inspects the node it names, or,
    /// with no link or a key it can't read, a key at random.
    pub(crate) fn start(&mut self, tree: &LoadedTree, linked: Option<&LinkedLookup>) {
        if let Some(linked) = linked
            && self.run(tree, linked.key())
        {
            if let Some(level) = linked.level() {
                self.inspect_level(level);
            }
            return;
        }
        self.run_scenario(tree, Scenario::Random);
    }

    /// Looks up a key that shows `scenario`.
    pub(crate) fn run_scenario(&mut self, tree: &LoadedTree, scenario: Scenario) {
        let mut state = *self.seed.peek();
        // xorshift64*: enough to wander the tree.
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        self.seed.set(state);
        if let Some(key) = scenario.query(tree, state.wrapping_mul(0x2545_F491_4F6C_DD1D)) {
            self.run(tree, &key);
        }
    }

    /// Looks up the first key under the node at `at`, so the lookup goes
    /// through it, and inspects that node.
    pub(crate) fn run_into(&mut self, tree: &LoadedTree, at: NodeAt) {
        if let Some(key) = tree.first_key_under(at)
            && self.run(tree, &key)
        {
            self.inspect(at);
        }
    }

    /// The leaf `delta` places from the one the lookup ended at.
    pub(crate) fn leaf_beside(&self, tree: &LoadedTree, delta: isize) -> Option<NodeAt> {
        let last = self.trace.read().as_ref()?.steps().last()?.at;
        (last.level() == 0)
            .then(|| tree.shape().neighbor(last, delta))
            .flatten()
    }

    /// Looks up the first key of the leaf `delta` places from the one the
    /// lookup ended at.
    pub(crate) fn run_beside(&mut self, tree: &LoadedTree, delta: isize) {
        if let Some(leaf) = self.leaf_beside(tree, delta) {
            self.run_into(tree, leaf);
        }
    }

    pub(crate) fn inspect(&mut self, at: NodeAt) {
        self.inspected.set(Some(at));
    }

    /// Inspects the lookup's node at `level`, if it reached that level.
    fn inspect_level(&mut self, level: usize) {
        let at = self
            .trace
            .peek()
            .as_ref()
            .and_then(|trace| trace.steps().iter().find(|step| step.at.level() == level))
            .map(TraceStep::at);
        if at.is_some() {
            self.inspected.set(at);
        }
    }

    /// Inspects the node `delta` steps down the path from the inspected
    /// one, up if negative.
    pub(crate) fn inspect_along(&mut self, delta: isize) {
        let next = {
            let trace = self.trace.peek();
            let Some(trace) = trace.as_ref() else {
                return;
            };
            let inspected = *self.inspected.peek();
            let place = trace
                .steps()
                .iter()
                .position(|step| Some(step.at) == inspected)
                .unwrap_or(trace.steps().len().saturating_sub(1));
            place
                .checked_add_signed(delta)
                .and_then(|place| trace.steps().get(place))
                .map(TraceStep::at)
        };
        if let Some(next) = next {
            self.inspected.set(Some(next));
        }
    }

    /// Plays the lookup's way down again.
    pub(crate) fn replay(&mut self) {
        let count = *self.replay.peek();
        self.replay.set(count + 1);
    }

    pub(crate) fn clear(&mut self) {
        self.trace.set(None);
        self.inspected.set(None);
        self.error.set(None);
    }
}

/// The toolbar's search: the key looked up and where it ended, as a
/// button that opens the panel to look up another, by typing it, at
/// random, beside the last one, or as one of the lookups worth seeing.
#[component]
pub(crate) fn LookupMenu(tree: LoadedTree, tracer: Tracer, open: Signal<bool>) -> Element {
    let mut text = use_signal(String::new);
    let trace = tracer.trace();
    // A lookup started elsewhere shows its key here.
    let query = trace.as_ref().map(|trace| trace.query().to_string());
    use_effect(use_reactive!(|query| {
        if let Some(query) = query {
            text.set(query);
        }
    }));
    let previous = tracer.leaf_beside(&tree, -1);
    let next = tracer.leaf_beside(&tree, 1);
    let kind = tree.key_kind();
    let error = tracer.error();
    let invalid = error.is_some();
    let shown_key = trace
        .as_ref()
        .map(|trace| trace.query().to_string())
        .unwrap_or_default();
    let outcome = trace
        .as_ref()
        .map(|trace| trace.outcome_brief(tree.shape()))
        .unwrap_or_default();
    let mut tracer = tracer;
    let mut open = open;
    let (submit_tree, random_tree, left_tree, right_tree, scenario_tree) = (
        tree.clone(),
        tree.clone(),
        tree.clone(),
        tree.clone(),
        tree.clone(),
    );
    rsx! {
        div { class: "menu lookup-menu",
            button {
                id: "lookup-trigger",
                class: "toolbar-pill lookup-pill",
                "aria-haspopup": "dialog",
                "aria-expanded": if open() { "true" } else { "false" },
                title: "Look up another key (/)",
                onclick: move |_| open.set(!open()),
                IconGlyph { icon: Icon::Search }
                span { class: "pill-key data", {shown_key} }
                span { class: "pill-outcome", {outcome} }
                kbd { class: "key-hint", "/" }
            }
            Popover {
                open,
                label: "Look up a key",
                trigger: "lookup-trigger",
                align_end: false,
                form {
                    class: "lookup-form",
                    role: "search",
                    onsubmit: move |evt| {
                        evt.prevent_default();
                        tracer.run(&submit_tree, &text.peek());
                        if tracer.error().is_none() {
                            open.set(false);
                        }
                    },
                    label { class: "sr-only", r#for: "lookup", "Key" }
                    input {
                        id: "lookup",
                        class: if invalid { "lookup-input data invalid" } else { "lookup-input data" },
                        placeholder: kind.example(),
                        spellcheck: false,
                        autocomplete: "off",
                        "aria-invalid": if invalid { "true" } else { "false" },
                        "aria-describedby": "lookup-error",
                        value: "{text}",
                        onmounted: move |evt| async move {
                            let _ = evt.data().set_focus(true).await;
                        },
                        oninput: move |evt| text.set(evt.value()),
                    }
                    button { class: "button-primary", r#type: "submit", "Look up" }
                }
                p { id: "lookup-error", class: "lookup-error", role: "alert",
                    if let Some(error) = error {
                        "{error}. Try {kind.example()}."
                    }
                }
                div { class: "lookup-actions",
                    button {
                        class: "button",
                        onclick: move |_| {
                            tracer.run_scenario(&random_tree, Scenario::Random);
                            open.set(false);
                        },
                        IconGlyph { icon: Icon::Shuffle }
                        "Random key"
                        kbd { class: "key-hint inline", "R" }
                    }
                    div { class: "button-pair", role: "group", "aria-label": "Neighboring leaves",
                        button {
                            class: "button",
                            disabled: previous.is_none(),
                            "aria-label": "First key of the previous leaf",
                            onclick: move |_| {
                                tracer.run_beside(&left_tree, -1);
                                open.set(false);
                            },
                            IconGlyph { icon: Icon::Left }
                            "Leaf"
                        }
                        button {
                            class: "button",
                            disabled: next.is_none(),
                            "aria-label": "First key of the next leaf",
                            onclick: move |_| {
                                tracer.run_beside(&right_tree, 1);
                                open.set(false);
                            },
                            "Leaf"
                            IconGlyph { icon: Icon::Right }
                        }
                    }
                }
                p { class: "popover-heading", "Worth seeing" }
                ul { class: "scenario-list",
                    for scenario in Scenario::offered(kind).iter().copied() {
                        {
                            let tree = scenario_tree.clone();
                            rsx! {
                                li {
                                    button {
                                        class: "scenario-row",
                                        onclick: move |_| {
                                            tracer.run_scenario(&tree, scenario);
                                            open.set(false);
                                        },
                                        span { class: "scenario-name", {scenario.title()} }
                                        span { class: "scenario-shows", {scenario.shows()} }
                                    }
                                }
                            }
                        }
                    }
                }
                p { class: "popover-footnote",
                    kbd { "/" }
                    " search · "
                    kbd { "R" }
                    " random · "
                    kbd { "[" }
                    kbd { "]" }
                    " leaves · "
                    kbd { "↑" }
                    kbd { "↓" }
                    " path · "
                    kbd { "←" }
                    kbd { "→" }
                    " back, forward"
                }
            }
        }
    }
}
