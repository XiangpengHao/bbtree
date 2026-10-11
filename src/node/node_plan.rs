//! Node plans (D47, `doc/07-node-plan.md`): every choice that depends on the
//! key type, made in one place.
//!
//! A map's strategy resolves, for its key type, to how its keys get inline
//! keys, if they do ([`KeyInliner`]). [`NodePlan::of`] turns that into what
//! the nodes of each kind store beside their keys and how a search tells
//! their keys apart:
//!
//! | keys | inline keys, leaf / internal | also stored | keys whose inline keys tie |
//! |---|---|---|---|
//! | without inline keys | none | nothing | `Ord` (D19) |
//! | with an `InlineKey` impl: std integers, every key under `WithInlineKey` | the impl's width in both | nothing | `Ord` (D19) |
//! | std byte arrays | 2 / 2 (D32) | internal: children before keys (D39) | bytes; a point lookup's one candidate all at once (D38) |
//! | std heap byte strings | 8 / 4 (D43) | leaf: length tags (D36); internal: a local prefix (D33) | bytes, past the tie |
//!
//! What sets the rows apart is what a tie costs. A key with an `InlineKey`
//! impl orders by its `Ord`, which may be user code: the nodes call it only
//! while searching, never while keys move (D26). A std byte array holds its
//! bytes in the node, so a tie costs a load from a node already read. A std
//! heap byte string holds them elsewhere, so a tie costs a read past the
//! node: its leaves keep long inline keys and the keys' lengths to avoid
//! one, and its internal nodes, which most searches pass through without
//! ending at a key, keep short ones. Reading a std byte string calls no user
//! code, so its nodes may read keys while keys move.
//!
//! Layouts (`layout.rs`), slots (`slots.rs`), searches (`handle.rs`) and
//! prefix upkeep (`prefix.rs`, `local_prefix.rs`) read the plan; none of them
//! asks what the key type is.
//!
//! A plan is a pure function of `K`, `S` and the node kind, which folds to a
//! constant after monomorphization. Code asks for it where it uses it, from
//! those types, and never receives it as an argument or keeps it in a
//! field: a plan handed across a call that is not inlined would be a value
//! read at run time, and the code behind the call would serve every plan.

#![forbid(unsafe_code)]

use core::mem;

use super::inline_keys::KeyInliner;
use super::page::{ByteString, PageKind};
use crate::inline_key::{InlineKeyStrategy, InlineWidth};

/// What the nodes of one kind store beside their keys, and how a search tells
/// their keys apart, for keys `K` under one map's strategy: a row of the
/// table above, for one kind. See the module documentation for how to get
/// one.
pub(super) struct NodePlan<K> {
    /// How the keys get inline keys, or `None` if they have none.
    inliner: Option<KeyInliner<K>>,
    /// How many bytes of each key's inline key the node stores.
    inline_width: Option<InlineWidth>,
    /// Whether the node stores fewer bytes of each inline key than a leaf,
    /// so that a window it hands out, which may go into a leaf, is read from
    /// the key again (D43).
    rehydrates_windows: bool,
    /// Whether the node stores each key's length, whose 2-byte tags filter a
    /// point lookup's candidates (D36).
    length_tags: bool,
    /// Whether the node may search past the bytes its fences share, when all
    /// of its keys share more (D33).
    local_prefix: bool,
    /// Whether an internal node puts its children before its keys (D39).
    children_first: bool,
    /// How a search runs over all of a node's keys when it has no inline key
    /// to compare with theirs: in nodes without inline keys, and for a lookup
    /// through a borrowed form the nodes can't encode (D19).
    full_search: KeySearch,
    /// How a search runs over the keys whose inline keys tie with its own.
    tied_search: KeySearch,
    /// Whether a point lookup confirms its one candidate by comparing all of
    /// its bytes at once, rather than those past the tie (D38).
    whole_key_equality: bool,
}

impl<K> Clone for NodePlan<K> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K> Copy for NodePlan<K> {}

impl<K> NodePlan<K> {
    /// The plan of the nodes of kind `Kind` in a map with strategy `S`.
    #[inline(always)]
    pub(super) fn of<S: InlineKeyStrategy<K>, Kind: PageKind>() -> Self {
        let Some(inliner) = KeyInliner::<K>::resolve::<S>() else {
            return Self::without_inline_keys();
        };
        match inliner {
            KeyInliner::InlineKeyTrait { width, .. } => Self::with_inline_key_impl(inliner, width),
            KeyInliner::ByteString(bytes) if bytes.holds_its_bytes() => {
                Self::for_byte_arrays::<Kind>(inliner)
            }
            KeyInliner::ByteString(_) => Self::for_heap_byte_strings::<Kind>(inliner),
        }
    }

    /// Keys without inline keys: a node stores the keys alone, and a search
    /// compares them with `Ord`. The other plans start from this one.
    #[inline(always)]
    fn without_inline_keys() -> Self {
        Self {
            inliner: None,
            inline_width: None,
            rehydrates_windows: false,
            length_tags: false,
            local_prefix: false,
            children_first: false,
            full_search: const { KeySearch::for_key::<K>() },
            tied_search: const { KeySearch::for_key::<K>() },
            whole_key_equality: false,
        }
    }

    /// Keys with an `InlineKey` impl: std integers under `Auto`, and every
    /// key under `WithInlineKey` (D18, D31). Both kinds keep the impl's
    /// width, and keys whose inline keys tie compare with `Ord`, searched as
    /// the key type picks.
    #[inline(always)]
    fn with_inline_key_impl(inliner: KeyInliner<K>, width: InlineWidth) -> Self {
        Self {
            inliner: Some(inliner),
            inline_width: Some(width),
            ..Self::without_inline_keys()
        }
    }

    /// Std byte arrays, `[u8; 16]`, `[u8; 20]` and `[u8; 32]`, which hold
    /// their bytes in the node: a tie costs a load from the node, so 2 bytes
    /// of each inline key do, which pack more of them per cache line (D32).
    /// Internal nodes put their children right after the inline keys, which
    /// leaves the keys within cache lines (D39). Keys whose inline keys tie
    /// compare by their bytes, and a point lookup's one candidate by all of
    /// them at once, a length the compiler knows (D38).
    #[inline(always)]
    fn for_byte_arrays<Kind: PageKind>(inliner: KeyInliner<K>) -> Self {
        Self {
            inliner: Some(inliner),
            inline_width: Some(InlineWidth::Two),
            children_first: !Kind::IS_LEAF,
            tied_search: KeySearch::Binary,
            whole_key_equality: true,
            ..Self::without_inline_keys()
        }
    }

    /// Std heap byte strings, such as `String`, `Vec<u8>`, `Box<str>` and
    /// `&str`, which hold their bytes elsewhere: a tie costs a read past the
    /// node. Leaves keep 8 bytes of each inline key, and each key's length
    /// (D36). Internal nodes, which most searches pass through without
    /// ending at a key, keep 4 bytes, and read a window's other 4 from its
    /// key when they hand it out (D43); and when all of their keys share
    /// more than their fences do, they search past it (D33). Keys whose
    /// inline keys tie compare by their bytes past the tie.
    #[inline(always)]
    fn for_heap_byte_strings<Kind: PageKind>(inliner: KeyInliner<K>) -> Self {
        let leaf = Kind::IS_LEAF;
        Self {
            inliner: Some(inliner),
            inline_width: Some(if leaf {
                InlineWidth::Eight
            } else {
                InlineWidth::Four
            }),
            rehydrates_windows: !leaf,
            length_tags: leaf,
            local_prefix: !leaf,
            tied_search: KeySearch::Binary,
            ..Self::without_inline_keys()
        }
    }

    /// How the keys get inline keys, or `None` if they have none.
    #[inline(always)]
    pub(super) fn inliner(self) -> Option<KeyInliner<K>> {
        self.inliner
    }

    /// How many bytes of each key's inline key the node stores, or `None` if
    /// it stores no inline keys.
    #[inline(always)]
    pub(super) fn inline_width(self) -> Option<InlineWidth> {
        self.inline_width
    }

    /// Whether a window the node hands out is read from its key again, as
    /// the node stores fewer bytes of it than a leaf does (D43).
    #[inline(always)]
    pub(super) fn rehydrates_windows(self) -> bool {
        self.rehydrates_windows
    }

    /// Whether the node stores each key's length as a 2-byte tag (D36).
    #[inline(always)]
    pub(super) fn has_length_tags(self) -> bool {
        self.length_tags
    }

    /// The tag the node stores for `key`, if it stores length tags: its
    /// length in bytes, truncated to 16 bits.
    #[inline(always)]
    pub(super) fn length_tag(self, key: &K) -> Option<u16> {
        self.length_tags.then(|| {
            let bytes = self
                .byte_string()
                .expect("only std byte strings have length tags");
            bytes.bytes(key).len() as u16
        })
    }

    /// The std byte strings whose shared bytes a local search prefix skips,
    /// if the node may keep one (D33).
    #[inline(always)]
    pub(super) fn local_prefix_bytes(self) -> Option<ByteString<K>> {
        self.byte_string().filter(|_| self.local_prefix)
    }

    /// Whether an internal node puts its children before its keys (D39).
    #[inline(always)]
    pub(super) fn has_children_first(self) -> bool {
        self.children_first
    }

    /// How a search runs over all of a node's keys when it has no inline key
    /// to compare with theirs (D19).
    #[inline(always)]
    pub(super) fn full_search(self) -> KeySearch {
        self.full_search
    }

    /// How a search runs over the keys whose inline keys tie with its own.
    #[inline(always)]
    pub(super) fn tied_search(self) -> KeySearch {
        self.tied_search
    }

    /// Whether a point lookup confirms its one candidate by comparing all of
    /// its bytes at once (D38).
    #[inline(always)]
    pub(super) fn has_whole_key_equality(self) -> bool {
        self.whole_key_equality
    }

    /// The proof that the keys are std byte strings, if they are.
    #[inline(always)]
    fn byte_string(self) -> Option<ByteString<K>> {
        match self.inliner? {
            KeyInliner::ByteString(bytes) => Some(bytes),
            KeyInliner::InlineKeyTrait { .. } => None,
        }
    }
}

/// How a search runs over keys by comparing them (D19).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum KeySearch {
    /// A binary search with no early exit, picking each next half with a
    /// conditional move rather than a branch.
    Branchless,
    /// A plain binary search, which stops at an equal key.
    Binary,
}

impl KeySearch {
    /// Branchless for keys with no drop glue and at most 16 bytes: integers,
    /// `char`, small tuples of them. Their comparisons are a few instructions
    /// on data already loaded, so a chain of conditional moves is cheapest.
    ///
    /// Everything else (`String`, `Vec`, `Box`, large arrays) uses plain
    /// binary search. Without specialization this is a heuristic: `&str` and
    /// `&[u8]` keys also have no drop glue and fit in 16 bytes, so they take
    /// the branchless path although their comparisons follow a pointer,
    /// unless inline keys decide (D18).
    const fn for_key<K>() -> Self {
        if !mem::needs_drop::<K>() && mem::size_of::<K>() <= 16 {
            Self::Branchless
        } else {
            Self::Binary
        }
    }
}

#[cfg(test)]
mod tests {
    use std::boxed::Box;
    use std::string::String;
    use std::vec::Vec;

    use super::{KeySearch, NodePlan};
    use crate::inline_key::{Auto, InlineKeyStrategy, InlineWidth, WithInlineKey};
    use crate::node::handle::{Internal, Leaf};
    use crate::node::page::PageKind;
    use crate::node::test_support::Tracked;

    /// A plan's answers, for comparing whole rows of the table at once.
    #[derive(Debug, PartialEq, Eq)]
    struct PlanRow {
        inline_width: Option<InlineWidth>,
        rehydrates_windows: bool,
        length_tags: bool,
        local_prefix: bool,
        children_first: bool,
        full_search: KeySearch,
        tied_search: KeySearch,
        whole_key_equality: bool,
    }

    impl PlanRow {
        fn of<K, S: InlineKeyStrategy<K>, Kind: PageKind>() -> Self {
            let plan = NodePlan::<K>::of::<S, Kind>();
            Self {
                inline_width: plan.inline_width(),
                rehydrates_windows: plan.rehydrates_windows(),
                length_tags: plan.has_length_tags(),
                local_prefix: plan.local_prefix_bytes().is_some(),
                children_first: plan.has_children_first(),
                full_search: plan.full_search(),
                tied_search: plan.tied_search(),
                whole_key_equality: plan.has_whole_key_equality(),
            }
        }

        /// The row of keys without inline keys, searched as `full_search`.
        fn without_inline_keys(full_search: KeySearch) -> Self {
            Self {
                inline_width: None,
                rehydrates_windows: false,
                length_tags: false,
                local_prefix: false,
                children_first: false,
                full_search,
                tied_search: full_search,
                whole_key_equality: false,
            }
        }
    }

    #[test]
    fn key_types_pick_their_search() {
        assert_eq!(KeySearch::for_key::<u64>(), KeySearch::Branchless);
        assert_eq!(KeySearch::for_key::<u32>(), KeySearch::Branchless);
        assert_eq!(KeySearch::for_key::<(u64, u64)>(), KeySearch::Branchless);
        assert_eq!(KeySearch::for_key::<String>(), KeySearch::Binary);
        assert_eq!(KeySearch::for_key::<[u8; 32]>(), KeySearch::Binary);
        // The documented misfire: a borrowed string looks like plain data.
        assert_eq!(KeySearch::for_key::<&str>(), KeySearch::Branchless);
    }

    #[test]
    fn keys_without_inline_keys_store_nothing_else() {
        let plain = PlanRow::without_inline_keys(KeySearch::Branchless);
        assert_eq!(PlanRow::of::<u32, Auto, Leaf>(), plain);
        assert_eq!(PlanRow::of::<u32, Auto, Internal>(), plain);
        assert_eq!(PlanRow::of::<[u8; 8], Auto, Leaf>(), plain);
        let plain = PlanRow::without_inline_keys(KeySearch::Binary);
        assert_eq!(PlanRow::of::<Tracked<'_>, Auto, Leaf>(), plain);
        assert_eq!(PlanRow::of::<Vec<i8>, Auto, Internal>(), plain);
    }

    #[test]
    fn inline_key_impls_keep_their_width_in_both_kinds() {
        let two_bytes = PlanRow {
            inline_width: Some(InlineWidth::Two),
            ..PlanRow::without_inline_keys(KeySearch::Branchless)
        };
        assert_eq!(PlanRow::of::<u64, Auto, Leaf>(), two_bytes);
        assert_eq!(PlanRow::of::<i128, Auto, Internal>(), two_bytes);
        assert_eq!(PlanRow::of::<u64, WithInlineKey, Internal>(), two_bytes);

        // A std byte string under `WithInlineKey` is a key like any other.
        let eight_bytes = PlanRow {
            inline_width: Some(InlineWidth::Eight),
            ..PlanRow::without_inline_keys(KeySearch::Binary)
        };
        assert_eq!(PlanRow::of::<String, WithInlineKey, Leaf>(), eight_bytes);
        assert_eq!(
            PlanRow::of::<String, WithInlineKey, Internal>(),
            eight_bytes
        );
        assert_eq!(
            PlanRow::of::<[u8; 32], WithInlineKey, Internal>(),
            eight_bytes
        );
    }

    #[test]
    fn byte_arrays_keep_two_bytes_and_put_children_first() {
        for (leaf, internal, full_search) in [
            (
                PlanRow::of::<[u8; 16], Auto, Leaf>(),
                PlanRow::of::<[u8; 16], Auto, Internal>(),
                KeySearch::Branchless,
            ),
            (
                PlanRow::of::<[u8; 20], Auto, Leaf>(),
                PlanRow::of::<[u8; 20], Auto, Internal>(),
                KeySearch::Binary,
            ),
            (
                PlanRow::of::<[u8; 32], Auto, Leaf>(),
                PlanRow::of::<[u8; 32], Auto, Internal>(),
                KeySearch::Binary,
            ),
        ] {
            let expected = PlanRow {
                inline_width: Some(InlineWidth::Two),
                tied_search: KeySearch::Binary,
                whole_key_equality: true,
                ..PlanRow::without_inline_keys(full_search)
            };
            assert_eq!(leaf, expected);
            assert_eq!(
                internal,
                PlanRow {
                    children_first: true,
                    ..expected
                }
            );
        }
    }

    #[test]
    fn heap_byte_strings_split_their_work_between_the_kinds() {
        fn assert_heap_byte_string<K>(full_search: KeySearch) {
            let base = PlanRow {
                tied_search: KeySearch::Binary,
                ..PlanRow::without_inline_keys(full_search)
            };
            assert_eq!(
                PlanRow::of::<K, Auto, Leaf>(),
                PlanRow {
                    inline_width: Some(InlineWidth::Eight),
                    length_tags: true,
                    ..base
                }
            );
            assert_eq!(
                PlanRow::of::<K, Auto, Internal>(),
                PlanRow {
                    inline_width: Some(InlineWidth::Four),
                    rehydrates_windows: true,
                    local_prefix: true,
                    ..base
                }
            );
        }
        assert_heap_byte_string::<String>(KeySearch::Binary);
        assert_heap_byte_string::<Vec<u8>>(KeySearch::Binary);
        assert_heap_byte_string::<Box<str>>(KeySearch::Binary);
        assert_heap_byte_string::<Box<[u8]>>(KeySearch::Binary);
        assert_heap_byte_string::<&str>(KeySearch::Branchless);
        assert_heap_byte_string::<&[u8]>(KeySearch::Branchless);
    }

    #[test]
    fn length_tags_are_byte_lengths() {
        let leaf = NodePlan::<String>::of::<Auto, Leaf>();
        assert_eq!(leaf.length_tag(&String::from("tags")), Some(4));
        assert_eq!(leaf.length_tag(&"x".repeat(65_537)), Some(1));
        let internal = NodePlan::<String>::of::<Auto, Internal>();
        assert_eq!(internal.length_tag(&String::from("tags")), None);
    }
}
