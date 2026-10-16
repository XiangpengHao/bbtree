//! The key looked up, above the path, as the lookup reads its bytes on the
//! way down. As each node's turn comes in the path's replay
//! ([`Trace::LEVEL_MS`] a level), the bytes that node skips, its prefix,
//! dim, and a frame moves onto the bytes its search compares, the key's
//! inline key there (D18). Down the path the frame steps right as the
//! fences close in and the prefixes grow (`doc/05-fence-key.md`, D50), and
//! it rests on the last node's. It marks only what the trace and the
//! tree's shape say: each node's prefix length and inline-key width.

use std::fmt::Write;

use dioxus::prelude::*;

use crate::inspected_tree::LoadedTree;
use crate::trace::Trace;
use crate::workload::KeyKind;

/// Where one node's search reads the key: past the bytes it skips, as many
/// as its inline keys keep.
#[derive(Clone, Copy, PartialEq, Debug)]
struct WindowStop {
    /// The node's row on the path, root first: its turn in the replay.
    row: usize,
    skipped: usize,
    width: usize,
}

/// The key's bytes, and the windows the lookup reads them through.
#[derive(Clone, PartialEq, Debug)]
struct KeyWalk {
    /// Each byte as drawn, and the zeros a window reads past the key's
    /// end.
    cells: Vec<String>,
    /// How many of the cells are the key's own bytes.
    key_len: usize,
    stops: Vec<WindowStop>,
    /// One cell's width, in px.
    cell: f64,
    /// The path's rows, which the replay takes a level each.
    rows: usize,
}

impl KeyWalk {
    /// How long, in ms, the frame takes to slide to the next node's window.
    const SLIDE_MS: f64 = 140.0;

    fn of(tree: &LoadedTree, trace: &Trace) -> Self {
        let kind = tree.key_kind();
        let shape = tree.shape();
        let bytes = trace.query_bytes().bytes();
        // Nodes whose search compares inline keys; not those that compare
        // whole keys, nor the way down to a separator's value, which
        // compares nothing.
        let stops: Vec<WindowStop> = trace
            .steps()
            .iter()
            .enumerate()
            .filter(|(_, step)| step.inline_key().is_some())
            .filter_map(|(row, step)| {
                let at = step.at();
                Some(WindowStop {
                    row,
                    skipped: shape.level(at.level()).prefix_len(at.index()),
                    width: step.inline_width()?,
                })
            })
            .collect();
        let read = stops
            .iter()
            .map(|stop| stop.skipped + stop.width)
            .max()
            .unwrap_or(0);
        let cells = (0..bytes.len().max(read))
            .map(|index| match bytes.get(index) {
                Some(&byte) if kind.is_text() && byte.is_ascii_graphic() => {
                    char::from(byte).to_string()
                }
                Some(_) | None if kind.is_text() => "·".to_string(),
                Some(&byte) => format!("{byte:02x}"),
                None => "00".to_string(),
            })
            .collect();
        Self {
            cells,
            key_len: bytes.len(),
            stops,
            cell: if kind.is_text() { 8.0 } else { 21.0 },
            rows: trace.steps().len(),
        }
    }

    /// The row whose node's window first reads cell `index`, if one does.
    fn read_from(&self, index: usize) -> Option<usize> {
        self.stops
            .iter()
            .find(|stop| index < stop.skipped + stop.width)
            .map(|stop| stop.row)
    }

    /// The row whose node first skips byte `index`, if one does.
    fn skipped_from(&self, index: usize) -> Option<usize> {
        self.stops
            .iter()
            .find(|stop| stop.skipped > index)
            .map(|stop| stop.row)
    }

    /// The frame's walk from window to window, as CSS keyframes named
    /// `name`, over the whole replay.
    fn keyframes(&self, name: &str) -> String {
        let level = f64::from(Trace::LEVEL_MS);
        let total = level * self.rows.max(1) as f64;
        let at = |ms: f64| (100.0 * ms / total).clamp(0.0, 100.0);
        let place = |stop: &WindowStop| {
            format!(
                "transform:translateX({}px);width:{}px;",
                stop.skipped as f64 * self.cell,
                stop.width as f64 * self.cell
            )
        };
        let mut frames = String::new();
        if let Some(first) = self.stops.first() {
            let start = first.row as f64 * level;
            let _ = write!(
                frames,
                "0%{{{}opacity:0}}{:.2}%{{{}opacity:0}}{:.2}%{{{}opacity:1}}",
                place(first),
                at(start),
                place(first),
                at(start + 80.0),
                place(first)
            );
        }
        for pair in self.stops.windows(2) {
            let start = pair[1].row as f64 * level;
            let _ = write!(
                frames,
                "{:.2}%{{{}}}{:.2}%{{{}}}",
                at(start - Self::SLIDE_MS),
                place(&pair[0]),
                at(start),
                place(&pair[1])
            );
        }
        format!("@keyframes {name}{{{frames}}}")
    }
}

/// The key looked up, with the bytes each node of its way down skips and
/// compares marked in turn.
#[component]
pub(crate) fn QueryKey(
    tree: LoadedTree,
    trace: Trace,
    /// Changes with every lookup and replay, so the walk plays again.
    replay: u64,
) -> Element {
    let walk = KeyWalk::of(&tree, &trace);
    let kind = tree.key_kind();
    let text = trace.query().to_string();
    let name = format!("key-walk-{replay}");
    let level = Trace::LEVEL_MS;
    let last = walk.stops.last().copied();
    let total = level as usize * walk.rows.max(1);
    rsx! {
        span {
            key: "{replay}",
            class: "query-key data",
            style: "--cell:{walk.cell}px;",
            title: "Each node on the way down skips the bytes its keys share, dimmed, and compares the next few, its inline key, framed.",
            "aria-label": text.clone(),
            style { {walk.keyframes(&name)} }
            span { class: "key-cells", "aria-hidden": "true",
                if let Some(stop) = last {
                    span {
                        class: "key-frame",
                        style: "transform:translateX({stop.skipped as f64 * walk.cell}px);width:{stop.width as f64 * walk.cell}px;animation-name:{name};animation-duration:{total}ms;",
                    }
                }
                for (index, cell) in walk.cells.iter().enumerate() {
                    {
                        // Zeros past the key's end show once a window reads
                        // them; skipped bytes dim at their node's turn.
                        let (class, row) = if index >= walk.key_len {
                            ("key-cell key-padding", walk.read_from(index))
                        } else {
                            match walk.skipped_from(index) {
                                Some(row) => ("key-cell key-skipped", Some(row)),
                                None => ("key-cell", None),
                            }
                        };
                        let delay = row
                            .map(|row| format!("animation-delay:{}ms;", row as u32 * level))
                            .unwrap_or_default();
                        rsx! {
                            span { class, style: delay, {cell.clone()} }
                        }
                    }
                }
            }
            if kind == KeyKind::Integers {
                span { class: "key-number", {text.clone()} }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use bbtree::BBTreeMap;

    use super::*;
    use crate::inspected_tree::InspectedMap;

    #[test]
    fn the_frame_steps_right_as_the_prefixes_grow() {
        let map: BBTreeMap<u64, u64> = (0..200_000).map(|key| (key * 5, key)).collect();
        let tree = LoadedTree::new(InspectedMap::new(map, KeyKind::Integers));
        let walk = KeyWalk::of(&tree, &tree.trace("123455").unwrap());
        assert_eq!(walk.cells.len(), 8);
        assert!(walk.stops.len() >= 2);
        assert!(
            walk.stops
                .windows(2)
                .all(|pair| pair[0].skipped <= pair[1].skipped)
        );
        // The bytes every key shares are skipped from the root on.
        assert_eq!(walk.skipped_from(0), Some(0));
        assert!(walk.keyframes("walk").starts_with("@keyframes walk{0%{"));
    }
}
