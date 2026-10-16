//! What the reader points at, shared by every panel: a part of one node, or
//! one of bbtree's ideas, so that each panel showing it lights it up; and
//! the hint by the pointer that says what it is. While the reader points at
//! nothing, the panels light what the way down's current search step does,
//! as pointing at that step would (`descent.rs`).

use dioxus::prelude::*;

use crate::concepts::Concept;
use crate::tree_shape::NodeAt;

/// A part of a node that more than one panel draws.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum NodePart {
    /// Key `index` with all that is kept for it: its inline key, length tag
    /// and, in a leaf, its value.
    Entry(usize),
    /// Child `edge` of an internal node.
    Child(usize),
    /// A leaf's head.
    Head,
}

/// What the panels light up: a part of one node, or an idea.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Light {
    Part(NodeAt, NodePart),
    Concept(Concept),
}

/// What a hint says, and the point it hangs from.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct Hint {
    anchor: (f64, f64),
    title: String,
    body: String,
    decision: Option<&'static str>,
}

impl Hint {
    pub(crate) fn new(
        anchor: (f64, f64),
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        Self {
            anchor,
            title: title.into(),
            body: body.into(),
            decision: None,
        }
    }

    fn of_concept(concept: Concept, anchor: (f64, f64)) -> Self {
        Self {
            anchor,
            title: concept.title().to_string(),
            body: concept.definition().to_string(),
            decision: concept.decision(),
        }
    }
}

/// What the reader points at: owned by the app, handed to each panel.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Spotlight {
    part: Signal<Option<(NodeAt, NodePart)>>,
    concept: Signal<Option<Concept>>,
    /// The hint last shown, kept once it hides so that it fades out where
    /// it stood, still saying what it said.
    hint: Signal<Option<Hint>>,
    hint_shown: Signal<bool>,
    /// What the way down's current search step lights.
    current_step: Signal<Option<Light>>,
}

impl Spotlight {
    pub(crate) fn new(
        part: Signal<Option<(NodeAt, NodePart)>>,
        concept: Signal<Option<Concept>>,
        hint: Signal<Option<Hint>>,
        hint_shown: Signal<bool>,
        current_step: Signal<Option<Light>>,
    ) -> Self {
        Self {
            part,
            concept,
            hint,
            hint_shown,
            current_step,
        }
    }

    /// What the panels light: what the reader points at, or, pointing at
    /// nothing, what the way down's current step lights.
    fn lit(&self) -> Option<Light> {
        let pointed = match (*self.part.read(), *self.concept.read()) {
            (Some((at, part)), _) => Some(Light::Part(at, part)),
            (None, Some(concept)) => Some(Light::Concept(concept)),
            (None, None) => None,
        };
        pointed.or(*self.current_step.read())
    }

    /// The part of the node at `at` lit, if any.
    pub(crate) fn part_of(&self, at: NodeAt) -> Option<NodePart> {
        match self.lit() {
            Some(Light::Part(lit_at, part)) if lit_at == at => Some(part),
            _ => None,
        }
    }

    /// The part of the node at `at` the reader points at, read without
    /// following it: for event handlers.
    pub(crate) fn peek_part_of(&self, at: NodeAt) -> Option<NodePart> {
        self.part
            .peek()
            .filter(|(pointed, _)| *pointed == at)
            .map(|(_, part)| part)
    }

    /// The idea lit, if any.
    pub(crate) fn concept(&self) -> Option<Concept> {
        match self.lit() {
            Some(Light::Concept(concept)) => Some(concept),
            _ => None,
        }
    }

    /// Lights what a step of the search names, without a hint, as the
    /// reader points at the step; or nothing.
    pub(crate) fn light(&mut self, light: Option<Light>) {
        match light {
            Some(Light::Part(at, part)) => self.light_part(at, part),
            Some(Light::Concept(concept)) => self.light_concept(Some(concept)),
            None => self.clear(),
        }
    }

    /// Lights what the way down's current step names, while the reader
    /// points at nothing.
    pub(crate) fn light_current_step(&mut self, light: Option<Light>) {
        if *self.current_step.peek() != light {
            self.current_step.set(light);
        }
    }

    /// Points at `part` of the node at `at`, with what to say about it.
    pub(crate) fn point_at_part(&mut self, at: NodeAt, part: NodePart, hint: Option<Hint>) {
        if *self.part.peek() != Some((at, part)) {
            self.part.set(Some((at, part)));
        }
        if self.concept.peek().is_some() {
            self.concept.set(None);
        }
        match hint {
            Some(hint) => self.raise_hint(hint),
            None => self.lower_hint(),
        }
    }

    /// Points at an idea from `anchor`, and says what it is.
    pub(crate) fn point_at_concept(&mut self, concept: Concept, anchor: (f64, f64)) {
        if *self.concept.peek() != Some(concept) {
            self.concept.set(Some(concept));
        }
        if self.part.peek().is_some() {
            self.part.set(None);
        }
        self.raise_hint(Hint::of_concept(concept, anchor));
    }

    /// Lights up an idea without a hint: a step of the search that names
    /// it in its own words.
    pub(crate) fn light_concept(&mut self, concept: Option<Concept>) {
        if *self.concept.peek() != concept {
            self.concept.set(concept);
        }
        if self.part.peek().is_some() {
            self.part.set(None);
        }
        self.lower_hint();
    }

    /// Says what something is that no other panel draws.
    pub(crate) fn show_hint(&mut self, hint: Hint) {
        if self.part.peek().is_some() {
            self.part.set(None);
        }
        if self.concept.peek().is_some() {
            self.concept.set(None);
        }
        self.raise_hint(hint);
    }

    /// Lights up a part without a hint.
    pub(crate) fn light_part(&mut self, at: NodeAt, part: NodePart) {
        self.point_at_part(at, part, None);
    }

    /// Moves the hint with the pointer.
    pub(crate) fn move_hint(&mut self, anchor: (f64, f64)) {
        if !*self.hint_shown.peek() {
            return;
        }
        if let Some(hint) = self.hint.write().as_mut() {
            hint.anchor = anchor;
        }
    }

    pub(crate) fn clear(&mut self) {
        if self.part.peek().is_some() {
            self.part.set(None);
        }
        if self.concept.peek().is_some() {
            self.concept.set(None);
        }
        self.lower_hint();
    }

    fn raise_hint(&mut self, hint: Hint) {
        self.hint.set(Some(hint));
        if !*self.hint_shown.peek() {
            self.hint_shown.set(true);
        }
    }

    /// Hides the hint, keeping what it says and where it stands: it fades
    /// out there rather than empty, from wherever no hint puts it.
    fn lower_hint(&mut self) {
        if *self.hint_shown.peek() {
            self.hint_shown.set(false);
        }
    }
}

/// The hint, by the pointer, above it unless that leaves the window.
#[component]
pub(crate) fn HintLayer(spotlight: Spotlight) -> Element {
    let hint = spotlight.hint.read().clone();
    let shown = *spotlight.hint_shown.read();
    let (x, y) = hint.as_ref().map_or((0.0, 0.0), |hint| hint.anchor);
    let side = if y < 220.0 { "below" } else { "above" };
    let shown_class = if shown { "shown" } else { "" };
    rsx! {
        div {
            id: "hint",
            role: "tooltip",
            "aria-hidden": if shown { "false" } else { "true" },
            class: "hint {shown_class} {side}",
            style: "left:clamp(168px, {x}px, calc(100vw - 168px));top:{y}px;",
            if let Some(hint) = hint {
                p { class: "hint-title", {hint.title} }
                p { class: "hint-body", {hint.body} }
                if let Some(decision) = hint.decision {
                    p { class: "hint-source", "bbtree design note {decision}" }
                }
            }
        }
    }
}
