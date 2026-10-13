//! A guided reading of the page for someone who knows B-trees but not this
//! page: a panel under the path that goes through what each part shows,
//! one step at a time, with numbered marks pinned to the very parts it
//! names. Its words come from the lookup on screen, so it reads true for
//! any key.

use bbtree::inspect::{SlotContents, TraceOutcome};
use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;

use crate::count_text::Group;
use crate::icons::{Icon, IconGlyph};
use crate::inspected_tree::LoadedTree;
use crate::memory_map::PageMarks;
use crate::node_inspector::InspectorTab;
use crate::scenarios::Scenario;
use crate::trace::{Trace, TraceStep, Tracer};
use crate::tree_shape::NodeAt;

/// Where the guide is: owned by the app, handed to its panel, its marks
/// and the keys that step it.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Guide {
    step: Signal<usize>,
    open: Signal<bool>,
}

impl Guide {
    pub(crate) fn new(step: Signal<usize>, open: Signal<bool>) -> Self {
        Self { step, open }
    }

    pub(crate) fn is_open(&self) -> bool {
        *self.open.read()
    }

    /// Opens the guide at its first step.
    pub(crate) fn start(&mut self) {
        self.step.set(0);
        self.open.set(true);
    }

    pub(crate) fn close(&mut self) {
        self.open.set(false);
    }

    /// Moves `delta` steps through the guide for `trace`, if it is open.
    pub(crate) fn go(&mut self, delta: isize, trace: &Trace) {
        if !*self.open.peek() {
            return;
        }
        let last = GuideStep::all(trace.steps().len()).len() - 1;
        let step = self.step.peek().saturating_add_signed(delta).min(last);
        self.step.set(step);
    }
}

/// One step of the guide.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GuideStep {
    StartHere,
    ThePath,
    ReadingACard,
    /// What the lookup did at the node in row `row` of the path.
    Level(usize),
    ThePage,
    YourTurn,
}

/// A part of the page a step names, by a CSS selector, and what it says
/// about it.
#[derive(Clone, PartialEq, Debug)]
struct Pin {
    selector: String,
    says: String,
    anchor: Anchor,
}

/// Where along the top of its part a pin's number sits, so numbers on one
/// card don't stack.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Anchor {
    Start,
    Center,
    End,
}

impl Anchor {
    /// The badge's left edge over a part from `x`, `width` wide.
    fn badge_x(self, x: f64, width: f64) -> f64 {
        match self {
            Anchor::Start => x - 4.0,
            Anchor::Center => x + width / 2.0 - 9.0,
            Anchor::End => x + width - 14.0,
        }
    }
}

/// What a step shows: its words, the parts it pins, the part it rings, and
/// what the inspector should show meanwhile.
#[derive(Clone, PartialEq, Debug)]
struct GuidePage {
    title: String,
    lead: String,
    pins: Vec<Pin>,
    ring: Option<String>,
    inspect: Option<NodeAt>,
    tab: Option<InspectorTab>,
}

impl Pin {
    fn new(selector: impl Into<String>, says: impl Into<String>) -> Self {
        Self::at(selector, says, Anchor::Center)
    }

    fn at(selector: impl Into<String>, says: impl Into<String>, anchor: Anchor) -> Self {
        Self {
            selector: selector.into(),
            says: says.into(),
            anchor,
        }
    }
}

impl GuideStep {
    fn all(levels: usize) -> Vec<Self> {
        let mut steps = vec![
            GuideStep::StartHere,
            GuideStep::ThePath,
            GuideStep::ReadingACard,
        ];
        steps.extend((0..levels).map(GuideStep::Level));
        steps.extend([GuideStep::ThePage, GuideStep::YourTurn]);
        steps
    }

    /// The selector of the card in row `row` of the path.
    fn card(row: usize) -> String {
        format!(".path-card[data-row=\"{row}\"]")
    }

    fn page(self, tree: &LoadedTree, trace: &Trace) -> GuidePage {
        let shape = tree.shape();
        let steps = trace.steps();
        let leaf_row = steps.len().saturating_sub(1);
        let leaf = steps.last();
        match self {
            GuideStep::StartHere => GuidePage {
                title: "Start here".to_string(),
                lead: format!(
                    "This page follows one lookup in a bbtree of {} {} keys: the key {}. As in any B+-tree, a lookup reads one node per level, from the root down to a leaf, here {} nodes. This guide goes through how each part is drawn; Next, or the → key, moves on.",
                    Group(shape.entries()),
                    tree.key_kind().label(),
                    trace.query(),
                    steps.len()
                ),
                pins: vec![
                    Pin::new(".summary-key", "The key looked up, and where the lookup ended."),
                    Pin::new(
                        ".summary-cost",
                        "What it cost: the nodes it read, and the 64-byte cache lines it read of them. bbtree is built to keep that number small.",
                    ),
                    Pin::new("#lookup-trigger", "Look up another key here, or press /."),
                    Pin::new("#workload-trigger", "The tree on show: change its keys, its size, or how it was built."),
                ],
                ring: Some(".path-summary".to_string()),
                inspect: None,
                tab: None,
            },
            GuideStep::ThePath => GuidePage {
                title: "The path".to_string(),
                lead: "The left side draws the lookup's way down the tree, one card per node.".to_string(),
                pins: vec![
                    Pin::new(
                        Self::card(0),
                        "A card is a node the lookup read: the root on top, the leaf at the bottom. Click a card to see it in full on the right.",
                    ),
                    Pin::new(
                        ".level-label",
                        "The node's level, and which of that level's nodes it is, counting from the left.",
                    ),
                    Pin::new(
                        ".edge-taken",
                        "The black line runs from where the lookup left a node to the node it read next.",
                    ),
                    Pin::new(
                        ".gauge",
                        "Grey bars are the other children of the node above, the ones the lookup skipped, each filled as full as it is. Click one to look up its first key.",
                    ),
                ],
                ring: Some(".path-tree".to_string()),
                inspect: None,
                tab: None,
            },
            GuideStep::ReadingACard => {
                let card = Self::card(leaf_row);
                let mut pins = Vec::new();
                if let Some(step) = leaf {
                    let level = shape.level(step.at().level());
                    let (len, capacity) = (level.len(step.at().index()), level.capacity());
                    let prefix = level.prefix_len(step.at().index());
                    let (lines, total) = Self::lines_read(tree, step);
                    pins.push(Pin::at(
                        format!("{card} .card-count"),
                        format!("{len}/{capacity} keys: the node holds {len} keys and has room for {capacity}; the bar beside shows how full it is."),
                        Anchor::Start,
                    ));
                    if prefix > 0 {
                        pins.push(Pin::new(
                            format!("{card} .card-prefix"),
                            format!("skips {prefix} B: every key in this node starts with the same {prefix} bytes, so its search starts after them."),
                        ));
                    }
                    pins.push(Pin::new(
                        format!("{card} .card-lines"),
                        format!("reads {lines}: how many of the node's {total} cache lines the lookup read."),
                    ));
                    pins.push(Pin::at(
                        format!("{card} .ruler"),
                        "The ruler: one tick per slot. Dark ticks hold keys, pale ones are empty room. Point at a tick to see its key, click it to look the key up.",
                        Anchor::End,
                    ));
                    match step.outcome() {
                        TraceOutcome::Found { .. } => pins.push(Pin::new(
                            format!("{card} .slot-found"),
                            "Yellow fill: the slot where the lookup found its key.",
                        )),
                        TraceOutcome::Absent { .. } => pins.push(Pin::new(
                            format!("{card} .insert-caret"),
                            "Red caret: where the key would go if it were inserted.",
                        )),
                        _ => {}
                    }
                    pins.push(Pin::at(
                        format!("{card} .card-first"),
                        "The node's first and last keys: every key in it lies between them.",
                        Anchor::Start,
                    ));
                    pins.push(Pin::new(
                        format!("{card} .card-note"),
                        "What the lookup did here, in short.",
                    ));
                }
                GuidePage {
                    title: "Reading a card".to_string(),
                    lead: "Take the leaf, the last node the lookup read.".to_string(),
                    pins,
                    ring: Some(card),
                    inspect: leaf.map(TraceStep::at),
                    tab: None,
                }
            }
            GuideStep::Level(row) => {
                let step = &steps[row.min(leaf_row)];
                let card = Self::card(row);
                let mut pins = Vec::new();
                if step.tied().is_some_and(|tied| !tied.is_empty()) && step.found().is_none() {
                    pins.push(Pin::new(
                        format!("{card} .slot-tied"),
                        "Yellow outlines: the keys whose inline key equals the search key's. Only these can match, so only these are compared in full.",
                    ));
                }
                match step.outcome() {
                    TraceOutcome::Found { .. } => pins.push(Pin::new(
                        format!("{card} .slot-found"),
                        "Yellow fill: the key itself.",
                    )),
                    TraceOutcome::Absent { .. } => pins.push(Pin::new(
                        format!("{card} .insert-caret"),
                        "Red caret: where the key would go.",
                    )),
                    TraceOutcome::Descend { .. } | TraceOutcome::ToHead { .. } => pins.push(Pin::new(
                        format!("{card} .edge-notch"),
                        "Black notch: the child the lookup went down to.",
                    )),
                    TraceOutcome::Head => {}
                }
                pins.push(Pin::new(
                    ".search-steps",
                    "The same search, step by step, on the right. Point at a step to light up what it read.",
                ));
                GuidePage {
                    title: format!(
                        "Step {} of {}: {}",
                        row + 1,
                        steps.len(),
                        shape.level_name(step.at().level())
                    ),
                    lead: Self::level_story(tree, trace, row),
                    pins,
                    ring: Some(card),
                    inspect: Some(step.at()),
                    tab: None,
                }
            }
            GuideStep::ThePage => {
                let mut pins = vec![
                    Pin::new(".line-offset[data-line=\"7\"]", "Rows 0 to 15: one 64-byte cache line each."),
                    Pin::new(".piece.slot-header", "Grey: the header, with the node's length and prefix."),
                ];
                let mut size = 0;
                let mut read = String::new();
                if let Some(step) = leaf {
                    let page = tree.page_map(step.at());
                    size = page.size();
                    read = Self::lines_read(tree, step).0;
                    for array in page.arrays() {
                        let middle = array.used() / 2;
                        let pin = match array.contents() {
                            SlotContents::InlineKeys => Some((
                                "slot-inline",
                                middle,
                                "Orange: the inline keys, packed together, so one compare reads them all.",
                            )),
                            SlotContents::Keys => Some(("slot-keys", middle, "Blue: the full keys.")),
                            SlotContents::Values => Some((
                                "slot-trailing",
                                middle,
                                "Green: the values, each beside its key, so reading the key brings its value along.",
                            )),
                            SlotContents::Edges => {
                                Some(("slot-trailing", middle, "Green: the child pointers."))
                            }
                            _ => None,
                        };
                        if let Some((class, slot, says)) = pin {
                            pins.push(Pin::new(format!(".piece.{class}[data-slot=\"{slot}\"]"), says));
                        }
                        if array.contents() == SlotContents::Keys && array.used() < array.capacity() {
                            pins.push(Pin::new(
                                format!(".piece.slot-keys.slot-empty[data-slot=\"{}\"]", array.capacity() - 1),
                                "Pale: empty slots, room for inserts.",
                            ));
                        }
                    }
                }
                pins.push(Pin::new(
                    ".memory-map .line-read:last-of-type",
                    format!("Yellow dots: the lines the lookup read, {read}; a hollow dot, a line it may have read."),
                ));
                GuidePage {
                    title: "Reading the page".to_string(),
                    lead: format!(
                        "On the right is the inspected node's memory, its {} B page, drawn the way the processor fetches it: one row per cache line, every slot where bbtree's layout puts it.",
                        Group(size)
                    ),
                    pins,
                    ring: Some(".memory-map-figure".to_string()),
                    inspect: leaf.map(TraceStep::at),
                    tab: Some(InspectorTab::Page),
                }
            }
            GuideStep::YourTurn => GuidePage {
                title: "Your turn".to_string(),
                lead: "Point at anything to see what it is: the same slot lights up in every view. Click a card to inspect it; ↑ and ↓ move along the path. Worth seeing:".to_string(),
                pins: vec![Pin::new("#lookup-trigger", "Search for any key, or press /.")],
                ring: None,
                inspect: None,
                tab: None,
            },
        }
    }

    /// The cache lines the lookup surely read of the node of `step`, and
    /// its page's lines.
    fn lines_read(tree: &LoadedTree, step: &TraceStep) -> (String, usize) {
        let page = tree.page_map(step.at());
        let (certain, possible) =
            PageMarks::of_step(step, step.at().level() == 0).lines_read(&page);
        let total = page.size().div_ceil(64);
        let lines = match (certain, possible) {
            (1, 0) => "1 line".to_string(),
            (certain, 0) => format!("{certain} lines"),
            (certain, possible) => format!("{certain}–{} lines", certain + possible),
        };
        (lines, total)
    }

    /// What the lookup did at the node in row `row`, as a short story.
    fn level_story(tree: &LoadedTree, trace: &Trace, row: usize) -> String {
        let shape = tree.shape();
        let steps = trace.steps();
        let step = &steps[row];
        let at = step.at();
        let level = shape.level(at.level());
        let len = level.len(at.index());
        let prefix = level.prefix_len(at.index());
        let name = shape.node_name(at);
        let mut story = format!("{} holds {len} keys", Self::capitalized(&name));
        if prefix > 0 {
            story.push_str(&format!(
                ", all starting with the same {prefix} bytes, which its search skips"
            ));
        }
        story.push('.');
        if let (Some(window), Some(tied)) = (step.inline_key(), step.tied()) {
            let width = window.split_whitespace().count();
            let which = if prefix > 0 { "next" } else { "first" };
            story.push_str(&format!(
                " It compares the key's {which} {width} bytes, {window}, with all {len} inline keys at once"
            ));
            story.push_str(&match tied.len() {
                0 => ": none is equal, so the inline keys alone place the key.".to_string(),
                1 => format!(
                    ": only slot {}'s is equal, so only that key is compared in full.",
                    tied.start
                ),
                n => format!(
                    ": {n} are equal (the yellow outlines), so a binary search compares a few of those keys in full."
                ),
            });
        }
        let next = steps.get(row + 1).map(|next| shape.node_name(next.at()));
        story.push(' ');
        story.push_str(&match step.outcome() {
            TraceOutcome::Descend { edge } => format!(
                "The key belongs under child {edge}, the black notch, so the lookup goes down to {}.",
                next.unwrap_or_default()
            ),
            TraceOutcome::Found { index } if at.level() == 0 => format!(
                "Slot {index} holds the key, and its value sits right beside it: done."
            ),
            TraceOutcome::Found { index } => format!(
                "Slot {index} holds the key itself, as a separator. bbtree stores each key once, so the key's value waits in the head of the leftmost leaf to its right: the lookup goes on down child {}, the dashed line.",
                index + 1
            ),
            TraceOutcome::ToHead { .. } => format!(
                "On its way to the separator's value, the lookup just takes the first child, to {}, without searching.",
                next.unwrap_or_default()
            ),
            TraceOutcome::Head => {
                "This leaf's head holds the separator's value: done.".to_string()
            }
            TraceOutcome::Absent { insert_at } => format!(
                "No key is equal, so the map doesn't hold it; an insert would put it at slot {insert_at}, the red caret."
            ),
        });
        let (lines, total) = Self::lines_read(tree, step);
        story.push_str(&format!(" It read {lines} of the node's {total}."));
        story
    }

    fn capitalized(text: &str) -> String {
        let mut chars = text.chars();
        chars
            .next()
            .map(|first| first.to_uppercase().chain(chars).collect())
            .unwrap_or_default()
    }
}

/// The guide's panel: the step's words, its numbered parts, and the way
/// through.
#[component]
pub(crate) fn GuidePanel(
    tree: LoadedTree,
    trace: Trace,
    guide: Guide,
    tracer: Tracer,
    tab: Signal<InspectorTab>,
) -> Element {
    let steps = GuideStep::all(trace.steps().len());
    let index = (*guide.step.read()).min(steps.len() - 1);
    let step = steps[index];
    let page = step.page(&tree, &trace);
    let (inspect, wanted_tab) = (page.inspect, page.tab);
    let mut tracer = tracer;
    let mut tab = tab;
    use_effect(use_reactive!(|index, inspect, wanted_tab| {
        let _ = index;
        if let Some(at) = inspect {
            tracer.inspect(at);
        }
        if let Some(wanted) = wanted_tab {
            tab.set(wanted);
        }
    }));
    let mut guide = guide;
    let count = steps.len();
    let scenario_tree = tree.clone();
    let move_trace = trace.clone();
    let back_trace = trace.clone();
    rsx! {
        aside { class: "guide-panel", "aria-label": "Guide", "aria-live": "polite",
            header { class: "guide-head",
                span { class: "guide-count data", "Guide · {index + 1} of {count}" }
                button {
                    class: "icon-button quiet",
                    title: "Close the guide",
                    "aria-label": "Close the guide",
                    onclick: move |_| guide.close(),
                    IconGlyph { icon: Icon::Close }
                }
            }
            div { class: "guide-body", key: "{index}",
                h2 { class: "guide-title", {page.title.clone()} }
                p { class: "guide-lead", {page.lead.clone()} }
                if !page.pins.is_empty() {
                    ol { class: "guide-pins",
                        for (number, pin) in page.pins.iter().enumerate() {
                            li {
                                span { class: "guide-num", "{number + 1}" }
                                span { {pin.says.clone()} }
                            }
                        }
                    }
                }
                if step == GuideStep::YourTurn {
                    div { class: "guide-scenarios",
                        for scenario in [Scenario::Separator, Scenario::PastTheEnd, Scenario::Missing] {
                            if Scenario::offered(tree.key_kind()).contains(&scenario) {
                                {
                                    let tree = scenario_tree.clone();
                                    rsx! {
                                        button {
                                            class: "scenario-row",
                                            onclick: move |_| tracer.run_scenario(&tree, scenario),
                                            span { class: "scenario-name", {scenario.title()} }
                                            span { class: "scenario-shows", {scenario.shows()} }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            footer { class: "guide-nav",
                button {
                    class: "button",
                    disabled: index == 0,
                    onclick: move |_| guide.go(-1, &back_trace),
                    IconGlyph { icon: Icon::Left }
                    "Back"
                }
                div { class: "guide-dots", "aria-hidden": "true",
                    for dot in 0..count {
                        span { class: if dot == index { "guide-dot current" } else if dot < index { "guide-dot done" } else { "guide-dot" } }
                    }
                }
                if index + 1 < count {
                    button {
                        class: "button-primary",
                        onclick: move |_| guide.go(1, &move_trace),
                        "Next"
                        IconGlyph { icon: Icon::Right }
                    }
                } else {
                    button { class: "button-primary", onclick: move |_| guide.close(), "Done" }
                }
            }
        }
    }
}

/// The step's numbered marks and ring, pinned over the parts it names
/// wherever they are on screen; measured again whenever the page moves.
#[component]
pub(crate) fn GuideMarks(
    tree: LoadedTree,
    trace: Trace,
    guide: Guide,
    /// What moves the parts: a lookup's replay, the inspected node, the tab.
    replay: u64,
    inspected: Option<NodeAt>,
    tab: InspectorTab,
) -> Element {
    let steps = GuideStep::all(trace.steps().len());
    let index = (*guide.step.read()).min(steps.len() - 1);
    let page = steps[index].page(&tree, &trace);
    let mut selectors: Vec<String> = page.ring.iter().cloned().collect();
    let has_ring = page.ring.is_some();
    let anchors: Vec<Anchor> = page.pins.iter().map(|pin| pin.anchor).collect();
    selectors.extend(page.pins.iter().map(|pin| pin.selector.clone()));
    let mut rects = use_signal(Vec::<Option<[f64; 4]>>::new);
    let mut measured_for = use_signal(Vec::<String>::new);
    let mut moved = use_signal(|| 0u64);
    // The page moves the parts when it scrolls or resizes.
    use_future(move || async move {
        let mut moves = document::eval(
            r#"
            let pending = false;
            const tell = () => {
                if (pending) return;
                pending = true;
                requestAnimationFrame(() => { pending = false; dioxus.send(0); });
            };
            window.addEventListener('resize', tell);
            window.addEventListener('scroll', tell, true);
            "#,
        );
        while moves.recv::<u64>().await.is_ok() {
            let count = *moved.peek();
            moved.set(count + 1);
        }
    });
    let moves = moved();
    use_effect(use_reactive!(|selectors, replay, inspected, tab, moves| {
        let _ = (replay, inspected, tab, moves);
        let list: Vec<String> = selectors
            .iter()
            .map(|selector| format!("'{}'", selector.replace('\\', "\\\\").replace('\'', "\\'")))
            .collect();
        let script = format!(
            "return [{}].map((selector) => {{
                const element = document.querySelector(selector);
                if (!element) return null;
                const rect = element.getBoundingClientRect();
                if (rect.width === 0 && rect.height === 0) return null;
                return [rect.x + window.scrollX, rect.y + window.scrollY, rect.width, rect.height];
            }});",
            list.join(",")
        );
        // Marks of another step would show in the wrong places meanwhile.
        if *measured_for.peek() != selectors {
            rects.set(Vec::new());
            measured_for.set(selectors.clone());
        }
        spawn(async move {
            // Once now, and once more when the cards have glided into place;
            // unless the guide has moved on to another step meanwhile.
            for wait in [0, 520] {
                TimeoutFuture::new(wait).await;
                if let Ok(measured) = document::eval(&script)
                    .join::<Vec<Option<[f64; 4]>>>()
                    .await
                    && *measured_for.peek() == selectors
                {
                    rects.set(measured);
                }
            }
        });
    }));
    let measured = rects();
    let (ring, pins) = if has_ring {
        (
            measured.first().copied().flatten(),
            measured.get(1..).unwrap_or_default().to_vec(),
        )
    } else {
        (None, measured)
    };
    rsx! {
        div { class: "guide-marks", "aria-hidden": "true",
            if let Some([x, y, width, height]) = ring {
                div {
                    class: "guide-ring",
                    style: "transform:translate({x - 6.0}px,{y - 6.0}px);width:{width + 12.0}px;height:{height + 12.0}px;",
                }
            }
            for (number, (rect, anchor)) in pins.iter().zip(anchors.iter()).enumerate() {
                if let Some([x, y, width, _]) = rect {
                    span {
                        key: "{index}-{number}",
                        class: "guide-badge",
                        style: "transform:translate({anchor.badge_x(*x, *width)}px,{(y - 12.0).max(2.0)}px);--order:{number};",
                        "{number + 1}"
                    }
                }
            }
        }
    }
}
