//! A lookup, step by step, as bbtree's own search takes it
//! (`TreeView::trace`), kept as plain data for the panels; what starts one;
//! and the bar to start one from.

use std::ops::Range;

use bbtree::inspect::TraceOutcome;
use dioxus::prelude::*;

use crate::inspected_tree::LoadedTree;
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
    /// The keys whose inline keys tie with it: the only ones compared in
    /// full.
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

    /// Where the lookup ended, in a sentence.
    pub(crate) fn outcome_text(&self, shape: &TreeShape) -> String {
        let Some(last) = self.steps.last() else {
            return "The map is empty".to_string();
        };
        let levels = self.steps.len();
        let place = shape.node_name(last.at);
        match last.outcome {
            TraceOutcome::Found { index } => {
                format!("Found in {place}, slot {index} · {levels} nodes read")
            }
            TraceOutcome::Absent { insert_at } => format!(
                "Not in the map: it would go into {place} at slot {insert_at} · {levels} nodes read"
            ),
            TraceOutcome::Head => {
                let separator = self
                    .steps
                    .iter()
                    .find(|step| step.found().is_some())
                    .map(|step| shape.node_name(step.at))
                    .unwrap_or_default();
                format!("Found as a separator in {separator}; its value is the head of {place}")
            }
            TraceOutcome::Descend { .. } | TraceOutcome::ToHead { .. } => {
                format!("Stopped at {place}")
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
}

impl Tracer {
    pub(crate) fn new(
        trace: Signal<Option<Trace>>,
        inspected: Signal<Option<NodeAt>>,
        error: Signal<Option<String>>,
        seed: Signal<u64>,
    ) -> Self {
        Self {
            trace,
            inspected,
            error,
            seed,
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

    /// Looks up the key `query` names, and inspects the node it ends at.
    pub(crate) fn run(&mut self, tree: &LoadedTree, query: &str) {
        match tree.trace(query) {
            Ok(trace) => {
                self.inspected.set(trace.steps().last().map(TraceStep::at));
                self.trace.set(Some(trace));
                self.error.set(None);
            }
            Err(error) => self.error.set(Some(error)),
        }
    }

    /// Looks up a key of the map picked at random.
    pub(crate) fn run_random(&mut self, tree: &LoadedTree) {
        let mut state = *self.seed.peek();
        // xorshift64*: enough to wander the tree.
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        self.seed.set(state);
        if let Some(key) = tree.random_key(state.wrapping_mul(0x2545_F491_4F6C_DD1D)) {
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

    pub(crate) fn inspect(&mut self, at: NodeAt) {
        self.inspected.set(Some(at));
    }

    pub(crate) fn clear(&mut self) {
        self.trace.set(None);
        self.inspected.set(None);
        self.error.set(None);
    }
}

/// The bar to look a key up from, with where the last lookup ended.
#[component]
pub(crate) fn TraceBar(tree: LoadedTree, tracer: Tracer) -> Element {
    let mut text = use_signal(String::new);
    let trace = tracer.trace();
    // A lookup started elsewhere shows its key here.
    let query = trace.as_ref().map(|trace| trace.query().to_string());
    use_effect(use_reactive!(|query| {
        if let Some(query) = query {
            text.set(query);
        }
    }));
    let shape = tree.shape();
    let last_leaf = trace
        .as_ref()
        .and_then(|trace| trace.steps().last())
        .map(TraceStep::at)
        .filter(|at| at.level() == 0);
    let previous = last_leaf.and_then(|at| shape.neighbor(at, -1));
    let next = last_leaf.and_then(|at| shape.neighbor(at, 1));
    let placeholder = tree.key_kind().example();
    let outcome = trace.as_ref().map(|trace| trace.outcome_text(shape));
    let (tree_a, tree_b, tree_c, tree_d) = (tree.clone(), tree.clone(), tree.clone(), tree.clone());
    let mut tracer = tracer;

    rsx! {
        div { class: "flex flex-wrap items-center gap-x-3 gap-y-2 border-b border-[var(--rule)] bg-[var(--panel)] px-5 py-2.5",
            form {
                class: "flex items-center gap-2",
                onsubmit: move |evt| {
                    evt.prevent_default();
                    tracer.run(&tree_a, &text.peek());
                },
                label { class: "eyebrow", r#for: "lookup", "Look up" }
                input {
                    id: "lookup",
                    class: "data w-[22rem] rounded-[5px] border border-[var(--rule-strong)] bg-[var(--paper)] px-2.5 py-1.5 text-[13px] outline-none focus:border-[var(--ink)]",
                    placeholder,
                    spellcheck: false,
                    autocomplete: "off",
                    value: "{text}",
                    oninput: move |evt| text.set(evt.value()),
                }
                button { class: "button-primary", r#type: "submit", "Trace" }
            }
            button {
                class: "button",
                onclick: move |_| tracer.run_random(&tree_b),
                "Random key"
            }
            div { class: "flex items-center gap-1",
                button {
                    class: "button",
                    disabled: previous.is_none(),
                    title: "Look up the first key of the leaf to the left",
                    onclick: move |_| {
                        if let Some(at) = previous {
                            tracer.run_into(&tree_c, at);
                        }
                    },
                    "← Previous leaf"
                }
                button {
                    class: "button",
                    disabled: next.is_none(),
                    title: "Look up the first key of the leaf to the right",
                    onclick: move |_| {
                        if let Some(at) = next {
                            tracer.run_into(&tree_d, at);
                        }
                    },
                    "Next leaf →"
                }
            }
            div { class: "ml-auto text-[13px]",
                if let Some(error) = tracer.error() {
                    span { class: "text-[#b42318]", {error} }
                } else if let Some(outcome) = outcome {
                    span { class: "text-[var(--graphite)]", {outcome} }
                }
            }
        }
    }
}
