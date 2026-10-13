//! A lookup, step by step, as bbtree's own search takes it
//! (`TreeView::trace`), kept as plain data for the panels; what starts one;
//! and the bar to start one from.

use std::ops::Range;

use bbtree::inspect::TraceOutcome;
use dioxus::prelude::*;

use crate::icons::{Icon, IconGlyph};
use crate::inspected_tree::LoadedTree;
use crate::memory_map::PageMarks;
use crate::scenarios::Scenario;
use crate::spotlight::{Hint, Spotlight};
use crate::tree_shape::{NodeAt, TreeShape};

/// One lookup, from the root down.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct Trace {
    query: String,
    steps: Vec<TraceStep>,
}

/// What a lookup did at one node.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct TraceStep {
    at: NodeAt,
    outcome: TraceOutcome,
    /// The searched key's inline key at the node's prefix, in hex, as many
    /// bytes as the node keeps.
    inline_key: Option<String>,
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
        tied: Option<Range<usize>>,
        edge_taken: Option<usize>,
    ) -> Self {
        Self {
            at,
            outcome,
            inline_key,
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
    pub(crate) fn new(query: String, steps: Vec<TraceStep>) -> Self {
        Self { query, steps }
    }

    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    pub(crate) fn steps(&self) -> &[TraceStep] {
        &self.steps
    }

    pub(crate) fn step_at(&self, at: NodeAt) -> Option<&TraceStep> {
        self.steps.iter().find(|step| step.at == at)
    }

    /// The cache lines the lookup read over all its nodes: surely, and how
    /// many more it may have read in searches among tied keys.
    pub(crate) fn lines_read(&self, tree: &LoadedTree) -> (usize, usize) {
        self.steps.iter().fold((0, 0), |(certain, possible), step| {
            let marks = PageMarks::of_step(step, step.at.level() == 0);
            let (node_certain, node_possible) = marks.lines_read(&tree.page_map(step.at));
            (certain + node_certain, possible + node_possible)
        })
    }

    /// Where the lookup ended, in a sentence.
    pub(crate) fn outcome_text(&self, shape: &TreeShape) -> String {
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

    /// Looks up the key `query` names, and inspects the node it ends at.
    pub(crate) fn run(&mut self, tree: &LoadedTree, query: &str) {
        match tree.trace(query) {
            Ok(trace) => {
                self.inspected.set(trace.steps().last().map(TraceStep::at));
                self.trace.set(Some(trace));
                self.error.set(None);
                self.replay();
            }
            Err(error) => self.error.set(Some(error)),
        }
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
    /// through it.
    pub(crate) fn run_into(&mut self, tree: &LoadedTree, at: NodeAt) {
        if let Some(key) = tree.first_key_under(at) {
            self.run(tree, &key);
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

/// The bar to look a key up from: by typing it, at random, beside the last
/// one, or as one of the scenarios worth seeing.
#[component]
pub(crate) fn LookupBar(tree: LoadedTree, tracer: Tracer, spotlight: Spotlight) -> Element {
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
    let mut tracer = tracer;
    let mut spotlight = spotlight;
    let (submit_tree, random_tree, left_tree, right_tree, scenario_tree) = (
        tree.clone(),
        tree.clone(),
        tree.clone(),
        tree.clone(),
        tree.clone(),
    );
    rsx! {
        div { class: "lookup-bar",
            form {
                class: "lookup-form",
                role: "search",
                onsubmit: move |evt| {
                    evt.prevent_default();
                    tracer.run(&submit_tree, &text.peek());
                },
                label { class: "lookup-label", r#for: "lookup", "Look up" }
                div { class: "lookup-field",
                    input {
                        id: "lookup",
                        class: if invalid { "lookup-input data invalid" } else { "lookup-input data" },
                        placeholder: kind.example(),
                        spellcheck: false,
                        autocomplete: "off",
                        "aria-invalid": if invalid { "true" } else { "false" },
                        "aria-describedby": "lookup-error",
                        value: "{text}",
                        oninput: move |evt| text.set(evt.value()),
                    }
                    kbd { class: "key-hint", "aria-hidden": "true", "/" }
                }
                button { class: "button-primary", r#type: "submit", "Look up" }
            }
            div { class: "lookup-actions",
                button {
                    class: "button",
                    title: "Look up a key of the map at random (R)",
                    onclick: move |_| tracer.run_scenario(&random_tree, Scenario::Random),
                    IconGlyph { icon: Icon::Shuffle }
                    "Random"
                }
                div { class: "button-pair", role: "group", "aria-label": "Neighboring leaves",
                    button {
                        class: "button",
                        disabled: previous.is_none(),
                        title: "Look up the first key of the leaf to the left ([)",
                        "aria-label": "Previous leaf",
                        onclick: move |_| tracer.run_beside(&left_tree, -1),
                        IconGlyph { icon: Icon::Left }
                        "Leaf"
                    }
                    button {
                        class: "button",
                        disabled: next.is_none(),
                        title: "Look up the first key of the leaf to the right (])",
                        "aria-label": "Next leaf",
                        onclick: move |_| tracer.run_beside(&right_tree, 1),
                        "Leaf"
                        IconGlyph { icon: Icon::Right }
                    }
                }
            }
            div { class: "scenarios", role: "group", "aria-label": "Lookups worth seeing",
                span { class: "scenarios-label", "Try" }
                for scenario in Scenario::offered(kind).iter().copied() {
                    {
                        let tree = scenario_tree.clone();
                        rsx! {
                            button {
                                class: "scenario",
                                onclick: move |_| tracer.run_scenario(&tree, scenario),
                                onmouseenter: move |evt| {
                                    let point = evt.client_coordinates();
                                    spotlight.show_hint(Hint::new((point.x, point.y), scenario.label(), scenario.shows()));
                                },
                                onmouseleave: move |_| spotlight.clear(),
                                {scenario.label()}
                            }
                        }
                    }
                }
            }
            p { id: "lookup-error", class: "lookup-error", role: "alert",
                if let Some(error) = error {
                    "{error}. Try {kind.example()}."
                }
            }
        }
    }
}
