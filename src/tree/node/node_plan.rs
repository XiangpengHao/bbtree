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
//! | std integers | the impl's width in both | nothing | `Ord` (D19) |
//! | every key under `WithInlineKey` | the impl's width in both; 8 / 4 with `cmp_from` (D43) | leaf: hash tags, if the impl has them and they fit (D36) | `Ord` (D19), or the impl's `cmp_from` past the tie |
//! | std byte arrays | 2 / 2 (D32) | nothing | bytes; an exact-key lookup's one candidate all at once (D38) |
//! | std heap byte strings | 8 / 4 (D43) | leaf: hash tags, where they fit (D36) | bytes, past the tie |
//!
//! With inline keys, every row keeps its separator keys as leaf heads, and
//! its internal nodes keep only inline keys and children (D49). Their
//! searches compare inline keys, and read a separator's key, down in its
//! leaf, only on a tie. Without inline keys, internal nodes keep their
//! separator keys (D17).
//!
//! What sets the rows apart is what a tie costs. A key with an `InlineKey`
//! impl orders by its `Ord`, or its `cmp_from`, which may be user code: the
//! nodes call it only while searching, never while keys move (D26). A std
//! byte array holds its bytes in the node, so a tie costs a load from a node
//! already read. A std heap byte string holds them elsewhere, so a tie costs
//! a read past the node. Its leaves keep long inline keys and the keys' hash
//! tags, their lengths, to avoid one. Its internal nodes, which most
//! searches pass through without ending at a key, keep short ones, as do
//! those of keys whose impl has `cmp_from`. They read a window's other bytes
//! from its key while keys move (D43), a key's own impl behind an
//! abort-on-unwind guard (D48).
//!
//! Layouts, slots, searches and prefix upkeep read the plan, or the
//! [`KeyInliner`] it is made from; none of them asks what the key type is.
//!
//! A plan is a pure function of `K`, `S` and the node kind, which folds to a
//! constant after monomorphization. Code asks for it where it uses it, from
//! those types, and never receives it as an argument or keeps it in a
//! field. A plan handed across a call that is not inlined would be a value
//! read at run time, and the code behind the call would serve every plan.

#![forbid(unsafe_code)]

use core::borrow::Borrow;
use core::cmp::Ordering;
use core::mem;

use super::inline_keys::KeyInliner;
use super::page::NodeKind;
use super::slots::KeyList;
use super::type_identity::ByteString;
use crate::inline_key::{InlineKeyStrategy, InlineWidth};

/// What the nodes of one kind store beside their keys, and how a search tells
/// their keys apart, for keys `K` under one map's strategy: a row of the
/// module documentation's table, for one kind. The module documentation also
/// says how to get one.
pub(in crate::tree) struct NodePlan<K> {
    /// How the keys get inline keys, or `None` if they have none.
    inliner: Option<KeyInliner<K>>,
    /// How many bytes of each key's inline key the node stores.
    inline_width: Option<InlineWidth>,
    /// Whether the node stores fewer bytes of each inline key than a leaf,
    /// so that a window it hands out, which may go into a leaf, is read from
    /// the key again (D43).
    rehydrates_windows: bool,
    /// Whether the node stores each key's hash tag, which filters an
    /// exact-key lookup's candidates (D36): a leaf of heap byte strings, or
    /// of keys whose impl has tags, where its layout has room for them.
    hash_tags: bool,
    /// The search over the candidates: the keys whose inline keys equal the
    /// search key's.
    candidate_search: KeySearch,
    /// Whether an exact-key lookup confirms its one candidate by comparing
    /// all of its bytes at once, rather than those past the tie (D38).
    whole_key_equality: bool,
}

impl<K> Clone for NodePlan<K> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K> Copy for NodePlan<K> {}

impl<K> NodePlan<K> {
    /// The plan of the nodes of kind `Kind` in a map with strategy `S`. A
    /// pure function of `K`, `S` and `Kind` that folds to a constant: every
    /// page of a map and kind gets the same answer, so accesses match its
    /// allocation layout.
    #[inline(always)]
    pub(in crate::tree) fn of<S: InlineKeyStrategy<K>, Kind: NodeKind>() -> Self {
        let Some(inliner) = KeyInliner::<K>::resolve::<S>() else {
            return Self::without_inline_keys();
        };
        match inliner {
            KeyInliner::InlineKeyTrait {
                width,
                hash_tag,
                cmp_from,
                ..
            } => Self::with_inline_key_impl::<Kind>(
                inliner,
                width,
                hash_tag.is_some(),
                cmp_from.is_some(),
            ),
            KeyInliner::ByteString(byte_string) if byte_string.holds_its_bytes() => {
                Self::for_byte_arrays(inliner)
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
            hash_tags: false,
            candidate_search: const { KeySearch::for_key::<K>() },
            whole_key_equality: false,
        }
    }

    /// Keys with an `InlineKey` impl: std integers under `Auto`, and every
    /// key under `WithInlineKey` (D18, D31). Both kinds keep the impl's
    /// width, and keys whose inline keys tie compare with `Ord`, searched as
    /// the key type picks. Leaves keep the impl's hash tags, if it has them,
    /// where their layout has room for them (D36).
    ///
    /// An impl with `cmp_from` compares ties past the bytes known equal
    /// instead (`SearchKey::cmp_tied`). Then, as for std heap byte strings,
    /// internal nodes keep 4 bytes of 8-byte inline keys, and read a
    /// window's other 4 from its key when they hand it out (D43): the extra
    /// ties cost little only because they skip what is known equal.
    #[inline(always)]
    fn with_inline_key_impl<Kind: NodeKind>(
        inliner: KeyInliner<K>,
        width: InlineWidth,
        has_hash_tag: bool,
        has_cmp_from: bool,
    ) -> Self {
        let narrower_inside = has_cmp_from && width == InlineWidth::Eight && !Kind::IS_LEAF;
        Self {
            inliner: Some(inliner),
            inline_width: Some(if narrower_inside {
                InlineWidth::Four
            } else {
                width
            }),
            rehydrates_windows: narrower_inside,
            hash_tags: has_hash_tag && Kind::has_room_for_hash_tags(width),
            ..Self::without_inline_keys()
        }
    }

    /// Std byte arrays (`[u8; 16]`, `[u8; 20]` and `[u8; 32]`) hold their
    /// bytes in the node, where a tie costs only a load. So 2 bytes of each
    /// inline key do, which packs more of them per cache line (D32). Keys
    /// whose inline keys tie compare by their bytes. An exact-key lookup
    /// compares its one candidate by all of them at once, in a length the
    /// compiler knows (D38).
    #[inline(always)]
    fn for_byte_arrays(inliner: KeyInliner<K>) -> Self {
        Self {
            inliner: Some(inliner),
            inline_width: Some(InlineWidth::Two),
            candidate_search: KeySearch::EarlyExit,
            whole_key_equality: true,
            ..Self::without_inline_keys()
        }
    }

    /// Std heap byte strings, such as `String`, `Vec<u8>`, `Box<str>` and
    /// `&str`, which hold their bytes elsewhere: a tie costs a read past the
    /// node. Leaves keep 8 bytes of each inline key, and each key's hash
    /// tag, its length, where their layout has room for it (D36). Internal nodes, which most
    /// searches pass through without ending at a key, keep 4 bytes, and read
    /// a window's other 4 from its key when they hand it out (D43). Keys
    /// whose inline keys tie compare by their bytes past the tie.
    #[inline(always)]
    fn for_heap_byte_strings<Kind: NodeKind>(inliner: KeyInliner<K>) -> Self {
        let leaf = Kind::IS_LEAF;
        Self {
            inliner: Some(inliner),
            inline_width: Some(if leaf {
                InlineWidth::Eight
            } else {
                InlineWidth::Four
            }),
            rehydrates_windows: !leaf,
            hash_tags: Kind::has_room_for_hash_tags(InlineWidth::Eight),
            candidate_search: KeySearch::EarlyExit,
            ..Self::without_inline_keys()
        }
    }

    /// How the keys get inline keys, or `None` if they have none.
    #[inline(always)]
    pub(in crate::tree) fn inliner(self) -> Option<KeyInliner<K>> {
        self.inliner
    }

    /// How many bytes of each key's inline key the node stores, or `None` if
    /// it stores no inline keys.
    #[inline(always)]
    pub(in crate::tree) fn inline_width(self) -> Option<InlineWidth> {
        self.inline_width
    }

    /// Whether a window the node hands out is read from its key again, as
    /// the node stores fewer bytes of it than a leaf does (D43).
    #[inline(always)]
    pub(in crate::tree) fn rehydrates_windows(self) -> bool {
        self.rehydrates_windows
    }

    /// Whether the node stores each key's hash tag (D36): a leaf of heap
    /// byte strings, or of keys whose impl has tags, whose layout has room
    /// for them.
    #[inline(always)]
    pub(in crate::tree) fn has_hash_tags(self) -> bool {
        self.hash_tags
    }

    /// The tag the node stores for `key`, if it stores hash tags
    /// ([`KeyInliner::hash_tag`]).
    #[inline(always)]
    pub(in crate::tree) fn hash_tag(self, key: &K) -> Option<u8> {
        if self.hash_tags {
            self.inliner?.hash_tag(key)
        } else {
            None
        }
    }

    /// How a search runs over all of a node's keys when it has no inline key
    /// to compare with theirs: in nodes without inline keys, and for a lookup
    /// through a borrowed form the nodes can't encode (D19). The key type
    /// alone decides it, the same in every plan.
    #[inline(always)]
    pub(super) fn all_keys_search(self) -> KeySearch {
        const { KeySearch::for_key::<K>() }
    }

    /// The search over the candidates: the keys whose inline keys equal the
    /// search key's.
    #[inline(always)]
    pub(super) fn candidate_search(self) -> KeySearch {
        self.candidate_search
    }

    /// Whether an exact-key lookup confirms its one candidate by comparing
    /// all of its bytes at once (D38).
    #[inline(always)]
    pub(super) fn has_whole_key_equality(self) -> bool {
        self.whole_key_equality
    }

    /// Whether every lookup's node search is inlined (`leaf.rs`,
    /// `internal.rs`), or left to LLVM at each caller, as for std heap byte
    /// strings; an insert's leaf search is always inlined. Other keys' node
    /// searches are short; LLVM may outline them, and then each level returns
    /// through the stack. A heap byte string's search reads keys past the
    /// node, so a call costs little beside it. Inlined into every lookup, it
    /// doubles the lookup's code and puts its state on the stack (D41).
    #[inline(always)]
    pub(in crate::tree) fn inlines_node_search(self) -> bool {
        !self.is_for_heap_byte_strings()
    }

    /// Whether the keys are std heap byte strings, whose ties read their
    /// bytes past the node: the batch fetches a lone candidate's first line
    /// of them ahead (`doc/12-batch-api.md`).
    #[inline(always)]
    pub(in crate::tree) fn is_for_heap_byte_strings(self) -> bool {
        self.byte_string()
            .is_some_and(|byte_string| !byte_string.holds_its_bytes())
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
    /// A binary search that branches on each comparison and stops at an
    /// equal key: for keys whose comparisons cost more than a mispredict.
    EarlyExit,
}

impl KeySearch {
    /// `keys` searched for `key` this way: `Ok(i)` if key `i` equals it,
    /// otherwise `Err(i)` with `i` where it belongs. The branchless search
    /// compares by `Ord`; the early-exit one with `cmp_key(probe)`, how
    /// `key` orders against key `probe`.
    #[inline(always)]
    pub(super) fn search<'a, K, Q>(
        self,
        keys: impl KeyList<'a, K>,
        key: &Q,
        cmp_key: impl Fn(&K) -> Ordering,
    ) -> Result<usize, usize>
    where
        K: 'a + Borrow<Q>,
        Q: Ord + ?Sized,
    {
        match self {
            Self::Branchless => keys.branchless_search_index(key),
            Self::EarlyExit => keys.early_exit_search_index(cmp_key),
        }
    }

    /// Branchless for keys with no drop glue and at most 16 bytes: integers,
    /// `char`, small tuples of them. Their comparisons are a few instructions
    /// on data already loaded, so a chain of conditional moves is cheapest.
    ///
    /// Everything else (`String`, `Vec`, `Box`, large arrays) uses the
    /// early-exit search. Without specialization this is a heuristic: `&str`
    /// and `&[u8]` keys also have no drop glue and fit in 16 bytes, so they
    /// take the branchless path although their comparisons follow a pointer,
    /// unless inline keys decide (D18).
    const fn for_key<K>() -> Self {
        if !mem::needs_drop::<K>() && mem::size_of::<K>() <= 16 {
            Self::Branchless
        } else {
            Self::EarlyExit
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
    use crate::tree::node::internal::Internal;
    use crate::tree::node::latch::Latched;
    use crate::tree::node::leaf::Leaf;
    use crate::tree::node::page::NodeKind;
    use crate::tree::test_support::Tracked;

    /// A plan's answers, for comparing whole rows of the table at once.
    #[derive(Debug, PartialEq, Eq)]
    struct PlanRow {
        inline_width: Option<InlineWidth>,
        rehydrates_windows: bool,
        hash_tags: bool,
        all_keys_search: KeySearch,
        candidate_search: KeySearch,
        whole_key_equality: bool,
    }

    impl PlanRow {
        fn of<K, S: InlineKeyStrategy<K>, Kind: NodeKind>() -> Self {
            let plan = NodePlan::<K>::of::<S, Kind>();
            Self {
                inline_width: plan.inline_width(),
                rehydrates_windows: plan.rehydrates_windows(),
                hash_tags: plan.has_hash_tags(),
                all_keys_search: plan.all_keys_search(),
                candidate_search: plan.candidate_search(),
                whole_key_equality: plan.has_whole_key_equality(),
            }
        }

        /// The row of keys without inline keys, searched as `all_keys_search`.
        fn without_inline_keys(all_keys_search: KeySearch) -> Self {
            Self {
                inline_width: None,
                rehydrates_windows: false,
                hash_tags: false,
                all_keys_search,
                candidate_search: all_keys_search,
                whole_key_equality: false,
            }
        }
    }

    #[test]
    fn key_types_pick_their_search() {
        assert_eq!(KeySearch::for_key::<u64>(), KeySearch::Branchless);
        assert_eq!(KeySearch::for_key::<u32>(), KeySearch::Branchless);
        assert_eq!(KeySearch::for_key::<(u64, u64)>(), KeySearch::Branchless);
        assert_eq!(KeySearch::for_key::<String>(), KeySearch::EarlyExit);
        assert_eq!(KeySearch::for_key::<[u8; 32]>(), KeySearch::EarlyExit);
        // The documented misfire: a borrowed string looks like plain data.
        assert_eq!(KeySearch::for_key::<&str>(), KeySearch::Branchless);
    }

    #[test]
    fn keys_without_inline_keys_store_nothing_else() {
        let plain = PlanRow::without_inline_keys(KeySearch::Branchless);
        assert_eq!(PlanRow::of::<u32, Auto, Leaf<u32, (), Auto>>(), plain);
        assert_eq!(PlanRow::of::<u32, Auto, Internal<u32, (), Auto>>(), plain);
        assert_eq!(
            PlanRow::of::<[u8; 8], Auto, Leaf<[u8; 8], (), Auto>>(),
            plain
        );
        let plain = PlanRow::without_inline_keys(KeySearch::EarlyExit);
        assert_eq!(
            PlanRow::of::<Tracked<'_>, Auto, Leaf<Tracked<'_>, (), Auto>>(),
            plain
        );
        assert_eq!(
            PlanRow::of::<Vec<i8>, Auto, Internal<Vec<i8>, (), Auto>>(),
            plain
        );
    }

    #[test]
    fn inline_key_impls_keep_their_width_in_both_kinds() {
        let two_bytes = PlanRow {
            inline_width: Some(InlineWidth::Two),
            ..PlanRow::without_inline_keys(KeySearch::Branchless)
        };
        assert_eq!(PlanRow::of::<u64, Auto, Leaf<u64, (), Auto>>(), two_bytes);
        assert_eq!(
            PlanRow::of::<i128, Auto, Internal<i128, (), Auto>>(),
            two_bytes
        );
        assert_eq!(
            PlanRow::of::<u64, WithInlineKey, Internal<u64, (), WithInlineKey>>(),
            two_bytes
        );

        // A std byte array under `WithInlineKey` is a key like any other.
        let eight_bytes = PlanRow {
            inline_width: Some(InlineWidth::Eight),
            ..PlanRow::without_inline_keys(KeySearch::EarlyExit)
        };
        assert_eq!(
            PlanRow::of::<[u8; 32], WithInlineKey, Leaf<[u8; 32], (), WithInlineKey>>(),
            eight_bytes
        );
        assert_eq!(
            PlanRow::of::<[u8; 32], WithInlineKey, Internal<[u8; 32], (), WithInlineKey>>(),
            eight_bytes
        );
    }

    /// An impl with `cmp_from`, as `String`'s, keeps 4 bytes of its 8-byte
    /// inline keys in internal nodes, which read a window's others from its
    /// key (D43), and all 8 in leaves.
    #[test]
    fn inline_key_impls_with_cmp_from_keep_four_bytes_inside() {
        let leaf = PlanRow {
            inline_width: Some(InlineWidth::Eight),
            ..PlanRow::without_inline_keys(KeySearch::EarlyExit)
        };
        assert_eq!(
            PlanRow::of::<String, WithInlineKey, Leaf<String, (), WithInlineKey>>(),
            leaf
        );
        assert_eq!(
            PlanRow::of::<String, WithInlineKey, Internal<String, (), WithInlineKey>>(),
            PlanRow {
                inline_width: Some(InlineWidth::Four),
                rehydrates_windows: true,
                ..leaf
            }
        );
    }

    #[test]
    fn byte_arrays_keep_two_bytes() {
        for (leaf, internal, all_keys_search) in [
            (
                PlanRow::of::<[u8; 16], Auto, Leaf<[u8; 16], (), Auto>>(),
                PlanRow::of::<[u8; 16], Auto, Internal<[u8; 16], (), Auto>>(),
                KeySearch::Branchless,
            ),
            (
                PlanRow::of::<[u8; 20], Auto, Leaf<[u8; 20], (), Auto>>(),
                PlanRow::of::<[u8; 20], Auto, Internal<[u8; 20], (), Auto>>(),
                KeySearch::EarlyExit,
            ),
            (
                PlanRow::of::<[u8; 32], Auto, Leaf<[u8; 32], (), Auto>>(),
                PlanRow::of::<[u8; 32], Auto, Internal<[u8; 32], (), Auto>>(),
                KeySearch::EarlyExit,
            ),
        ] {
            let expected = PlanRow {
                inline_width: Some(InlineWidth::Two),
                candidate_search: KeySearch::EarlyExit,
                whole_key_equality: true,
                ..PlanRow::without_inline_keys(all_keys_search)
            };
            assert_eq!(leaf, expected);
            assert_eq!(internal, expected);
        }
    }

    #[test]
    fn heap_byte_strings_split_their_work_between_the_kinds() {
        fn assert_heap_byte_string<K>(all_keys_search: KeySearch) {
            let base = PlanRow {
                candidate_search: KeySearch::EarlyExit,
                ..PlanRow::without_inline_keys(all_keys_search)
            };
            // Leaves of `()` values have no room for hash tags; see
            // `leaves_keep_hash_tags_where_they_fit` for leaves that do.
            assert_eq!(
                PlanRow::of::<K, Auto, Leaf<K, (), Auto>>(),
                PlanRow {
                    inline_width: Some(InlineWidth::Eight),
                    ..base
                }
            );
            assert_eq!(
                PlanRow::of::<K, Auto, Internal<K, (), Auto>>(),
                PlanRow {
                    inline_width: Some(InlineWidth::Four),
                    rehydrates_windows: true,
                    ..base
                }
            );
        }
        assert_heap_byte_string::<String>(KeySearch::EarlyExit);
        assert_heap_byte_string::<Vec<u8>>(KeySearch::EarlyExit);
        assert_heap_byte_string::<Box<str>>(KeySearch::EarlyExit);
        assert_heap_byte_string::<Box<[u8]>>(KeySearch::EarlyExit);
        assert_heap_byte_string::<&str>(KeySearch::Branchless);
        assert_heap_byte_string::<&[u8]>(KeySearch::Branchless);
    }

    /// A key's own impl's hash tags, where it has them, go in leaves whose
    /// layout has room for them, never in internal nodes (D36). Integers
    /// have none.
    #[test]
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
    fn inline_key_impls_keep_their_hash_tags_in_leaves() {
        let tagged = PlanRow {
            inline_width: Some(InlineWidth::Eight),
            hash_tags: true,
            ..PlanRow::without_inline_keys(KeySearch::EarlyExit)
        };
        assert_eq!(
            PlanRow::of::<String, WithInlineKey, Leaf<String, u64, WithInlineKey>>(),
            tagged
        );
        assert_eq!(
            PlanRow::of::<String, WithInlineKey, Internal<String, u64, WithInlineKey>>(),
            PlanRow {
                inline_width: Some(InlineWidth::Four),
                rehydrates_windows: true,
                hash_tags: false,
                ..tagged
            }
        );
        let integers = PlanRow::of::<u64, WithInlineKey, Leaf<u64, u64, WithInlineKey>>();
        assert!(!integers.hash_tags);
    }

    /// A leaf's plan tags each key with its length, saturated
    /// (`hash_tags.rs`), where the leaf keeps tags; an internal node's,
    /// and a leaf's without room for tags, never does.
    #[test]
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
    fn tagged_leaves_tag_keys_with_their_lengths() {
        let leaf = NodePlan::<String>::of::<Auto, Leaf<String, u64, Auto>>();
        assert_eq!(leaf.hash_tag(&String::from("tags")), Some(4));
        assert_eq!(leaf.hash_tag(&"x".repeat(257)), Some(u8::MAX));
        let untagged = NodePlan::<String>::of::<Auto, Leaf<String, (), Auto>>();
        assert_eq!(untagged.hash_tag(&String::from("tags")), None);
        let internal = NodePlan::<String>::of::<Auto, Internal<String, u64, Auto>>();
        assert_eq!(internal.hash_tag(&String::from("tags")), None);
    }

    /// A leaf's plan keeps hash tags only where its layout has room for
    /// them, which the values' size and alignment and the latch word decide
    /// (D36).
    #[test]
    #[cfg_attr(not(target_pointer_width = "64"), ignore = "assumes 64-bit pointers")]
    fn leaves_keep_hash_tags_where_they_fit() {
        fn keeps_tags<V, S: InlineKeyStrategy<String>>() -> bool {
            NodePlan::<String>::of::<S, Leaf<String, V, S>>().has_hash_tags()
        }
        assert!(keeps_tags::<u64, Auto>());
        assert!(keeps_tags::<u64, Latched<Auto>>());
        assert!(keeps_tags::<u128, Auto>());
        assert!(keeps_tags::<u128, Latched<Auto>>());
        assert!(keeps_tags::<String, Auto>());
        assert!(!keeps_tags::<(), Auto>());
        assert!(!keeps_tags::<(), Latched<Auto>>());
    }
}
