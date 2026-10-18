//! A figure of the post that reads a tree: what a lookup, or a batch, did,
//! named by a link to the explorer (`ExplorerLink`), on a tree the post
//! builds once for every figure that shows it. A figure builds its tree as
//! the reader nears it and runs its lookup once the reader sees it; its
//! link opens the same lookup in the explorer.
//!
//! Each figure owns what the explorer's panels would share (the tracer, the
//! batcher, the inspector's tab), so one figure's lookup is its own.

use dioxus::prelude::*;

use crate::batch_view::Batcher;
use crate::element_ids::ElementIds;
use crate::inspected_tree::LoadedTree;
use crate::node_inspector::InspectorTab;
use crate::shared_link::ExplorerLink;
use crate::trace::Tracer;
use crate::workload::Workload;

/// The trees the post's figures show, each built once: figures of one
/// workload share its tree. Owned by the post, handed to each figure.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct TreeStore {
    built: Signal<Vec<(Workload, LoadedTree)>>,
    building: Signal<Vec<Workload>>,
}

impl TreeStore {
    /// A store with no trees: a hook, for the post.
    pub(crate) fn use_new() -> Self {
        Self {
            built: use_signal(Vec::new),
            building: use_signal(Vec::new),
        }
    }

    /// The tree of `workload`, once built.
    fn tree(&self, workload: Workload) -> Option<LoadedTree> {
        self.built
            .read()
            .iter()
            .find(|(built, _)| *built == workload)
            .map(|(_, tree)| tree.clone())
    }

    fn is_building(&self, workload: Workload) -> bool {
        self.building.read().contains(&workload)
    }

    /// Builds the tree of `workload`, unless it is built or being built.
    fn request(&self, workload: Workload) {
        let built = self
            .built
            .peek()
            .iter()
            .any(|(built, _)| *built == workload);
        if built || self.building.peek().contains(&workload) {
            return;
        }
        let (mut built, mut building) = (self.built, self.building);
        building.write().push(workload);
        spawn(async move {
            let tree = workload.build(|_| {}).await;
            building.write().retain(|under_way| *under_way != workload);
            built.write().push((workload, tree));
        });
    }
}

/// One figure: what its link names, what its panels share, and how far
/// the reader has come to it.
#[derive(Clone, PartialEq)]
pub(crate) struct Figure {
    link: ExplorerLink,
    trees: TreeStore,
    tracer: Tracer,
    batcher: Batcher,
    tab: Signal<InspectorTab>,
    /// Whether the reader has come within a screen of the figure, and
    /// whether they have seen it.
    near: Signal<bool>,
    seen: Signal<bool>,
    /// Whether the figure's lookup has started.
    started: Signal<bool>,
    ids: ElementIds,
}

impl Figure {
    /// The figure of what `link` names, on a tree of `trees`: a hook, for
    /// the figure's component.
    pub(crate) fn use_new(link: ExplorerLink, trees: TreeStore) -> Self {
        // A figure shows what its link names, the same at every visit.
        let seed = use_signal(|| 1);
        let figure = Self {
            link,
            trees,
            tracer: Tracer::use_new(seed),
            batcher: Batcher::use_new(seed),
            tab: use_signal(|| InspectorTab::Page),
            near: use_signal(|| false),
            seen: use_signal(|| false),
            started: use_signal(|| false),
            ids: ElementIds::of_current(),
        };
        let advancing = figure.clone();
        use_effect(move || advancing.advance());
        figure
    }

    /// The figure's tree, once built.
    pub(crate) fn tree(&self) -> Option<LoadedTree> {
        self.trees.tree(self.link.workload())
    }

    pub(crate) fn batcher(&self) -> Batcher {
        self.batcher
    }

    /// Whether the figure's lookup has started, so its panels draw.
    fn is_started(&self) -> bool {
        *self.started.read()
    }

    fn frame_id(&self) -> String {
        self.ids.id("figure")
    }

    /// Builds the figure's tree once the reader nears it, and runs its
    /// lookup once they see it and the tree is built, to its end: a figure
    /// draws what the lookup did, whole.
    fn advance(&self) {
        let workload = self.link.workload();
        if (self.near)() {
            self.trees.request(workload);
        }
        if !(self.seen)() || *self.started.peek() {
            return;
        }
        if let Some(tree) = self.trees.tree(workload) {
            let mut started = self.started;
            started.set(true);
            self.link
                .start_on(&tree, self.tracer, self.batcher, self.tab);
            let mut tracer = self.tracer;
            tracer.step_descent(isize::MAX);
        }
    }

    /// Hears when the reader comes within a screen of the figure's frame,
    /// and when it comes into view: its panels replace their room while
    /// only its top edge shows, so what grows pushes down what the reader
    /// can't yet see.
    fn watch_reader(&self) {
        let (mut near, mut seen) = (self.near, self.seen);
        let frame = self.frame_id();
        spawn(async move {
            let mut heard = document::eval(
                r#"
                const frame = document.getElementById(await dioxus.recv());
                if (!frame) return;
                const tell = (options, what) => {
                    const observer = new IntersectionObserver((entries) => {
                        if (!entries.some((entry) => entry.isIntersecting)) return;
                        observer.disconnect();
                        dioxus.send(what);
                    }, options);
                    observer.observe(frame);
                };
                tell({ rootMargin: '100% 0px' }, 'near');
                tell({ threshold: 0 }, 'seen');
                "#,
            );
            let _ = heard.send(frame);
            while let Ok(what) = heard.recv::<String>().await {
                if !*near.peek() {
                    near.set(true);
                }
                if what == "seen" {
                    seen.set(true);
                }
            }
        });
    }
}

/// A figure's frame: its panels (`children`) once its lookup starts, room
/// for them until then, its caption, and the link that opens it in the
/// explorer. `room` is about how tall the panels draw, on a desk and on a
/// phone, so that the page barely moves when they replace the room.
#[component]
pub(crate) fn FigureFrame(
    figure: Figure,
    room: (u32, u32),
    caption: Element,
    children: Element,
) -> Element {
    let started = figure.is_started();
    let building = figure.trees.is_building(figure.link.workload());
    let explorer = figure.link.href();
    let watched = figure.clone();
    rsx! {
        figure {
            id: figure.frame_id(),
            class: "figure",
            onmounted: move |_| watched.watch_reader(),
            div {
                class: if started { "figure-view" } else { "figure-view waiting" },
                style: "--room:{room.0}px;--room-narrow:{room.1}px",
                if started {
                    {children}
                } else if building {
                    p { class: "figure-wait", role: "status", "Building the tree…" }
                }
            }
            figcaption { class: "figure-caption",
                div { class: "figure-text", {caption} }
                a {
                    class: "figure-explore",
                    href: explorer,
                    target: "_blank",
                    rel: "noopener",
                    "Open in the explorer"
                }
            }
        }
    }
}
