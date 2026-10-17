//! A node's page as the processor fetches it: one row per 64-byte cache
//! line, every slot where the layout puts it, and what a lookup read, in the
//! order it read it: the header, every inline key at once, the tied keys,
//! then the result.

use std::ops::Range;

use bbtree::inspect::{PageMap, SlotContents, TraceOutcome};
use dioxus::prelude::*;

use crate::concepts::Concept;
use crate::node_inspector::NodeDetail;
use crate::spotlight::{Hint, NodePart, Spotlight};
use crate::trace::TraceStep;

/// Bytes per cache line, and so per row.
const LINE: usize = 64;
/// Pixels per byte across, and per line down.
const BYTE_WIDTH: f64 = 8.0;
const LINE_HEIGHT: f64 = 16.0;
/// Room left of the rows for their offsets and read marks, and above them
/// for the byte ruler.
const GUTTER: f64 = 54.0;
const TOP: f64 = 16.0;

/// What a lookup read of one node, by slot.
#[derive(Clone, PartialEq, Debug, Default)]
pub(crate) struct PageMarks {
    /// Whether the node was searched, which reads its length: a node on the
    /// way to a separator's value is only passed through.
    searched: bool,
    /// Whether the search read the inline keys: all of them, as one count
    /// over the capacity does on x86-64 (D18).
    read_inline_keys: bool,
    /// The keys whose inline keys tied with the searched key's: the search
    /// goes on among them, comparing full keys.
    tied: Range<usize>,
    /// The key that equals the searched key.
    found: Option<usize>,
    /// The child the lookup went down to.
    edge: Option<usize>,
    /// Whether the lookup read the leaf's head: a separator's value.
    head: bool,
    leaf: bool,
}

/// How surely the lookup read a slot.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Certainty {
    /// Read by a binary search among tied keys, if it probed there.
    Possible,
    Certain,
}

/// When in a node's search a read happens, in order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum ReadPhase {
    Header,
    InlineKeys,
    Ties,
    Result,
}

impl PageMarks {
    /// What the lookup read of a node from its trace step; `leaf` says
    /// whether the node is one.
    pub(crate) fn of_step(step: &TraceStep, leaf: bool) -> Self {
        Self {
            searched: !matches!(
                step.outcome(),
                TraceOutcome::ToHead { .. } | TraceOutcome::Head
            ),
            read_inline_keys: step.inline_key().is_some(),
            tied: step.tied().unwrap_or(0..0),
            found: step.found(),
            edge: step.edge_taken(),
            head: step.outcome() == TraceOutcome::Head,
            leaf,
        }
    }

    /// How many of `page`'s cache lines the lookup surely read, and how
    /// many more it may have read in its search among tied keys.
    pub(crate) fn lines_read(&self, page: &PageMap) -> (usize, usize) {
        let drawing = MapDrawing::of(page, self);
        (
            drawing.count(Certainty::Certain),
            drawing.count(Certainty::Possible),
        )
    }

    /// Whether slot `slot` of an array of `contents` is where the lookup
    /// ended.
    fn is_result(&self, contents: SlotContents, slot: usize) -> bool {
        match contents {
            SlotContents::Keys | SlotContents::InlineKeys | SlotContents::LengthTags => {
                self.found == Some(slot)
            }
            SlotContents::Values => self.leaf && self.found == Some(slot),
            SlotContents::Edges => self.edge == Some(slot),
            SlotContents::HeadKey | SlotContents::Head => self.head,
            SlotContents::Header | SlotContents::RootArea => false,
        }
    }

    /// Whether slot `slot` of an array of `contents` belongs to a tied key
    /// that isn't the result.
    fn is_candidate(&self, contents: SlotContents, slot: usize) -> bool {
        matches!(contents, SlotContents::Keys | SlotContents::InlineKeys)
            && self.tied.contains(&slot)
            && self.found != Some(slot)
    }

    /// Whether, how surely and when the lookup read slot `slot` of an
    /// array of `contents`, `used` if the node holds something there.
    fn read(
        &self,
        contents: SlotContents,
        slot: usize,
        used: bool,
    ) -> Option<(Certainty, ReadPhase)> {
        let certain = |phase| Some((Certainty::Certain, phase));
        match contents {
            SlotContents::Header | SlotContents::RootArea if self.searched => {
                certain(ReadPhase::Header)
            }
            SlotContents::InlineKeys if self.read_inline_keys => certain(ReadPhase::InlineKeys),
            _ if !used => None,
            _ if self.is_result(contents, slot) => certain(ReadPhase::Result),
            SlotContents::Keys if self.tied.contains(&slot) => match self.tied.len() {
                1 => certain(ReadPhase::Ties),
                _ => Some((Certainty::Possible, ReadPhase::Ties)),
            },
            SlotContents::LengthTags if self.tied.contains(&slot) && self.tied.len() > 1 => {
                certain(ReadPhase::Ties)
            }
            _ => None,
        }
    }
}

/// How the map draws, names and links what a slot array holds.
pub(crate) trait SlotLook: Copy {
    fn class(self) -> &'static str;
    fn label(self) -> &'static str;
    /// What the legend calls the array: a head is drawn as the values are.
    fn legend_label(self) -> &'static str;
    /// The idea the array stands for, which the legend lights up.
    fn concept(self) -> Option<Concept>;
    /// The part of the node slot `slot` of this array belongs to.
    fn part(self, slot: usize) -> Option<NodePart>;
    /// Whether slot `slot` of this array is what `concept` names, of a node
    /// marked with `marks`.
    fn shows(self, slot: usize, concept: Concept, marks: &PageMarks) -> bool;
}

impl SlotLook for SlotContents {
    fn class(self) -> &'static str {
        match self {
            SlotContents::Header | SlotContents::RootArea => "slot-header",
            SlotContents::InlineKeys => "slot-inline",
            SlotContents::LengthTags => "slot-tags",
            SlotContents::Keys | SlotContents::HeadKey => "slot-keys",
            SlotContents::Head | SlotContents::Values | SlotContents::Edges => "slot-trailing",
        }
    }

    fn label(self) -> &'static str {
        match self {
            SlotContents::Header => "header",
            SlotContents::RootArea => "root area",
            SlotContents::InlineKeys => "inline key",
            SlotContents::LengthTags => "length tag",
            SlotContents::Keys => "key",
            SlotContents::HeadKey => "head key",
            SlotContents::Head => "head",
            SlotContents::Values => "value",
            SlotContents::Edges => "child",
        }
    }

    fn legend_label(self) -> &'static str {
        match self {
            SlotContents::Header => "header",
            SlotContents::RootArea => "root area",
            SlotContents::InlineKeys => "inline keys",
            SlotContents::LengthTags => "length tags",
            SlotContents::Keys | SlotContents::HeadKey => "keys",
            SlotContents::Head | SlotContents::Values => "values",
            SlotContents::Edges => "children",
        }
    }

    fn concept(self) -> Option<Concept> {
        match self {
            SlotContents::Header | SlotContents::RootArea => None,
            SlotContents::InlineKeys => Some(Concept::InlineKey),
            SlotContents::LengthTags => Some(Concept::LengthTag),
            SlotContents::Keys => None,
            SlotContents::HeadKey | SlotContents::Head | SlotContents::Values => {
                Some(Concept::Head)
            }
            SlotContents::Edges => Some(Concept::Child),
        }
    }

    fn part(self, slot: usize) -> Option<NodePart> {
        match self {
            SlotContents::Header | SlotContents::RootArea => None,
            SlotContents::InlineKeys
            | SlotContents::LengthTags
            | SlotContents::Keys
            | SlotContents::Values => Some(NodePart::Entry(slot)),
            SlotContents::Edges => Some(NodePart::Child(slot)),
            SlotContents::HeadKey | SlotContents::Head => Some(NodePart::Head),
        }
    }

    fn shows(self, slot: usize, concept: Concept, marks: &PageMarks) -> bool {
        match concept {
            Concept::Tie => {
                marks.tied.contains(&slot)
                    && matches!(
                        self,
                        SlotContents::Keys | SlotContents::InlineKeys | SlotContents::LengthTags
                    )
            }
            Concept::FencePrefix => self == SlotContents::Header,
            // The root's prefix length is what every key shares; the bytes
            // are in the root area (D50).
            Concept::SharedPrefix => {
                matches!(self, SlotContents::Header | SlotContents::RootArea)
            }
            Concept::Separator => {
                (self == SlotContents::Keys && !marks.leaf) || self == SlotContents::HeadKey
            }
            Concept::Page | Concept::Wasm32 => true,
            concept => self.concept() == Some(concept),
        }
    }
}

/// One slot's piece within one line.
#[derive(PartialEq)]
struct SlotPiece {
    x: f64,
    y: f64,
    width: f64,
    contents: SlotContents,
    slot: usize,
    bytes: Range<usize>,
    used: bool,
    read: Option<(Certainty, ReadPhase)>,
    result: bool,
    candidate: bool,
}

/// What the lookup read of one line: the surest read of any slot in it,
/// and the earliest phase that read it so surely.
#[derive(Clone, Copy, PartialEq, Debug)]
struct LineRead {
    certainty: Certainty,
    phase: ReadPhase,
}

/// The pieces of every slot of a page, and which lines the lookup read.
struct MapDrawing {
    pieces: Vec<SlotPiece>,
    lines: Vec<Option<LineRead>>,
}

impl MapDrawing {
    fn of(page: &PageMap, marks: &PageMarks) -> Self {
        let mut lines: Vec<Option<LineRead>> = vec![None; page.size().div_ceil(LINE)];
        let mut pieces = Vec::new();
        for array in page.arrays() {
            if array.slot_size() == 0 {
                continue;
            }
            let contents = array.contents();
            for slot in 0..array.capacity() {
                let bytes = array.slot_bytes(slot);
                let used = slot < array.used();
                let read = marks.read(contents, slot, used);
                if let Some((certainty, phase)) = read {
                    for line in bytes.start / LINE..bytes.end.div_ceil(LINE) {
                        if let Some(slot_line) = lines.get_mut(line) {
                            *slot_line =
                                Some(Self::surer(*slot_line, LineRead { certainty, phase }));
                        }
                    }
                }
                let result = used && marks.is_result(contents, slot);
                let candidate = used && marks.is_candidate(contents, slot);
                pieces.extend(Self::split_by_line(&bytes).map(|piece| SlotPiece {
                    x: GUTTER + (piece.start % LINE) as f64 * BYTE_WIDTH + 0.5,
                    y: TOP + (piece.start / LINE) as f64 * LINE_HEIGHT + 1.5,
                    width: piece.len() as f64 * BYTE_WIDTH - 1.0,
                    contents,
                    slot,
                    bytes: bytes.clone(),
                    used,
                    read,
                    result,
                    candidate,
                }));
            }
        }
        Self { pieces, lines }
    }

    /// The surer of two reads of a line, the earlier if equally sure.
    fn surer(line: Option<LineRead>, read: LineRead) -> LineRead {
        match line {
            Some(line)
                if line.certainty > read.certainty
                    || line.certainty == read.certainty && line.phase <= read.phase =>
            {
                line
            }
            _ => read,
        }
    }

    fn count(&self, certainty: Certainty) -> usize {
        self.lines
            .iter()
            .filter(|line| line.is_some_and(|line| line.certainty == certainty))
            .count()
    }

    /// What the page's arrays hold, in page order, once each.
    fn legend(page: &PageMap) -> Vec<SlotContents> {
        let mut seen: Vec<SlotContents> = Vec::new();
        for array in page.arrays() {
            // Keys and the head's key, values and the head, share a hue
            // and a name.
            let contents = match array.contents() {
                SlotContents::Head => SlotContents::Values,
                SlotContents::HeadKey => SlotContents::Keys,
                contents => contents,
            };
            if !seen.contains(&contents) {
                seen.push(contents);
            }
        }
        seen
    }

    /// `bytes`, cut where cache lines start.
    fn split_by_line(bytes: &Range<usize>) -> impl Iterator<Item = Range<usize>> + '_ {
        let mut start = bytes.start;
        std::iter::from_fn(move || {
            (start < bytes.end).then(|| {
                let end = bytes.end.min((start / LINE + 1) * LINE);
                let piece = start..end;
                start = end;
                piece
            })
        })
    }
}

impl SlotPiece {
    /// The piece's classes: its array's hue, and what the lookup and the
    /// reader did with it.
    fn class(&self, lit: Option<bool>) -> String {
        let mut class = format!("piece {}", self.contents.class());
        if !self.used {
            class.push_str(" slot-empty");
        }
        if self.result {
            class.push_str(" slot-marked");
        } else if self.candidate {
            class.push_str(" slot-candidate");
        } else if self.read.is_some() && self.used {
            class.push_str(" slot-read");
        }
        match lit {
            Some(true) => class.push_str(" lit"),
            Some(false) => class.push_str(" unlit"),
            None => {}
        }
        class
    }

    /// When its mark appears, in the lookup's order of reads.
    fn delay(&self) -> u32 {
        self.read.map_or(0, |(_, phase)| ReadOrder::delay(phase))
    }
}

/// The pace of the reads' replay.
struct ReadOrder;

impl ReadOrder {
    const STEP_MS: u32 = 170;

    fn delay(phase: ReadPhase) -> u32 {
        Self::STEP_MS * phase as u32
    }
}

/// The node's page, one cache line per row, with what the lookup read of
/// it. Pointing at a slot lights it up in every panel; clicking one picks
/// its part.
#[component]
pub(crate) fn MemoryMap(
    detail: NodeDetail,
    marks: PageMarks,
    /// Changes with every lookup, so the reads replay.
    replay: u64,
    spotlight: Spotlight,
    on_pick: EventHandler<NodePart>,
) -> Element {
    let page = detail.page();
    let at = detail.at();
    let drawing = MapDrawing::of(page, &marks);
    let lines = drawing.lines.len();
    let width = GUTTER + LINE as f64 * BYTE_WIDTH + 2.0;
    let height = TOP + lines as f64 * LINE_HEIGHT + 2.0;
    let certain = drawing.count(Certainty::Certain);
    let possible = drawing.count(Certainty::Possible);
    let pointed = spotlight.part_of(at);
    let concept = spotlight.concept();
    let mut spotlight = spotlight;
    rsx! {
        figure { class: "memory-map-figure",
            figcaption { class: "map-headline",
                span { class: "map-count",
                    span { class: "map-count-number", "{certain}" }
                    " of {lines} "
                    span { class: "term-static", "cache lines" }
                    " read"
                }
                if possible > 0 {
                    span { class: "map-count-maybe", "+ up to {possible} more in the search among tied keys" }
                }
            }
            svg {
                class: if pointed.is_some() || concept.is_some() { "memory-map focused" } else { "memory-map" },
                view_box: "0 0 {width} {height}",
                "role": "img",
                "aria-label": "The page, {page.size()} bytes in {lines} cache lines of 64 bytes; the lookup read {certain} of them for sure",
                onmouseleave: move |_| spotlight.clear(),
                text {
                    class: "byte-tick",
                    x: GUTTER - 20.0,
                    y: TOP - 5.0,
                    text_anchor: "end",
                    "line"
                }
                for column in [0usize, 16, 32, 48] {
                    text {
                        class: "byte-tick",
                        x: GUTTER + column as f64 * BYTE_WIDTH + 1.0,
                        y: TOP - 5.0,
                        if column == 0 { "byte 0" } else { "{column}" }
                    }
                }
                for line in 0..lines {
                    rect {
                        class: "line-track",
                        x: GUTTER,
                        y: TOP + line as f64 * LINE_HEIGHT + 1.0,
                        width: LINE as f64 * BYTE_WIDTH,
                        height: LINE_HEIGHT - 2.0,
                    }
                    text {
                        class: "line-offset",
                        "data-line": "{line}",
                        x: GUTTER - 20.0,
                        y: TOP + line as f64 * LINE_HEIGHT + 11.5,
                        text_anchor: "end",
                        "{line}"
                    }
                }
                g { key: "{replay}-{at.level()}-{at.index()}",
                    for (line, read) in drawing.lines.iter().enumerate() {
                        if let Some(read) = read {
                            circle {
                                class: if read.certainty == Certainty::Certain { "line-read" } else { "line-read maybe" },
                                style: "animation-delay:{ReadOrder::delay(read.phase)}ms;",
                                cx: GUTTER - 10.0,
                                cy: TOP + line as f64 * LINE_HEIGHT + 8.0,
                                r: 3.75,
                            }
                        }
                    }
                }
                g { key: "{replay}-{at.level()}-{at.index()}",
                    for piece in drawing.pieces.iter() {
                        {
                            let part = piece.contents.part(piece.slot);
                            let lit = match (pointed, concept) {
                                (Some(pointed), _) => Some(part == Some(pointed)),
                                (None, Some(concept)) => Some(piece.contents.shows(piece.slot, concept, &marks)),
                                (None, None) => None,
                            };
                            let (contents, slot, bytes, used) = (piece.contents, piece.slot, piece.bytes.clone(), piece.used);
                            let detail = detail.clone();
                            rsx! {
                                rect {
                                    class: piece.class(lit),
                                    "data-slot": "{slot}",
                                    style: "animation-delay:{piece.delay()}ms;",
                                    x: piece.x,
                                    y: piece.y,
                                    width: piece.width,
                                    height: LINE_HEIGHT - 3.0,
                                    rx: 1.5,
                                    onmouseenter: move |evt| {
                                        let point = evt.client_coordinates();
                                        let hint = Hint::new(
                                            (point.x, point.y),
                                            format!("{} {slot}", contents.label()),
                                            detail.describe_slot(contents, slot, used, &bytes),
                                        );
                                        match part {
                                            Some(part) => spotlight.point_at_part(at, part, Some(hint)),
                                            None => spotlight.show_hint(hint),
                                        }
                                    },
                                    onmousemove: move |evt| {
                                        let point = evt.client_coordinates();
                                        spotlight.move_hint((point.x, point.y));
                                    },
                                    onclick: move |_| {
                                        if let Some(part) = part.filter(|_| used) {
                                            on_pick.call(part);
                                        }
                                    },
                                }
                            }
                        }
                    }
                }
            }
            div { class: "map-legend",
                for contents in MapDrawing::legend(page) {
                    span {
                        class: "legend-item",
                        onmouseenter: move |_| spotlight.light_concept(contents.concept()),
                        onmouseleave: move |_| spotlight.clear(),
                        svg { class: "legend-swatch", view_box: "0 0 12 12", "aria-hidden": "true",
                            rect { class: contents.class(), width: 12, height: 12, rx: 2 }
                        }
                        {contents.legend_label()}
                    }
                }
                span { class: "legend-item",
                    svg { class: "legend-swatch", view_box: "0 0 12 12", "aria-hidden": "true",
                        rect { class: "slot-keys slot-empty", width: 12, height: 12, rx: 2 }
                    }
                    "empty"
                }
                span { class: "legend-item",
                    svg { class: "legend-swatch", view_box: "0 0 12 12", "aria-hidden": "true",
                        rect { class: "slot-keys slot-marked", x: 1, y: 1, width: 10, height: 10, rx: 2 }
                    }
                    "where it ended"
                }
                span {
                    class: "legend-item",
                    onmouseenter: move |_| spotlight.light_concept(Some(Concept::Tie)),
                    onmouseleave: move |_| spotlight.clear(),
                    svg { class: "legend-swatch", view_box: "0 0 12 12", "aria-hidden": "true",
                        rect { class: "slot-keys slot-candidate", x: 1, y: 1, width: 10, height: 10, rx: 2 }
                    }
                    "tied"
                }
                span { class: "legend-item",
                    svg { class: "legend-swatch", view_box: "0 0 12 12", "aria-hidden": "true",
                        circle { class: "line-read still", cx: 6, cy: 6, r: 4 }
                    }
                    "line read"
                }
                if possible > 0 {
                    span { class: "legend-item",
                        svg { class: "legend-swatch", view_box: "0 0 12 12", "aria-hidden": "true",
                            circle { class: "line-read maybe still", cx: 6, cy: 6, r: 3.5 }
                        }
                        "maybe read"
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use bbtree::unsync::BBTreeMap;

    use super::*;

    fn marks(tied: Range<usize>, found: Option<usize>) -> PageMarks {
        PageMarks {
            searched: true,
            read_inline_keys: true,
            tied,
            found,
            edge: None,
            head: false,
            leaf: true,
        }
    }

    #[test]
    fn pieces_cover_each_slot_within_its_lines() {
        let map: BBTreeMap<String, u64> = (0..20_000u64)
            .map(|key| (format!("https://example.com/{key:08}"), key))
            .collect();
        let tree = map.inspect();
        let mut leaf = tree.root().unwrap();
        while let Some(child) = leaf.child(1) {
            leaf = child;
        }
        let page = leaf.page_map();
        let drawing = MapDrawing::of(&page, &marks(2..4, Some(3)));
        let right = GUTTER + LINE as f64 * BYTE_WIDTH;
        assert!(
            drawing
                .pieces
                .iter()
                .all(|piece| piece.x >= GUTTER && piece.x + piece.width <= right)
        );
        assert!(
            drawing.lines[0].is_some_and(|line| line.certainty == Certainty::Certain),
            "the header's line is always read"
        );
        assert!(drawing.pieces.iter().any(|piece| piece.result));
        let covered: usize = page
            .arrays()
            .iter()
            .filter(|array| array.slot_size() > 0)
            .map(|array| array.capacity() * array.slot_size())
            .sum();
        let drawn: f64 = drawing.pieces.iter().map(|piece| piece.width + 1.0).sum();
        assert_eq!(drawn, covered as f64 * BYTE_WIDTH);
    }

    #[test]
    fn a_search_among_ties_reads_them_only_maybe() {
        let map: BBTreeMap<u64, u64> = (0..1_000u64).map(|key| (key, key)).collect();
        let tree = map.inspect();
        let mut leaf = tree.root().unwrap();
        while let Some(child) = leaf.child(0) {
            leaf = child;
        }
        let page = leaf.page_map();
        let one = MapDrawing::of(&page, &marks(3..4, Some(3)));
        assert_eq!(one.count(Certainty::Possible), 0);
        let many = MapDrawing::of(&page, &marks(0..leaf.len(), Some(5)));
        assert!(many.count(Certainty::Possible) > 0);
        assert!(
            many.pieces
                .iter()
                .filter(|piece| piece.contents == SlotContents::Keys && piece.slot != 5)
                .all(|piece| !piece
                    .read
                    .is_some_and(|(certainty, _)| certainty == Certainty::Certain))
        );
    }

    #[test]
    fn slots_split_where_lines_start() {
        let pieces: Vec<_> = MapDrawing::split_by_line(&(60..70)).collect();
        assert_eq!(pieces, vec![60..64, 64..70]);
        let whole: Vec<_> = MapDrawing::split_by_line(&(0..8)).collect();
        assert_eq!(whole, vec![0..8]);
    }
}
