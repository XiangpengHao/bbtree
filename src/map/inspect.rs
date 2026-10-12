//! The entry point to read-only views of a map's nodes, for the visualizer
//! (`vis/`), behind the `inspect` feature (`node/inspect.rs`).

#![forbid(unsafe_code)]

use crate::inline_key::InlineKeyStrategy;
use crate::node::inspect::TreeView;

use super::BBTreeMap;

impl<K, V, S: InlineKeyStrategy<K>> BBTreeMap<K, V, S> {
    /// The map's tree, to read node by node: its keys, inline keys, prefixes
    /// and page layouts, as the map stores them. For the visualizer; the
    /// `inspect` feature promises no stability.
    pub fn inspect(&self) -> TreeView<'_, K, V, S> {
        TreeView::new(self.root.as_ref().map(|root| root.borrow()))
    }
}
