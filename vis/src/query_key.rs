//! The key looked up, above the path, as the lookup reads its bytes on the
//! way down, step by step (`descent.rs`): once a node skips its prefix, the
//! bytes it skips dim; once it compares, a frame slides onto the bytes it
//! compares, the key's inline key there (D18); while it compares tied keys
//! in full, the bytes after the frame are marked. Down the path the frame
//! steps right as the fences close in and the prefixes grow
//! (`doc/05-fence-key.md`, D50). It marks only what the trace and the
//! tree's shape say: each node's prefix length and inline-key width.

use dioxus::prelude::*;

use crate::descent::DescentPlan;
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
}

impl KeyWalk {
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
        }
    }

    /// The window of the last node whose search has compared, once
    /// `shown` steps are shown.
    fn frame(&self, plan: &DescentPlan, shown: usize) -> Option<WindowStop> {
        self.stops
            .iter()
            .rev()
            .find(|stop| plan.phase(stop.row, shown) >= 2)
            .copied()
    }

    /// The bytes the last node to skip its prefix skips.
    fn skipped(&self, plan: &DescentPlan, shown: usize) -> usize {
        self.stops
            .iter()
            .rev()
            .find(|stop| plan.phase(stop.row, shown) >= 1)
            .map_or(0, |stop| stop.skipped)
    }
}

/// The key looked up, with the bytes the way down skipped and compared so
/// far marked.
#[component]
pub(crate) fn QueryKey(
    tree: LoadedTree,
    trace: Trace,
    plan: DescentPlan,
    /// How many steps of the way down are shown.
    shown: usize,
) -> Element {
    let walk = KeyWalk::of(&tree, &trace);
    let kind = tree.key_kind();
    let text = trace.query().to_string();
    let frame = walk.frame(&plan, shown);
    let skipped = walk.skipped(&plan, shown);
    // A node comparing tied keys in full reads on past its window.
    let tie_from = plan
        .current(shown)
        .filter(|&(row, _)| plan.phase(row, shown) == 3)
        .and(frame)
        .map(|stop| stop.skipped + stop.width);
    let read_to = frame.map_or(0, |stop| stop.skipped + stop.width);
    // Every state sets every property: the page patches a style, so one
    // left out would keep its last value.
    let frame_style = match frame {
        Some(stop) => format!(
            "opacity:1;transform:translateX({}px);width:{}px;",
            stop.skipped as f64 * walk.cell,
            stop.width as f64 * walk.cell
        ),
        None => "opacity:0;transform:translateX(0px);width:0px;".to_string(),
    };
    rsx! {
        span {
            class: "query-key data",
            style: "--cell:{walk.cell}px;",
            title: "Each node on the way down skips the bytes its keys share, dimmed, and compares the next few, its inline key, framed.",
            "aria-label": text.clone(),
            span { class: "key-cells", "aria-hidden": "true",
                span { class: "key-frame", style: frame_style }
                for (index, cell) in walk.cells.iter().enumerate() {
                    {
                        let class = if index >= walk.key_len {
                            if index < read_to { "key-cell key-padding" } else { "key-cell key-padding hidden" }
                        } else if index < skipped {
                            "key-cell key-skipped"
                        } else if tie_from.is_some_and(|from| index >= from) {
                            "key-cell key-compared"
                        } else {
                            "key-cell"
                        };
                        rsx! {
                            span { class, {cell.clone()} }
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
        let plan = DescentPlan::of(&tree, &tree.trace("123455").unwrap());
        assert_eq!(walk.skipped(&plan, 0), 0);
        assert!(walk.skipped(&plan, 1) > 0);
        assert!(walk.frame(&plan, 1).is_none());
        assert_eq!(walk.frame(&plan, plan.total()), walk.stops.last().copied());
    }
}
