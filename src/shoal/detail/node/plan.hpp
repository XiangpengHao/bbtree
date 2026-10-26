// Node plans (OG-D47, OG-design/07-node-plan.md): every choice that depends on
// the key type, made in one place, at compile time (`tree/node/node_plan.rs`).
//
// | keys | inline keys, leaf / internal | also stored | ties |
// |---|---|---|---|
// | without inline keys | none | nothing | `operator<` (OG-D19) |
// | std integers | the impl's width in both | nothing | `operator<` |
// | a key's own impl | the impl's width; 8 / 4 with `cmp_from` (OG-D43) | leaf
// hash tags if any and they fit (OG-D36) | `operator<`, or `cmp_from` past the
// tie | | byte arrays | 2 / 2 (OG-D32) | nothing | bytes; a lone candidate
// whole (OG-D38) | | heap byte strings | 8 / 4 (OG-D43) | leaf hash tags where
// they fit | bytes, past the tie |
//
// With inline keys, separator keys are leaf heads, and internal nodes keep only
// inline keys and children (OG-D49). Layouts, slots, searches and prefix
// upkeep read the plan; none asks what the key type is.
#pragma once

#include <cstddef>
#include <cstdint>
#include <type_traits>

#include "../../inline_key.hpp"
#include "../../node_config.hpp"
#include "../target.hpp"
#include "layout.hpp"

namespace shoal {

/// No leaf annex: what every map keeps but one given another (leaf_annex.rs).
struct NoLeafAnnex {};

namespace detail {

/// What a map tells its nodes besides its key and value types: its node
/// config, its leaves' annex, and whether threads share it (the Rust
/// strategy's `Config`, `LeafAnnex` and `LATCHED`; D6).
template <class Config = DefaultNodeConfig, class Annex = NoLeafAnnex, bool Latched = false>
struct Strategy {
  using config = Config;
  using annex = Annex;
  static constexpr bool latched = Latched;
};

/// The room a slot of `T` takes: none for an empty type, as Rust's `()`.
template <class T>
struct SlotShape {
  static constexpr bool is_zero_sized = std::is_empty_v<T>;
  static constexpr std::size_t size = is_zero_sized ? 0 : sizeof(T);
  static constexpr std::size_t align = alignof(T);
};

/// How a search runs over keys by comparing them (OG-D19).
enum class KeySearch : std::uint8_t {
  /// A binary search with no early exit, a conditional move per step.
  Branchless,
  /// A binary search that branches on each comparison and stops at an equal
  /// key: for keys whose comparisons cost more than a mispredict.
  EarlyExit,
};

/// Whether `T` has the member `is_std_integer` set.
template <class T, class = void>
struct IsStdInteger : std::false_type {};
template <class T>
struct IsStdInteger<T, std::void_t<decltype(T::is_std_integer)>>
    : std::bool_constant<T::is_std_integer> {};

/// How keys `K` get their inline keys: `InlineKey<K>`, resolved into what the
/// nodes ask of it (`KeyInliner`).
template <class K>
struct KeyInliner {
  using Traits = InlineKey<K>;

  static constexpr bool enabled = Traits::enabled;

  static constexpr bool resolve_is_byte_string() {
    if constexpr (enabled) return Traits::is_byte_string; else return false;
  }
  /// A byte string ordered by its bytes, whose inline keys are windows of
  /// them.
  static constexpr bool is_byte_string = resolve_is_byte_string();

  static constexpr bool resolve_holds_its_bytes() {
    if constexpr (is_byte_string) return Traits::holds_its_bytes; else return false;
  }
  /// A byte string whose value is its bytes, a fixed array: ties cost a load
  /// from the node already read.
  static constexpr bool holds_its_bytes = resolve_holds_its_bytes();
  /// A byte string that points to its bytes elsewhere.
  static constexpr bool is_heap_byte_string = is_byte_string && !holds_its_bytes;
  /// A key with an inline-key impl of its own (or a std integer's).
  static constexpr bool is_impl = enabled && !is_byte_string;
  static constexpr bool is_std_integer = enabled && IsStdInteger<Traits>::value;

  static constexpr InlineWidth resolve_impl_width() {
    if constexpr (is_impl) return Traits::width; else return InlineWidth::Eight;
  }
  static constexpr InlineWidth impl_width = resolve_impl_width();

  static constexpr bool resolve_impl_hash_tag() {
    if constexpr (is_impl) return Traits::has_hash_tag; else return false;
  }
  static constexpr bool resolve_impl_cmp_from() {
    if constexpr (is_impl) return Traits::has_cmp_from; else return false;
  }
  static constexpr bool impl_has_hash_tag = resolve_impl_hash_tag();
  static constexpr bool impl_has_cmp_from = resolve_impl_cmp_from();

  /// Whether `K` is ordered by its own bytes alone, every bit pattern of them
  /// a `K`: std integers and byte arrays. A copy read while a writer changes
  /// it compares without reading anything else (optimistic reads, OG-D57).
  static constexpr bool orders_by_own_bytes = is_std_integer || holds_its_bytes;

  /// The key's bytes, for a byte string.
  static ByteView bytes(const K& key) noexcept { return Traits::bytes(key); }

  /// `key`'s inline key at `offset`: what a node whose keys share their first
  /// `offset` bytes stores beside it.
  static inline InlineWord inline_key_at(const K& key, std::size_t offset) noexcept {
    if constexpr (is_byte_string) {
      return ByteWindows(Traits::bytes(key)).inline_key_at(offset);
    } else {
      return Traits::inline_key_at(key, offset);
    }
  }

  /// `key`'s hash tag (OG-D36): a byte string's length, saturated; an impl's
  /// own.
  static inline std::uint8_t hash_tag(const K& key) noexcept {
    if constexpr (is_byte_string) {
      return ByteWindows(Traits::bytes(key)).hash_tag();
    } else if constexpr (impl_has_hash_tag) {
      return Traits::hash_tag(key);
    } else {
      return 0;
    }
  }

  /// How many leading bytes the zero-padded encodings of `a` and `b` share,
  /// compared a window at a time, at most the longest prefix a node records;
  /// 0 for keys without inline keys. A node bounded by `a` and `b` may skip
  /// that many bytes of its keys (05-fence-key.md).
  static std::size_t common_prefix_len(const K& a, const K& b) noexcept {
    constexpr std::size_t max_len = InlineKeyNodeHeader::max_prefix_len;
    if constexpr (!enabled) {
      return 0;
    } else if constexpr (is_byte_string) {
      const ByteWindows left(Traits::bytes(a)), right(Traits::bytes(b));
      const std::size_t limit = left.size() > right.size() ? left.size() : right.size();
      const std::size_t difference = first_difference(
          limit, [&](std::size_t offset) { return left.inline_key_at(offset); },
          [&](std::size_t offset) { return right.inline_key_at(offset); });
      return difference < max_len ? difference : max_len;
    } else {
      const std::size_t difference = first_difference(
          max_len, [&](std::size_t offset) { return Traits::inline_key_at(a, offset); },
          [&](std::size_t offset) { return Traits::inline_key_at(b, offset); });
      return difference < max_len ? difference : max_len;
    }
  }

  /// The prefix length of a node whose fences are `low` and `high`: what they
  /// share. Along the tree's borders, where one is missing, `shared_len`, what
  /// every key of the tree shares (OG-D50).
  static std::size_t fence_prefix_len(const K* low, const K* high, std::size_t shared_len) noexcept {
    if (low != nullptr && high != nullptr) return common_prefix_len(*low, *high);
    return shared_len;
  }

  /// How many leading bytes `key`'s encoding shares with `shared`, at most
  /// `shared_len`; 0 for keys without inline keys. A byte string compares by
  /// its bytes, so that a key is at least as long as what it shares.
  static std::size_t common_prefix_len_with_bytes(const K& key, const std::uint8_t* shared,
                                                  std::size_t shared_len) noexcept {
    if constexpr (!enabled) {
      return 0;
    } else if constexpr (is_byte_string) {
      const ByteView bytes = Traits::bytes(key);
      const std::size_t limit = bytes.size < shared_len ? bytes.size : shared_len;
      std::size_t count = 0;
      while (count < limit && bytes.data[count] == shared[count]) ++count;
      return count;
    } else {
      const ByteWindows shared_windows(ByteView{shared, shared_len});
      const std::size_t difference = first_difference(
          shared_len, [&](std::size_t offset) { return Traits::inline_key_at(key, offset); },
          [&](std::size_t offset) { return shared_windows.inline_key_at(offset); });
      return difference < shared_len ? difference : shared_len;
    }
  }

  /// Writes the first bytes of `key`'s encoding into `bytes`, at most
  /// `capacity`, and returns how many: a byte string's, as many as it has; a
  /// std integer's, its size; a key with its own impl, all of them,
  /// zero-padded; a key without inline keys, none.
  static std::size_t write_leading_bytes(const K& key, std::uint8_t* bytes,
                                         std::size_t capacity) noexcept {
    if constexpr (!enabled) {
      return 0;
    } else {
      std::size_t len = capacity;
      if constexpr (is_byte_string) {
        len = Traits::bytes(key).size;
      } else if constexpr (is_std_integer) {
        len = sizeof(K);
      }
      if (len > capacity) len = capacity;
      for (std::size_t offset = 0; offset < len; offset += 8) {
        const InlineWord window = inline_key_at(key, offset);
        const std::size_t chunk = len - offset < 8 ? len - offset : 8;
        for (std::size_t index = 0; index < chunk; ++index) {
          bytes[offset + index] = static_cast<std::uint8_t>(window >> (56 - 8 * index));
        }
      }
      return len;
    }
  }

 private:
  /// Where two zero-padded encodings first differ before `limit`, compared a
  /// window at a time, or `SIZE_MAX` if they don't.
  template <class AAt, class BAt>
  static std::size_t first_difference(std::size_t limit, AAt a_at, BAt b_at) noexcept {
    for (std::size_t offset = 0; offset < limit; offset += 8) {
      const InlineWord differing_bits = a_at(offset) ^ b_at(offset);
      if (differing_bits != 0) {
        return offset + static_cast<std::size_t>(Bits::leading_zeros(differing_bits)) / 8;
      }
    }
    return SIZE_MAX;
  }
};

/// What the nodes of one kind store beside their keys, and how a search tells
/// their keys apart: a row of the table above, for one kind.
struct NodePlan {
  /// How many bytes of each inline key the node stores; 0 for none.
  std::size_t inline_width;
  /// Whether a window the node hands out is read from its key again, as the
  /// node stores fewer bytes of it than a leaf (OG-D43).
  bool rehydrates_windows;
  /// Whether the node stores each key's hash tag (OG-D36).
  bool hash_tags;
  /// The search over the candidates, the keys whose inline keys tie.
  KeySearch candidate_search;
  /// The search over all keys, without an inline key to compare.
  KeySearch all_keys_search;
  /// Whether an exact-key lookup confirms its lone candidate by all of its
  /// bytes at once (OG-D38).
  bool whole_key_equality;
  /// Whether every lookup's node search is inlined, or left to the compiler,
  /// as for heap byte strings (OG-D41).
  bool inlines_node_search;

  constexpr bool has_inline_keys() const { return inline_width != 0; }
};

/// The plans and layouts of the nodes of a map with keys `K`, values `V` and
/// strategy `S`, each a constant (OG-D47, OG-D59).
template <class K, class V, class S>
struct NodeShape {
  using Inliner = KeyInliner<K>;
  using Config = typename S::config;
  using Annex = typename S::annex;

  static_assert(!std::is_empty_v<K>, "a key type takes room");
  static_assert(Inliner::enabled || !S::latched, "a shared TreeMap's keys need inline keys");
  static_assert(std::is_empty_v<Annex> || S::latched, "only a TreeMap's leaves keep an annex");

  static constexpr bool latched = S::latched;

  static constexpr KeySearch search_for_key() {
    return std::is_trivially_destructible_v<K> && sizeof(K) <= 16 ? KeySearch::Branchless
                                                                  : KeySearch::EarlyExit;
  }

  static constexpr std::size_t leaf_page_size =
      NodeLayoutMath::page_size(Config::leaf_page_size, 1024);
  static constexpr std::size_t internal_page_size =
      NodeLayoutMath::page_size(Config::internal_page_size, NodeLayoutMath::cache_line_size);
  static constexpr std::size_t plain_internal_page_size =
      NodeLayoutMath::page_size(Config::plain_internal_page_size, NodeLayoutMath::cache_line_size);

  static constexpr HeaderShape leaf_header() {
    return latched ? HeaderShape{true, SlotShape<Annex>::size, SlotShape<Annex>::align}
                   : HeaderShape{false, 0, 1};
  }
  static constexpr HeaderShape internal_header() { return HeaderShape{latched, 0, 1}; }

  static constexpr TrailingSlot value_slot() {
    return TrailingSlot{SlotShape<V>::size, SlotShape<V>::align, true};
  }
  static constexpr TrailingSlot edge_slot() {
    return TrailingSlot{sizeof(void*), alignof(void*), false};
  }

  static constexpr NodeLayoutMath math() { return NodeLayoutMath(sizeof(K), alignof(K)); }

  /// Whether a leaf with inline keys of `width` has room for hash tags.
  static constexpr bool leaf_has_room_for_hash_tags(std::size_t width) {
    return math()
        .compute_leaf(leaf_page_size, width, true, leaf_header(), value_slot())
        .has_hash_tags();
  }

  static constexpr NodePlan plan(bool is_leaf) {
    NodePlan plan{0, false, false, search_for_key(), search_for_key(), false, true};
    if constexpr (Inliner::is_impl) {
      const std::size_t width = static_cast<std::size_t>(Inliner::impl_width);
      const bool narrower_inside = Inliner::impl_has_cmp_from && width == 8 && !is_leaf;
      plan.inline_width = narrower_inside ? 4 : width;
      plan.rehydrates_windows = narrower_inside;
      plan.hash_tags = Inliner::impl_has_hash_tag && is_leaf && leaf_has_room_for_hash_tags(width);
    } else if constexpr (Inliner::holds_its_bytes) {
      plan.inline_width = 2;
      plan.candidate_search = KeySearch::EarlyExit;
      plan.whole_key_equality = true;
    } else if constexpr (Inliner::is_heap_byte_string) {
      plan.inline_width = is_leaf ? 8 : 4;
      plan.rehydrates_windows = !is_leaf;
      plan.hash_tags = is_leaf && leaf_has_room_for_hash_tags(8);
      plan.candidate_search = KeySearch::EarlyExit;
      plan.inlines_node_search = false;
    }
    return plan;
  }

  static constexpr NodePlan leaf_plan = plan(true);
  static constexpr NodePlan internal_plan = plan(false);

  /// Whether the map's nodes hold inline keys: both kinds do, or neither.
  static constexpr bool has_inline_keys = leaf_plan.has_inline_keys();

  static constexpr LeafLayout leaf_layout = math().compute_leaf(
      leaf_page_size, leaf_plan.inline_width, leaf_plan.hash_tags, leaf_header(), value_slot());

  static constexpr InternalLayout internal_layout =
      math().compute_internal(internal_page_size, plain_internal_page_size,
                              internal_plan.inline_width, internal_header(), edge_slot());

  /// Whether a leaf pairs its keys with its values in entries (OG-D29), which
  /// then move whole: with inline keys, unless the values take no room.
  static constexpr bool paired_leaf_entries = leaf_layout.keys.key_stride != sizeof(K);

  static constexpr std::size_t leaf_capacity = leaf_layout.keys.capacity;
  static constexpr std::size_t internal_capacity = internal_layout.keys.capacity;

  /// Where the root area starts in a root page: past room for either kind.
  static constexpr std::size_t root_area_offset =
      leaf_layout.page.size > internal_layout.page.size ? leaf_layout.page.size
                                                        : internal_layout.page.size;
  static constexpr std::size_t root_page_align =
      leaf_layout.page.align > internal_layout.page.align ? leaf_layout.page.align
                                                          : internal_layout.page.align;

  /// The fewest keys a node of `capacity` below the root holds between
  /// removals (OG-D22): an eighth, at least 4.
  static constexpr std::size_t min_len(std::size_t capacity) {
    return capacity / 8 > 4 ? capacity / 8 : 4;
  }
};

}  // namespace detail
}  // namespace shoal
