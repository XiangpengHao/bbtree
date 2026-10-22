//! A batch lookup as bbtree's `get_many` runs it (`TreeView::trace_many`,
//! doc 12): a lane for each key, which descends as a single-key lookup does
//! but, before it reads a line it may not have, prefetches the line and
//! yields; and every poll the executor made of a lane, in its order, with
//! the line the lane fetched before it yielded. Plain data for the panels,
//! read from bbtree, never recomputed.
//!
//! The panels count time in turns: each poll of a lane is a turn on the
//! core, from one prefetch to the next, in the order the batch runs them on
//! its one core. A prefetched line is on its way from its turn to its key's
//! next, which reads it; the trace says nothing of when the line arrives.

use bbtree::inspect::{LaneFetch, TraceOutcome};

use crate::inspected_tree::LoadedTree;
use crate::timeline::Timeline;
use crate::trace::Trace;
use crate::tree_shape::NodeAt;

/// One batch lookup.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct BatchRun {
    /// Each lane's key looked up alone (`TreeView::trace`): the way down a
    /// lane takes, and its result.
    lanes: Vec<Trace>,
    polls: Vec<LanePoll>,
    /// Each poll's lane's next poll, which reads the line it fetched.
    next: Vec<Option<usize>>,
    /// Whether `get_many` runs lanes on this map: whether it is past the
    /// size below which it looks keys up one at a time.
    batches: bool,
}

/// One poll of a lane: it ran from where it last yielded to its next fetch,
/// or to its end.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct LanePoll {
    lane: usize,
    /// What the lane fetched before it yielded; `None` if the poll ended it.
    fetched: Option<Fetched>,
}

/// A line a lane fetched: what for, and where it lies, if in a node's page.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Fetched {
    fetch: LaneFetch,
    /// The node and the line, counted from its page's start; `None` for
    /// the query's bytes or a key's on the heap.
    line: Option<(NodeAt, usize)>,
}

/// How a lane ended, from its key's lookup.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LaneEnd {
    /// The key lacks what every key of the map shares (D50): no node read.
    Outside,
    /// Found as a separator in an internal node (D17).
    Separator,
    /// The leaf's inline keys, or its keys, rule the key out.
    Absent,
    /// Found in its leaf.
    Found,
}

impl Fetched {
    pub(crate) fn new(fetch: LaneFetch, line: Option<(NodeAt, usize)>) -> Self {
        Self { fetch, line }
    }

    pub(crate) fn fetch(&self) -> LaneFetch {
        self.fetch
    }

    pub(crate) fn line(&self) -> Option<(NodeAt, usize)> {
        self.line
    }
}

impl LanePoll {
    pub(crate) fn new(lane: usize, fetched: Option<Fetched>) -> Self {
        Self { lane, fetched }
    }

    pub(crate) fn lane(&self) -> usize {
        self.lane
    }

    pub(crate) fn fetched(&self) -> Option<Fetched> {
        self.fetched
    }
}

impl BatchRun {
    /// When, in its turn, a key's prefetch goes out: at its end.
    pub(crate) const FETCH_AT: f64 = 0.9;

    pub(crate) fn new(lanes: Vec<Trace>, polls: Vec<LanePoll>, batches: bool) -> Self {
        let mut next = vec![None; polls.len()];
        let mut last: Vec<Option<usize>> = vec![None; lanes.len()];
        for (index, poll) in polls.iter().enumerate() {
            if let Some(previous) = last[poll.lane] {
                next[previous] = Some(index);
            }
            last[poll.lane] = Some(index);
        }
        Self {
            lanes,
            polls,
            next,
            batches,
        }
    }

    /// Where poll `index` starts on the time axis: one turn a unit.
    pub(crate) fn start(&self, index: usize) -> f64 {
        index as f64
    }

    /// Where the last turn ends.
    pub(crate) fn duration(&self) -> f64 {
        self.polls.len() as f64
    }

    /// When poll `index`'s fetch goes out.
    pub(crate) fn fetched_at(&self, index: usize) -> f64 {
        self.start(index) + Self::FETCH_AT
    }

    /// When the line poll `index` fetched is read: its lane's next poll.
    pub(crate) fn read_at(&self, index: usize) -> Option<f64> {
        self.next[index].map(|next| self.start(next))
    }

    /// The batch as a timeline to play: a step a poll, about eight seconds
    /// a batch but no poll a blur or a crawl, the first round slow enough
    /// to follow each lane's first poll.
    pub(crate) fn timeline(&self) -> Timeline {
        let polls = self.polls.len();
        let lanes = self.lanes.len();
        let first_round = if lanes < polls {
            self.start(lanes)
        } else {
            self.duration()
        };
        Timeline::new(
            (0..polls).map(|poll| self.start(poll)).collect(),
            self.duration(),
            (8000.0 / self.duration().max(1.0)).clamp(70.0, 260.0),
        )
        .with_slow_start(first_round, 560.0)
    }

    pub(crate) fn lanes(&self) -> &[Trace] {
        &self.lanes
    }

    pub(crate) fn polls(&self) -> &[LanePoll] {
        &self.polls
    }

    pub(crate) fn batches(&self) -> bool {
        self.batches
    }

    /// How lane `lane` ended.
    pub(crate) fn end(&self, lane: usize) -> LaneEnd {
        let trace = &self.lanes[lane];
        let ended_at = trace.steps().iter().find(|step| {
            matches!(
                step.outcome(),
                TraceOutcome::Found { .. } | TraceOutcome::Absent { .. }
            )
        });
        match ended_at {
            None => LaneEnd::Outside,
            Some(step) if step.at().level() > 0 => LaneEnd::Separator,
            Some(step) if step.found().is_some() => LaneEnd::Found,
            Some(_) => LaneEnd::Absent,
        }
    }

    /// The nodes the batch reads, each level's in key order, the root's
    /// level first: the root, which every lane searches unless its key lies
    /// outside the map's keys, and every node whose page holds a line some
    /// lane fetched.
    pub(crate) fn nodes(&self, root: Option<NodeAt>) -> Vec<Vec<NodeAt>> {
        let mut nodes: Vec<NodeAt> = self
            .polls
            .iter()
            .filter_map(|poll| poll.fetched?.line.map(|(at, _)| at))
            .collect();
        let searched_root = (0..self.lanes.len()).any(|lane| self.end(lane) != LaneEnd::Outside);
        nodes.extend(root.filter(|_| searched_root));
        nodes.sort_by_key(|at| (std::cmp::Reverse(at.level()), at.index()));
        nodes.dedup();
        let mut levels: Vec<Vec<NodeAt>> = Vec::new();
        for at in nodes {
            match levels.last_mut() {
                Some(level) if level[0].level() == at.level() => level.push(at),
                _ => levels.push(vec![at]),
            }
        }
        levels
    }
}

/// Where a lane stands once a poll has started.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct LaneMoment {
    /// The node it reads or waits on: the root before its first poll, and
    /// none for a key that lacks what every key shares.
    node: Option<NodeAt>,
    /// Its last poll by then, as an index into the run's polls.
    last: Option<usize>,
    /// Whether it has its result.
    ended: bool,
}

impl LaneMoment {
    pub(crate) fn node(&self) -> Option<NodeAt> {
        self.node
    }

    pub(crate) fn last(&self) -> Option<usize> {
        self.last
    }

    pub(crate) fn ended(&self) -> bool {
        self.ended
    }
}

impl BatchRun {
    /// Where every lane stands once poll `now` has started; before any, with
    /// `None`.
    pub(crate) fn moments(&self, root: Option<NodeAt>, now: Option<usize>) -> Vec<LaneMoment> {
        let mut moments: Vec<LaneMoment> = (0..self.lanes.len())
            .map(|lane| LaneMoment {
                node: root.filter(|_| self.end(lane) != LaneEnd::Outside),
                last: None,
                ended: false,
            })
            .collect();
        let polled = now.map_or(0, |now| now + 1).min(self.polls.len());
        for (index, poll) in self.polls[..polled].iter().enumerate() {
            let moment = &mut moments[poll.lane];
            moment.last = Some(index);
            match poll.fetched {
                Some(fetched) => {
                    if let Some((at, _)) = fetched.line {
                        moment.node = Some(at);
                    }
                }
                None => moment.ended = true,
            }
        }
        moments
    }
}

/// The keys a batch looks up, as the lookup bar takes them.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct BatchKeys {
    keys: Vec<String>,
}

impl BatchKeys {
    /// How many keys a batch looks up: few enough to follow every one,
    /// enough for their lines to be fetched together.
    pub(crate) const LANES: usize = 4;

    pub(crate) fn new(keys: Vec<String>) -> Self {
        Self { keys }
    }

    /// Keys of `tree` picked by `seed`, and, where the map's keys leave
    /// gaps between them, the last one a key it lacks.
    pub(crate) fn random(tree: &LoadedTree, seed: u64) -> Self {
        let mut state = seed | 1;
        let keys = (0..Self::LANES)
            .filter_map(|lane| {
                // xorshift64*: enough to wander the tree.
                state ^= state >> 12;
                state ^= state << 25;
                state ^= state >> 27;
                let key = tree.random_key(state.wrapping_mul(0x2545_F491_4F6C_DD1D))?;
                Some(match tree.key_kind().missing_after(&key) {
                    Some(missing) if lane + 1 == Self::LANES => missing,
                    _ => key,
                })
            })
            .collect();
        Self { keys }
    }

    pub(crate) fn keys(&self) -> &[String] {
        &self.keys
    }

    /// The same keys, key `index` replaced by `key`.
    pub(crate) fn with_key(&self, index: usize, key: String) -> Self {
        let mut keys = self.keys.clone();
        keys[index] = key;
        Self { keys }
    }
}
