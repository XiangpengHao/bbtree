//! The tree, level by level: each level a row of fixed height, the root on
//! top, and the x axis counting leaves, zoomable from the whole tree down to
//! one page across the screen. Only the span around the view is in the
//! document, drawn at the current zoom in its own pixels, so every number
//! that reaches the DOM stays about screen-sized however large the tree is;
//! a pan within the span moves one transform (`doc/10-visualization.md`).

use std::cell::Cell;
use std::fmt::Write;
use std::ops::Range;
use std::rc::Rc;

use dioxus::html::geometry::WheelDelta;
use dioxus::prelude::*;

use crate::inspected_tree::LoadedTree;
use crate::page_bar::PageBar;
use crate::tree_shape::{Level, LevelStats, NodeAt, TreeShape};

/// What the rows color their nodes by.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ColorBy {
    /// Keys over capacity.
    Fill,
    /// The bytes the node's searches skip (D26, D33).
    Prefix,
}

impl ColorBy {
    pub(crate) const ALL: [ColorBy; 2] = [ColorBy::Fill, ColorBy::Prefix];

    pub(crate) fn label(self) -> &'static str {
        match self {
            ColorBy::Fill => "fill",
            ColorBy::Prefix => "prefix",
        }
    }

    /// The ramp step, 1 to 7, of nodes `nodes` of `level`; 0 for no prefix.
    fn step(self, shape: &TreeShape, level: &Level, nodes: Range<usize>) -> usize {
        let share = match self {
            ColorBy::Fill => level.mean_fill(nodes),
            ColorBy::Prefix => {
                let longest = shape.longest_prefix();
                let prefix = level.mean_prefix(nodes);
                if prefix == 0.0 || longest == 0 {
                    return 0;
                }
                prefix / longest as f64
            }
        };
        1 + (share.clamp(0.0, 1.0) * 6.999) as usize
    }
}

/// Where each level's row sits, in pixels: rows don't zoom.
#[derive(Clone, Copy, PartialEq, Debug)]
struct RowLayout {
    height: usize,
}

impl RowLayout {
    const TOP: f64 = 28.0;
    const PITCH: f64 = 78.0;
    const BOX: f64 = 46.0;

    fn top(&self, level: usize) -> f64 {
        Self::TOP + (self.height - level) as f64 * Self::PITCH
    }

    /// The level whose row's boxes are at `y`, if any.
    fn level_at(&self, y: f64) -> Option<usize> {
        let row = ((y - Self::TOP) / Self::PITCH).floor();
        if row < 0.0 || row > self.height as f64 || y - Self::TOP - row * Self::PITCH > Self::BOX {
            return None;
        }
        Some(self.height - row as usize)
    }

    fn total_height(&self) -> f64 {
        self.top(0) + Self::BOX + Self::TOP
    }
}

/// What part of the tree the canvas shows: leaf position `left` at its
/// left edge, `scale` pixels per leaf.
#[derive(Clone, Copy, PartialEq, Debug)]
struct HorizontalView {
    left: f64,
    scale: f64,
}

impl HorizontalView {
    /// Room left at either side of a framed span, in pixels.
    const MARGIN: f64 = 32.0;

    /// The view that frames leaf positions `span` in `width` pixels.
    fn framing(span: Range<f64>, width: f64) -> Self {
        let room = (width - 2.0 * Self::MARGIN).max(1.0);
        let scale = room / (span.end - span.start).max(1e-9);
        Self {
            left: span.start - Self::MARGIN / scale,
            scale,
        }
    }

    fn leaf_x(&self, screen_x: f64) -> f64 {
        self.left + screen_x / self.scale
    }

    fn screen_x(&self, leaf_x: f64) -> f64 {
        (leaf_x - self.left) * self.scale
    }

    /// Zoomed by `factor` about `screen_x`, which keeps the leaf position
    /// under it, within `scales`.
    fn zoomed(self, factor: f64, screen_x: f64, scales: Range<f64>) -> Self {
        if !factor.is_finite() {
            return self;
        }
        let anchor = self.leaf_x(screen_x);
        let scale = (self.scale * factor).clamp(scales.start, scales.end);
        Self {
            left: anchor - screen_x / scale,
            scale,
        }
    }

    fn panned(self, by_px: f64) -> Self {
        Self {
            left: self.left - by_px / self.scale,
            ..self
        }
    }

    /// The scales a tree of `leaves` takes in `width` pixels: from half the
    /// whole tree's width down to one leaf across the screen.
    fn scales(leaves: usize, width: f64) -> Range<f64> {
        let whole = Self::framing(0.0..leaves.max(1) as f64, width).scale;
        whole / 2.0..width.max(whole)
    }
}

/// The leaf positions in the document: the view and a screen's width on
/// either side, drawn at one scale. A pan inside it only moves it; leaving
/// it, or zooming, draws it anew.
#[derive(Clone, Copy, PartialEq, Debug)]
struct DrawnSpan {
    start: f64,
    end: f64,
    scale: f64,
}

impl DrawnSpan {
    fn follow(previous: Option<Self>, view: HorizontalView, width: f64) -> Self {
        let visible = view.left..view.leaf_x(width);
        if let Some(previous) = previous.filter(|span| {
            span.scale == view.scale && span.start <= visible.start && visible.end <= span.end
        }) {
            return previous;
        }
        let reach = width / view.scale;
        Self {
            start: visible.start - reach,
            end: visible.end + reach,
            scale: view.scale,
        }
    }

    fn local_x(&self, leaf_x: f64) -> f64 {
        (leaf_x - self.start) * self.scale
    }

    fn width_px(&self) -> f64 {
        (self.end - self.start) * self.scale
    }
}

/// How much a boxed node shows, by how wide it is on screen. Nodes too
/// narrow for a box of their own are drawn as one strip per row instead.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Resolution {
    /// A colored box each.
    Swatch,
    /// A box with its numbers.
    Labelled,
    /// A box with its page laid out.
    Page,
}

impl Resolution {
    const NARROWEST_SWATCH: f64 = 4.0;
    const NARROWEST_LABEL: f64 = 92.0;
    const NARROWEST_PAGE: f64 = 300.0;

    fn of_width(width: f64) -> Self {
        if width >= Self::NARROWEST_PAGE {
            Resolution::Page
        } else if width >= Self::NARROWEST_LABEL {
            Resolution::Labelled
        } else {
            Resolution::Swatch
        }
    }
}

/// What the rows draw for one span: all plain data, made in one pass.
struct RowsDrawing {
    width: f64,
    height: f64,
    /// One path per ramp step, of every strip row's bins at that step.
    strips: Vec<(usize, String)>,
    /// From each boxed node up to its parent's row.
    links: String,
    boxes: Vec<NodeBox>,
}

/// One node drawn as a box, in the span's pixels.
#[derive(Clone, PartialEq)]
struct NodeBox {
    at: NodeAt,
    x: f64,
    y: f64,
    width: f64,
    resolution: Resolution,
    step: usize,
    len: usize,
    capacity: usize,
    prefix_len: usize,
    /// Whether the box shows its numbers: it fits on screen whole. A wider
    /// node's caption follows the view instead ([`Overlays`]).
    labelled: bool,
    /// Whether the box is the whole node, not cut to the span: only then
    /// does its page lay out across it.
    whole: bool,
}

impl RowsDrawing {
    /// Pixels each strip bin covers.
    const BIN: f64 = 2.0;

    fn of(shape: &TreeShape, span: DrawnSpan, color_by: ColorBy, screen_width: f64) -> Self {
        let height = shape.height().unwrap_or(0);
        let rows = RowLayout { height };
        let mut steps: Vec<String> = vec![String::new(); 8];
        let mut drawing = Self {
            width: span.width_px(),
            height: rows.total_height(),
            strips: Vec::new(),
            links: String::new(),
            boxes: Vec::new(),
        };
        let mut boxed_below: Vec<NodeBox> = Vec::new();
        for level_index in 0..=height {
            let level = shape.level(level_index);
            let nodes = level.nodes_across(span.start..span.end);
            let top = rows.top(level_index);
            // Crowded when the nodes in the span average too narrow a box.
            let crowded = !nodes.is_empty() && {
                let leaves = level.span(nodes.start).start..level.span(nodes.end - 1).end;
                let drawn = (leaves.end.min(span.end) - leaves.start.max(span.start)) * span.scale;
                drawn / (nodes.len() as f64) < Resolution::NARROWEST_SWATCH
            };
            if crowded {
                Self::draw_strip(&mut steps, shape, level, span, color_by, top);
                boxed_below.clear();
                continue;
            }
            let mut boxed = Vec::with_capacity(nodes.len());
            for index in nodes {
                let leaves = level.span(index);
                let full_width = (leaves.end - leaves.start) * span.scale;
                // A node wider than the span is cut to it; any other is drawn
                // whole, so its page lays out across its true width.
                let (start, end) = if full_width > span.width_px() {
                    (leaves.start.max(span.start), leaves.end.min(span.end))
                } else {
                    (leaves.start, leaves.end)
                };
                let x = span.local_x(start);
                let width = (end - start) * span.scale;
                let gap = if width > 8.0 { 1.0 } else { 0.0 };
                boxed.push(NodeBox {
                    at: NodeAt::new(level_index, index),
                    x: x + gap,
                    y: top,
                    width: (width - 2.0 * gap).max(1.0),
                    resolution: Resolution::of_width(full_width),
                    step: color_by.step(shape, level, index..index + 1),
                    len: level.len(index),
                    capacity: level.capacity(),
                    prefix_len: level.prefix_len(index),
                    labelled: full_width <= screen_width,
                    whole: full_width <= span.width_px(),
                });
            }
            for child in &boxed_below {
                let x = child.x + child.width / 2.0;
                let _ = write!(drawing.links, "M{x} {}V{}", top + RowLayout::BOX, child.y);
            }
            drawing.boxes.extend(boxed.iter().cloned());
            boxed_below = boxed;
        }
        drawing.strips = steps
            .into_iter()
            .enumerate()
            .filter(|(_, path)| !path.is_empty())
            .collect();
        drawing
    }

    /// Bins `level`'s nodes in the span per [`Self::BIN`] pixels, each bin
    /// colored by the mean of the nodes in it, and adds runs of bins of a
    /// step to that step's path.
    fn draw_strip(
        steps: &mut [String],
        shape: &TreeShape,
        level: &Level,
        span: DrawnSpan,
        color_by: ColorBy,
        top: f64,
    ) {
        let bins = (span.width_px() / Self::BIN).ceil() as usize;
        let mut run: Option<(usize, f64)> = None;
        let close_run = |run: Option<(usize, f64)>, end: f64, steps: &mut [String]| {
            if let Some((step, start)) = run {
                let _ = write!(
                    steps[step],
                    "M{start} {top}h{}v{}h{}z",
                    end - start,
                    RowLayout::BOX,
                    start - end
                );
            }
        };
        for bin in 0..bins {
            let x = bin as f64 * Self::BIN;
            let leaves = span.start + x / span.scale..span.start + (x + Self::BIN) / span.scale;
            let nodes = level.nodes_across(leaves);
            let step = (!nodes.is_empty()).then(|| color_by.step(shape, level, nodes));
            match (run, step) {
                (Some((current, _)), Some(step)) if current == step => {}
                (_, step) => {
                    close_run(run, x, steps);
                    run = step.map(|step| (step, x));
                }
            }
        }
        close_run(run, bins as f64 * Self::BIN, steps);
    }
}

/// The tree on a canvas you pan and zoom along the keys.
#[component]
pub(crate) fn TreeCanvas(
    tree: Signal<Option<LoadedTree>>,
    selected: Signal<Option<NodeAt>>,
    focus: Signal<Option<NodeAt>>,
    color_by: ColorBy,
) -> Element {
    let mut view = use_signal(|| None::<HorizontalView>);
    let mut container = use_signal(|| (0.0f64, 0.0f64, 0.0f64));
    let mounted: Signal<Option<Rc<MountedData>>> = use_signal(|| None);
    let mut press = use_signal(|| None::<(f64, bool)>);
    let fitted_for = use_signal(|| None::<LoadedTree>);
    let drawn = use_hook(|| Rc::new(Cell::new(None::<DrawnSpan>)));

    let refresh_rect = use_callback(move |_: ()| {
        let element = mounted.peek().clone();
        if let Some(element) = element {
            spawn(async move {
                if let Ok(rect) = element.get_client_rect().await {
                    let size = (rect.origin.x, rect.origin.y, rect.width());
                    if *container.peek() != size {
                        container.set(size);
                    }
                }
            });
        }
    });

    // A new tree is framed whole, once the canvas has a width.
    use_effect(move || {
        let current = tree.read().clone();
        let width = container.read().2;
        let mut fitted_for = fitted_for;
        if width <= 0.0 || *fitted_for.peek() == current {
            return;
        }
        if let Some(loaded) = &current {
            let leaves = loaded.shape().leaf_count();
            view.set(Some(HorizontalView::framing(0.0..leaves as f64, width)));
        }
        fitted_for.set(current);
    });

    // A node to zoom to, from the detail panel.
    use_effect(move || {
        let Some(at) = *focus.read() else { return };
        let width = container.peek().2;
        if let Some(loaded) = tree.peek().as_ref() {
            let span = loaded.shape().level(at.level()).span(at.index());
            view.set(Some(HorizontalView::framing(span, width)));
        }
        let mut focus = focus;
        focus.set(None);
    });

    let loaded = tree.read().clone();
    let width = container.read().2;
    let current_view = *view.read();

    let node_at_point = move |client_x: f64, client_y: f64| -> Option<NodeAt> {
        let (left, top, _) = *container.peek();
        let view = (*view.peek())?;
        let tree = tree.peek();
        let shape = tree.as_ref()?.shape();
        let rows = RowLayout {
            height: shape.height()?,
        };
        let level = rows.level_at(client_y - top)?;
        shape.node_over(level, view.leaf_x(client_x - left))
    };

    let on_wheel = move |evt: Event<WheelData>| {
        evt.prevent_default();
        let Some(current) = *view.peek() else { return };
        let Some(leaves) = tree.peek().as_ref().map(|tree| tree.shape().leaf_count()) else {
            return;
        };
        let (left, _, width) = *container.peek();
        let delta = match evt.delta() {
            WheelDelta::Pixels(delta) => (delta.x, delta.y),
            WheelDelta::Lines(delta) => (delta.x * 16.0, delta.y * 16.0),
            WheelDelta::Pages(delta) => (delta.x * width, delta.y * width),
        };
        let modifiers = evt.modifiers();
        let anchor = evt.client_coordinates().x - left;
        let scales = HorizontalView::scales(leaves, width);
        let next = if modifiers.ctrl() || modifiers.meta() {
            current.zoomed((-delta.1 * 0.01).exp(), anchor, scales)
        } else if modifiers.shift() {
            current.panned(-delta.1)
        } else if delta.0.abs() > delta.1.abs() {
            current.panned(-delta.0)
        } else {
            current.zoomed((-delta.1 * 0.002).exp(), anchor, scales)
        };
        view.set(Some(next));
    };

    let on_key = move |evt: Event<KeyboardData>| {
        let Some(current) = *view.peek() else { return };
        let Some(leaves) = tree.peek().as_ref().map(|tree| tree.shape().leaf_count()) else {
            return;
        };
        let width = container.peek().2;
        let scales = HorizontalView::scales(leaves, width);
        let next = match evt.key() {
            Key::ArrowLeft => current.panned(width / 4.0),
            Key::ArrowRight => current.panned(-width / 4.0),
            Key::Character(text) if text == "+" || text == "=" => {
                current.zoomed(1.5, width / 2.0, scales)
            }
            Key::Character(text) if text == "-" => current.zoomed(1.0 / 1.5, width / 2.0, scales),
            Key::Character(text) if text == "0" => {
                HorizontalView::framing(0.0..leaves as f64, width)
            }
            Key::Escape => {
                let mut selected = selected;
                selected.set(None);
                return;
            }
            _ => return,
        };
        evt.prevent_default();
        view.set(Some(next));
    };

    let span = current_view.filter(|_| width > 0.0).map(|current| {
        let span = DrawnSpan::follow(drawn.get(), current, width);
        drawn.set(Some(span));
        span
    });

    rsx! {
        div {
            class: "relative min-w-0 flex-1 cursor-grab select-none overflow-hidden bg-[var(--surface)] outline-none focus-visible:ring-2 focus-visible:ring-[var(--accent)] focus-visible:ring-inset",
            tabindex: "0",
            "aria-label": "Tree canvas: scroll to zoom along the keys, drag or shift-scroll to pan, click a node for its page",
            onmounted: move |evt| {
                let mut mounted = mounted;
                mounted.set(Some(evt.data()));
                refresh_rect.call(());
            },
            onresize: move |_| refresh_rect.call(()),
            onwheel: on_wheel,
            onkeydown: on_key,
            onpointerdown: move |evt: Event<PointerData>| {
                refresh_rect.call(());
                press.set(Some((evt.client_coordinates().x, false)));
            },
            onpointermove: move |evt: Event<PointerData>| {
                let Some((last, moved)) = *press.peek() else { return };
                if evt.held_buttons().is_empty() {
                    press.set(None);
                    return;
                }
                let x = evt.client_coordinates().x;
                if !moved && (x - last).abs() < 3.0 {
                    return;
                }
                let current = *view.peek();
                if let Some(current) = current {
                    view.set(Some(current.panned(x - last)));
                }
                press.set(Some((x, true)));
            },
            onpointerup: move |evt: Event<PointerData>| {
                let pressed = press.take();
                if let Some((_, false)) = pressed {
                    let point = evt.client_coordinates();
                    let mut selected = selected;
                    selected.set(node_at_point(point.x, point.y));
                }
            },
            ondoubleclick: move |evt: Event<MouseData>| {
                let point = evt.client_coordinates();
                if let Some(at) = node_at_point(point.x, point.y) {
                    let mut focus = focus;
                    focus.set(Some(at));
                }
            },
            if let (Some(loaded), Some(view), Some(span)) = (loaded.clone(), current_view, span) {
                div {
                    class: "absolute left-0 top-0",
                    style: "transform: translateX({view.screen_x(span.start)}px);",
                    TreeRows { tree: loaded.clone(), span, color_by, screen_width: width }
                }
                Overlays { tree: loaded, view, width, selected: *selected.read() }
            } else {
                div { class: "absolute inset-0 grid place-items-center text-[13px] text-[var(--muted)]",
                    "Build a tree to see it here"
                }
            }
        }
    }
}

/// The span's rows, drawn again only when the span, the tree or the color
/// changes.
#[component]
fn TreeRows(tree: LoadedTree, span: DrawnSpan, color_by: ColorBy, screen_width: f64) -> Element {
    let drawing = RowsDrawing::of(tree.shape(), span, color_by, screen_width);
    rsx! {
        svg {
            class: "pointer-events-none absolute left-0 top-0",
            width: drawing.width,
            height: drawing.height,
            for (step, path) in drawing.strips {
                path { class: "ramp-{step}", d: path }
            }
            path { class: "tree-link", d: drawing.links }
        }
        // The boxes' keyed list is its parent's only child, which keeps the
        // keyed diff.
        div {
            for node in drawing.boxes {
                NodeBoxView { key: "{node.at.level()}-{node.at.index()}", tree: tree.clone(), node, color_by }
            }
        }
    }
}

/// One boxed node, with as much as its width allows.
#[component]
fn NodeBoxView(tree: LoadedTree, node: NodeBox, color_by: ColorBy) -> Element {
    let style = format!(
        "left:{}px;top:{}px;width:{}px;height:{}px;",
        node.x,
        node.y,
        node.width,
        RowLayout::BOX
    );
    let fill = node.len as f64 / node.capacity.max(1) as f64;
    if node.resolution == Resolution::Swatch {
        return rsx! {
            div { class: "node-box absolute rounded-[3px] ramp-{node.step}", style }
        };
    }
    let numbers = match color_by {
        ColorBy::Fill => format!("{}/{}", node.len, node.capacity),
        ColorBy::Prefix => format!(
            "{}/{} · prefix {}",
            node.len, node.capacity, node.prefix_len
        ),
    };
    let laid_out = node.resolution == Resolution::Page && node.whole;
    let range = (laid_out && node.labelled)
        .then(|| tree.key_range(node.at))
        .flatten();
    let page = laid_out.then(|| tree.page_map(node.at));
    rsx! {
        div {
            class: "node-box absolute flex flex-col justify-between overflow-hidden rounded-[4px] bg-[var(--surface)] px-1.5 pb-1 pt-0.5",
            style,
            div { class: "tabular flex items-baseline gap-2 truncate text-[11px] leading-4 text-[var(--ink-2)]",
                if node.labelled {
                    span { class: "font-medium text-[var(--ink)]", {numbers} }
                }
                if let Some((first, last)) = range {
                    span { class: "truncate font-mono text-[10.5px]", "{first} … {last}" }
                }
            }
            match page {
                Some(page) => rsx! {
                    div { class: "overflow-hidden rounded-[2px]",
                        PageBar { page, height: 12.0 }
                    }
                },
                None => rsx! {
                    div { class: "h-1.5 overflow-hidden rounded-full bg-[var(--grid)]",
                        div { class: "h-full ramp-{node.step}", style: "width:{fill * 100.0}%;" }
                    }
                },
            }
        }
    }
}

/// What follows the view on every frame: the level labels, the selection
/// ring, and captions for nodes wider than the screen.
#[component]
fn Overlays(
    tree: LoadedTree,
    view: HorizontalView,
    width: f64,
    selected: Option<NodeAt>,
) -> Element {
    let shape = tree.shape();
    let Some(height) = shape.height() else {
        return rsx! {};
    };
    let rows = RowLayout { height };
    let stats = shape.level_stats();
    let ring = selected.filter(|at| at.level() <= height).map(|at| {
        let span = shape.level(at.level()).span(at.index());
        let x0 = view.screen_x(span.start).max(-8.0);
        let x1 = view.screen_x(span.end).min(width + 8.0);
        (x0, (x1 - x0).max(2.0), rows.top(at.level()))
    });
    let visible = view.left..view.leaf_x(width);
    let captions: Vec<(f64, f64, String)> = (0..=height)
        .filter_map(|level_index| {
            let level = shape.level(level_index);
            let nodes = level.nodes_across(visible.clone());
            // Only a node wider than the screen needs a caption kept in view.
            let index = nodes.clone().find(|&index| {
                let span = level.span(index);
                (span.end - span.start) * view.scale > width
            })?;
            let span = level.span(index);
            let x = view.screen_x(span.start).max(8.0);
            let fill = level.len(index) as f64 / level.capacity() as f64 * 100.0;
            let mut text = format!(
                "{}/{} keys · {fill:.0}%",
                level.len(index),
                level.capacity()
            );
            if let Some((first, last)) = tree.key_range(NodeAt::new(level_index, index)) {
                let _ = write!(text, " · {first} … {last}");
            }
            Some((x, rows.top(level_index), text))
        })
        .collect();
    rsx! {
        for (x, top, text) in captions {
            div {
                class: "tabular pointer-events-none absolute max-w-[70%] truncate whitespace-nowrap font-mono text-[10.5px] leading-4 text-[var(--ink-2)]",
                style: "left:{x}px;top:{top + 4.0}px;",
                {text}
            }
        }
        if let Some((x, ring_width, top)) = ring {
            div {
                class: "pointer-events-none absolute rounded-[5px] ring-2 ring-[var(--accent)]",
                style: "left:{x - 2.0}px;top:{top - 2.0}px;width:{ring_width + 4.0}px;height:{RowLayout::BOX + 4.0}px;",
            }
        }
        div { class: "pointer-events-none absolute left-0 top-0 h-full",
            for row in stats {
                div {
                    class: "tabular absolute left-2 whitespace-nowrap rounded bg-[var(--surface)] px-1.5 text-[10.5px] leading-4 text-[var(--muted)] shadow-[0_0_0_1px_var(--ring)]",
                    style: "top:{rows.top(row.level) - 18.0}px;",
                    {row.row_label(height)}
                }
            }
        }
    }
}

impl LevelStats {
    /// The label of the level's row, in a tree of `height` levels above
    /// the leaves.
    fn row_label(&self, height: usize) -> String {
        let name = if self.level == height {
            "root".to_string()
        } else if self.level == 0 {
            "leaves".to_string()
        } else {
            format!("level {}", self.level)
        };
        let plural = if self.nodes == 1 { "" } else { "s" };
        format!(
            "{name} · {} node{plural} · {:.0}% full",
            Group(self.nodes),
            self.mean_fill * 100.0
        )
    }
}

/// A count with its digits grouped in threes.
pub(crate) struct Group(pub(crate) usize);

impl std::fmt::Display for Group {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let digits = self.0.to_string();
        for (index, digit) in digits.chars().enumerate() {
            if index > 0 && (digits.len() - index).is_multiple_of(3) {
                f.write_str(",")?;
            }
            write!(f, "{digit}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_keeps_the_leaf_under_the_pointer() {
        let view = HorizontalView {
            left: 1234.5,
            scale: 0.01,
        };
        let zoomed = view.zoomed(8.0, 640.0, 1e-9..1e9);
        assert!((view.leaf_x(640.0) - zoomed.leaf_x(640.0)).abs() < 1e-6);
        assert_eq!(zoomed.scale, 0.08);
    }

    #[test]
    fn a_span_holds_still_until_the_view_leaves_it() {
        let view = HorizontalView {
            left: 1e5,
            scale: 2.0,
        };
        let span = DrawnSpan::follow(None, view, 1600.0);
        assert_eq!(
            DrawnSpan::follow(Some(span), view.panned(-700.0), 1600.0),
            span
        );
        let far = view.panned(-3000.0);
        let moved = DrawnSpan::follow(Some(span), far, 1600.0);
        assert_ne!(moved, span);
        assert!(moved.start <= far.left && far.leaf_x(1600.0) <= moved.end);
        let zoomed = view.zoomed(1.1, 800.0, 1e-9..1e9);
        assert_ne!(
            DrawnSpan::follow(Some(span), zoomed, 1600.0).scale,
            span.scale
        );
    }

    #[test]
    fn rows_stack_from_the_root_down() {
        let rows = RowLayout { height: 3 };
        assert!(rows.top(3) < rows.top(0));
        assert_eq!(rows.level_at(rows.top(2) + 1.0), Some(2));
        assert_eq!(rows.level_at(rows.top(2) + RowLayout::BOX + 2.0), None);
        assert_eq!(rows.level_at(0.0), None);
    }

    #[test]
    fn counts_group_their_digits() {
        assert_eq!(Group(7).to_string(), "7");
        assert_eq!(Group(1000).to_string(), "1,000");
        assert_eq!(Group(10_000_000).to_string(), "10,000,000");
    }
}

/// Fill or prefix, with the ramp's legend.
#[component]
pub(crate) fn ColorPicker(color_by: Signal<ColorBy>) -> Element {
    let current = color_by();
    let (low, high) = match current {
        ColorBy::Fill => ("empty", "full"),
        ColorBy::Prefix => ("0 B", "longest"),
    };
    rsx! {
        div { class: "flex items-center gap-3 text-[12px]",
            div { class: "flex overflow-hidden rounded-md border border-[var(--grid)]",
                for option in ColorBy::ALL {
                    button {
                        class: if option == current { "bg-[var(--plane)] px-2.5 py-1 font-medium" } else { "px-2.5 py-1 text-[var(--ink-2)]" },
                        "aria-pressed": option == current,
                        onclick: move |_| {
                            let mut color_by = color_by;
                            color_by.set(option);
                        },
                        "{option.label()}"
                    }
                }
            }
            div { class: "flex items-center gap-1.5 text-[11px] text-[var(--muted)]",
                {low}
                div { class: "flex",
                    for step in 1..=7 {
                        span { class: "inline-block h-2.5 w-3 ramp-{step}" }
                    }
                }
                {high}
            }
        }
    }
}
