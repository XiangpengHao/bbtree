//! What the visualizer builds: a key type, how many entries, in what order,
//! and the build itself, in chunks between which the page stays responsive.

use bbtree::BBTreeMap;
use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;

use crate::count_text::Group;
use crate::icons::{Icon, IconGlyph};
use crate::inspected_tree::{InspectedMap, LoadedTree, VisKey};
use crate::popover::Popover;
use crate::shared_link::LinkedLookup;
use crate::trace::Tracer;

/// The kinds of key on offer, each picked for what bbtree does with it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum KeyKind {
    /// `u64`: dense integers, with 2-byte inline keys (D31) and local
    /// search prefixes where fences are missing (D33).
    Integers,
    /// `String` URLs sharing long prefixes: fence prefixes, local search
    /// prefixes and length tags (D26, D33, D36).
    Urls,
    /// `[u8; 16]` ids: 2-byte summaries, children before keys, local search
    /// prefixes (D32, D39, D33).
    Ids,
}

impl KeyKind {
    pub(crate) const ALL: [KeyKind; 3] = [KeyKind::Integers, KeyKind::Urls, KeyKind::Ids];

    pub(crate) fn label(self) -> &'static str {
        match self {
            KeyKind::Integers => "u64",
            KeyKind::Urls => "URLs",
            KeyKind::Ids => "16-byte ids",
        }
    }

    /// The kind's name in a link.
    pub(crate) fn slug(self) -> &'static str {
        match self {
            KeyKind::Integers => "u64",
            KeyKind::Urls => "urls",
            KeyKind::Ids => "ids",
        }
    }

    pub(crate) fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.slug() == slug)
    }

    /// Whether the kind's keys read as text, a byte a character, rather
    /// than as hex.
    pub(crate) fn is_text(self) -> bool {
        self == KeyKind::Urls
    }

    /// What the kind shows of bbtree, for its hint.
    pub(crate) fn shows(self) -> &'static str {
        match self {
            KeyKind::Integers => {
                "u64 keys: 2-byte inline keys, and fence prefixes that grow with depth."
            }
            KeyKind::Urls => {
                "String keys sharing long prefixes: 8-byte inline keys past fence and search prefixes."
            }
            KeyKind::Ids => {
                "[u8; 16] keys: 2-byte inline keys, and internal nodes that put children before keys."
            }
        }
    }

    /// A key greater than every key of this kind the builds make.
    pub(crate) fn past_the_end(self) -> &'static str {
        match self {
            KeyKind::Integers => "18446744073709551615",
            KeyKind::Urls => "https://shop.example.com/~",
            KeyKind::Ids => "ffffffffffffffffffffffffffffffff",
        }
    }

    /// A key just after `key` that no build of this kind makes, if keys of
    /// the kind leave gaps: integers and ids are dense.
    pub(crate) fn missing_after(self, key: &str) -> Option<String> {
        match self {
            KeyKind::Urls => Some(format!("{key}-draft")),
            KeyKind::Integers | KeyKind::Ids => None,
        }
    }

    /// A key of this kind, as the lookup bar takes it.
    pub(crate) fn example(self) -> &'static str {
        match self {
            KeyKind::Integers => "a u64, such as 396070",
            KeyKind::Urls => "a URL, such as https://shop.example.com/orders/000291177",
            KeyKind::Ids => "32 hex digits, such as 62627472656500000000000000003039",
        }
    }

    /// Why `query` names no key of this kind.
    pub(crate) fn parse_error(self, query: &str) -> String {
        match self {
            KeyKind::Integers => format!("\"{query}\" isn't a u64"),
            KeyKind::Urls => format!("\"{query}\" isn't a key"),
            KeyKind::Ids => format!("\"{query}\" isn't 32 hex digits"),
        }
    }

    /// The most entries a build of this kind offers: string keys take tens
    /// of bytes each beside the tree, in at most 4 GB of wasm32 memory.
    pub(crate) fn max_count(self) -> usize {
        match self {
            KeyKind::Integers | KeyKind::Ids => 10_000_000,
            KeyKind::Urls => 1_000_000,
        }
    }

    /// The key at position `index` in key order, of `count`.
    fn integer_key(index: u64) -> u64 {
        index
    }

    fn url_key(index: u64, count: u64) -> String {
        const SECTIONS: [&str; 4] = ["accounts", "orders", "products", "users"];
        let section = SECTIONS[(index * SECTIONS.len() as u64 / count.max(1)) as usize];
        format!("https://shop.example.com/{section}/{index:09}")
    }

    fn id_key(index: u64) -> [u8; 16] {
        let mut id = [0; 16];
        id[..6].copy_from_slice(b"bbtree");
        id[8..].copy_from_slice(&index.to_be_bytes());
        id
    }
}

/// The orders entries go in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum InsertOrder {
    /// One insert at a time, in a scrambled order: the fill of a tree grown
    /// by splits.
    Random,
    /// One insert at a time, in key order: ascending inserts fill the leaves
    /// they append to (D45).
    Ascending,
    /// All at once, sorted, into full nodes (`collect`, D25).
    Collected,
}

impl InsertOrder {
    pub(crate) const ALL: [InsertOrder; 3] = [
        InsertOrder::Random,
        InsertOrder::Ascending,
        InsertOrder::Collected,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            InsertOrder::Random => "random inserts",
            InsertOrder::Ascending => "ascending inserts",
            InsertOrder::Collected => "collect (bulk)",
        }
    }

    /// The order's name in a link.
    pub(crate) fn slug(self) -> &'static str {
        match self {
            InsertOrder::Random => "random",
            InsertOrder::Ascending => "ascending",
            InsertOrder::Collected => "collect",
        }
    }

    pub(crate) fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|order| order.slug() == slug)
    }

    /// What the order does to the tree.
    pub(crate) fn shows(self) -> &'static str {
        match self {
            InsertOrder::Random => {
                "One insert at a time, in a scrambled order: nodes fill up and split wherever keys land."
            }
            InsertOrder::Ascending => {
                "One insert at a time, in key order: every insert lands at the tree's right edge, and splits there leave full nodes behind."
            }
            InsertOrder::Collected => {
                "All at once from sorted keys, by bbtree's bulk builder: every node as full as it can be."
            }
        }
    }
}

/// One build: a kind of key, how many, in what order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Workload {
    kind: KeyKind,
    count: usize,
    order: InsertOrder,
}

impl Workload {
    /// The entry counts on offer.
    pub(crate) const COUNTS: [usize; 4] = [10_000, 100_000, 1_000_000, 10_000_000];

    /// Inserts between two yields to the browser.
    const CHUNK: usize = 50_000;

    pub(crate) fn new(kind: KeyKind, count: usize, order: InsertOrder) -> Self {
        Self {
            kind,
            count: count.min(kind.max_count()),
            order,
        }
    }

    /// The build the page starts with.
    pub(crate) fn first() -> Self {
        Self::new(KeyKind::Integers, 1_000_000, InsertOrder::Random)
    }

    pub(crate) fn kind(&self) -> KeyKind {
        self.kind
    }

    pub(crate) fn count(&self) -> usize {
        self.count
    }

    pub(crate) fn order(&self) -> InsertOrder {
        self.order
    }

    /// Builds the map, reporting the share done as it goes.
    pub(crate) async fn build(self, progress: impl FnMut(f64)) -> LoadedTree {
        let count = self.count as u64;
        match self.kind {
            KeyKind::Integers => {
                let map = self.build_map(KeyKind::integer_key, progress).await;
                LoadedTree::new(InspectedMap::new(map, self.kind))
            }
            KeyKind::Urls => {
                let map = self
                    .build_map(|index| KeyKind::url_key(index, count), progress)
                    .await;
                LoadedTree::new(InspectedMap::new(map, self.kind))
            }
            KeyKind::Ids => {
                let map = self.build_map(KeyKind::id_key, progress).await;
                LoadedTree::new(InspectedMap::new(map, self.kind))
            }
        }
    }

    async fn build_map<K: VisKey>(
        self,
        key_at: impl Fn(u64) -> K,
        mut progress: impl FnMut(f64),
    ) -> BBTreeMap<K, u64> {
        let count = self.count as u64;
        if self.order == InsertOrder::Collected {
            progress(0.0);
            TimeoutFuture::new(0).await;
            return (0..count).map(|index| (key_at(index), index)).collect();
        }
        let mut indices: Box<dyn Iterator<Item = u64>> = match self.order {
            InsertOrder::Random => Box::new(ScrambledIndices::new(count)),
            _ => Box::new(0..count),
        };
        let mut map = BBTreeMap::new();
        let mut done = 0;
        loop {
            let before = done;
            for index in indices.by_ref().take(Self::CHUNK) {
                map.insert(key_at(index), index);
                done += 1;
            }
            if done == before {
                return map;
            }
            progress(done as f64 / count as f64);
            TimeoutFuture::new(0).await;
        }
    }
}

/// The indices `0..count` in a scrambled order, none held in memory: a
/// full-period linear congruential walk over the next power of two, through
/// a bijective mix, skipping values past `count`.
struct ScrambledIndices {
    state: u64,
    bits: u32,
    count: u64,
    remaining: u64,
}

impl ScrambledIndices {
    fn new(count: u64) -> Self {
        Self {
            state: 0,
            bits: count.next_power_of_two().trailing_zeros().max(2),
            count,
            remaining: count,
        }
    }

    fn mask(&self) -> u64 {
        (1u64 << self.bits) - 1
    }

    /// A bijection of `0..2^bits`: xorshifts and an odd multiply, each
    /// invertible within the bits.
    fn mix(&self, mut value: u64) -> u64 {
        let half = self.bits / 2;
        value ^= value >> half;
        value = value.wrapping_mul(0x9E37_79B9_7F4A_7C15) & self.mask();
        value ^ (value >> half)
    }
}

impl Iterator for ScrambledIndices {
    type Item = u64;

    fn next(&mut self) -> Option<u64> {
        if self.remaining == 0 {
            return None;
        }
        loop {
            // Multiplier 1 mod 4 and an odd increment: one cycle through all
            // of `0..2^bits` (Hull–Dobell).
            self.state = self
                .state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407)
                & self.mask();
            let index = self.mix(self.state);
            if index < self.count {
                self.remaining -= 1;
                return Some(index);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrambled_indices_are_each_index_once() {
        for count in [1, 2, 3, 1000, 4096, 50_001] {
            let mut seen = vec![false; count as usize];
            for index in ScrambledIndices::new(count) {
                assert!(!seen[index as usize], "{index} twice of {count}");
                seen[index as usize] = true;
            }
            assert!(seen.iter().all(|&seen| seen), "an index missing of {count}");
        }
    }

    #[test]
    fn scrambled_indices_are_not_in_order() {
        let first: Vec<u64> = ScrambledIndices::new(1_000_000).take(5).collect();
        assert!(first.windows(2).any(|pair| pair[0] > pair[1]));
    }

    #[test]
    fn keys_ascend_with_their_index() {
        let count = 1000;
        for index in 1..count {
            assert!(KeyKind::url_key(index - 1, count) < KeyKind::url_key(index, count));
            assert!(KeyKind::id_key(index - 1) < KeyKind::id_key(index));
        }
    }
}

/// What builds the tree: the workload shown, the build under way, and the
/// tree it built. Owned by the app, handed to what starts builds.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Builder {
    tree: Signal<Option<LoadedTree>>,
    workload: Signal<Workload>,
    progress: Signal<Option<f64>>,
    took: Signal<Option<f64>>,
}

impl Builder {
    pub(crate) fn new(
        tree: Signal<Option<LoadedTree>>,
        workload: Signal<Workload>,
        progress: Signal<Option<f64>>,
        took: Signal<Option<f64>>,
    ) -> Self {
        Self {
            tree,
            workload,
            progress,
            took,
        }
    }

    pub(crate) fn workload(&self) -> Workload {
        *self.workload.read()
    }

    /// Whether a built tree is shown.
    pub(crate) fn is_built(&self) -> bool {
        self.tree.read().is_some()
    }

    /// The built tree, if one is shown.
    pub(crate) fn tree(&self) -> Option<LoadedTree> {
        self.tree.peek().clone()
    }

    /// The share of the build done, while one runs.
    fn progress(&self) -> Option<f64> {
        *self.progress.read()
    }

    /// How long the last build took, in seconds.
    fn took(&self) -> Option<f64> {
        *self.took.read()
    }

    /// Builds the tree of `workload` anew, unless a build is under way, and
    /// then looks up the key `linked` names, or one at random.
    pub(crate) fn build(&self, workload: Workload, linked: Option<LinkedLookup>, tracer: Tracer) {
        if self.progress.peek().is_some() {
            return;
        }
        let (mut tree, mut shown, mut progress, mut took) =
            (self.tree, self.workload, self.progress, self.took);
        let mut tracer = tracer;
        shown.set(workload);
        spawn(async move {
            // Drop the old tree first: at ten million entries, two don't fit.
            tracer.clear();
            tree.set(None);
            took.set(None);
            progress.set(Some(0.0));
            let start = web_time::Instant::now();
            let built = workload.build(move |done| progress.set(Some(done))).await;
            took.set(Some(start.elapsed().as_secs_f64()));
            tracer.start(&built, linked.as_ref());
            tree.set(Some(built));
            progress.set(None);
        });
    }
}

/// The tree being shown, as a toolbar button that opens its build
/// controls: key kind, size and order. Any change builds the tree anew, so
/// the controls always say what is shown, and they wait while a build
/// runs: at ten million entries, two trees don't fit. A finished build
/// looks up a key at random, so there is a path to see.
#[component]
pub(crate) fn WorkloadMenu(builder: Builder, tracer: Tracer) -> Element {
    let mut open = use_signal(|| false);
    let choose = move |chosen: Workload| {
        if chosen != *builder.workload.peek() {
            builder.build(chosen, None, tracer);
        }
    };
    let current = builder.workload();
    let building = builder.progress();
    rsx! {
        div { class: "menu workload-menu",
            button {
                id: "workload-trigger",
                class: "toolbar-pill",
                "aria-haspopup": "dialog",
                "aria-expanded": if open() { "true" } else { "false" },
                title: "Change the tree",
                onclick: move |_| open.set(!open()),
                span { class: "pill-strong", {current.kind().label()} }
                span { class: "data", "{Group(current.count())}" }
                span { class: "pill-quiet", {current.order().label()} }
                if let Some(done) = building {
                    span { class: "fill-track", "aria-hidden": "true",
                        span { class: "fill-bar", style: "transform:scaleX({done});" }
                    }
                }
                IconGlyph { icon: Icon::Disclose }
            }
            Popover {
                open,
                label: "The tree to build",
                trigger: "workload-trigger",
                align_end: false,
                fieldset { class: "popover-field",
                    legend { class: "popover-heading", "Keys" }
                    div { class: "segmented",
                        for kind in KeyKind::ALL {
                            label { class: "segment",
                                input {
                                    class: "sr-only",
                                    r#type: "radio",
                                    name: "key-kind",
                                    checked: kind == current.kind(),
                                    disabled: building.is_some(),
                                    onchange: move |_| choose(Workload::new(kind, current.count(), current.order())),
                                }
                                span { {kind.label()} }
                            }
                        }
                    }
                    p { class: "popover-note", {current.kind().shows()} }
                }
                div { class: "popover-row",
                    label { class: "popover-field",
                        span { class: "popover-heading", "How many" }
                        select {
                            class: "select",
                            disabled: building.is_some(),
                            onchange: move |evt| {
                                let count = evt.value().parse().unwrap_or(current.count());
                                choose(Workload::new(current.kind(), count, current.order()));
                            },
                            for count in Workload::COUNTS.into_iter().filter(|&count| count <= current.kind().max_count()) {
                                option { value: "{count}", selected: count == current.count(), "{Group(count)}" }
                            }
                        }
                    }
                    label { class: "popover-field",
                        span { class: "popover-heading", "Built by" }
                        select {
                            class: "select",
                            disabled: building.is_some(),
                            onchange: move |evt| {
                                let order = InsertOrder::ALL
                                    .into_iter()
                                    .find(|order| order.label() == evt.value())
                                    .unwrap_or(InsertOrder::Random);
                                choose(Workload::new(current.kind(), current.count(), order));
                            },
                            for order in InsertOrder::ALL {
                                option { value: order.label(), selected: order == current.order(), {order.label()} }
                            }
                        }
                    }
                }
                p { class: "popover-note", {current.order().shows()} }
                p { class: "popover-footnote data", role: "status",
                    if let Some(done) = building {
                        "building… {done * 100.0:.0}%"
                    } else if let Some(seconds) = builder.took() {
                        "built in {seconds:.1} s, then read once, node by node"
                    }
                }
            }
        }
    }
}
