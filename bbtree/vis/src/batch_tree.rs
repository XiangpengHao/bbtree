//! The tree as a batch reads it: every node some lane reads, level by level
//! from the root, each drawn as its page, a strip of 64-byte lines tinted by
//! what each line holds, as the page map tints its slots. Each lane is a
//! numbered token on the node it reads or waits on. A line a lane fetched
//! outlines when the fetch goes out and fills while it is on its way, until
//! the lane's next poll reads it; then it fades to a read line's yellow. An
//! edge draws from the parent's line that holds it to the child's header
//! when a lane takes it.
//!
//! Point at a token to light its lane in every view; click it to see its
//! key looked up alone.

use std::rc::Rc;

use bbtree::inspect::{LaneFetch, PageMap};
use dioxus::prelude::*;

use crate::batch_run::BatchRun;
use crate::batch_view::Batcher;
use crate::inspected_tree::LoadedTree;
use crate::memory_map::{PageLines, SlotLook};

use crate::trace::Tracer;
use crate::tree_shape::NodeAt;

/// A page drawn as a strip of lines, each tinted by the slot array that
/// takes most of its bytes, by the page's layout; padding is untinted.
#[derive(Clone, PartialEq, Debug)]
struct PageStrip {
    lines: Vec<&'static str>,
}

impl PageStrip {
    /// The strip as one background: a band a line, in its array's tint,
    /// `cell` px wide with a pixel between.
    fn background(&self, cell: f64) -> String {
        let bands: Vec<String> = self
            .lines
            .iter()
            .enumerate()
            .map(|(line, class)| {
                let (from, to) = (line as f64 * cell, (line as f64 + 1.0) * cell);
                let tint = if class.is_empty() {
                    "--well".to_string()
                } else {
                    format!("--strip-{class}")
                };
                format!(
                    "var({tint}) {from:.2}px {:.2}px, transparent {:.2}px {to:.2}px",
                    to - 1.0,
                    to - 1.0
                )
            })
            .collect();
        format!(
            "background:linear-gradient(to right, {}) 0 2px / 100% calc(100% - 4px) no-repeat, var(--panel);",
            bands.join(", ")
        )
    }

    fn of(page: &PageMap) -> Self {
        let lines = PageLines::of(page)
            .holds()
            .iter()
            .map(|holds| holds.map_or("", |contents| contents.class()))
            .collect();
        Self { lines }
    }
}

/// The geometry of the batch's tree in its panel, in pixels.
struct BatchTreeLayout {
    rows: Vec<RowSpot>,
    pages: Vec<PageSpot>,
    edges: Vec<EdgeSpot>,
    height: f64,
    /// Narrow: level labels sit above their rows.
    narrow: bool,
}

/// A level's row.
#[derive(Clone, PartialEq)]
struct RowSpot {
    level: usize,
    /// Where its label starts.
    x: f64,
    y: f64,
}

/// A node's page.
#[derive(Clone, PartialEq)]
struct PageSpot {
    at: NodeAt,
    x: f64,
    y: f64,
    cell: f64,
    strip: PageStrip,
}

/// An edge a lane took, from its parent's line to the child's header.
#[derive(Clone, PartialEq)]
struct EdgeSpot {
    path: String,
    /// When the first lane to take it fetched the child's header: the end
    /// of that poll.
    taken_at: f64,
    lanes: Vec<usize>,
}

impl RowSpot {
    /// Where the level's label sits: level with the strips, or above the
    /// row.
    fn label_y(&self, narrow: bool) -> f64 {
        if narrow {
            self.y - 20.0
        } else {
            self.y + BatchTreeLayout::TOKEN_ROW + BatchTreeLayout::STRIP_HEIGHT / 2.0 - 8.0
        }
    }
}

impl PageSpot {
    fn center(&self) -> f64 {
        self.x + self.width() / 2.0
    }

    fn width(&self) -> f64 {
        self.cell * self.strip.lines.len() as f64
    }

    fn line_x(&self, line: usize) -> f64 {
        self.x + (line as f64 + 0.5) * self.cell
    }
}

impl BatchTreeLayout {
    const LABEL_WIDTH: f64 = 104.0;
    const NARROW: f64 = 560.0;
    /// A level's row: the least, and the most when the panel has room.
    const MIN_ROW: f64 = 64.0;
    const MAX_ROW: f64 = 136.0;
    /// The tokens' row, above each strip.
    const TOKEN_ROW: f64 = 26.0;
    const STRIP_HEIGHT: f64 = 20.0;
    const MAX_CELL: f64 = 12.0;
    /// From one token's center to the next: 24 px apart, as small targets
    /// must be (WCAG 2.5.8).
    const TOKEN_GAP: f64 = 24.0;
    /// Kept below the levels for the legend.
    const LEGEND_HEIGHT: f64 = 44.0;
    fn of(tree: &LoadedTree, run: &BatchRun, width: f64, height: f64) -> Self {
        let narrow = width < Self::NARROW;
        // The panel's own margin, inside its edges.
        let inset = if narrow { 12.0 } else { 20.0 };
        let (left, label_room) = if narrow {
            (inset, 22.0)
        } else {
            (inset + Self::LABEL_WIDTH, 0.0)
        };
        let usable = (width - left - inset).max(80.0);
        let levels = run.nodes(tree.shape().root());
        // The levels spread over the panel's height, within bounds.
        let row_height = (height / levels.len().max(1) as f64).clamp(Self::MIN_ROW, Self::MAX_ROW);
        let mut rows = Vec::new();
        let mut pages = Vec::new();
        for (row, nodes) in levels.iter().enumerate() {
            let y = row as f64 * (row_height + label_room) + label_room;
            let level = nodes[0].level();
            rows.push(RowSpot { level, y, x: inset });
            let slot = usable / nodes.len() as f64;
            for (index, &at) in nodes.iter().enumerate() {
                let strip = PageStrip::of(&tree.page_map(at));
                let lines = strip.lines.len().max(1) as f64;
                let cell = ((slot - 24.0) / lines).clamp(2.0, Self::MAX_CELL);
                let strip_width = cell * lines;
                pages.push(PageSpot {
                    at,
                    x: left + slot * index as f64 + (slot - strip_width) / 2.0,
                    y,
                    cell,
                    strip,
                });
            }
        }
        let edges = Self::edges(run, &pages);
        let height = rows.last().map_or(0.0, |row| {
            row.y + Self::TOKEN_ROW + Self::STRIP_HEIGHT + 8.0
        });
        Self {
            rows,
            pages,
            edges,
            height,
            narrow,
        }
    }

    /// The edges lanes took: from the line of the parent that each lane
    /// fetched for its edge to the child whose header it fetched next.
    fn edges(run: &BatchRun, pages: &[PageSpot]) -> Vec<EdgeSpot> {
        let page = |at: NodeAt| pages.iter().find(|page| page.at == at);
        let mut edge_lines = vec![None; run.lanes().len()];
        let mut edges: Vec<(NodeAt, usize, NodeAt, EdgeSpot)> = Vec::new();
        for (index, poll) in run.polls().iter().enumerate() {
            let Some(fetched) = poll.fetched() else {
                continue;
            };
            match (fetched.fetch(), fetched.line()) {
                (LaneFetch::Edge, Some(line)) => edge_lines[poll.lane()] = Some(line),
                (LaneFetch::Header, Some((child, _))) => {
                    let Some((parent, line)) = edge_lines[poll.lane()] else {
                        continue;
                    };
                    let taken_at = run.fetched_at(index);
                    if let Some((_, _, _, edge)) = edges
                        .iter_mut()
                        .find(|(from, at, to, _)| (*from, *at, *to) == (parent, line, child))
                    {
                        edge.lanes.push(poll.lane());
                        continue;
                    }
                    let (Some(from), Some(to)) = (page(parent), page(child)) else {
                        continue;
                    };
                    let (x0, y0) = (
                        from.line_x(line),
                        from.y + Self::TOKEN_ROW + Self::STRIP_HEIGHT,
                    );
                    let (x1, y1) = (to.line_x(0), to.y + Self::TOKEN_ROW - 2.0);
                    let bend = (y1 - y0) * 0.55;
                    let path = format!(
                        "M{x0:.1} {y0:.1} C{x0:.1} {:.1} {x1:.1} {:.1} {x1:.1} {y1:.1}",
                        y0 + bend,
                        y1 - bend
                    );
                    edges.push((
                        parent,
                        line,
                        child,
                        EdgeSpot {
                            path,
                            taken_at,
                            lanes: vec![poll.lane()],
                        },
                    ));
                }
                _ => {}
            }
        }
        edges.into_iter().map(|(_, _, _, edge)| edge).collect()
    }
}

#[component]
pub(crate) fn BatchTree(
    tree: LoadedTree,
    run: BatchRun,
    now: Signal<Option<usize>>,
    pointed: Signal<Option<usize>>,
    batcher: Batcher,
    tracer: Tracer,
) -> Element {
    // The panel's width, and the height its levels may spread over.
    let mut size = use_signal(|| (0.0f64, 0.0f64));
    let mounted: Signal<Option<Rc<MountedData>>> = use_signal(|| None);
    let measure = use_callback(move |_: ()| {
        if let Some(element) = mounted.peek().clone() {
            spawn(async move {
                if let Ok(rect) = element.get_client_rect().await {
                    let measured = (rect.width(), rect.height() - BatchTreeLayout::LEGEND_HEIGHT);
                    if *size.peek() != measured {
                        size.set(measured);
                    }
                }
            });
        }
    });
    let (width, height) = size();
    let layout = (width > 0.0).then(|| BatchTreeLayout::of(&tree, &run, width, height));
    let shape = tree.shape();
    let moments = run.moments(shape.root(), now());
    let lit = pointed();
    let mut pointed = pointed;
    rsx! {
        section {
            class: "batch-tree",
            "aria-label": "The nodes the batch reads, with each lane where it waits",
            onmounted: move |evt| {
                let mut mounted = mounted;
                mounted.set(Some(evt.data()));
                measure.call(());
            },
            onresize: move |_| measure.call(()),
            if let Some(layout) = layout {
                div {
                    class: "batch-tree-canvas",
                    style: "height:{layout.height}px;",
                    svg {
                        class: "batch-edges",
                        width,
                        height: layout.height,
                        "aria-hidden": "true",
                        for edge in layout.edges.iter() {
                            path {
                                class: if lit.is_some_and(|lane| edge.lanes.contains(&lane)) { "batch-edge lit" } else { "batch-edge" },
                                style: "--at:{edge.taken_at};",
                                d: "{edge.path}",
                            }
                        }
                    }
                    for row in layout.rows.iter() {
                        div {
                            key: "row-{row.level}",
                            class: if layout.narrow { "batch-level narrow" } else { "batch-level" },
                            style: "transform:translate({row.x}px,{row.label_y(layout.narrow)}px);",
                            span { class: "display", {shape.level_name(row.level)} }
                        }
                    }
                    for page in layout.pages.iter() {
                        div {
                            key: "page-{page.at.level()}-{page.at.index()}",
                            class: "batch-page",
                            style: "transform:translate({page.x}px,{page.y + BatchTreeLayout::TOKEN_ROW}px);width:{page.width()}px;--cell:{page.cell}px;",
                            title: "{shape.short_name(page.at)}: {page.strip.lines.len()} cache lines",
                            div {
                                class: "page-strip",
                                style: page.strip.background(page.cell),
                                for (index, poll) in run.polls().iter().enumerate() {
                                    if let Some((at, line)) = poll.fetched().and_then(|fetched| fetched.line())
                                        && at == page.at
                                    {
                                        {
                                            let issued = run.fetched_at(index);
                                            let used = run.read_at(index).unwrap_or(1.0e6);
                                            let flight = (used - issued).clamp(0.05, 1.0e5);
                                            let lit_class = if lit == Some(poll.lane()) { " lit" } else { "" };
                                            rsx! {
                                                span {
                                                    class: "fetched-line{lit_class}",
                                                    style: "left:{line as f64 * page.cell}px;--issued:{issued};--flight:{flight};--used:{used};",
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    for (lane, moment) in moments.iter().enumerate() {
                        {
                            let spot = moment
                                .node()
                                .and_then(|at| layout.pages.iter().find(|page| page.at == at));
                            let here: Vec<usize> = moments
                                .iter()
                                .enumerate()
                                .filter(|(_, other)| other.node() == moment.node())
                                .map(|(other, _)| other)
                                .collect();
                            let place = here.iter().position(|&other| other == lane).unwrap_or(0);
                            let (x, y) = match spot {
                                Some(page) => {
                                    (
                                        page.center() - here.len() as f64 * BatchTreeLayout::TOKEN_GAP / 2.0
                                            + place as f64 * BatchTreeLayout::TOKEN_GAP,
                                        page.y,
                                    )
                                }
                                None => (8.0 + lane as f64 * BatchTreeLayout::TOKEN_GAP, 2.0),
                            };
                            let last = moment.last();
                            let running = last.is_some() && last == now();
                            let waits = last
                                .filter(|_| !moment.ended())
                                .map(|last| {
                                    let issued = run.fetched_at(last);
                                    let next = run.read_at(last).unwrap_or(issued);
                                    (issued, (next - issued).max(0.05))
                                });
                            let state = if running {
                                "running"
                            } else if moment.ended() {
                                "ended"
                            } else if waits.is_some() {
                                "waiting"
                            } else {
                                "ready"
                            };
                            let (issued, flight) = waits.unwrap_or((0.0, 1.0));
                            let lit_class = if lit == Some(lane) { " lit" } else { "" };
                            let key = run.lanes()[lane].query().to_string();
                            let tree = tree.clone();
                            let (mut batcher, mut tracer) = (batcher, tracer);
                            rsx! {
                                button {
                                    key: "lane-{lane}",
                                    class: "lane-token {state}{lit_class}",
                                    style: "transform:translate({x}px,{y}px);--issued:{issued};--flight:{flight};",
                                    title: "Key {lane}: {key}. Click to look it up alone.",
                                    "aria-label": "Key {lane}, {key}: look it up alone",
                                    onmouseenter: move |_| pointed.set(Some(lane)),
                                    onmouseleave: move |_| pointed.set(None),
                                    onfocus: move |_| pointed.set(Some(lane)),
                                    onblur: move |_| pointed.set(None),
                                    onclick: move |_| {
                                        batcher.close();
                                        tracer.run(&tree, &key);
                                    },
                                    "{lane}"
                                }
                            }
                        }
                    }
                }
            }
            div { class: "batch-tree-legend", "aria-hidden": "true",
                span { class: "legend-item",
                    span { class: "legend-token running" }
                    "on the core"
                }
                span { class: "legend-item",
                    span { class: "legend-token waiting" }
                    "its line on its way"
                }
                span { class: "legend-item",
                    span { class: "legend-token ended" }
                    "done"
                }
                span { class: "legend-item",
                    span { class: "legend-line" }
                    "a prefetched line"
                }
            }
        }
    }
}
