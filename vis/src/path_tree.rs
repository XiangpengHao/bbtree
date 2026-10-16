//! The tree as one lookup sees it: a card for each node on the way from the
//! root to the key, each under the edge its parent took, with the parent's
//! other children beside it as gauges of how full they are.
//!
//! Every lookup plays its way down: at each node all the inline keys light
//! up at once, as the search compares them, then settle dark where they are
//! smaller than the key's and pale where larger; the tied ones stay
//! outlined, the result drops in, and the edge to the next node draws. Between
//! lookups the cards glide to their new places. Point at a ruler's slot to
//! see its key and click it to look it up; click a card to inspect it, a
//! gauge to look up its first key.

use std::fmt::Write;
use std::rc::Rc;

use bbtree::inspect::TraceOutcome;
use dioxus::prelude::*;

use crate::count_text::Group;
use crate::inspected_tree::LoadedTree;
use crate::memory_map::PageMarks;
use crate::path_summary::PathLegend;
use crate::spotlight::{Hint, NodePart, Spotlight};
use crate::trace::{Trace, TraceStep, Tracer};
use crate::tree_shape::NodeAt;

/// The path's geometry in the panel, in pixels.
struct PathLayout {
    cards: Vec<CardSpot>,
    gauges: Vec<GaugeSpot>,
    /// How many siblings didn't fit, past the gauges on each side.
    more: Vec<MoreSpot>,
    /// Each card's way down from its parent's edge, the root's empty.
    taken: Vec<String>,
    /// Whether a card's way down leads to a separator's value.
    value_way: Vec<bool>,
    /// Narrow: cards span the panel, labels sit above them, no gauges.
    narrow: bool,
    height: f64,
}

/// Where a card sits, and its ruler.
#[derive(Clone, PartialEq)]
struct CardSpot {
    row: usize,
    step: TraceStep,
    x: f64,
    y: f64,
    width: f64,
    /// Where the ruler of slots starts and how wide each slot is.
    ruler_x: f64,
    slot_width: f64,
    /// Cache lines the lookup read of the node: surely, and possibly.
    lines_read: (usize, usize),
}

/// The count of siblings past the last gauge that fit.
#[derive(Clone, PartialEq)]
struct MoreSpot {
    x: f64,
    y: f64,
    hidden: usize,
}

/// A sibling of a path node, drawn as a gauge, and its edge from the parent.
#[derive(Clone, PartialEq)]
struct GaugeSpot {
    at: NodeAt,
    x: f64,
    y: f64,
    fill: f64,
    label: String,
    curve: String,
}

impl PathLayout {
    const GUTTER: f64 = 128.0;
    const RIGHT: f64 = 28.0;
    /// Below this width, cards span the panel.
    const NARROW: f64 = 720.0;
    /// Room kept beside a card for the count of siblings that don't fit.
    const BESIDE: f64 = 56.0;
    const CARD_HEIGHT: f64 = 70.0;
    /// From one row's top to the next, at most, when there is room.
    const ROW_HEIGHT: f64 = 124.0;
    /// Above each card in a narrow panel, for its level's name.
    const LABEL_HEIGHT: f64 = 22.0;
    const RULER_TOP: f64 = 28.0;
    const RULER_HEIGHT: f64 = 14.0;
    const RULER_INSET: f64 = 14.0;
    const GAUGE_WIDTH: f64 = 11.0;
    const GAUGE_HEIGHT: f64 = 40.0;
    const GAUGE_GAP: f64 = 5.0;
    /// Room for "+12" past the gauges that don't fit.
    const MORE_WIDTH: f64 = 34.0;
    /// The least height that fits `rows` nodes, side by side with the
    /// inspector.
    fn least_height(rows: usize) -> f64 {
        rows.max(1) as f64 * (Self::CARD_HEIGHT + 16.0) + 20.0
    }

    /// The height the path wants for `rows` nodes, at `width`.
    fn wanted_height(rows: usize, width: f64) -> f64 {
        let rows = rows.max(1) as f64;
        if width > 0.0 && width < Self::NARROW {
            rows * (Self::CARD_HEIGHT + Self::LABEL_HEIGHT + 26.0) + 12.0
        } else {
            rows * Self::ROW_HEIGHT + 20.0
        }
    }

    fn of(tree: &LoadedTree, trace: &Trace, width: f64, height: f64) -> Self {
        let narrow = width < Self::NARROW;
        let shape = tree.shape();
        let steps = trace.steps();
        let rows = steps.len().max(1) as f64;
        let row_height = if narrow {
            Self::CARD_HEIGHT + Self::LABEL_HEIGHT + 26.0
        } else {
            ((height - 20.0) / rows).clamp(Self::CARD_HEIGHT + 16.0, Self::ROW_HEIGHT)
        };
        let (left, right) = if narrow {
            (16.0, width - 16.0)
        } else {
            (
                Self::GUTTER,
                (width - Self::RIGHT).max(Self::GUTTER + 320.0),
            )
        };
        let card_width = if narrow {
            right - left
        } else {
            ((right - left) * 0.66).clamp(320.0, 680.0)
        };
        let mut layout = Self {
            cards: Vec::new(),
            gauges: Vec::new(),
            more: Vec::new(),
            taken: Vec::new(),
            value_way: Vec::new(),
            narrow,
            height: 12.0 + rows * row_height,
        };
        // A wide panel taller than the path centers it.
        let above = if narrow {
            0.0
        } else {
            ((height - layout.height) / 2.0).max(0.0)
        };
        layout.height += above;
        let mut toward_value = false;
        for (row, step) in steps.iter().enumerate() {
            let at = step.at();
            let level = shape.level(at.level());
            let top = above + 10.0 + row as f64 * row_height;
            let y = if narrow {
                top + Self::LABEL_HEIGHT
            } else {
                top + (row_height - Self::CARD_HEIGHT) / 2.0
            };
            let parent = row.checked_sub(1).map(|row| layout.cards[row].clone());
            let anchor = parent
                .as_ref()
                .and_then(|parent| Some((parent, parent.step.edge_taken()?)))
                .map(|(parent, edge)| parent.ruler_x + edge as f64 * parent.slot_width);
            let x = match anchor {
                Some(_) if narrow => left,
                Some(anchor) => (anchor - card_width / 2.0).clamp(
                    left + Self::BESIDE,
                    (right - card_width - Self::BESIDE).max(left + Self::BESIDE),
                ),
                None => left + (right - left - card_width) / 2.0,
            };
            let ruler_x = x + Self::RULER_INSET;
            let slot_width =
                (card_width - 2.0 * Self::RULER_INSET) / level.capacity().max(1) as f64;
            let page = tree.page_map(at);
            let card = CardSpot {
                row,
                step: step.clone(),
                x,
                y,
                width: card_width,
                ruler_x,
                slot_width,
                lines_read: PageMarks::of_step(step, at.level() == 0).lines_read(&page),
            };
            let mut way = String::new();
            if let (Some(parent), Some(anchor)) = (&parent, anchor) {
                let from_y = parent.y + Self::RULER_TOP + Self::RULER_HEIGHT + 4.0;
                Self::curve(&mut way, (anchor, from_y), (x + card_width / 2.0, y));
                if !narrow {
                    layout.place_gauges(tree, parent, &card, (left, right), from_y);
                }
            }
            layout.taken.push(way);
            layout.value_way.push(toward_value);
            if step.found().is_some() && at.level() > 0 {
                toward_value = true;
            }
            layout.cards.push(card);
        }
        layout
    }

    /// The siblings of `card`'s node, the other children of `parent`'s,
    /// as gauges either side of it, the nearest first, as many as fit.
    fn place_gauges(
        &mut self,
        tree: &LoadedTree,
        parent: &CardSpot,
        card: &CardSpot,
        (left, right): (f64, f64),
        from_y: f64,
    ) {
        let shape = tree.shape();
        let Some(taken) = parent.step.edge_taken() else {
            return;
        };
        let parent_at = parent.step.at();
        let children = shape.level(parent_at.level()).children(parent_at.index());
        let level = shape.level(card.step.at().level());
        let pitch = Self::GAUGE_WIDTH + Self::GAUGE_GAP;
        let y = card.y + (Self::CARD_HEIGHT - Self::GAUGE_HEIGHT) / 2.0;
        let sides: [(Box<dyn Iterator<Item = usize>>, f64, f64); 2] = [
            (
                Box::new((0..taken).rev()),
                card.x - 12.0 - Self::GAUGE_WIDTH,
                -pitch,
            ),
            (
                Box::new(taken + 1..children.len()),
                card.x + card.width + 12.0,
                pitch,
            ),
        ];
        for (edges, start, step) in sides {
            let edges: Vec<usize> = edges.collect();
            let room = if step < 0.0 {
                start + Self::GAUGE_WIDTH - left
            } else {
                right - start
            };
            let mut fits = ((room + Self::GAUGE_GAP) / pitch).floor().max(0.0) as usize;
            if fits < edges.len() {
                fits = fits.saturating_sub((Self::MORE_WIDTH / pitch).ceil() as usize);
            }
            for (nth, &edge) in edges.iter().take(fits).enumerate() {
                let at = NodeAt::new(card.step.at().level(), children.start + edge);
                let x = start + nth as f64 * step;
                let len = level.len(at.index());
                let mut curve = String::new();
                Self::curve(
                    &mut curve,
                    (parent.ruler_x + edge as f64 * parent.slot_width, from_y),
                    (x + Self::GAUGE_WIDTH / 2.0, y),
                );
                self.gauges.push(GaugeSpot {
                    at,
                    x,
                    y,
                    fill: len as f64 / level.capacity().max(1) as f64,
                    label: format!(
                        "{} of {} keys, child {edge} of its parent",
                        len,
                        level.capacity()
                    ),
                    curve,
                });
            }
            let hidden = edges.len() - fits.min(edges.len());
            if hidden > 0 {
                let past = start + fits as f64 * step;
                let x = if step < 0.0 {
                    past + Self::GAUGE_WIDTH - Self::MORE_WIDTH
                } else {
                    past
                };
                self.more.push(MoreSpot { x, y, hidden });
            }
        }
    }

    /// A vertical S-curve from `from` down to `to`.
    fn curve(path: &mut String, from: (f64, f64), to: (f64, f64)) {
        let middle = (from.1 + to.1) / 2.0;
        let _ = write!(
            path,
            "M{:.1} {:.1}C{:.1} {middle:.1} {:.1} {middle:.1} {:.1} {:.1}",
            from.0, from.1, from.0, to.0, to.0, to.1
        );
    }

    /// The faint fan of every gauge's edge.
    fn fan(&self) -> String {
        self.gauges
            .iter()
            .map(|gauge| gauge.curve.as_str())
            .collect()
    }
}

/// The lookup's path through the tree.
#[component]
pub(crate) fn PathTree(
    tree: LoadedTree,
    trace: Trace,
    inspected: Option<NodeAt>,
    /// Changes with every lookup, so the way down replays.
    replay: u64,
    tracer: Tracer,
    spotlight: Spotlight,
) -> Element {
    let mut size = use_signal(|| (0.0f64, 0.0f64));
    let mounted: Signal<Option<Rc<MountedData>>> = use_signal(|| None);
    let mut pointed_gauge = use_signal(|| None::<usize>);
    let measure = use_callback(move |_: ()| {
        if let Some(element) = mounted.peek().clone() {
            spawn(async move {
                if let Ok(rect) = element.get_client_rect().await {
                    let measured = (rect.width(), rect.height());
                    if *size.peek() != measured {
                        size.set(measured);
                    }
                }
            });
        }
    });
    let (width, height) = size();
    let layout = (width > 0.0).then(|| PathLayout::of(&tree, &trace, width, height));
    let shape = tree.shape();
    let wanted = PathLayout::wanted_height(trace.steps().len(), width);
    let least = PathLayout::least_height(trace.steps().len());
    let narrow = layout.as_ref().is_some_and(|layout| layout.narrow);
    let fan = layout.as_ref().map(PathLayout::fan).unwrap_or_default();
    let gauge_curve = pointed_gauge().and_then(|index| {
        layout
            .as_ref()
            .and_then(|layout| layout.gauges.get(index))
            .map(|gauge| gauge.curve.clone())
    });
    rsx! {
        section {
            class: if narrow { "path-tree narrow" } else { "path-tree" },
            style: "--wanted:{wanted}px;--least:{least}px;--level-ms:{Trace::LEVEL_MS}ms;",
            "aria-label": "The lookup's path from the root",
            onmounted: move |evt| {
                let mut mounted = mounted;
                mounted.set(Some(evt.data()));
                measure.call(());
            },
            onresize: move |_| measure.call(()),
            if !trace.steps().is_empty() {
                PathLegend { spotlight }
            }
            if let Some(layout) = layout {
                svg {
                    class: "path-edges",
                    width,
                    height: layout.height.max(height),
                    "aria-hidden": "true",
                    path { class: "edge-other", d: fan }
                    if let Some(curve) = gauge_curve {
                        path { class: "edge-sibling", d: curve }
                    }
                    for (row, way) in layout.taken.iter().enumerate().skip(1) {
                        if layout.value_way[row] {
                            path {
                                key: "{replay}-{row}",
                                class: "edge-taken edge-value",
                                style: "--row:{row - 1};",
                                d: way.as_str(),
                            }
                        } else {
                            path {
                                key: "{replay}-{row}",
                                class: "edge-taken draws",
                                style: "--row:{row - 1};",
                                "pathLength": "1",
                                d: way.as_str(),
                            }
                        }
                    }
                }
                for card in layout.cards.iter() {
                    div {
                        key: "label-{card.row}",
                        class: "level-label",
                        style: if narrow { format!("transform:translate({}px,{}px);", card.x, card.y - PathLayout::LABEL_HEIGHT) } else { format!("transform:translate(16px,{}px);", card.y + 12.0) },
                        span { class: "display", {shape.level_name(card.step.at().level())} }
                        span { class: "data level-place",
                            "{Group(card.step.at().index() + 1)} of {Group(shape.level(card.step.at().level()).node_count())}"
                        }
                    }
                }
                for card in layout.cards.iter().cloned() {
                    PathCard {
                        key: "card-{card.row}",
                        tree: tree.clone(),
                        selected: inspected == Some(card.step.at()),
                        card,
                        replay,
                        tracer,
                        spotlight,
                    }
                }
                for (index, gauge) in layout.gauges.into_iter().enumerate() {
                    Gauge {
                        tree: tree.clone(),
                        gauge,
                        tracer,
                        spotlight,
                        on_point: move |pointed: bool| pointed_gauge.set(pointed.then_some(index)),
                    }
                }
                for more in layout.more {
                    span {
                        class: "data more-siblings",
                        style: "transform:translate({more.x}px,{more.y + 13.0}px);width:{PathLayout::MORE_WIDTH}px;",
                        title: "{more.hidden} more siblings",
                        "+{more.hidden}"
                    }
                }
            }
        }
    }
}

/// A sibling: how full it is, and a way to look up its first key.
#[component]
fn Gauge(
    tree: LoadedTree,
    gauge: GaugeSpot,
    tracer: Tracer,
    spotlight: Spotlight,
    on_point: EventHandler<bool>,
) -> Element {
    let mut tracer = tracer;
    let mut spotlight = spotlight;
    let at = gauge.at;
    let name = tree.shape().node_name(at);
    let label = gauge.label.clone();
    rsx! {
        button {
            class: "gauge",
            tabindex: "-1",
            style: "transform:translate({gauge.x}px,{gauge.y}px);width:{PathLayout::GAUGE_WIDTH}px;height:{PathLayout::GAUGE_HEIGHT}px;",
            "aria-label": "{name}: {gauge.label}. Look up its first key",
            onmouseenter: move |evt| {
                let point = evt.client_coordinates();
                spotlight.show_hint(Hint::new((point.x, point.y), name.clone(), format!("{label}. Click to look up its first key.")));
                on_point.call(true);
            },
            onmouseleave: move |_| {
                spotlight.clear();
                on_point.call(false);
            },
            onclick: move |_| tracer.run_into(&tree, at),
            span { class: "gauge-fill", style: "transform:scaleY({gauge.fill});" }
        }
    }
}

/// One node on the path: its fill, its prefix, and its slots as a ruler,
/// each key marked by how its inline key compared with the searched key's.
#[component]
fn PathCard(
    tree: LoadedTree,
    card: CardSpot,
    selected: bool,
    replay: u64,
    tracer: Tracer,
    spotlight: Spotlight,
) -> Element {
    let step = card.step.clone();
    let at = step.at();
    let shape = tree.shape();
    let level = shape.level(at.level());
    let len = level.len(at.index());
    let capacity = level.capacity();
    let prefix = level.prefix_len(at.index());
    let tied = step.tied().unwrap_or(0..0);
    let found = step.found();
    let edge = step.edge_taken();
    let insert_at = match step.outcome() {
        TraceOutcome::Absent { insert_at } => Some(insert_at),
        _ => None,
    };
    let compared = step.inline_key().is_some();
    let ruler_width = card.width - 2.0 * PathLayout::RULER_INSET;
    let note = card.note(&tree, len);
    let first = (len > 0).then(|| CardSpot::short_key(&tree, at, 0));
    let last = (len > 1).then(|| CardSpot::short_key(&tree, at, len - 1));
    let (lines, maybe) = card.lines_read;
    let pointed = spotlight.part_of(at);
    let name = shape.node_name(at);
    let label =
        format!("{name}: {len} of {capacity} keys; {note}; {lines} cache lines read. Inspect it.");
    let slot_width = card.slot_width;
    let mut tracer = tracer;
    let mut spotlight = spotlight;
    let point_tree = tree.clone();
    let pick_tree = tree.clone();
    let slot_under = move |x: f64| {
        let slot = (x / slot_width).floor();
        (slot >= 0.0 && (slot as usize) < len).then_some(slot as usize)
    };
    rsx! {
        div {
            class: if selected { "path-card selected" } else { "path-card" },
            style: "transform:translate({card.x}px,{card.y}px);width:{card.width}px;height:{PathLayout::CARD_HEIGHT}px;--row:{card.row};",
            role: "button",
            tabindex: "0",
            "data-row": "{card.row}",
            "aria-pressed": if selected { "true" } else { "false" },
            "aria-label": label,
            onclick: move |_| tracer.inspect(at),
            onkeydown: move |evt| {
                if matches!(evt.key(), Key::Enter) || evt.key() == Key::Character(" ".to_string()) {
                    evt.prevent_default();
                    tracer.inspect(at);
                }
            },
            div { class: "card-top data",
                span {
                    class: "card-count",
                    onmouseenter: move |evt| {
                        let point = evt.client_coordinates();
                        spotlight.show_hint(Hint::new((point.x, point.y), "Keys", format!("The node holds {len} keys and has room for {capacity}; the bar shows how full it is.")));
                    },
                    onmouseleave: move |_| spotlight.clear(),
                    "{len}/{capacity} keys"
                }
                span { class: "fill-track", "aria-hidden": "true",
                    span { class: "fill-bar", style: "transform:scaleX({len as f64 / capacity.max(1) as f64});" }
                }
                if prefix > 0 {
                    span {
                        class: "card-chip card-prefix",
                        onmouseenter: move |evt| {
                            let point = evt.client_coordinates();
                            spotlight.show_hint(Hint::new((point.x, point.y), "Skipped prefix", format!("Every key in this node starts with the same {prefix} bytes, so its search starts after them.")));
                        },
                        onmouseleave: move |_| spotlight.clear(),
                        "skips {prefix} B"
                    }
                }
                if lines > 0 {
                    span {
                        class: "card-chip card-lines",
                        onmouseenter: move |evt| {
                            let point = evt.client_coordinates();
                            spotlight.show_hint(Hint::new((point.x, point.y), "Cache lines read", "How many of the node's 64-byte cache lines the lookup read; a range when a search among tied keys may have read more."));
                        },
                        onmouseleave: move |_| spotlight.clear(),
                        match (lines, maybe) {
                            (1, 0) => "reads 1 line".to_string(),
                            (lines, 0) => format!("reads {lines} lines"),
                            (lines, maybe) => format!("reads {lines}–{} lines", lines + maybe),
                        }
                    }
                }
            }
            div {
                class: "ruler",
                style: "left:{PathLayout::RULER_INSET}px;top:{PathLayout::RULER_TOP}px;width:{ruler_width}px;height:{PathLayout::RULER_HEIGHT}px;",
                onmousemove: move |evt| {
                    let point = evt.client_coordinates();
                    match slot_under(evt.element_coordinates().x) {
                        Some(slot) if spotlight.peek_part_of(at) != Some(NodePart::Entry(slot)) => {
                            let (shared, rest) = point_tree.key_text_split(at, slot);
                            let kind = if at.level() == 0 { "key" } else { "separator" };
                            spotlight.point_at_part(
                                at,
                                NodePart::Entry(slot),
                                Some(Hint::new((point.x, point.y), format!("slot {slot}"), format!("{kind} {shared}{rest} · click to look it up"))),
                            );
                        }
                        Some(_) => spotlight.move_hint((point.x, point.y)),
                        None => spotlight.clear(),
                    }
                },
                onmouseleave: move |_| spotlight.clear(),
                onclick: move |evt| {
                    evt.stop_propagation();
                    if let Some(slot) = slot_under(evt.element_coordinates().x) {
                        let (shared, rest) = pick_tree.key_text_split(at, slot);
                        tracer.run_in(&pick_tree, &format!("{shared}{rest}"), at);
                    }
                },
                div { class: "ruler-marks", key: "{replay}",
                    for slot in 0..capacity {
                        span {
                            class: SlotMark::of(slot, len, found, &tied, compared).class(pointed == Some(NodePart::Entry(slot))),
                            style: "left:{slot as f64 * card.slot_width}px;width:{(card.slot_width - 1.0).max(0.5)}px;",
                        }
                    }
                    if let Some(edge) = edge {
                        span {
                            class: "edge-notch",
                            style: "left:{edge as f64 * card.slot_width - 1.5}px;",
                        }
                    }
                    if let Some(insert_at) = insert_at {
                        span {
                            class: "insert-caret",
                            style: "left:{insert_at as f64 * card.slot_width - 1.0}px;",
                        }
                    }
                    if let Some(NodePart::Child(child)) = pointed {
                        span {
                            class: "edge-pointed",
                            style: "left:{child as f64 * card.slot_width - 1.5}px;",
                        }
                    }
                }
            }
            div { class: "card-bounds data",
                span { class: "card-first", {first.unwrap_or_default()} }
                span { class: "card-note", {note} }
                span { class: "card-last", {last.unwrap_or_default()} }
            }
        }
    }
}

/// How a ruler draws one slot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SlotMark {
    /// Where the lookup ended.
    Found,
    /// A key whose inline key equals the searched key's.
    Tied,
    /// A key whose inline key the compare found smaller than the searched
    /// key's: counted, these place the key.
    Smaller,
    /// A key whose inline key the compare found larger.
    Larger,
    /// A key of a node the lookup compared no inline keys in.
    Key,
    /// A slot holding no key: the count reads it all the same, if
    /// `compared`, as it reads the inline keys' whole array (doc 10).
    Empty { compared: bool },
}

impl SlotMark {
    fn of(
        slot: usize,
        len: usize,
        found: Option<usize>,
        tied: &std::ops::Range<usize>,
        compared: bool,
    ) -> Self {
        if found == Some(slot) {
            SlotMark::Found
        } else if tied.contains(&slot) {
            SlotMark::Tied
        } else if slot >= len {
            SlotMark::Empty { compared }
        } else if !compared {
            SlotMark::Key
        } else if slot < tied.start {
            SlotMark::Smaller
        } else {
            SlotMark::Larger
        }
    }

    fn class(self, pointed: bool) -> String {
        let mark = match self {
            SlotMark::Found => "slot-found",
            SlotMark::Tied => "slot-tied",
            SlotMark::Smaller => "slot-smaller compared",
            SlotMark::Larger => "slot-larger compared",
            SlotMark::Key => "slot-key",
            SlotMark::Empty { compared: true } => return "slot slot-room compared".to_string(),
            SlotMark::Empty { compared: false } => return "slot slot-room".to_string(),
        };
        if pointed {
            format!("slot {mark} slot-pointed")
        } else {
            format!("slot {mark}")
        }
    }
}

impl CardSpot {
    /// What the lookup did at this card's node, of `len` keys, in a few
    /// words.
    fn note(&self, tree: &LoadedTree, len: usize) -> String {
        let at = self.step.at();
        match self.step.outcome() {
            TraceOutcome::Descend { edge } => {
                let bound = |index| Self::short_key(tree, at, index);
                match (edge.checked_sub(1), edge < len) {
                    (Some(left), true) => {
                        format!("child {edge}: {} < key < {}", bound(left), bound(edge))
                    }
                    (None, _) => format!("child 0: key < {}", bound(0)),
                    (Some(left), false) => format!("child {edge}: key > {}", bound(left)),
                }
            }
            TraceOutcome::Found { index } if at.level() == 0 => format!("found at slot {index}"),
            TraceOutcome::Found { index } => format!("separator at slot {index}: value below"),
            TraceOutcome::Absent { insert_at } => format!("absent: would go at slot {insert_at}"),
            TraceOutcome::ToHead { edge } => format!("child {edge}, toward the value"),
            TraceOutcome::Head => "its head holds the value".to_string(),
        }
    }

    /// Key `index` of the node at `at`, without the bytes every key of the
    /// node shares: "…/000819745".
    fn short_key(tree: &LoadedTree, at: NodeAt, index: usize) -> String {
        match tree.key_text_split(at, index) {
            (shared, rest) if shared.is_empty() => rest,
            (_, rest) => format!("…{rest}"),
        }
    }
}
