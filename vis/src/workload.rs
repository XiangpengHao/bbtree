//! What the visualizer builds: a key type, how many entries, in what order,
//! and the build itself, in chunks between which the page stays responsive.

use bbtree::BBTreeMap;
use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;

use crate::count_text::Group;
use crate::inspected_tree::{InspectedMap, LoadedTree, VisKey};
use crate::trace::Tracer;

/// The kinds of key on offer, each picked for what bbtree does with it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum KeyKind {
    /// `u64`: integers, with 8-byte inline keys (D31).
    Integers,
    /// `String` URLs sharing long prefixes: fence prefixes, local search
    /// prefixes and length tags (D26, D33, D36).
    Urls,
    /// `[u8; 16]` ids: 2-byte summaries, children before keys (D32, D39).
    Ids,
}

impl KeyKind {
    pub(crate) const ALL: [KeyKind; 3] = [KeyKind::Integers, KeyKind::Urls, KeyKind::Ids];

    pub(crate) fn label(self) -> &'static str {
        match self {
            KeyKind::Integers => "u64",
            KeyKind::Urls => "String URLs",
            KeyKind::Ids => "[u8; 16] ids",
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

/// The build controls: key kind, size and order, and the build in flight.
/// A finished build looks up a key at random, so there is a path to see.
#[component]
pub(crate) fn WorkloadPicker(tree: Signal<Option<LoadedTree>>, tracer: Tracer) -> Element {
    let mut workload =
        use_signal(|| Workload::new(KeyKind::Integers, 1_000_000, InsertOrder::Random));
    let progress = use_signal(|| None::<f64>);
    let took = use_signal(|| None::<f64>);
    let build = use_callback(move |workload: Workload| {
        let (mut tree, mut tracer, mut progress, mut took) = (tree, tracer, progress, took);
        spawn(async move {
            // Drop the old tree first: at ten million entries, two don't fit.
            tracer.clear();
            tree.set(None);
            took.set(None);
            progress.set(Some(0.0));
            let start = web_time::Instant::now();
            let built = workload.build(move |done| progress.set(Some(done))).await;
            took.set(Some(start.elapsed().as_secs_f64()));
            tracer.run_random(&built);
            tree.set(Some(built));
            progress.set(None);
        });
    });
    use_hook(move || build.call(*workload.peek()));

    let current = workload();
    let building = progress();
    rsx! {
        div { class: "flex flex-wrap items-center gap-2",
            select {
                class: "select",
                "aria-label": "Key type",
                onchange: move |evt| {
                    let kind = KeyKind::ALL
                        .into_iter()
                        .find(|kind| kind.label() == evt.value())
                        .unwrap_or(KeyKind::Integers);
                    workload.set(Workload::new(kind, current.count(), current.order()));
                },
                for kind in KeyKind::ALL {
                    option { value: kind.label(), selected: kind == current.kind(), {kind.label()} }
                }
            }
            select {
                class: "select",
                "aria-label": "Entries",
                onchange: move |evt| {
                    let count = evt.value().parse().unwrap_or(current.count());
                    workload.set(Workload::new(current.kind(), count, current.order()));
                },
                for count in Workload::COUNTS.into_iter().filter(|&count| count <= current.kind().max_count()) {
                    option { value: "{count}", selected: count == current.count(), "{Group(count)} entries" }
                }
            }
            select {
                class: "select",
                "aria-label": "Insert order",
                onchange: move |evt| {
                    let order = InsertOrder::ALL
                        .into_iter()
                        .find(|order| order.label() == evt.value())
                        .unwrap_or(InsertOrder::Random);
                    workload.set(Workload::new(current.kind(), current.count(), order));
                },
                for order in InsertOrder::ALL {
                    option { value: order.label(), selected: order == current.order(), {order.label()} }
                }
            }
            button {
                class: "button-primary",
                disabled: building.is_some(),
                onclick: move |_| build.call(current),
                "Build"
            }
            if let Some(done) = building {
                span { class: "data flex items-center gap-2 text-[12px] text-[var(--graphite)]",
                    span { class: "fill-track w-24",
                        span { class: "fill-bar", style: "width:{done * 100.0}%;" }
                    }
                    "building"
                }
            } else if let Some(seconds) = took() {
                span {
                    class: "data text-[12px] text-[var(--muted)]",
                    title: "The inserts, then one read of every node",
                    "built in {seconds:.1} s"
                }
            }
        }
    }
}
