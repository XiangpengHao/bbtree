//! The page's state in its address, so a copied link opens on what its
//! sender saw: `?keys=u64&n=1000000&build=random&key=851191&level=3&view=page`.
//! Builds are deterministic, so the tree comes back node for node; the key
//! is looked up again, and the node at `level` on its way down inspected.
//!
//! Each lookup and each new tree is an entry of the browser's history, so
//! its back and forward, or `←` and `→`, step through them; inspecting
//! another node or tab only rewrites the entry.

use dioxus::prelude::*;

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

/// What a link says.
#[derive(Clone, PartialEq, Debug)]
struct PageLink {
    workload: Workload,
    lookup: Option<LinkedLookup>,
    tab: InspectorTab,
}

impl PageLink {
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
        Self {
            workload: Workload::new(kind, count, order),
            lookup,
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
        if let Some(lookup) = &self.lookup {
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
    fn is_another_step(&self, other: &PageLink) -> bool {
        self.workload != other.workload
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

    /// Shows what the link names: builds its tree if another is shown,
    /// looks up its key, inspects its node and opens its tab.
    fn open(self, builder: Builder, tracer: Tracer, tab: Signal<InspectorTab>) {
        let (mut tracer, mut tab) = (tracer, tab);
        tab.set(self.tab);
        match builder.tree() {
            Some(tree) if self.workload == builder.workload() => {
                tracer.start(&tree, self.lookup.as_ref())
            }
            _ => builder.build(self.workload, self.lookup, tracer),
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
pub(crate) fn SharedLink(builder: Builder, tracer: Tracer, tab: Signal<InspectorTab>) -> Element {
    let mut started = use_signal(|| false);
    // The link last put in the address bar, and whether the next one only
    // rewrites it: the first, and one the history stepped to.
    let mut written = use_signal(|| None::<PageLink>);
    let mut rewrite_next = use_signal(|| true);
    use_future(move || async move {
        PageLink::read().await.open(builder, tracer, tab);
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
            PageLink::from_pairs(&pairs).open(builder, tracer, tab);
        }
    });
    use_effect(move || {
        let workload = builder.workload();
        let (trace, inspected, tab) = (tracer.trace(), tracer.inspected(), tab());
        // Until the link is read and its tree built, the address keeps it.
        if !started() || !builder.is_built() {
            return;
        }
        let Some(trace) = trace else {
            return;
        };
        let level = inspected
            .filter(|at| trace.step_at(*at).is_some())
            .map(|at| at.level());
        let link = PageLink {
            workload,
            lookup: Some(LinkedLookup {
                key: trace.query().to_string(),
                level,
            }),
            tab,
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

    fn pairs(link: &PageLink) -> Vec<(String, String)> {
        link.pairs()
            .into_iter()
            .map(|(name, value)| (name.to_string(), value))
            .collect()
    }

    #[test]
    fn links_read_back_what_they_say() {
        let link = PageLink {
            workload: Workload::new(KeyKind::Urls, 100_000, InsertOrder::Collected),
            lookup: Some(LinkedLookup {
                key: "https://shop.example.com/users/000000042".to_string(),
                level: Some(1),
            }),
            tab: InspectorTab::Slots,
        };
        assert_eq!(PageLink::from_pairs(&pairs(&link)), link);
    }

    #[test]
    fn what_a_link_leaves_out_starts_as_the_page_does() {
        let link = PageLink::from_pairs(&[
            ("keys".to_string(), "floats".to_string()),
            ("n".to_string(), "12345".to_string()),
        ]);
        assert_eq!(link.workload, Workload::first());
        assert_eq!(link.lookup, None);
        assert_eq!(link.tab, InspectorTab::Page);
    }
}
