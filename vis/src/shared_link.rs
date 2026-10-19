//! The explorer's state in its address, so a copied link opens on what its
//! sender saw: `?keys=u64&n=1000000&build=random&key=851191&level=3&view=page`,
//! or for a batch `?keys=u64&n=1000000&build=random&bk=…&bk=…&bk=…&bk=…`.
//! Builds are deterministic, so the tree comes back node for node; the key
//! is looked up again, and the node at `level` on its way down inspected,
//! or the batch's four keys looked up again. A figure of the post starts
//! from such a link too, and links to the explorer with it.
//!
//! Each lookup and each new tree is an entry of the browser's history, so
//! its back and forward, or `←` and `→`, step through them; inspecting
//! another node or tab only rewrites the entry.

use dioxus::prelude::*;

use crate::batch_run::BatchKeys;
use crate::batch_view::Batcher;
use crate::inspected_tree::LoadedTree;
use crate::node_inspector::InspectorTab;
use crate::trace::Tracer;
use crate::workload::{Builder, InsertOrder, KeyKind, Workload};

/// A lookup a link names: its key, and the level of the node on its way
/// down to inspect, 0 for the leaf.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct LinkedLookup {
    key: String,
    level: Option<usize>,
}

impl LinkedLookup {
    pub(crate) fn key(&self) -> &str {
        &self.key
    }

    pub(crate) fn level(&self) -> Option<usize> {
        self.level
    }
}

/// What a link to the explorer says: the tree, and the lookup or batch on
/// it.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct ExplorerLink {
    workload: Workload,
    lookup: Option<LinkedLookup>,
    /// A batch, shown instead of one key.
    batch: Option<BatchKeys>,
    tab: InspectorTab,
}

impl ExplorerLink {
    /// The link a query string's `pairs` make: what they leave out or
    /// misname is as the page starts without a link.
    fn from_pairs(pairs: &[(String, String)]) -> Self {
        let value = |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        };
        let first = Workload::first();
        let kind = value("keys")
            .and_then(KeyKind::from_slug)
            .unwrap_or(first.kind());
        let count = value("n")
            .and_then(|count| count.parse().ok())
            .filter(|count| Workload::COUNTS.contains(count))
            .unwrap_or(first.count());
        let order = value("build")
            .and_then(InsertOrder::from_slug)
            .unwrap_or(first.order());
        let lookup = value("key").map(|key| LinkedLookup {
            key: key.to_string(),
            level: value("level").and_then(|level| level.parse().ok()),
        });
        let tab = value("view")
            .and_then(InspectorTab::from_slug)
            .unwrap_or(InspectorTab::Page);
        let batch_keys: Vec<String> = pairs
            .iter()
            .filter(|(name, _)| name == "bk")
            .map(|(_, key)| key.clone())
            .collect();
        let batch = (batch_keys.len() == BatchKeys::LANES).then(|| BatchKeys::new(batch_keys));
        Self {
            workload: Workload::new(kind, count, order),
            lookup,
            batch,
            tab,
        }
    }

    /// The link as a query string's pairs, in a fixed order.
    fn pairs(&self) -> Vec<(&'static str, String)> {
        let mut pairs = vec![
            ("keys", self.workload.kind().slug().to_string()),
            ("n", self.workload.count().to_string()),
            ("build", self.workload.order().slug().to_string()),
        ];
        if let Some(batch) = &self.batch {
            pairs.extend(batch.keys().iter().map(|key| ("bk", key.clone())));
        } else if let Some(lookup) = &self.lookup {
            pairs.push(("key", lookup.key.clone()));
            if let Some(level) = lookup.level {
                pairs.push(("level", level.to_string()));
            }
        }
        pairs.push(("view", self.tab.slug().to_string()));
        pairs
    }

    /// Whether `other` shows another lookup than this link, or another
    /// tree: a step of its own in the history.
    fn is_another_step(&self, other: &ExplorerLink) -> bool {
        self.workload != other.workload
            || self.batch != other.batch
            || self.lookup.as_ref().map(LinkedLookup::key)
                != other.lookup.as_ref().map(LinkedLookup::key)
    }

    /// The link the page was opened with.
    async fn read() -> Self {
        let pairs = document::eval("return [...new URLSearchParams(location.search)];")
            .join::<Vec<(String, String)>>()
            .await
            .unwrap_or_default();
        Self::from_pairs(&pairs)
    }

    /// Puts the link in the address bar: as a new history entry, numbered
    /// one past the current one, or in place of the current one.
    fn show(&self, write: AddressWrite) {
        let eval = document::eval(
            r#"
            const [pairs, push] = await dioxus.recv();
            const search = '?' + new URLSearchParams(pairs);
            const step = history.state?.step ?? 0;
            if (push) history.pushState({ step: step + 1 }, '', search);
            else if (location.search !== search || history.state?.step === undefined)
                history.replaceState({ step }, '', search);
            "#,
        );
        let _ = eval.send((self.pairs(), write == AddressWrite::Push));
    }

    /// Shows what the link names in the explorer: builds its tree if
    /// another is shown, then starts on it.
    fn open(self, builder: Builder, tracer: Tracer, batcher: Batcher, tab: Signal<InspectorTab>) {
        match builder.peek_tree() {
            Some(tree) if self.workload == builder.workload() => {
                self.start_on(&tree, tracer, batcher, tab)
            }
            _ => {
                self.show_view(batcher, tab);
                builder.build(self.workload, self.lookup, tracer);
            }
        }
    }

    /// Shows what the link names on `tree`, the tree of its workload: looks
    /// up its key, inspects its node and opens its tab, or shows its batch.
    pub(crate) fn start_on(
        &self,
        tree: &LoadedTree,
        tracer: Tracer,
        batcher: Batcher,
        tab: Signal<InspectorTab>,
    ) {
        let mut tracer = tracer;
        self.show_view(batcher, tab);
        tracer.start(tree, self.lookup.as_ref());
    }

    /// Opens the link's tab, and its batch or its single key.
    fn show_view(&self, batcher: Batcher, tab: Signal<InspectorTab>) {
        let (mut batcher, mut tab) = (batcher, tab);
        tab.set(self.tab);
        match &self.batch {
            Some(keys) => batcher.open(keys.clone()),
            None => batcher.close(),
        }
    }
}

/// How a link goes into the address bar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AddressWrite {
    /// As a new history entry.
    Push,
    /// In place of the current one.
    Replace,
}

/// A step through the lookups the page has shown, as the browser's history
/// keeps them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum HistoryStep {
    Back,
    Forward,
}

impl HistoryStep {
    /// Takes the step, if there is one: back stops at the page's first
    /// lookup rather than leave the page.
    pub(crate) fn take(self) {
        document::eval(match self {
            HistoryStep::Back => "if ((history.state?.step ?? 0) > 0) history.back();",
            HistoryStep::Forward => "history.forward();",
        });
    }
}

/// Starts the page from the link it was opened with, keeps the address in
/// step with what the page shows, and shows what the browser's back and
/// forward step to. Draws nothing.
#[component]
pub(crate) fn SharedLink(
    builder: Builder,
    tracer: Tracer,
    batcher: Batcher,
    tab: Signal<InspectorTab>,
) -> Element {
    let mut started = use_signal(|| false);
    // The link last put in the address bar, and whether the next one only
    // rewrites it: the first, and one the history stepped to.
    let mut written = use_signal(|| None::<ExplorerLink>);
    let mut rewrite_next = use_signal(|| true);
    use_future(move || async move {
        ExplorerLink::read()
            .await
            .open(builder, tracer, batcher, tab);
        started.set(true);
        let mut steps = document::eval(
            r#"
            window.addEventListener('popstate', () => {
                dioxus.send([...new URLSearchParams(location.search)]);
            });
            "#,
        );
        while let Ok(pairs) = steps.recv::<Vec<(String, String)>>().await {
            rewrite_next.set(true);
            ExplorerLink::from_pairs(&pairs).open(builder, tracer, batcher, tab);
        }
    });
    use_effect(move || {
        let workload = builder.workload();
        let (trace, inspected, tab) = (tracer.trace(), tracer.inspected(), tab());
        let batch = batcher
            .is_shown()
            .then(|| builder.peek_tree().map(|tree| batcher.keys_for(&tree)))
            .flatten();
        // Until the link is read and its tree built, the address keeps it.
        if !started() || !builder.is_built() {
            return;
        }
        let link = match (batch, trace) {
            (Some(batch), _) => ExplorerLink {
                workload,
                lookup: None,
                batch: Some(batch),
                tab,
            },
            (None, Some(trace)) => {
                let level = inspected
                    .filter(|at| trace.step_at(*at).is_some())
                    .map(|at| at.level());
                ExplorerLink {
                    workload,
                    lookup: Some(LinkedLookup {
                        key: trace.query().to_string(),
                        level,
                    }),
                    batch: None,
                    tab,
                }
            }
            (None, None) => return,
        };
        let before = written.peek().clone();
        if before.as_ref() == Some(&link) {
            rewrite_next.set(false);
            return;
        }
        let write = match before {
            Some(before) if !*rewrite_next.peek() && before.is_another_step(&link) => {
                AddressWrite::Push
            }
            _ => AddressWrite::Replace,
        };
        rewrite_next.set(false);
        link.show(write);
        written.set(Some(link));
    });
    rsx! {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(link: &ExplorerLink) -> Vec<(String, String)> {
        link.pairs()
            .into_iter()
            .map(|(name, value)| (name.to_string(), value))
            .collect()
    }

    #[test]
    fn links_read_back_what_they_say() {
        let link = ExplorerLink {
            workload: Workload::new(KeyKind::Urls, 100_000, InsertOrder::Collected),
            lookup: Some(LinkedLookup {
                key: "https://shop.example.com/users/000000042".to_string(),
                level: Some(1),
            }),
            batch: None,
            tab: InspectorTab::Slots,
        };
        assert_eq!(ExplorerLink::from_pairs(&pairs(&link)), link);
        let batch = ExplorerLink {
            lookup: None,
            batch: Some(BatchKeys::new(
                ["1", "2", "3", "https://a.b/c?d=e&f"]
                    .map(String::from)
                    .to_vec(),
            )),
            ..link
        };
        assert_eq!(ExplorerLink::from_pairs(&pairs(&batch)), batch);
    }

    #[test]
    fn what_a_link_leaves_out_starts_as_the_page_does() {
        let link = ExplorerLink::from_pairs(&[
            ("keys".to_string(), "floats".to_string()),
            ("n".to_string(), "12345".to_string()),
        ]);
        assert_eq!(link.workload, Workload::first());
        assert_eq!(link.lookup, None);
        assert_eq!(link.tab, InspectorTab::Page);
    }
}
