//! A node's page as the processor fetches it: one row per 64-byte cache
//! line, every slot where the layout puts it, and the slots a lookup read
//! marked as with a highlighter.

use std::ops::Range;

use bbtree::inspect::{PageMap, SlotContents};
use dioxus::prelude::*;

/// Bytes per cache line, and so per row.
const LINE: usize = 64;
/// Pixels per byte across, and per line down.
const BYTE_WIDTH: f64 = 8.0;
const LINE_HEIGHT: f64 = 15.0;
/// Room left of the rows for their offsets and marks.
const GUTTER: f64 = 52.0;

/// What a lookup read of a page, by key slot.
#[derive(Clone, PartialEq, Debug, Default)]
pub(crate) struct PageMarks {
    /// Keys whose inline keys tied with the searched key's: compared in
    /// full.
    pub(crate) candidates: Range<usize>,
    pub(crate) result: Option<PageResult>,
    /// Whether the search read the inline keys, all of them, as its count
    /// does (D18).
    pub(crate) read_inline_keys: bool,
}

/// Where a lookup ended in a page.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum PageResult {
    /// Key `index` equals the searched key: the key, and in a leaf its
    /// value.
    Key(usize),
    /// The lookup went down edge `index`.
    Edge(usize),
    /// The head holds the value.
    Head,
}

/// How the map draws and names what a slot array holds.
pub(crate) trait SlotLook {
    fn class(self) -> &'static str;
    fn label(self) -> &'static str;
}

impl SlotLook for SlotContents {
    fn class(self) -> &'static str {
        match self {
            SlotContents::Header => "slot-header",
            SlotContents::InlineKeys => "slot-inline",
            SlotContents::LengthTags => "slot-tags",
            SlotContents::Keys => "slot-keys",
            SlotContents::Head | SlotContents::Values | SlotContents::Edges => "slot-trailing",
        }
    }

    fn label(self) -> &'static str {
        match self {
            SlotContents::Header => "header",
            SlotContents::InlineKeys => "inline key",
            SlotContents::LengthTags => "length tag",
            SlotContents::Keys => "key",
            SlotContents::Head => "head",
            SlotContents::Values => "value",
            SlotContents::Edges => "child",
        }
    }
}

/// One slot's piece within one line.
#[derive(PartialEq)]
struct SlotPiece {
    x: f64,
    y: f64,
    width: f64,
    class: String,
    title: String,
}

/// The pieces of every slot of `page`, with `marks`, and the lines the
/// lookup read.
struct MapDrawing {
    pieces: Vec<SlotPiece>,
    read_lines: Vec<bool>,
}

impl MapDrawing {
    fn of(page: &PageMap, marks: &PageMarks, keys: &[String]) -> Self {
        let lines = page.size().div_ceil(LINE);
        let mut read_lines = vec![false; lines];
        let mut pieces = Vec::new();
        let mut read = |bytes: &Range<usize>| {
            for line in bytes.start / LINE..bytes.end.div_ceil(LINE) {
                if let Some(read) = read_lines.get_mut(line) {
                    *read = true;
                }
            }
        };
        for array in page.arrays() {
            if array.slot_size() == 0 {
                continue;
            }
            for slot in 0..array.capacity() {
                let bytes = array.slot_bytes(slot);
                let used = slot < array.used();
                let marked = Self::marked(array.contents(), slot, marks);
                let candidate = marks.candidates.contains(&slot)
                    && matches!(
                        array.contents(),
                        SlotContents::InlineKeys | SlotContents::Keys
                    );
                if used && (marked || candidate)
                    || array.contents() == SlotContents::Header
                    || array.contents() == SlotContents::InlineKeys && marks.read_inline_keys
                {
                    read(&bytes);
                }
                let mut class = array.contents().class().to_string();
                if !used {
                    class.push_str(" slot-empty");
                }
                if used && marked {
                    class.push_str(" slot-marked");
                } else if used && candidate {
                    class.push_str(" slot-candidate");
                }
                let mut title = format!(
                    "{} {slot} · bytes {}–{}",
                    array.contents().label(),
                    bytes.start,
                    bytes.end - 1
                );
                if array.contents() == SlotContents::Keys
                    && let Some(key) = keys.get(slot)
                {
                    title = format!("{title} · {key}");
                }
                if !used {
                    title.push_str(" · empty");
                }
                pieces.extend(Self::split_by_line(&bytes).map(|piece| SlotPiece {
                    x: GUTTER + (piece.start % LINE) as f64 * BYTE_WIDTH + 0.5,
                    y: (piece.start / LINE) as f64 * LINE_HEIGHT + 1.0,
                    width: piece.len() as f64 * BYTE_WIDTH - 1.0,
                    class: class.clone(),
                    title: title.clone(),
                }));
            }
        }
        Self { pieces, read_lines }
    }

    /// Whether the lookup ended on this slot.
    fn marked(contents: SlotContents, slot: usize, marks: &PageMarks) -> bool {
        match (marks.result, contents) {
            (
                Some(PageResult::Key(index)),
                SlotContents::Keys | SlotContents::InlineKeys | SlotContents::LengthTags,
            ) => slot == index,
            (Some(PageResult::Key(index)), SlotContents::Values) => slot == index,
            (Some(PageResult::Edge(index)), SlotContents::Edges) => slot == index,
            (Some(PageResult::Head), SlotContents::Head) => true,
            _ => false,
        }
    }

    /// What the page's arrays hold, in page order, once each: a head is
    /// drawn as the values are.
    fn legend(page: &PageMap) -> Vec<SlotContents> {
        let mut seen = Vec::new();
        for array in page.arrays() {
            let contents = match array.contents() {
                SlotContents::Head => SlotContents::Values,
                contents => contents,
            };
            if !seen.contains(&contents) {
                seen.push(contents);
            }
        }
        seen
    }

    fn legend_label(contents: SlotContents) -> &'static str {
        match contents {
            SlotContents::Header => "header",
            SlotContents::InlineKeys => "inline keys",
            SlotContents::LengthTags => "length tags",
            SlotContents::Keys => "keys",
            SlotContents::Head | SlotContents::Values => "values · head",
            SlotContents::Edges => "children",
        }
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

/// `page`, one cache line per row, with what a lookup read of it.
#[component]
pub(crate) fn MemoryMap(page: PageMap, marks: PageMarks, keys: Vec<String>) -> Element {
    let drawing = MapDrawing::of(&page, &marks, &keys);
    let lines = drawing.read_lines.len();
    let width = GUTTER + LINE as f64 * BYTE_WIDTH;
    let height = lines as f64 * LINE_HEIGHT + 1.0;
    let read = drawing.read_lines.iter().filter(|&&read| read).count();
    rsx! {
        figure { class: "flex flex-col gap-2",
            svg {
                class: "memory-map block",
                width,
                height,
                view_box: "0 0 {width} {height}",
                "role": "img",
                "aria-label": "Page of {page.size()} bytes in {lines} cache lines; the lookup read {read}",
                for line in 0..lines {
                    rect {
                        class: "line-track",
                        x: GUTTER,
                        y: line as f64 * LINE_HEIGHT + 1.0,
                        width: LINE as f64 * BYTE_WIDTH,
                        height: LINE_HEIGHT - 2.0,
                    }
                    text {
                        class: "line-offset",
                        x: GUTTER - 18.0,
                        y: line as f64 * LINE_HEIGHT + 11.0,
                        text_anchor: "end",
                        "{line * LINE:03x}"
                    }
                    if drawing.read_lines[line] {
                        circle {
                            class: "line-read",
                            cx: GUTTER - 9.0,
                            cy: line as f64 * LINE_HEIGHT + 7.5,
                            r: 3.5,
                        }
                    }
                }
                for piece in drawing.pieces {
                    rect {
                        class: piece.class,
                        x: piece.x,
                        y: piece.y,
                        width: piece.width,
                        height: LINE_HEIGHT - 3.0,
                        rx: 1.5,
                        title { {piece.title} }
                    }
                }
            }
            figcaption { class: "flex flex-wrap items-center gap-x-3.5 gap-y-1 text-[11.5px] text-[var(--graphite)]",
                for contents in MapDrawing::legend(&page) {
                    span { class: "flex items-center gap-1.5",
                        svg { width: 10, height: 10,
                            rect { class: contents.class(), width: 10, height: 10, rx: 2 }
                        }
                        {MapDrawing::legend_label(contents)}
                    }
                }
                span { class: "flex items-center gap-1.5",
                    svg { width: 10, height: 10,
                        rect { class: "slot-keys slot-empty", width: 10, height: 10, rx: 2 }
                    }
                    "room"
                }
                span { class: "flex items-center gap-1.5",
                    svg { width: 10, height: 10,
                        rect { class: "slot-keys slot-marked", width: 10, height: 10, rx: 2 }
                    }
                    "what the lookup read"
                }
                span { class: "flex items-center gap-1.5",
                    svg { width: 10, height: 10,
                        circle { class: "line-read", cx: 5, cy: 5, r: 3.5 }
                    }
                    "cache lines it touched: {read} of {lines}"
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use bbtree::BBTreeMap;

    use super::*;

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
        let marks = PageMarks {
            candidates: 2..4,
            result: Some(PageResult::Key(3)),
            read_inline_keys: true,
        };
        let drawing = MapDrawing::of(&page, &marks, &[]);
        let right = GUTTER + LINE as f64 * BYTE_WIDTH;
        assert!(
            drawing
                .pieces
                .iter()
                .all(|piece| piece.x >= GUTTER && piece.x + piece.width <= right)
        );
        assert!(drawing.read_lines[0], "the header's line is always read");
        assert!(
            drawing
                .pieces
                .iter()
                .any(|piece| piece.class.contains("slot-marked"))
        );
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
    fn slots_split_where_lines_start() {
        let pieces: Vec<_> = MapDrawing::split_by_line(&(60..70)).collect();
        assert_eq!(pieces, vec![60..64, 64..70]);
        let whole: Vec<_> = MapDrawing::split_by_line(&(0..8)).collect();
        assert_eq!(whole, vec![0..8]);
    }
}
