//! The tree as one lookup sees it: a card for each node on the way from the
//! root to the key, each under the edge its parent took, with the parent's
//! other children beside it as gauges of how full they are. Click a card to
//! inspect it, a gauge to look up its first key.

use std::fmt::Write;
use std::rc::Rc;

use bbtree::inspect::TraceOutcome;
use dioxus::prelude::*;

use crate::count_text::Group;
use crate::inspected_tree::LoadedTree;
use crate::trace::{Trace, TraceStep, Tracer};
use crate::tree_shape::NodeAt;

/// The path's geometry in the panel, in pixels.
struct PathLayout {
    cards: Vec<CardSpot>,
    gauges: Vec<GaugeSpot>,
    /// How many siblings didn't fit, past the gauges on each side.
    more: Vec<MoreSpot>,
    /// The parent edge each card hangs from, bold; those of the gauges,
    /// faint; and the value's way, dashed.
    taken: String,
    others: String,
    value_way: String,
}

/// Where a card sits, and its ruler.
#[derive(Clone, PartialEq)]
struct CardSpot {
    step: TraceStep,
    x: f64,
    y: f64,
    width: f64,
    /// Where the ruler of slots starts and how wide each slot is.
    ruler_x: f64,
    slot_width: f64,
}

/// The count of siblings past the last gauge that fit.
#[derive(Clone, PartialEq)]
struct MoreSpot {
    x: f64,
    y: f64,
    hidden: usize,
}

/// A sibling of a path node, drawn as a gauge.
#[derive(Clone, PartialEq)]
struct GaugeSpot {
    at: NodeAt,
    x: f64,
    y: f64,
    fill: f64,
    label: String,
}

impl PathLayout {
    const GUTTER: f64 = 150.0;
    const RIGHT: f64 = 28.0;
    /// Room kept beside a card for the count of siblings that don't fit.
    const BESIDE: f64 = 56.0;
    const CARD_HEIGHT: f64 = 66.0;
    /// From one row's top to the next, when there is room.
    const ROW_HEIGHT: f64 = 90.0;
    const RULER_TOP: f64 = 26.0;
    const RULER_HEIGHT: f64 = 14.0;
    const GAUGE_WIDTH: f64 = 11.0;
    const GAUGE_HEIGHT: f64 = 38.0;
    const GAUGE_GAP: f64 = 5.0;
    /// Room for "+12" past the gauges that don't fit.
    const MORE_WIDTH: f64 = 34.0;

    fn of(tree: &LoadedTree, trace: &Trace, width: f64, height: f64) -> Self {
        let shape = tree.shape();
        let steps = trace.steps();
        let rows = steps.len().max(1) as f64;
        let row_height = ((height - 24.0) / rows).clamp(Self::CARD_HEIGHT + 16.0, Self::ROW_HEIGHT);
        let left = Self::GUTTER;
        let right = (width - Self::RIGHT).max(left + 320.0);
        let card_width = ((right - left) * 0.58).clamp(320.0, 680.0);
        let mut layout = Self {
            cards: Vec::new(),
            gauges: Vec::new(),
            more: Vec::new(),
            taken: String::new(),
            others: String::new(),
            value_way: String::new(),
        };
        let mut value_way = false;
        for (row, step) in steps.iter().enumerate() {
            let at = step.at();
            let level = shape.level(at.level());
            let y = 12.0 + row as f64 * row_height + (row_height - Self::CARD_HEIGHT) / 2.0;
            let parent = row.checked_sub(1).map(|row| layout.cards[row].clone());
            let anchor = parent
                .as_ref()
                .and_then(|parent| Some((parent, parent.step.edge_taken()?)))
                .map(|(parent, edge)| parent.ruler_x + edge as f64 * parent.slot_width);
            let x = match anchor {
                Some(anchor) => (anchor - card_width / 2.0).clamp(
                    left + Self::BESIDE,
                    (right - card_width - Self::BESIDE).max(left + Self::BESIDE),
                ),
                None => left + (right - left - card_width) / 2.0,
            };
            let ruler_x = x + 14.0;
            let slot_width = (card_width - 28.0) / level.capacity().max(1) as f64;
            let card = CardSpot {
                step: step.clone(),
                x,
                y,
                width: card_width,
                ruler_x,
                slot_width,
            };
            if let (Some(parent), Some(anchor)) = (&parent, anchor) {
                let from_y = parent.y + Self::RULER_TOP + Self::RULER_HEIGHT + 6.0;
                let path = if value_way {
                    &mut layout.value_way
                } else {
                    &mut layout.taken
                };
                Self::curve(path, (anchor, from_y), (x + card_width / 2.0, y));
                layout.place_gauges(tree, parent, &card, (left, right), from_y);
            }
            if step.found().is_some() && at.level() > 0 {
                value_way = true;
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
                let fill = level.len(at.index()) as f64 / level.capacity().max(1) as f64;
                self.gauges.push(GaugeSpot {
                    at,
                    x,
                    y,
                    fill,
                    label: format!(
                        "{} · {}/{} keys · look up its first key",
                        shape.node_name(at),
                        level.len(at.index()),
                        level.capacity()
                    ),
                });
                let anchor = parent.ruler_x + edge as f64 * parent.slot_width;
                Self::curve(
                    &mut self.others,
                    (anchor, from_y),
                    (x + Self::GAUGE_WIDTH / 2.0, y),
                );
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
            "M{} {}C{} {middle} {} {middle} {} {}",
            from.0, from.1, from.0, to.0, to.0, to.1
        );
    }
}

/// The lookup's path through the tree.
#[component]
pub(crate) fn PathTree(
    tree: LoadedTree,
    trace: Trace,
    inspected: Option<NodeAt>,
    tracer: Tracer,
) -> Element {
    let mut size = use_signal(|| (0.0f64, 0.0f64));
    let mounted: Signal<Option<Rc<MountedData>>> = use_signal(|| None);
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
    let wanted = trace.steps().len() as f64 * PathLayout::ROW_HEIGHT + 24.0;
    rsx! {
        section {
            class: "path-tree relative max-h-[60%] min-h-0 flex-none overflow-hidden",
            style: "height:{wanted}px;",
            onmounted: move |evt| {
                let mut mounted = mounted;
                mounted.set(Some(evt.data()));
                measure.call(());
            },
            onresize: move |_| measure.call(()),
            if let Some(layout) = layout {
                svg { class: "pointer-events-none absolute inset-0", width, height,
                    path { class: "edge-other", d: layout.others }
                    path { class: "edge-taken", d: layout.taken }
                    path { class: "edge-value", d: layout.value_way }
                }
                for card in layout.cards.iter() {
                    div {
                        class: "pointer-events-none absolute flex flex-col items-start",
                        style: "left:20px;top:{card.y + 14.0}px;width:{PathLayout::GUTTER - 36.0}px;",
                        span { class: "display text-[11px]", {shape.level_name(card.step.at().level())} }
                        span { class: "data text-[11px] text-[var(--muted)]",
                            "{Group(card.step.at().index() + 1)} of {Group(shape.level(card.step.at().level()).node_count())}"
                        }
                    }
                }
                for gauge in layout.gauges {
                    Gauge { tree: tree.clone(), gauge, tracer }
                }
                for more in layout.more {
                    span {
                        class: "data absolute text-[11px] text-[var(--muted)]",
                        style: "left:{more.x}px;top:{more.y + 11.0}px;width:{PathLayout::MORE_WIDTH}px;text-align:center;",
                        "+{more.hidden}"
                    }
                }
                for card in layout.cards {
                    PathCard {
                        key: "{card.step.at().level()}",
                        tree: tree.clone(),
                        selected: inspected == Some(card.step.at()),
                        card,
                        tracer,
                    }
                }
            }
        }
    }
}

/// A sibling: how full it is, and a way to look up its first key.
#[component]
fn Gauge(tree: LoadedTree, gauge: GaugeSpot, tracer: Tracer) -> Element {
    let mut tracer = tracer;
    let at = gauge.at;
    rsx! {
        button {
            class: "gauge absolute",
            style: "left:{gauge.x}px;top:{gauge.y}px;width:{PathLayout::GAUGE_WIDTH}px;height:{PathLayout::GAUGE_HEIGHT}px;",
            title: "{gauge.label}",
            "aria-label": "{gauge.label}",
            onclick: move |_| tracer.run_into(&tree, at),
            span { class: "gauge-fill", style: "height:{gauge.fill * 100.0}%;" }
        }
    }
}

/// One node on the path: its fill, its prefix, and its slots as a ruler,
/// the ones the search compared marked.
#[component]
fn PathCard(tree: LoadedTree, card: CardSpot, selected: bool, tracer: Tracer) -> Element {
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
    let ruler_width = card.width - 28.0;
    let note = card.note(&tree, len);
    let first = (len > 0).then(|| CardSpot::short_key(&tree, at, 0));
    let last = (len > 1).then(|| CardSpot::short_key(&tree, at, len - 1));
    let shared = (prefix > 0 && len > 0)
        .then(|| tree.key_text_split(at, 0).0)
        .filter(|shared| !shared.is_empty());
    let mut tracer = tracer;
    rsx! {
        button {
            class: if selected { "path-card selected absolute text-left" } else { "path-card absolute text-left" },
            style: "left:{card.x}px;top:{card.y}px;width:{card.width}px;height:{PathLayout::CARD_HEIGHT}px;",
            onclick: move |_| tracer.inspect(at),
            div { class: "data absolute inset-x-3.5 top-1.5 flex items-center gap-2.5 whitespace-nowrap text-[11.5px] leading-4",
                span { class: "shrink-0 font-medium text-[var(--ink)]", "{len}/{capacity}" }
                span { class: "fill-track shrink-0",
                    span { class: "fill-bar", style: "width:{len as f64 / capacity.max(1) as f64 * 100.0}%;" }
                }
                if prefix > 0 {
                    span {
                        class: "shrink-0 text-[var(--graphite)]",
                        title: shared.map(|shared| format!("Every key here starts with {shared}")).unwrap_or_default(),
                        "prefix {prefix} B"
                    }
                }
                span { class: "ml-auto min-w-0 truncate text-[var(--graphite)]", {note} }
            }
            div {
                class: "absolute",
                style: "left:14px;top:{PathLayout::RULER_TOP}px;width:{ruler_width}px;height:{PathLayout::RULER_HEIGHT}px;",
                for slot in 0..capacity {
                    span {
                        class: if found == Some(slot) { "slot slot-found" } else if tied.contains(&slot) { "slot slot-tied" } else if slot < len { "slot slot-key" } else { "slot slot-room" },
                        style: "left:{slot as f64 * card.slot_width}px;width:{(card.slot_width - 1.0).max(0.5)}px;",
                    }
                }
                if let Some(edge) = edge {
                    span { class: "edge-notch", style: "left:{edge as f64 * card.slot_width - 1.5}px;" }
                }
            }
            div { class: "data absolute inset-x-3.5 bottom-1.5 flex justify-between gap-3 text-[10.5px] leading-3 text-[var(--muted)]",
                span { class: "truncate", {first.unwrap_or_default()} }
                span { class: "truncate text-right", {last.unwrap_or_default()} }
            }
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
                        format!("edge {edge}: {} < key < {}", bound(left), bound(edge))
                    }
                    (None, _) => format!("edge 0: key < {}", bound(0)),
                    (Some(left), false) => format!("edge {edge}: key > {}", bound(left)),
                }
            }
            TraceOutcome::Found { index } if at.level() == 0 => format!("found at slot {index}"),
            TraceOutcome::Found { index } => format!("slot {index} is a separator: value below"),
            TraceOutcome::Absent { insert_at } => format!("not here: would go at slot {insert_at}"),
            TraceOutcome::ToHead { edge } => format!("edge {edge}, toward the value"),
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
