//! A key's search, as `find` in bbtree's `tree/batch.rs` reads, in a few
//! lines: each `.await` follows a prefetch of the line the search reads
//! next, and lets the other keys take their turns while it comes. Every
//! key sits in the gutter of the line it stopped at; the key on the core
//! lights the lines its turn ran. Below the board, what that turn did, in a
//! sentence.

use bbtree::inspect::LaneFetch;
use dioxus::prelude::*;

use crate::batch_run::{BatchRun, LaneEnd};
use crate::inspected_tree::LoadedTree;

use crate::tree_shape::TreeShape;

/// A line of the program.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ProgramLine {
    Start,
    QueryWindow,
    LoopOpen,
    Search,
    Edge,
    Child,
    Header,
    LoopClose,
    LeafSearch,
    Candidate,
    CandidateBytes,
    Compare,
}

impl ProgramLine {
    const ALL: [ProgramLine; 12] = [
        ProgramLine::Start,
        ProgramLine::QueryWindow,
        ProgramLine::LoopOpen,
        ProgramLine::Search,
        ProgramLine::Edge,
        ProgramLine::Child,
        ProgramLine::Header,
        ProgramLine::LoopClose,
        ProgramLine::LeafSearch,
        ProgramLine::Candidate,
        ProgramLine::CandidateBytes,
        ProgramLine::Compare,
    ];

    /// The line's code, as its indent and its tokens, each with its kind.
    fn code(self) -> (usize, &'static [(Token, &'static str)]) {
        use Token::{Await, Compute, Keyword, Plain, Prefetch};
        match self {
            ProgramLine::Start => (0, &[(Keyword, "async fn "), (Plain, "search(key) {")]),
            ProgramLine::QueryWindow => (
                1,
                &[
                    (Prefetch, "prefetch"),
                    (Plain, "(key bytes)"),
                    (Await, ".await"),
                ],
            ),
            ProgramLine::LoopOpen => (1, &[(Keyword, "loop"), (Plain, " {")]),
            ProgramLine::Search => (
                2,
                &[
                    (Plain, "edge = "),
                    (Compute, "search"),
                    (Plain, "(node, key)"),
                ],
            ),
            ProgramLine::Edge => (
                2,
                &[
                    (Prefetch, "prefetch"),
                    (Plain, "(edge line)"),
                    (Await, ".await"),
                ],
            ),
            ProgramLine::Child => (
                2,
                &[
                    (Plain, "node = "),
                    (Compute, "child"),
                    (Plain, "(node, edge)"),
                ],
            ),
            ProgramLine::Header => (
                2,
                &[
                    (Prefetch, "prefetch"),
                    (Plain, "(node.header)"),
                    (Await, ".await"),
                ],
            ),
            ProgramLine::LoopClose => (
                1,
                &[
                    (Plain, "} "),
                    (Keyword, "while"),
                    (Plain, " node is internal"),
                ],
            ),
            ProgramLine::LeafSearch => (
                1,
                &[
                    (Plain, "slot = "),
                    (Compute, "search"),
                    (Plain, "(leaf, key)"),
                ],
            ),
            ProgramLine::Candidate => (
                1,
                &[
                    (Prefetch, "prefetch"),
                    (Plain, "(entry(slot))"),
                    (Await, ".await"),
                ],
            ),
            ProgramLine::CandidateBytes => (
                1,
                &[
                    (Prefetch, "prefetch"),
                    (Plain, "(key_bytes(slot))"),
                    (Await, ".await"),
                ],
            ),
            ProgramLine::Compare => (1, &[(Compute, "compare"), (Plain, "(key, slot)")]),
        }
    }

    /// What the line does, beside it.
    fn comment(self) -> &'static str {
        match self {
            ProgramLine::Start => "each key runs this",
            ProgramLine::QueryWindow => "string keys only",
            ProgramLine::LoopOpen => "from the root down",
            ProgramLine::Search => "count smaller inline keys",
            ProgramLine::Edge => "the child's address",
            ProgramLine::Child => "read the address",
            ProgramLine::Header => "where its search starts",
            ProgramLine::LoopClose => "",
            ProgramLine::LeafSearch => "one candidate, or absent",
            ProgramLine::Candidate => "its key and value",
            ProgramLine::CandidateBytes => "long strings: heap bytes",
            ProgramLine::Compare => "found, or not in the map",
        }
    }

    fn is_fetch(self) -> bool {
        self.code()
            .1
            .iter()
            .any(|(token, _)| *token == Token::Prefetch)
    }

    /// The fetch a line makes that only some keys need: lines no lane of
    /// the batch runs are dimmed.
    fn only_for(self) -> Option<LaneFetch> {
        match self {
            ProgramLine::QueryWindow => Some(LaneFetch::QueryWindow),
            ProgramLine::CandidateBytes => Some(LaneFetch::CandidateBytes),
            _ => None,
        }
    }

    /// Where a lane stops after fetching `fetch`; `leaf` for a header
    /// fetched of a leaf.
    fn of_fetch(fetch: LaneFetch) -> Self {
        match fetch {
            LaneFetch::QueryWindow => ProgramLine::QueryWindow,
            LaneFetch::Edge => ProgramLine::Edge,
            LaneFetch::Header => ProgramLine::Header,
            LaneFetch::CandidateEntry => ProgramLine::Candidate,
            LaneFetch::CandidateBytes => ProgramLine::CandidateBytes,
        }
    }
}

/// What a piece of a program line is, for its color.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Token {
    Plain,
    Keyword,
    /// Work on lines already at hand: a search, reading an address, a
    /// compare.
    Compute,
    /// A request for the line read next, which doesn't wait for it.
    Prefetch,
    /// Where the key lets the others take their turns until its own comes
    /// again.
    Await,
}

impl Token {
    fn class(self) -> &'static str {
        match self {
            Token::Plain => "tok-plain",
            Token::Keyword => "tok-keyword",
            Token::Compute => "tok-compute",
            Token::Prefetch => "tok-prefetch",
            Token::Await => "tok-await",
        }
    }
}

/// Where a lane is in its program: the line it stopped at, and whether it
/// has ended there.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct ProgramPlace {
    line: ProgramLine,
    ended: bool,
    /// Whether the node it last fetched a header of is a leaf.
    in_leaf: bool,
}

impl ProgramPlace {
    const START: Self = Self {
        line: ProgramLine::Start,
        ended: false,
        in_leaf: false,
    };

    /// Where the lane stops after a poll from here that fetched
    /// `fetched`, a header of a leaf if `leaf`, or that ended with `end`.
    fn after(self, fetched: Option<(LaneFetch, bool)>, end: LaneEnd) -> Self {
        match fetched {
            Some((fetch, leaf)) => Self {
                line: ProgramLine::of_fetch(fetch),
                ended: false,
                in_leaf: self.in_leaf || (fetch == LaneFetch::Header && leaf),
            },
            None => Self {
                line: match (end, self.line) {
                    (LaneEnd::Outside, _) => ProgramLine::Start,
                    (LaneEnd::Separator, _) => ProgramLine::Search,
                    (_, ProgramLine::Candidate | ProgramLine::CandidateBytes) => {
                        ProgramLine::Compare
                    }
                    _ => ProgramLine::LeafSearch,
                },
                ended: true,
                in_leaf: self.in_leaf,
            },
        }
    }

    /// The lines a poll from `self` to `to` runs: the code between the
    /// two places, around the loop where it goes back up.
    fn ran_to(self, to: Self) -> Vec<ProgramLine> {
        self.path_to(to).unwrap_or_else(|| vec![to.line])
    }

    /// The lines from `self` to `to`, if `find` in `tree/batch.rs` goes
    /// from one to the other in a poll.
    fn path_to(self, to: Self) -> Option<Vec<ProgramLine>> {
        use ProgramLine as L;
        let path: &[ProgramLine] = match (self.line, to.line) {
            (L::Start, L::QueryWindow) => &[L::QueryWindow],
            (L::Start, L::Start) => &[L::Start],
            (L::Start | L::QueryWindow, L::Edge) => &[L::LoopOpen, L::Search, L::Edge],
            (L::Start | L::QueryWindow, L::Search) => &[L::LoopOpen, L::Search],
            (L::Header, L::Edge) if !self.in_leaf => &[L::Search, L::Edge],
            (L::Header, L::Search) if !self.in_leaf => &[L::Search],
            (L::Edge, L::Header) => &[L::Child, L::Header],
            (L::Header, L::Candidate) if self.in_leaf => &[L::LeafSearch, L::Candidate],
            (L::Header, L::LeafSearch) if self.in_leaf => &[L::LeafSearch],
            (L::Candidate, L::CandidateBytes) => &[L::CandidateBytes],
            (L::Candidate | L::CandidateBytes, L::Compare) => &[L::Compare],
            _ => return None,
        };
        Some(path.to_vec())
    }
}

/// Every lane's place once a poll has started, and what that poll ran.
struct ProgramMoment {
    places: Vec<ProgramPlace>,
    /// The lane polled now, and the lines its poll ran.
    running: Option<(usize, Vec<ProgramLine>)>,
}

impl ProgramMoment {
    fn of(run: &BatchRun, now: Option<usize>) -> Self {
        let mut places = vec![ProgramPlace::START; run.lanes().len()];
        let mut running = None;
        let polled = now.map_or(0, |now| now + 1).min(run.polls().len());
        for (index, poll) in run.polls()[..polled].iter().enumerate() {
            let lane = poll.lane();
            let fetched = poll.fetched().map(|fetched| {
                let leaf = fetched.line().is_some_and(|(at, _)| at.level() == 0);
                (fetched.fetch(), leaf)
            });
            let from = places[lane];
            let to = from.after(fetched, run.end(lane));
            if Some(index) == now {
                running = Some((lane, from.ran_to(to)));
            }
            places[lane] = to;
        }
        Self { places, running }
    }
}

/// The current search, in a sentence.
struct PollStory<'a> {
    run: &'a BatchRun,
    shape: &'a TreeShape,
}

impl PollStory<'_> {
    fn tell(&self, now: Option<usize>) -> String {
        let Some(now) = now else {
            return String::new();
        };
        let poll = self.run.polls()[now];
        let key = poll.lane();
        let trace = &self.run.lanes()[key];
        let step_at = |at| trace.step_at(at);
        let name = |at| self.shape.node_name(at);
        let Some(fetched) = poll.fetched() else {
            let last = trace.steps().last();
            let at = last.map(|step| step.at());
            return match self.run.end(key) {
                LaneEnd::Outside => {
                    format!("Key {key} lacks the bytes every key shares: not in the map.")
                }
                LaneEnd::Separator => {
                    let separator = trace
                        .steps()
                        .iter()
                        .find(|step| step.found().is_some())
                        .map(|step| name(step.at()))
                        .unwrap_or_default();
                    format!("Key {key} is a separator in {separator}: found.")
                }
                LaneEnd::Found => format!(
                    "Key {key} compares the candidate in full: found in {}, slot {}.",
                    at.map(name).unwrap_or_default(),
                    last.and_then(|step| step.found()).unwrap_or(0)
                ),
                LaneEnd::Absent => format!(
                    "Key {key} isn't in {}: not in the map.",
                    at.map(name).unwrap_or_default()
                ),
            };
        };
        match (fetched.fetch(), fetched.line()) {
            (LaneFetch::QueryWindow, _) => {
                format!("Key {key} prefetches its own bytes that the root compares.")
            }
            (LaneFetch::Edge, Some((at, line))) => {
                let edge = step_at(at).and_then(|step| step.edge_taken()).unwrap_or(0);
                format!(
                    "Key {key} searches {} and goes down child {edge}: it prefetches line {line}, which holds that child's address.",
                    name(at)
                )
            }
            (LaneFetch::Header, Some((at, _))) => format!(
                "Key {key} reads the child's address, {}, and prefetches its header line.",
                name(at)
            ),
            (LaneFetch::CandidateEntry, Some((at, line))) => {
                let slot = step_at(at)
                    .and_then(|step| step.tied())
                    .map_or(0, |tied| tied.start);
                format!(
                    "Key {key} searches {}'s inline keys: only slot {slot} can match. It prefetches that entry, line {line}.",
                    name(at)
                )
            }
            (LaneFetch::CandidateBytes, _) => format!(
                "Key {key} reads the candidate's entry and prefetches the key's bytes the compare reads."
            ),
            (_, None) => format!("Key {key} prefetches a line."),
        }
    }
}

#[component]
pub(crate) fn LaneProgram(
    run: BatchRun,
    now: Signal<Option<usize>>,
    pointed: Signal<Option<usize>>,
) -> Element {
    let moment = ProgramMoment::of(&run, now());
    let lit = pointed();
    let (running_lane, ran) = moment
        .running
        .clone()
        .map_or((None, Vec::new()), |(lane, ran)| (Some(lane), ran));
    let fetches: Vec<LaneFetch> = run
        .polls()
        .iter()
        .filter_map(|poll| poll.fetched())
        .map(|fetched| fetched.fetch())
        .collect();
    rsx! {
            section {
                class: "lane-program",
                "aria-label": "The code each key runs, with each key at the line it stopped at",
                h2 { class: "batch-heading",
        "Every key runs this, in turns on a "
        strong { "single core" }
    }
                ol { class: "program",
                    for line in ProgramLine::ALL {
                        {
                            let (indent, tokens) = line.code();
                            let here: Vec<(usize, ProgramPlace)> = moment
                                .places
                                .iter()
                                .copied()
                                .enumerate()
                                .filter(|(_, place)| place.line == line)
                                .collect();
                            let mut class = String::from("program-line");
                            if line.is_fetch() {
                                class += " fetch";
                            }
                            if line.only_for().is_some_and(|fetch| !fetches.contains(&fetch)) {
                                class += " unused";
                            }
                            if ran.contains(&line) {
                                class += " ran";
                            }
                            if ran.last() == Some(&line) {
                                class += " stop";
                            }
                            rsx! {
                                li { class,
                                    span { class: "program-gutter",
                                        for (lane , place) in here.iter().copied() {
                                            span {
                                                class: format!(
                                                    "gutter-lane{}{}{}",
                                                    if place.ended { " ended" } else { "" },
                                                    if running_lane == Some(lane) { " running" } else { "" },
                                                    if lit == Some(lane) { " lit" } else { "" },
                                                ),
                                                "{lane}"
                                            }
                                        }
                                    }
                                    code { class: "program-code", style: "--indent:{indent};",
                                        for (token , text) in tokens.iter() {
                                            span { class: token.class(), {text.to_string()} }
                                        }
                                    }
                                    span { class: "program-comment", {line.comment()} }
                                }
                            }
                        }
                    }
                    li { class: "program-line",
                        span { class: "program-gutter" }
                        code { class: "program-code", style: "--indent:0;",
                            span { class: "tok-plain", "}}" }
                        }
                        span { class: "program-comment" }
                    }
                }
                p { class: "program-legend",
                    span { class: "legend-item",
                        span { class: "legend-swatch-text tok-compute", "search" }
                        "works on lines already here"
                    }
                    span { class: "legend-item",
                        span { class: "legend-swatch-text tok-prefetch", "prefetch" }
                        "asks for the line needed next, without waiting"
                    }
                    span { class: "legend-item",
                        span { class: "legend-swatch-text tok-await", ".await" }
                        "the other keys take their turns until this key's next"
                    }
                }
            }
        }
}

/// The turn the batch is at, in a sentence, under the code.
#[component]
pub(crate) fn PollCaption(
    tree: LoadedTree,
    run: BatchRun,
    now: Signal<Option<usize>>,
    playing: Signal<bool>,
) -> Element {
    let story = PollStory {
        run: &run,
        shape: tree.shape(),
    }
    .tell(now());
    rsx! {
        p {
            class: "poll-story",
            // Read out as the reader steps, not ten times a second.
            "aria-live": if playing() { "off" } else { "polite" },
            {story}
        }
    }
}

#[cfg(test)]
mod tests {
    use bbtree::unsync::BBTreeMap;

    use super::*;
    use crate::batch_run::BatchKeys;
    use crate::inspected_tree::{InspectedMap, LoadedTree};
    use crate::workload::KeyKind;

    /// Every search of `tree`'s batches moves its key along the program as
    /// `find` runs: never between lines the program doesn't link.
    fn check_program(tree: LoadedTree) {
        for seed in 1..20 {
            let run = tree
                .trace_many(BatchKeys::random(&tree, seed).keys())
                .unwrap();
            let mut places = vec![ProgramPlace::START; run.lanes().len()];
            for poll in run.polls() {
                let lane = poll.lane();
                let fetched = poll.fetched().map(|fetched| {
                    let leaf = fetched.line().is_some_and(|(at, _)| at.level() == 0);
                    (fetched.fetch(), leaf)
                });
                let to = places[lane].after(fetched, run.end(lane));
                assert!(
                    places[lane].path_to(to).is_some(),
                    "{:?} to {to:?}",
                    places[lane]
                );
                places[lane] = to;
            }
            assert!(places.iter().all(|place| place.ended));
        }
    }

    #[test]
    fn the_program_follows_every_lane_of_real_batches() {
        let integers: BBTreeMap<u64, u64> =
            (0..60_000).map(|key| (key * 7 % 60_000, key)).collect();
        check_program(LoadedTree::new(InspectedMap::new(
            integers,
            KeyKind::Integers,
        )));
        let urls: BBTreeMap<String, u64> = (0..30_000)
            .map(|index| {
                (
                    format!("https://shop.example.com/users/{:09}", index * 2),
                    index,
                )
            })
            .collect();
        check_program(LoadedTree::new(InspectedMap::new(urls, KeyKind::Urls)));
    }
}
