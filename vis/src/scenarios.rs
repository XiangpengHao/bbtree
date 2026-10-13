//! Lookups worth seeing, offered beside the lookup bar: each puts one of
//! bbtree's ideas to work on the tree that is built. A random key almost
//! never lands on the interesting cases.

use crate::inspected_tree::LoadedTree;
use crate::workload::KeyKind;

/// A kind of lookup that shows something.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Scenario {
    /// A key of the map, at random: the common case.
    Random,
    /// A key stored as a separator: found in an internal node, its value
    /// in the head of a leaf.
    Separator,
    /// A key past the last one, down the tree's right border, where no
    /// node has a fence prefix.
    PastTheEnd,
    /// A key the map doesn't hold, between two it does.
    Missing,
}

impl Scenario {
    /// The scenarios offered for keys of `kind`, besides a random key: keys
    /// that are dense integers have no gaps to miss in.
    pub(crate) fn offered(kind: KeyKind) -> &'static [Scenario] {
        match kind {
            KeyKind::Urls => &[Scenario::Separator, Scenario::Missing, Scenario::PastTheEnd],
            KeyKind::Integers | KeyKind::Ids => &[Scenario::Separator, Scenario::PastTheEnd],
        }
    }

    /// The scenario's name, standing alone.
    pub(crate) fn title(self) -> &'static str {
        match self {
            Scenario::Random => "A random key",
            Scenario::Separator => "A separator",
            Scenario::PastTheEnd => "Past the last key",
            Scenario::Missing => "A missing key",
        }
    }

    /// What the lookup shows.
    pub(crate) fn shows(self) -> &'static str {
        match self {
            Scenario::Random => "Looks up a key of the map at random.",
            Scenario::Separator => {
                "A key the root stores as a separator: the lookup finds it there, then goes down to the leaf whose head holds its value."
            }
            Scenario::PastTheEnd => {
                "A key greater than every key: the lookup follows the right border, where nodes have no fence prefix. Internal nodes there keep the bytes their keys share as a search prefix; where the key lacks them, the node sends it to its last child at once."
            }
            Scenario::Missing => {
                "A key between two keys of the map: the inline keys place it, and the leaf says where an insert would put it."
            }
        }
    }

    /// The key to look up on `tree`, picked by `seed` where there is a
    /// choice.
    pub(crate) fn query(self, tree: &LoadedTree, seed: u64) -> Option<String> {
        match self {
            Scenario::Random => tree.random_key(seed),
            Scenario::Separator => {
                let root = tree.shape().root().filter(|root| root.level() > 0)?;
                let len = tree.shape().level(root.level()).len(root.index());
                let index = (seed % len.max(1) as u64) as usize;
                let (shared, rest) = tree.key_text_split(root, index);
                Some(shared + &rest)
            }
            Scenario::PastTheEnd => Some(tree.key_kind().past_the_end().to_string()),
            Scenario::Missing => tree
                .random_key(seed)
                .and_then(|key| tree.key_kind().missing_after(&key)),
        }
    }
}
