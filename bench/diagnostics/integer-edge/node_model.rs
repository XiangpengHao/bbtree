//! Structural access model only: no latency, cache simulation, or mutation.
#![forbid(unsafe_code)]

use core::cmp::Ordering;
use core::fmt::Debug;
use core::mem::{align_of, size_of};
use core::ops::Range;

use super::handle::{
    ForceResult, Immut, Internal, LeafOrInternal, NodeKeys, NodeRef, SearchResult,
};
use super::inline_keys::SearchKey;
use super::layout::NodeLayout;
use crate::{Auto, InlineKey};

#[derive(Clone, Copy, Debug)]
struct Geometry {
    capacity: usize,
    summary_start: usize,
    summary_end: usize,
    key_size: usize,
    key_start: usize,
    edge_start: usize,
    edge_size: usize,
}

impl Geometry {
    fn layouts<K: InlineKey>() -> [Self; 2] {
        assert_eq!(K::WIDTH.bytes(), 2);
        let layout = NodeLayout::<K, u64>::internal::<Auto>();
        assert_eq!(layout.page.size(), 1024);
        assert_eq!(layout.page.align(), 64);
        let capacity = layout.keys.capacity;
        assert!(capacity < 64);
        let summary_start = layout.keys.inline_keys_offset.unwrap();
        let summary_end = summary_start + 2 * capacity;
        let original = Self {
            capacity,
            summary_start,
            summary_end,
            key_size: size_of::<K>(),
            key_start: layout.keys.keys_offset,
            edge_start: layout.edges_offset,
            edge_size: size_of::<usize>(),
        };
        assert_eq!(layout.keys.key_stride, original.key_size);
        assert_eq!(layout.edge_stride, original.edge_size);
        assert!(original.key_start >= summary_end);
        assert!(original.edge_start >= original.key_start + capacity * original.key_size);
        let edge_start = summary_end.next_multiple_of(align_of::<usize>());
        let edges_end = edge_start + (capacity + 1) * original.edge_size;
        let key_start = edges_end.next_multiple_of(align_of::<K>());
        assert!(key_start + capacity * original.key_size <= layout.page.size());
        let reordered = Self {
            key_start,
            edge_start,
            ..original
        };
        [original, reordered]
    }

    fn summaries(self) -> Lines {
        // Includes the header read; padding does not add another cache line.
        Lines::range(0, self.summary_end)
    }

    fn key(self, index: usize) -> Lines {
        assert!(index < self.capacity);
        Lines::range(self.key_start + index * self.key_size, self.key_size)
    }

    fn edge(self, index: usize) -> Lines {
        assert!(index <= self.capacity);
        Lines::range(self.edge_start + index * self.edge_size, self.edge_size)
    }

    fn all_live_edges(self, len: usize) -> Lines {
        assert!(len <= self.capacity);
        Lines::range(self.edge_start, (len + 1) * self.edge_size)
    }
}

#[derive(Clone, Copy, Default)]
struct Lines(u16);

impl Lines {
    fn range(start: usize, len: usize) -> Self {
        assert!(len > 0 && start + len <= 1024);
        let first = start / 64;
        let last = (start + len - 1) / 64;
        let mut bits = 0;
        for line in first..=last {
            bits |= 1_u16 << line;
        }
        Self(bits)
    }

    fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
    fn count(self) -> usize {
        self.0.count_ones() as usize
    }
    fn overlaps(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
}

#[derive(Clone, Copy, Default)]
struct Footprint {
    summaries: Lines,
    keys: Lines,
    edges: Lines,
}

impl Footprint {
    fn all(self) -> Lines {
        self.summaries.union(self.keys).union(self.edges)
    }
}

#[derive(Clone, Copy)]
struct CandidateTrace {
    indexes: [usize; 8],
    len: usize,
}

impl CandidateTrace {
    fn new() -> Self {
        Self {
            indexes: [0; 8],
            len: 0,
        }
    }

    fn compare<K: Ord>(&mut self, keys: NodeKeys<'_, K>, query: &K, index: usize) -> Ordering {
        assert!(self.len < self.indexes.len());
        self.indexes[self.len] = index;
        self.len += 1;
        keys[index].cmp(query)
    }

    // Mirrors NodeRef::branchless_search_index exactly. Every logical probe
    // is recorded, including the final comparison; cache-line unions dedup it.
    fn search<K: Ord>(
        &mut self,
        keys: NodeKeys<'_, K>,
        query: &K,
        run: Range<usize>,
    ) -> Result<usize, usize> {
        let mut size = run.len();
        if size == 0 {
            return Err(run.start);
        }
        let mut base = 0;
        while size > 1 {
            let half = size / 2;
            let mid = base + half;
            if self.compare(keys, query, run.start + mid) != Ordering::Greater {
                base = mid;
            }
            size -= half;
        }
        let index = run.start + base;
        match self.compare(keys, query, index) {
            Ordering::Equal => Ok(index),
            Ordering::Less => Err(index + 1),
            Ordering::Greater => Err(index),
        }
    }

    fn unique(self) -> usize {
        let mut indexes = 0_u64;
        for index in self.indexes[..self.len].iter().copied() {
            indexes |= 1 << index;
        }
        indexes.count_ones() as usize
    }

    fn footprint(self, geometry: Geometry, edge: usize) -> Footprint {
        let mut footprint = Footprint {
            summaries: geometry.summaries(),
            edges: geometry.edge(edge),
            ..Footprint::default()
        };
        for index in self.indexes[..self.len].iter().copied() {
            footprint.keys = footprint.keys.union(geometry.key(index));
        }
        footprint
    }
}

#[derive(Clone, Copy)]
struct LevelMetrics {
    searches: usize,
    live_keys: usize,
    summary_slots: usize,
    full_comparisons: usize,
    unique_candidates: usize,
    separator_hits: usize,
    run_lengths: [usize; 65],
    prefixes: [usize; 17],
    edge_indexes: [usize; 65],
    candidate_indexes: [usize; 64],
    search_lines: [usize; 2],
    key_lines_beyond_summary: [usize; 2],
    edge_lines_beyond_search: [usize; 2],
    edges_overlapping_summaries: [usize; 2],
    crossing_keys: [usize; 2],
    crossing_edges: [usize; 2],
    search_delta: [usize; 33],
    head_walks: usize,
    head_lines: [usize; 2],
}

impl Default for LevelMetrics {
    fn default() -> Self {
        Self {
            searches: 0,
            live_keys: 0,
            summary_slots: 0,
            full_comparisons: 0,
            unique_candidates: 0,
            separator_hits: 0,
            run_lengths: [0; 65],
            prefixes: [0; 17],
            edge_indexes: [0; 65],
            candidate_indexes: [0; 64],
            search_lines: [0; 2],
            key_lines_beyond_summary: [0; 2],
            edge_lines_beyond_search: [0; 2],
            edges_overlapping_summaries: [0; 2],
            crossing_keys: [0; 2],
            crossing_edges: [0; 2],
            search_delta: [0; 33],
            head_walks: 0,
            head_lines: [0; 2],
        }
    }
}

impl LevelMetrics {
    fn record(
        &mut self,
        layouts: [Geometry; 2],
        live: usize,
        prefix: usize,
        run: Range<usize>,
        candidates: CandidateTrace,
        result: Result<usize, usize>,
    ) {
        let edge = match result {
            Ok(index) => index + 1,
            Err(index) => index,
        };
        self.searches += 1;
        self.live_keys += live;
        self.summary_slots += layouts[0].capacity;
        self.full_comparisons += candidates.len;
        self.unique_candidates += candidates.unique();
        self.separator_hits += usize::from(result.is_ok());
        self.run_lengths[run.len()] += 1;
        self.prefixes[prefix] += 1;
        self.edge_indexes[edge] += 1;
        for index in candidates.indexes[..candidates.len].iter().copied() {
            self.candidate_indexes[index] += 1;
        }
        let mut counts = [0; 2];
        for (variant, layout) in layouts.into_iter().enumerate() {
            let footprint = candidates.footprint(layout, edge);
            counts[variant] = footprint.all().count();
            self.search_lines[variant] += counts[variant];
            self.key_lines_beyond_summary[variant] +=
                footprint.summaries.union(footprint.keys).count() - footprint.summaries.count();
            self.edge_lines_beyond_search[variant] +=
                footprint.all().count() - footprint.summaries.union(footprint.keys).count();
            self.edges_overlapping_summaries[variant] +=
                usize::from(footprint.edges.overlaps(footprint.summaries));
            self.crossing_edges[variant] += usize::from(footprint.edges.count() > 1);
            for index in candidates.indexes[..candidates.len].iter().copied() {
                self.crossing_keys[variant] += usize::from(layout.key(index).count() > 1);
            }
        }
        self.search_delta[(counts[1] as isize - counts[0] as isize + 16) as usize] += 1;
    }

    fn print(&self, height: usize) {
        println!(
            "height={height} searches={} mean_live={:.4} summary_slots={} comparisons={} unique_candidates={} separator_hits={} old_lines={} new_lines={} old_key_added={} new_key_added={} old_edge_added={} new_edge_added={} old_edge_summary_overlap={} new_edge_summary_overlap={} key_crossings={:?} edge_crossings={:?} head_walks={} head_lines={:?}",
            self.searches,
            self.live_keys as f64 / self.searches.max(1) as f64,
            self.summary_slots,
            self.full_comparisons,
            self.unique_candidates,
            self.separator_hits,
            self.search_lines[0],
            self.search_lines[1],
            self.key_lines_beyond_summary[0],
            self.key_lines_beyond_summary[1],
            self.edge_lines_beyond_search[0],
            self.edge_lines_beyond_search[1],
            self.edges_overlapping_summaries[0],
            self.edges_overlapping_summaries[1],
            self.crossing_keys,
            self.crossing_edges,
            self.head_walks,
            self.head_lines
        );
        println!(
            "height={height} run_lengths={:?}",
            Self::nonzero(&self.run_lengths)
        );
        println!(
            "height={height} prefixes={:?}",
            Self::nonzero(&self.prefixes)
        );
        println!(
            "height={height} edge_indexes={:?}",
            Self::nonzero(&self.edge_indexes)
        );
        println!(
            "height={height} candidate_indexes={:?}",
            Self::nonzero(&self.candidate_indexes)
        );
        let deltas: Vec<_> = self
            .search_delta
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, n)| *n > 0)
            .map(|(i, n)| (i as isize - 16, n))
            .collect();
        println!("height={height} new_minus_old_lines={deltas:?}");
    }

    fn nonzero(histogram: &[usize]) -> Vec<(usize, usize)> {
        histogram
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, n)| *n > 0)
            .collect()
    }
}

pub(crate) struct LookupTrace {
    layouts: [Geometry; 2],
    levels: [LevelMetrics; 16],
}

impl LookupTrace {
    pub(crate) fn new<K: InlineKey>() -> Self {
        Self {
            layouts: Geometry::layouts::<K>(),
            levels: [LevelMetrics::default(); 16],
        }
    }

    pub(crate) fn lookup<K: Copy + Ord + Debug + InlineKey>(
        &mut self,
        mut node: NodeRef<Immut<'_>, K, u64, Auto, LeafOrInternal>,
        query: &K,
    ) -> Option<u64> {
        let search = SearchKey::new(query);
        loop {
            match node.force() {
                ForceResult::Leaf(leaf) => {
                    return leaf.search_point(&search).map(|kv| *kv.into_key_value().1);
                }
                ForceResult::Internal(internal) => {
                    assert!(internal.local_prefix_len().is_none());
                    let slots = internal.inline_keys().unwrap();
                    let target = query.inline_key_at(internal.prefix_len()) & 0xffff_0000_0000_0000;
                    let low = (0..internal.len())
                        .take_while(|&i| slots.inline_key(i) < target)
                        .count();
                    let high = low
                        + (low..internal.len())
                            .take_while(|&i| slots.inline_key(i) == target)
                            .count();
                    let run = low..high;
                    let mut candidates = CandidateTrace::new();
                    let modeled = candidates.search(internal.keys(), query, run.clone());
                    let actual = internal.search(&search);
                    let expected = match &actual {
                        SearchResult::Found(kv) => Ok(kv.index()),
                        SearchResult::GoDown(edge) => Err(edge.index()),
                    };
                    assert_eq!(
                        modeled,
                        expected,
                        "height={} query={query:?}",
                        internal.height()
                    );
                    self.levels[internal.height()].record(
                        self.layouts,
                        internal.len(),
                        internal.prefix_len(),
                        run,
                        candidates,
                        modeled,
                    );
                    match actual {
                        SearchResult::GoDown(edge) => node = edge.descend(),
                        SearchResult::Found(kv) => {
                            assert_eq!(kv.into_key(), query);
                            return Some(self.resolve_head(kv.right_edge().descend()));
                        }
                    }
                }
            }
        }
    }

    fn resolve_head<K: InlineKey>(
        &mut self,
        mut node: NodeRef<Immut<'_>, K, u64, Auto, LeafOrInternal>,
    ) -> u64 {
        loop {
            match node.force() {
                ForceResult::Leaf(leaf) => return *leaf.head().unwrap().into_value(),
                ForceResult::Internal(internal) => {
                    let metrics = &mut self.levels[internal.height()];
                    metrics.head_walks += 1;
                    // Release head resolution knows the height and uses edge0;
                    // it does not search or load the node's header/summaries.
                    for (i, layout) in self.layouts.into_iter().enumerate() {
                        metrics.head_lines[i] += layout.edge(0).count();
                    }
                    node = internal.first_edge().descend();
                }
            }
        }
    }

    pub(crate) fn print(&self, operation: &str, samples: usize) {
        let total = self.levels.iter().fold([0_usize; 2], |mut sum, level| {
            for i in 0..2 {
                sum[i] += level.search_lines[i] + level.head_lines[i];
            }
            sum
        });
        println!(
            "operation={operation} samples={samples} old_internal_lines={} new_internal_lines={} saved_per_query={:.6}",
            total[0],
            total[1],
            (total[0] as f64 - total[1] as f64) / samples as f64
        );
        for (height, level) in self.levels.iter().enumerate() {
            if level.searches > 0 || level.head_walks > 0 {
                level.print(height);
            }
        }
    }
}

#[derive(Clone, Copy, Default)]
struct ShapeLevel {
    nodes: usize,
    keys: usize,
    possible_route_lines: [usize; 2],
}

pub(crate) struct TreeShape {
    layouts: [Geometry; 2],
    levels: [ShapeLevel; 16],
}

impl TreeShape {
    pub(crate) fn new<K: InlineKey>() -> Self {
        Self {
            layouts: Geometry::layouts::<K>(),
            levels: [ShapeLevel::default(); 16],
        }
    }

    pub(crate) fn visit<K: Copy + Ord + InlineKey>(
        &mut self,
        node: NodeRef<Immut<'_>, K, u64, Auto, LeafOrInternal>,
    ) {
        let height = node.height();
        self.levels[height].nodes += 1;
        self.levels[height].keys += node.len();
        if let ForceResult::Internal(internal) = node.force() {
            assert!(internal.local_prefix_len().is_none());
            let summaries = internal.inline_keys().unwrap();
            for index in 0..internal.len() {
                let expected = internal.keys()[index].inline_key_at(internal.prefix_len())
                    & 0xffff_0000_0000_0000;
                assert_eq!(summaries.inline_key(index), expected);
                if index > 0 {
                    assert!(summaries.inline_key(index - 1) <= expected);
                }
            }
            for (i, layout) in self.layouts.into_iter().enumerate() {
                self.levels[height].possible_route_lines[i] += layout
                    .summaries()
                    .union(layout.all_live_edges(internal.len()))
                    .count();
            }
            for index in 0..=internal.len() {
                self.visit(internal.edge_at(index).descend());
            }
        }
    }

    pub(crate) fn print(&self) {
        println!(
            "old_layout={:?} edge_first_layout={:?}",
            self.layouts[0], self.layouts[1]
        );
        for (height, level) in self
            .levels
            .iter()
            .enumerate()
            .filter(|(_, level)| level.nodes > 0)
        {
            println!(
                "shape height={height} nodes={} keys={} mean_keys={:.6} old_possible_route_lines={} new_possible_route_lines={}",
                level.nodes,
                level.keys,
                level.keys as f64 / level.nodes as f64,
                level.possible_route_lines[0],
                level.possible_route_lines[1]
            );
        }
    }
}
