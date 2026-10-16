//! A figure of the post: the explorer's own panels, drawn in place from a
//! link to the explorer (`ExplorerLink`), on a tree the post builds once
//! for every figure that shows it. A figure builds its tree as the reader
//! nears it and starts its lookup once the reader sees it, so the lookup
//! plays where it is read; its link opens the same lookup in the explorer.
//!
//! Each figure owns what its panels share in the explorer (the tracer, the
//! batcher, the spotlight, the inspector's tab), so clicking in one figure
//! changes it alone; all of them point things out in the page's one hint.

use dioxus::prelude::*;

use crate::batch_view::Batcher;
use crate::element_ids::ElementIds;
use crate::inspected_tree::LoadedTree;
use crate::node_inspector::InspectorTab;
use crate::shared_link::ExplorerLink;
use crate::spotlight::{PointerHint, Spotlight};
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
    spotlight: Spotlight,
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
    /// The figure of what `link` names, on a tree of `trees`, saying what
    /// is pointed at in the page's `hint`: a hook, for the figure's
    /// component.
    pub(crate) fn use_new(link: ExplorerLink, trees: TreeStore, hint: PointerHint) -> Self {
        // A figure shows what its link names, the same at every visit.
        let seed = use_signal(|| 1);
        let figure = Self {
            link,
            trees,
            tracer: Tracer::use_new(seed),
            batcher: Batcher::use_new(seed),
            spotlight: Spotlight::use_new(hint),
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

    pub(crate) fn tracer(&self) -> Tracer {
        self.tracer
    }

    pub(crate) fn spotlight(&self) -> Spotlight {
        self.spotlight
    }

    /// Whether the figure's lookup has started, so its panels draw.
    fn is_started(&self) -> bool {
        *self.started.read()
    }

    fn frame_id(&self) -> String {
        self.ids.id("figure")
    }

    /// Builds the figure's tree once the reader nears it, and starts its
    /// lookup once they see it and the tree is built.
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
        }
    }

    /// Hears when the reader comes within a screen of the figure's frame,
    /// and when a third of it is in view.
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
                tell({ threshold: 0.33 }, 'seen');
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
/// explorer.
#[component]
pub(crate) fn FigureFrame(figure: Figure, caption: Element, children: Element) -> Element {
    let started = figure.is_started();
    let building = figure.trees.is_building(figure.link.workload());
    let explorer = figure.link.href();
    let watched = figure.clone();
    rsx! {
        figure {
            id: figure.frame_id(),
            class: "figure",
            onmounted: move |_| watched.watch_reader(),
            div { class: if started { "figure-view" } else { "figure-view waiting" },
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
