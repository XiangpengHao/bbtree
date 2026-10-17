//! A census of a map's separators and of the ties its lookups meet, for
//! `doc/11-separator-key.md`, "Measure first" (1) and (2): per level, how
//! many separators would be exact, and how often a lookup's inline key ties
//! with a separator's, or with one that isn't exact; and how many misses
//! the tree's shared prefix places before any node is read (D50).
//!
//! It walks the tree through the `inspect` views, and traces lookups with
//! the map's own searches (`TreeView::trace`); nothing is timed. The keys
//! and the insertion order are those of the bench's single-map cells.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::process::ExitCode;

use bbtree::inspect::{NodeId, NodeView, TraceOutcome, TreeView};
use bbtree::unsync::BBTreeMap;
use bbtree::{Auto, InlineKey};

use crate::keys::{BenchKey, KeyMaker};
use crate::random::{RandomStream, SeededRng};

/// The inline-key widths a node could keep, in bytes.
const WIDTHS: [usize; 3] = [2, 4, 8];

/// How many leading bytes of two encodings a census compares at most: past
/// it, a separator is counted as not fitting any width.
const MAX_COMPARED_BYTES: usize = 4096;

/// A key's order-preserving encoding, read a window at a time through its
/// `InlineKey` impl, as the map's nodes read it.
struct Encoding;

impl Encoding {
    /// How many leading bytes the encodings of `a` and `b` share, both
    /// padded with zeros.
    fn common_prefix_len<K: InlineKey>(a: &K, b: &K) -> usize {
        let mut offset = 0;
        while offset < MAX_COMPARED_BYTES {
            let differing_bits = a.inline_key_at(offset) ^ b.inline_key_at(offset);
            if differing_bits != 0 {
                return offset + differing_bits.leading_zeros() as usize / 8;
            }
            offset += 8;
        }
        MAX_COMPARED_BYTES
    }

    fn byte_at<K: InlineKey>(key: &K, index: usize) -> u8 {
        (key.inline_key_at(index) >> 56) as u8
    }
}

/// How many bytes past its node's prefix the shortest separator between two
/// neighbouring keys needs (11-separator-key.md, "Exact separators"), or
/// `None` if no width holds it: keys of varying length whose separator ends
/// in a zero byte, which padding would blur.
#[derive(Clone, Copy)]
struct SeparatorFit {
    bytes_past_prefix: Option<usize>,
}

impl SeparatorFit {
    /// The fit of the separator between `left_max` and `right_min`, the
    /// first key of a child whose upper fence is `high`, in a node whose
    /// windows start at `prefix_len`.
    fn new<K: InlineKey>(
        left_max: &K,
        right_min: &K,
        high: Option<&K>,
        prefix_len: usize,
        fixed_width: bool,
    ) -> Self {
        let differing = Encoding::common_prefix_len(left_max, right_min);
        let kept_for_child = high.map_or(0, |high| Encoding::common_prefix_len(right_min, high));
        let len = (differing + 1).max(kept_for_child);
        let blurred = !fixed_width && Encoding::byte_at(right_min, len - 1) == 0;
        let bytes_past_prefix =
            (!blurred && len < MAX_COMPARED_BYTES).then(|| len.saturating_sub(prefix_len));
        Self { bytes_past_prefix }
    }

    fn is_exact_at(self, width: usize) -> bool {
        self.bytes_past_prefix.is_some_and(|bytes| bytes <= width)
    }
}

/// What lookups of one kind, hits or misses, met at one level.
#[derive(Default)]
struct LookupCounts {
    lookups: usize,
    /// Searches whose inline key tied with at least one separator's.
    ties: usize,
    /// The separators those ties involved.
    tied_separators: usize,
    /// Ties with at least one separator that isn't exact at the node's
    /// width: the ones that would still read a key.
    inexact_ties: usize,
}

impl LookupCounts {
    fn percent(count: usize, of: usize) -> f64 {
        100.0 * count as f64 / of.max(1) as f64
    }

    fn row(&self) -> String {
        format!(
            "{:.2}% | {:.2} | {:.2}%",
            Self::percent(self.ties, self.lookups),
            self.tied_separators as f64 / self.ties.max(1) as f64,
            Self::percent(self.inexact_ties, self.lookups),
        )
    }
}

/// What the census found at one level of the tree.
#[derive(Default)]
struct LevelCounts {
    nodes: usize,
    separators: usize,
    /// The width the nodes at this level keep.
    width: Option<usize>,
    /// Separators exact at each of [`WIDTHS`].
    exact: [usize; WIDTHS.len()],
    hits: LookupCounts,
    misses: LookupCounts,
    needs: WidthNeeds,
}

/// How many bytes past its fence prefix a node's inline keys would need, by
/// condition, one entry per node or separator.
#[derive(Default)]
struct WidthNeeds {
    /// Per node of two or more separators: for no two of them to share an
    /// inline key, one byte past the longest prefix neighbours share.
    distinct: Vec<usize>,
    /// Per separator: for its predecessor key, and so every key of the map,
    /// not to share its inline key, one byte past what the two share.
    no_key_ties: Vec<usize>,
    /// Per node of two or more separators: the bytes all of them share, an
    /// inline key no wider than which ties for every search.
    all_shared: Vec<usize>,
}

impl WidthNeeds {
    /// The `q`-th quantile of `values`, which it sorts.
    fn quantile(values: &mut [usize], q: f64) -> usize {
        if values.is_empty() {
            return 0;
        }
        values.sort_unstable();
        let index = ((values.len() - 1) as f64 * q).round() as usize;
        values[index]
    }

    /// `p50 / p90 / p99 / max` of `values`.
    fn quantiles(values: &mut [usize]) -> String {
        let [p50, p90, p99, max] = [0.5, 0.9, 0.99, 1.0].map(|q| Self::quantile(values, q));
        format!("{p50} / {p90} / {p99} / {max}")
    }

    /// The share of `values` at most `bound`.
    fn share_at_most(values: &[usize], bound: usize) -> f64 {
        LookupCounts::percent(
            values.iter().filter(|&&value| value <= bound).count(),
            values.len(),
        )
    }

    /// The share of `values` at least `bound`.
    fn share_at_least(values: &[usize], bound: usize) -> f64 {
        LookupCounts::percent(
            values.iter().filter(|&&value| value >= bound).count(),
            values.len(),
        )
    }
}

/// The census of one map.
struct SeparatorCensus<'a, K> {
    tree: TreeView<'a, K, u64, Auto>,
    /// The map's keys in order, to find each separator's predecessor.
    sorted_keys: Vec<&'a K>,
    fixed_width: bool,
    fits: HashMap<(NodeId, usize), SeparatorFit>,
    /// By depth below the root; the leaves last.
    levels: Vec<LevelCounts>,
    /// Misses the tree's shared prefix placed, reading no node (D50).
    misses_placed_by_shared_prefix: usize,
    misses: usize,
}

impl<'a, K: BenchKey + InlineKey> SeparatorCensus<'a, K> {
    fn new(map: &'a BBTreeMap<K, u64>, fixed_width: bool) -> Self {
        Self {
            tree: map.inspect(),
            sorted_keys: map.keys().collect(),
            fixed_width,
            fits: HashMap::new(),
            levels: Vec::new(),
            misses_placed_by_shared_prefix: 0,
            misses: 0,
        }
    }

    /// Counts every node and separator, from the root down.
    fn survey(&mut self) {
        if let Some(root) = self.tree.root() {
            self.survey_node(root, None);
        }
    }

    /// The counts of the level `node` is at, which the survey adds as it
    /// first reaches it.
    fn level_of(&mut self, node: &NodeView<'a, K, u64, Auto>) -> &mut LevelCounts {
        let depth = node.depth();
        if self.levels.len() <= depth {
            self.levels.resize_with(depth + 1, LevelCounts::default);
        }
        &mut self.levels[depth]
    }

    fn survey_node(&mut self, node: NodeView<'a, K, u64, Auto>, high: Option<&'a K>) {
        let counts = self.level_of(&node);
        counts.nodes += 1;
        if node.is_leaf() {
            return;
        }
        counts.width = node.inline_key_width();
        let fence_prefix_len = node.prefix_len();
        let len = node.len();
        if len >= 2 {
            let longest_shared_by_neighbours = (0..len - 1)
                .map(|index| Encoding::common_prefix_len(node.key(index), node.key(index + 1)))
                .max()
                .unwrap_or(0);
            let shared_by_all = Encoding::common_prefix_len(node.key(0), node.key(len - 1));
            let needs = &mut counts.needs;
            needs
                .distinct
                .push((longest_shared_by_neighbours + 1).saturating_sub(fence_prefix_len));
            needs
                .all_shared
                .push(shared_by_all.saturating_sub(fence_prefix_len));
        }
        let prefix_len = node.prefix_len();
        for index in 0..node.len() {
            let right_min = node.key(index);
            let right_high = (index + 1 < node.len())
                .then(|| node.key(index + 1))
                .or(high);
            let position = self
                .sorted_keys
                .binary_search(&right_min)
                .expect("a separator is a key");
            let left_max = self.sorted_keys[position - 1];
            let shared_with_predecessor = Encoding::common_prefix_len(left_max, right_min);
            self.level_of(&node)
                .needs
                .no_key_ties
                .push((shared_with_predecessor + 1).saturating_sub(fence_prefix_len));
            let fit = SeparatorFit::new(
                left_max,
                right_min,
                right_high,
                prefix_len,
                self.fixed_width,
            );
            let counts = self.level_of(&node);
            counts.separators += 1;
            for (exact, &width) in counts.exact.iter_mut().zip(&WIDTHS) {
                *exact += usize::from(fit.is_exact_at(width));
            }
            self.fits.insert((node.id(), index), fit);
        }
        for index in 0..=node.len() {
            let child_high = (index < node.len()).then(|| node.key(index)).or(high);
            let child = node.child(index).expect("an internal node's child");
            self.survey_node(child, child_high);
        }
    }

    /// Traces a lookup of `key`, counting what each internal node's search
    /// met, as a hit or a miss.
    fn trace(&mut self, key: &K, is_hit: bool) {
        if !is_hit {
            self.misses += 1;
            let placed = self.tree.placed_by_shared_prefix(key).is_some();
            self.misses_placed_by_shared_prefix += usize::from(placed);
        }
        for step in self.tree.trace(key) {
            let node = step.node();
            let searched = matches!(
                step.outcome(),
                TraceOutcome::Descend { .. } | TraceOutcome::Found { .. }
            );
            if node.is_leaf() || !searched {
                continue;
            }
            let width = node.inline_key_width().unwrap_or(0);
            let tied = step.tied().filter(|run| !run.is_empty());
            let inexact = tied.clone().is_some_and(|mut run| {
                run.any(|index| !self.fits[&(node.id(), index)].is_exact_at(width))
            });
            let level = self.level_of(&node);
            let counts = if is_hit {
                &mut level.hits
            } else {
                &mut level.misses
            };
            counts.lookups += 1;
            let Some(run) = tied else {
                continue;
            };
            counts.ties += 1;
            counts.tied_separators += run.len();
            counts.inexact_ties += usize::from(inexact);
        }
    }

    /// The inline-key widths each condition needs, per level: quantiles over
    /// nodes, or separators, and the shares the bench's widths meet.
    fn width_report(&mut self) -> String {
        let mut report = String::from(
            "\n### Bytes past the fence prefix: p50 / p90 / p99 / max\n\n\
             | depth | width | (a) separators distinct | distinct at 2 / 4 / 8 B | \
             (b) no key ties a separator | no ties at 2 / 4 / 8 B | \
             (c) shared by all separators | all tie at 2 / 4 B |\n\
             |--:|--:|--|--|--|--|--|--|\n",
        );
        let internal_levels = self.levels.len().saturating_sub(1);
        for (depth, counts) in self.levels.iter_mut().enumerate().take(internal_levels) {
            let needs = &mut counts.needs;
            let at_most = |values: &[usize]| {
                WIDTHS
                    .map(|width| format!("{:.1}%", WidthNeeds::share_at_most(values, width)))
                    .join(" / ")
            };
            let all_tie = [2, 4]
                .map(|width| {
                    format!(
                        "{:.1}%",
                        WidthNeeds::share_at_least(&needs.all_shared, width)
                    )
                })
                .join(" / ");
            let _ = writeln!(
                report,
                "| {depth} | {} | {} | {} | {} | {} | {} | {all_tie} |",
                counts
                    .width
                    .map_or("-".to_owned(), |width| width.to_string()),
                WidthNeeds::quantiles(&mut needs.distinct),
                at_most(&needs.distinct),
                WidthNeeds::quantiles(&mut needs.no_key_ties),
                at_most(&needs.no_key_ties),
                WidthNeeds::quantiles(&mut needs.all_shared),
            );
        }
        report
    }

    fn report(&self, profile: &str, entries: usize) -> String {
        let mut report = format!(
            "## {profile}, {entries} entries\n\n\
             | depth | nodes | separators | width | exact at 2 / 4 / 8 B | \
             hits: tie | sep./tie | inexact tie | \
             misses: tie | sep./tie | inexact tie |\n\
             |--:|--:|--:|--:|--|--:|--:|--:|--:|--:|--:|\n"
        );
        let internal_levels = self.levels.len().saturating_sub(1);
        for (depth, counts) in self.levels.iter().enumerate().take(internal_levels) {
            let exact = counts
                .exact
                .map(|exact| format!("{:.1}%", LookupCounts::percent(exact, counts.separators)))
                .join(" / ");
            let _ = writeln!(
                report,
                "| {depth} | {} | {} | {} | {exact} | {} | {} |",
                counts.nodes,
                counts.separators,
                counts
                    .width
                    .map_or("-".to_owned(), |width| width.to_string()),
                counts.hits.row(),
                counts.misses.row(),
            );
        }
        // Over the lookups that reached the leaves' parents.
        let leaf_parents = self.levels.len().saturating_sub(2);
        let per_lookup = |pick: fn(&LevelCounts) -> &LookupCounts,
                          count: fn(&LookupCounts) -> usize| {
            let lookups = self
                .levels
                .get(leaf_parents)
                .map_or(1, |counts| pick(counts).lookups.max(1));
            let total: usize = self.levels.iter().map(|counts| count(pick(counts))).sum();
            total as f64 / lookups as f64
        };
        let _ = writeln!(
            report,
            "\nPer lookup, all internal levels: hits {:.4} ties, {:.4} inexact; \
             misses {:.4} ties, {:.4} inexact. Leaves: {}. Every key shares \
             {} B; it placed {:.2}% of misses.",
            per_lookup(|counts| &counts.hits, |counts| counts.ties),
            per_lookup(|counts| &counts.hits, |counts| counts.inexact_ties),
            per_lookup(|counts| &counts.misses, |counts| counts.ties),
            per_lookup(|counts| &counts.misses, |counts| counts.inexact_ties),
            self.levels.last().map_or(0, |leaves| leaves.nodes),
            self.tree.shared_prefix().len(),
            LookupCounts::percent(self.misses_placed_by_shared_prefix, self.misses),
        );
        report
    }
}

/// One key profile's census: its keys, built and probed as the bench's
/// single-map cells do.
struct CensusRun<K> {
    present: Vec<K>,
    absent: Vec<K>,
}

impl<K: BenchKey + InlineKey> CensusRun<K> {
    /// `entries` keys at even indices, in the bench's random insertion
    /// order, and `probes` absent ones at odd indices, spread over them.
    fn new(mut keys: KeyMaker<K>, entries: usize, probes: usize) -> Self {
        let mut present: Vec<K> = (0..entries as u64)
            .map(|index| keys.key(2 * index))
            .collect();
        let stride = (entries / probes.max(1)).max(1) as u64;
        let absent = (0..probes as u64)
            .map(|position| keys.key(2 * position * stride + 1))
            .collect();
        SeededRng::new(RandomStream::InsertionOrder).shuffle(&mut present);
        Self { present, absent }
    }

    fn census(self, profile: &str, probes: usize, fixed_width: bool) -> String {
        let entries = self.present.len();
        let mut map = BBTreeMap::<K, u64>::new();
        for (index, key) in self.present.into_iter().enumerate() {
            map.insert(key, index as u64);
        }
        let mut census = SeparatorCensus::new(&map, fixed_width);
        census.survey();
        let mut rng = SeededRng::new(RandomStream::LookupOrder);
        let hits: Vec<K> = (0..probes)
            .map(|_| census.sorted_keys[rng.below(entries)].clone())
            .collect();
        for key in &hits {
            census.trace(key, true);
        }
        for key in &self.absent {
            census.trace(key, false);
        }
        let report = census.report(profile, entries);
        report + &census.width_report()
    }
}

/// Runs the census for the profile named by the first argument, at the
/// entry count of the second (10M by default), with as many hit and miss
/// probes as the third (1M by default).
pub fn run() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let number = |index: usize, default: usize| {
        arguments
            .get(index)
            .map_or(Ok(default), |text| text.replace('_', "").parse::<usize>())
    };
    let (Some(profile), Ok(entries), Ok(probes)) = (
        arguments.first(),
        number(1, 10_000_000),
        number(2, 1_000_000),
    ) else {
        eprintln!(
            "usage: bbtree-separator-census u64|u64-seq|str-json|str-path|hash32 [entries] [probes]"
        );
        return ExitCode::FAILURE;
    };
    let report =
        match profile.as_str() {
            "u64" => CensusRun::new(KeyMaker::random_u64(), entries, probes)
                .census(profile, probes, true),
            "u64-seq" => CensusRun::new(KeyMaker::sequential_u64(), entries, probes)
                .census(profile, probes, true),
            "str-json" => CensusRun::new(KeyMaker::json_keys(), entries, probes)
                .census(profile, probes, false),
            "str-path" => CensusRun::new(KeyMaker::registry_paths(entries), entries, probes)
                .census(profile, probes, false),
            "hash32" => CensusRun::new(KeyMaker::random_hashes(), entries, probes)
                .census(profile, probes, true),
            unknown => {
                eprintln!("unknown profile {unknown}");
                return ExitCode::FAILURE;
            }
        };
    print!("{report}");
    ExitCode::SUCCESS
}
