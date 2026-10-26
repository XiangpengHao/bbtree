// Batched lookups and inserts (OG-D53, OG-design/12-batch-api.md): many keys
// descend at once, so that one key's wait for memory overlaps the others'
// searches. A lookup is a chain of dependent misses, and one operation's
// chain doesn't overlap the next one's; lanes overlap them.
//
// Rust's lanes are async blocks on a round-robin executor. Its design notes
// measure a level-by-level scheduler 12-19% faster per key (10M `u64` hits
// 60.8 against 74.8 ns), which it gave up only because async lanes extend
// more easily; C++17 has no coroutines. So here the lanes go level by level
// (D11): each step searches every live lane at one depth, then
// follows every lane's chosen edge. Every leaf of the unsync tree is equally
// deep (OG-D17), so the first live lane's edge decides the next level's kind
// for all of them.
//
// Only `Prefetcher` prefetches, and only the line each lane reads next
// (OG-D46's exception, AGENTS.md): the query's window, the chosen edge, a
// child's header, a leaf's lone candidate's entry and the first key bytes its
// comparison reads, or the slot an insertion shifts.
#pragma once

#include <cstddef>
#include <cstdint>
#include <new>
#include <optional>
#include <type_traits>
#include <utility>

#include "../config.hpp"
#include "../target.hpp"
#include "tree.hpp"

namespace shoal::detail {

/// The only software prefetch (OG-D46): a hint about the line a lane reads
/// next, which reads nothing and can't fault.
struct Prefetcher {
  SHOAL_ALWAYS_INLINE static void hint(const void* line) noexcept {
    if (line == nullptr) return;
#if !defined(SHOAL_MSVC)
    __builtin_prefetch(line, 0, 3);
#elif defined(SHOAL_X86_64)
    _mm_prefetch(static_cast<const char*>(line), _MM_HINT_T0);
#elif defined(SHOAL_ARM64)
    __prefetch(line);
#endif
  }
};

/// A call-site marker that compiles the batch's leaf searches apart from the
/// single-key `get`'s (OG-D53).
struct BatchSearch {};

/// How many keys a batch takes at once: as many lanes as its arrays hold.
inline constexpr std::size_t batch_lanes = 32;

/// `N` slots of `T`, uninitialized until a lane constructs its own: making
/// a batch writes nothing. `T` is trivially destructible.
template <class T, std::size_t N>
class LaneArray {
  static_assert(std::is_trivially_destructible_v<T>);

 public:
  template <class... Args>
  void emplace(std::size_t index, Args&&... args) noexcept {
    ::new (static_cast<void*>(&slots_[index])) T(std::forward<Args>(args)...);
  }
  T& operator[](std::size_t index) noexcept { return *std::launder(reinterpret_cast<T*>(&slots_[index])); }

 private:
  struct alignas(T) Slot {
    unsigned char bytes[sizeof(T)];
  };
  Slot slots_[N];
};

/// Keys a batch looks up where they are: `keys[i]` is the key `pointers[i]`
/// points to, as a set operation looks up another set's elements in place.
template <class Q>
class PointedKeys {
 public:
  explicit PointedKeys(const Q* const* pointers) noexcept : pointers_(pointers) {}
  const Q& operator[](std::size_t index) const noexcept { return *pointers_[index]; }
  PointedKeys operator+(std::size_t offset) const noexcept { return PointedKeys(pointers_ + offset); }

 private:
  const Q* const* pointers_;
};

/// The key type of a batch's keys, `Keys` an array of them or `PointedKeys`.
template <class Keys>
using BatchKey = std::remove_cv_t<std::remove_reference_t<decltype(std::declval<const Keys&>()[0])>>;

template <class K, class V, class S>
class TreeBatch {
 public:
  using TreeType = Tree<K, V, S>;
  using Shape = NodeShape<K, V, S>;
  using LeafNode = Leaf<K, V, S>;
  using InternalNode = Internal<K, V, S>;
  using Ptr = NodePtr<K, V, S>;
  using LeafKind = typename LeafNode::Kind;

  /// Whether a tree of `length` entries is large enough for batches to pay
  /// for their lanes: below about an L2 of leaf pages, at 70% fill, there
  /// are too few misses to overlap (OG-doc 12).
  static constexpr bool batches(std::size_t length) noexcept {
    constexpr std::size_t pages_in_l2 =
        1024 * 1024 / Shape::leaf_layout.page.size > 0 ? 1024 * 1024 / Shape::leaf_layout.page.size : 1;
    constexpr std::size_t entries_per_leaf = Shape::leaf_capacity * 7 / 10 + 1;
    return length >= pages_in_l2 * entries_per_leaf;
  }

  /// Each key's value, as separate `find`s give them: `values[i]` for
  /// `keys[i]`, null for a key the tree lacks. `length` is the tree's.
  /// `keys` is an array of keys, or `PointedKeys`.
  template <class Keys>
  static void find_many(const TreeType& tree, Keys keys, std::size_t count, const V** values,
                        std::size_t length) noexcept {
    if (count < 2 || !batches(length) || !tree.has_root() || tree.root().is_leaf()) {
      for (std::size_t index = 0; index < count; ++index) values[index] = find_one(tree, keys[index]);
      return;
    }
    for (std::size_t start = 0; start < count; start += batch_lanes) {
      const std::size_t lanes = count - start < batch_lanes ? count - start : batch_lanes;
      find_lanes(tree, keys + start, lanes, values + start);
    }
  }

  /// Inserts `count` entries in order, as separate `insert`s do; `previous`
  /// gets each one's old value, if any.
  template <class Pages>
  static void insert_many(TreeType& tree, K* keys, V* values, std::size_t count,
                          std::optional<V>* previous, const Pages& pages, std::size_t& length) noexcept {
    std::size_t index = 0;
    while (index < count) {
      if (count - index < 2 || !batches(length)) {
        previous[index] = tree.insert(std::move(keys[index]), std::move(values[index]), pages, length);
        ++index;
        continue;
      }
      // Ascending keys: if the first appends, the rest most likely do too,
      // which the single-key insert does without a descent.
      if (tree.try_rightmost_append(keys[index], values[index])) {
        ++length;
        previous[index] = std::nullopt;
        ++index;
        continue;
      }
      const std::size_t lanes = count - index < batch_lanes ? count - index : batch_lanes;
      insert_lanes(tree, keys + index, values + index, lanes, previous + index, pages, length);
      index += lanes;
    }
  }

 private:
  /// One key of a batch too small to schedule, through the batch's own leaf
  /// search: the single-key `get` keeps its inlined descent.
  template <class Q>
  SHOAL_NOINLINE static const V* find_one(const TreeType& tree, const Q& key) noexcept {
    if (!tree.has_root()) return nullptr;
    const SearchKey<K, Q> search(key);
    if (tree.shared_prefix().place_outside(search)) return nullptr;
    const auto found = TreeType::template find_from<BatchSearch>(tree.root(), search);
    return found ? found->key_value().second : nullptr;
  }

  /// The lanes of one batch: each key's search key and node, and which lanes
  /// are still live, compacted after each step.
  template <class Q>
  struct Lanes {
    LaneArray<SearchKey<K, Q>, batch_lanes> keys;
    Ptr nodes[batch_lanes];
    std::uint16_t indexes[batch_lanes];
    std::uint16_t prefix_lens[batch_lanes];
    std::uint8_t live[batch_lanes];
    std::size_t live_len;
  };

  /// Starts a lane per key at the root, all live but those whose keys lack
  /// what every key shares (OG-D50), which no node holds.
  template <class Keys>
  static void start_lanes(const TreeType& tree, Keys keys, std::size_t count,
                          Lanes<BatchKey<Keys>>& lanes) noexcept {
    const SharedPrefix& shared = tree.shared_prefix();
    const Ptr root = tree.root().node_ptr();
    const std::size_t root_prefix_len = tree.root().prefix_len();
    lanes.live_len = 0;
    for (std::size_t lane = 0; lane < count; ++lane) {
      lanes.keys.emplace(lane, keys[lane]);
      lanes.nodes[lane] = root;
      if (shared.size() == 0 || !shared.place_outside(lanes.keys[lane])) {
        lanes.live[lanes.live_len++] = static_cast<std::uint8_t>(lane);
        Prefetcher::hint(lanes.keys[lane].window_line(root_prefix_len));
      }
    }
  }

  /// Goes down every live lane's internal levels, level by level, to its
  /// leaf, with the leaf's header prefetched. A lane whose key equals a
  /// separator stops there: `at_separator` gets it.
  template <class Q, class AtSeparator>
  static void descend_lanes(Lanes<Q>& lanes, AtSeparator at_separator) noexcept {
    for (;;) {
      std::size_t kept = 0;
      for (std::size_t at = 0; at < lanes.live_len; ++at) {
        const std::size_t lane = lanes.live[at];
        const InternalNode node = InternalNode::at(lanes.nodes[lane]);
        const SearchResult found = node.search(lanes.keys[lane]);
        if (found.found) {
          at_separator(lane, node, found.index);
          continue;
        }
        lanes.indexes[lane] = static_cast<std::uint16_t>(found.index);
        Prefetcher::hint(node.edge_line(found.index));
        lanes.live[kept++] = static_cast<std::uint8_t>(lane);
      }
      lanes.live_len = kept;
      if (kept == 0) return;
      for (std::size_t at = 0; at < lanes.live_len; ++at) {
        const std::size_t lane = lanes.live[at];
        const Ptr child = InternalNode::at(lanes.nodes[lane]).edge(lanes.indexes[lane]);
        lanes.nodes[lane] = child;
        Prefetcher::hint(child.page());
      }
      // Every leaf is as deep: the first lane's child tells every lane's.
      if (lanes.nodes[lanes.live[0]].is_leaf()) return;
    }
  }

  template <class Keys>
  static void find_lanes(const TreeType& tree, Keys keys, std::size_t count, const V** values) noexcept {
    for (std::size_t lane = 0; lane < count; ++lane) values[lane] = nullptr;
    Lanes<BatchKey<Keys>> lanes;
    start_lanes(tree, keys, count, lanes);
    descend_lanes(lanes, [&](std::size_t lane, InternalNode node, std::size_t index) {
      values[lane] = Found<K, V, S>::separator(node, index).key_value().second;
    });
    // The leaves: a lone candidate's entry, and for heap strings its bytes,
    // fetched before the comparison; ties to the leaf's search.
    std::size_t kept = 0;
    for (std::size_t at = 0; at < lanes.live_len; ++at) {
      const std::size_t lane = lanes.live[at];
      const LeafNode leaf = LeafNode::at(lanes.nodes[lane]);
      const CandidateLine line = leaf.candidate_line(lanes.keys[lane]);
      if (line.kind == CandidateLine::Kind::Absent) continue;
      if (line.kind == CandidateLine::Kind::One) {
        lanes.indexes[lane] = static_cast<std::uint16_t>(line.index);
        lanes.prefix_lens[lane] = static_cast<std::uint16_t>(leaf.prefix_len());
        Prefetcher::hint(line.line);
      } else {
        lanes.indexes[lane] = UINT16_MAX;
      }
      lanes.live[kept++] = static_cast<std::uint8_t>(lane);
    }
    lanes.live_len = kept;
    if constexpr (KeyInliner<K>::is_heap_byte_string) {
      for (std::size_t at = 0; at < lanes.live_len; ++at) {
        const std::size_t lane = lanes.live[at];
        if (lanes.indexes[lane] == UINT16_MAX) continue;
        Prefetcher::hint(LeafNode::at(lanes.nodes[lane])
                             .candidate_bytes_line(lanes.keys[lane], lanes.indexes[lane],
                                                   lanes.prefix_lens[lane]));
      }
    }
    for (std::size_t at = 0; at < lanes.live_len; ++at) {
      const std::size_t lane = lanes.live[at];
      const LeafNode leaf = LeafNode::at(lanes.nodes[lane]);
      const std::optional<std::size_t> index =
          lanes.indexes[lane] == UINT16_MAX
              ? leaf.template find<BatchSearch>(lanes.keys[lane])
              : leaf.find_candidate(lanes.keys[lane], lanes.indexes[lane], lanes.prefix_lens[lane]);
      if (index) values[lane] = &leaf.value(*index);
    }
  }

  /// Where a batch's key goes, from a read-only descent: unknown (insert
  /// from the root), its leaf with that leaf's length and search, or a
  /// separator equal to it.
  struct Destination {
    enum class Kind : std::uint8_t { FromRoot, Leaf, Separator };
    Kind kind;
    Ptr node;
    std::size_t searched_len;
    SearchResult found;
  };

  /// Descends for every key first, then inserts in input order, each at its
  /// destination while that is still valid (OG-doc 12, "Insertion and
  /// staleness"): an insertion that fit makes later keys in its leaf search
  /// again; a split drops that leaf's destinations and every separator's.
  template <class Pages>
  SHOAL_NOINLINE static void insert_lanes(TreeType& tree, K* keys, V* values, std::size_t count,
                                           std::optional<V>* previous, const Pages& pages,
                                           std::size_t& length) noexcept {
    if (!tree.has_root() || tree.root().is_leaf()) {
      for (std::size_t index = 0; index < count; ++index) {
        previous[index] = tree.insert(std::move(keys[index]), std::move(values[index]), pages, length);
      }
      return;
    }
    tree.forget_rightmost_leaf();
    Destination destinations[batch_lanes];
    {
      Lanes<K> lanes;
      start_lanes(tree, keys, count, lanes);
      for (std::size_t lane = 0; lane < count; ++lane) {
        destinations[lane] = Destination{Destination::Kind::FromRoot, Ptr(), 0, SearchResult::edge(0)};
      }
      descend_lanes(lanes, [&](std::size_t lane, InternalNode node, std::size_t index) {
        destinations[lane] =
            Destination{Destination::Kind::Separator, node.node_ptr(), index, SearchResult::at(index)};
      });
      for (std::size_t at = 0; at < lanes.live_len; ++at) {
        const std::size_t lane = lanes.live[at];
        const LeafNode leaf = LeafNode::at(lanes.nodes[lane]);
        const SearchResult found = leaf.search_always_inlined(lanes.keys[lane]);
        // The insertion shifts the keys from here: only the first line.
        if (!found.found && found.index < leaf.len()) Prefetcher::hint(leaf.entry_line(found.index));
        destinations[lane] = Destination{Destination::Kind::Leaf, leaf.node_ptr(), leaf.len(), found};
      }
    }
    bool descents_valid = true;
    for (std::size_t lane = 0; lane < count; ++lane) {
      const Destination destination = destinations[lane];
      if (!descents_valid || destination.kind == Destination::Kind::FromRoot) {
        // This insertion can split a leaf no destination names.
        descents_valid = false;
        previous[lane] = tree.insert(std::move(keys[lane]), std::move(values[lane]), pages, length);
        continue;
      }
      PrefixedKey<K> key = tree.shrink_shared_prefix_for(std::move(keys[lane]), pages);
      if (destination.kind == Destination::Kind::Separator) {
        V& slot = Found<K, V, S>::separator(InternalNode::at(destination.node), destination.found.index).value();
        previous[lane] = std::move(slot);
        slot = std::move(values[lane]);
        continue;
      }
      const LeafNode leaf = LeafNode::at(destination.node);
      // An insertion that fit since changed the leaf's length.
      const SearchResult found = leaf.len() == destination.searched_len
                                     ? destination.found
                                     : leaf.search_always_inlined(SearchKey<K, K>(key.key()));
      if (found.found) {
        V& slot = leaf.value(found.index);
        previous[lane] = std::move(slot);
        slot = std::move(values[lane]);
      } else if (leaf.len() < LeafNode::capacity()) {
        leaf.insert_fit(found.index, std::move(key).window_at(leaf.prefix_len()), std::move(values[lane]));
        ++length;
        previous[lane] = std::nullopt;
      } else {
        previous[lane] = tree.insert_with_stack(std::move(key), std::move(values[lane]), pages, length);
        for (std::size_t later = lane + 1; later < count; ++later) {
          Destination& next = destinations[later];
          if (next.kind == Destination::Kind::Separator ||
              (next.kind == Destination::Kind::Leaf && next.node == leaf.node_ptr())) {
            next.kind = Destination::Kind::FromRoot;
          }
        }
      }
    }
  }
};

}  // namespace shoal::detail
