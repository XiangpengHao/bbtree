//! What the reader points at, shared by every panel: a part of one node, or
//! one of bbtree's ideas, so that each panel showing it lights it up; and
//! the hint by the pointer that says what it is.

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
}

impl Spotlight {
    pub(crate) fn new(
        part: Signal<Option<(NodeAt, NodePart)>>,
        concept: Signal<Option<Concept>>,
        hint: Signal<Option<Hint>>,
        hint_shown: Signal<bool>,
    ) -> Self {
        Self {
            part,
            concept,
            hint,
            hint_shown,
        }
    }

    /// The part of the node at `at` the reader points at, if any.
    pub(crate) fn part_of(&self, at: NodeAt) -> Option<NodePart> {
        self.part
            .read()
            .filter(|(pointed, _)| *pointed == at)
            .map(|(_, part)| part)
    }

    /// The part of the node at `at` the reader points at, read without
    /// following it: for event handlers.
    pub(crate) fn peek_part_of(&self, at: NodeAt) -> Option<NodePart> {
        self.part
            .peek()
            .filter(|(pointed, _)| *pointed == at)
            .map(|(_, part)| part)
    }

    pub(crate) fn concept(&self) -> Option<Concept> {
        *self.concept.read()
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
