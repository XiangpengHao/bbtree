// Reads of a latched tree's nodes while other threads write them
// (`tree/node/optimistic.rs`): internal nodes always, by optimistic lock
// coupling, and leaves for lookups and hints (D57). A racing reader reads the
// node's version, reads the node in place, and checks the version after; a
// writer changing it meanwhile fails the check. So a read here:
//
// - reads the length once and clamps it to the capacity, so that the search
//   stays in bounds whatever it read (D14);
// - follows no edge before the node's version vouches for it;
// - runs no user code on what it read: where inline keys tie, it compares a
//   separator's key, a leaf's head (D49), with that leaf latched shared; keys
//   ordered by their own bytes alone it compares as an unlatched copy.
//
// These reads race with writers by design (PORTING.md, P7): ThreadSanitizer
// ignores them.
#pragma once

#include <cstddef>
#include <cstdint>
#include <cstring>
#include <optional>
#include <type_traits>

#include "../config.hpp"
#include "../node/internal.hpp"

namespace bbtree::detail {

/// Where a key goes from an internal node read optimistically: down `edge`,
/// with the node's `len` as read; `at_separator` if the key equals separator
/// `edge - 1`, whose entry is the head of the leftmost leaf below (D17).
/// 12 bytes, so that `std::optional<Routed>` returns in registers;
/// `at_separator` a byte, 0 or 1, that a descent ORs into its own without
/// converting it from the register it came back in, as a `bool` would.
struct Routed {
  Routed() = default;
  Routed(std::size_t edge, bool at_separator, std::size_t len) noexcept
      : edge(static_cast<std::uint32_t>(edge)), len(static_cast<std::uint32_t>(len)), at_separator(at_separator) {}

  std::uint32_t edge;
  std::uint32_t len;
  std::uint8_t at_separator;
};

/// A copy of a value of type `T`, read as bytes from a slot a writer may be
/// changing: a `T` only once a version check vouches for the read.
template <class T>
class RacingCopy {
 public:
  BBTREE_RACING_READ void read_from(const void* slot) noexcept {
    std::memcpy(static_cast<void*>(bytes_), slot, sizeof(T));
  }
  /// The value read, once the leaf's version vouched for the read.
  const T& assume_read() const noexcept {
    return *std::launder(reinterpret_cast<const T*>(bytes_));
  }

 private:
  alignas(T) unsigned char bytes_[sizeof(T)];
};

template <class K, class V, class S>
class Racing {
 public:
  using LeafNode = Leaf<K, V, S>;
  using InternalNode = Internal<K, V, S>;
  using AnyNode = Node<K, V, S>;
  using Ptr = NodePtr<K, V, S>;
  using Inliner = KeyInliner<K>;

  /// The length and prefix length of the `Kind` node on `node`'s page, and
  /// its inline keys, read in place: the length clamped to the capacity.
  template <class Kind>
  struct InlineRead {
    std::size_t len;
    std::size_t prefix_len;
    typename Kind::InlineKeys inline_keys;
  };

  template <class Kind>
  BBTREE_RACING_READ static InlineRead<Kind> load_inline_keys(Ptr node) noexcept {
    auto [len, prefix_len] = node.load_racing_header();
    if (len > Kind::capacity) len = Kind::capacity;
    return InlineRead<Kind>{len, prefix_len,
                            typename Kind::InlineKeys(KeyColumns<Kind>(node.page()).inline_key_slots(), len)};
  }

  // Leaves.

  /// Whether `key` may sort after every key of `leaf`, read in place: false
  /// if the leaf has no key, or a last key whose inline key alone sorts it
  /// after `key`. A hint for an append, which latches the leaf to be sure.
  template <class Q>
  BBTREE_RACING_READ static bool may_go_last(LeafNode leaf,
                                                                  const SearchKey<K, Q>& key) noexcept {
    const auto read = load_inline_keys<typename LeafNode::Kind>(leaf.node_ptr());
    if (read.len == 0) return false;
    if constexpr (SearchKey<K, Q>::has_inline_target) {
      return !read.inline_keys.is_above(read.len - 1, key.inline_target(read.prefix_len).inline_key());
    } else {
      return true;
    }
  }

  /// `Leaf::candidate_line`, read in place, and the prefix length read.
  template <class Q>
  BBTREE_RACING_READ static std::pair<CandidateLine, std::size_t> candidate_line(
      LeafNode leaf, const SearchKey<K, Q>& key) noexcept {
    const auto read = load_inline_keys<typename LeafNode::Kind>(leaf.node_ptr());
    if constexpr (SearchKey<K, Q>::has_inline_target) {
      return {leaf.candidate_line_among(key.inline_target(read.prefix_len), read.inline_keys, read.len),
              read.prefix_len};
    } else {
      return {CandidateLine{CandidateLine::Kind::Unknown, 0, nullptr}, read.prefix_len};
    }
  }

  /// The line of the entry an insertion of `key` would shift first, as the
  /// inline keys say, read in place; nullptr if it would go last.
  template <class Q>
  BBTREE_RACING_READ static const void* insertion_line(
      LeafNode leaf, const SearchKey<K, Q>& key) noexcept {
    if constexpr (SearchKey<K, Q>::has_inline_target) {
      const auto read = load_inline_keys<typename LeafNode::Kind>(leaf.node_ptr());
      const std::size_t edge =
          read.inline_keys.equal_range(key.inline_target(read.prefix_len).inline_key()).start;
      if (edge >= read.len) return nullptr;
      return leaf.key_slots().keys().slot(edge);
    } else {
      return nullptr;
    }
  }

  /// Whether candidate `index` is `key`, decided by the leaf alone, read in
  /// place: for a byte string that ends within its inline key, an exact hash
  /// tag tells the length, and equal lengths the key (D36, D57). `nullopt`
  /// where only the keys' bytes can tell.
  template <class Q>
  BBTREE_RACING_READ static std::optional<bool> candidate_is(
      LeafNode leaf, const SearchKey<K, Q>& key, std::size_t index, std::size_t prefix_len) noexcept {
    using Kind = typename LeafNode::Kind;
    if constexpr (Kind::has_hash_tags && SearchKey<K, Q>::has_hash_tag && SearchKey<K, Q>::by_bytes) {
      if (!key.template ends_within_window<Kind::width>(prefix_len)) return std::nullopt;
      const std::uint8_t query_tag = key.hash_tag();
      if (!HashTags<Kind::capacity>::is_exact_length(query_tag)) return std::nullopt;
      const std::uint8_t tag = __atomic_load_n(leaf.hash_tags() + index, __ATOMIC_RELAXED);
      return tag == query_tag;
    } else {
      return std::nullopt;
    }
  }

  /// Copies key `index`'s value into `value`, read in place.
  static void load_value(LeafNode leaf, std::size_t index, RacingCopy<V>& value) noexcept {
    value.read_from(leaf.node_ptr().value_slots().slot(index));
  }

  static void load_key(LeafNode leaf, std::size_t index, RacingCopy<K>& key) noexcept {
    key.read_from(leaf.key_slots().keys().slot(index));
  }

  /// Copies the head's key and value, if the leaf, as read, has a head.
  static bool load_head(LeafNode leaf, RacingCopy<K>* key, RacingCopy<V>* value) noexcept {
    const Ptr node = leaf.node_ptr();
    if (!node.load_racing_has_head()) return false;
    if (key != nullptr) key->read_from(node.head_key_slot());
    if (value != nullptr) value->read_from(node.head_and_value_slots().slot(0));
    return true;
  }

  // Internal nodes.

  /// Routes `key` through `node`, read at `version`, in place; `nullopt` if a
  /// tie found the node changed, or what it read is no node: start over.
  template <class Q>
  BBTREE_RACING_READ static std::optional<Routed> route(
      InternalNode node, const SearchKey<K, Q>& key, Version version) noexcept {
    using Kind = typename InternalNode::Kind;
    const auto read = load_inline_keys<Kind>(node.node_ptr());
    if constexpr (SearchKey<K, Q>::has_inline_target) {
      const Window target = key.inline_target(read.prefix_len);
      const IndexRange candidates = read.inline_keys.equal_range(target.inline_key());
      // Most searches tie with no separator: they stop here (D49).
      if (candidates.empty()) return Routed{candidates.start, false, read.len};
      return route_tied<true>(node, candidates, read.len, target, version, key);
    } else {
      return route_tied<false>(node, IndexRange{0, read.len}, read.len, Window::none(), version, key);
    }
  }

  /// The node's length, read in place and clamped to the capacity.
  static std::size_t len(InternalNode node) noexcept {
    const std::size_t len = node.node_ptr().load_racing_header().first;
    return len < InternalNode::capacity() ? len : InternalNode::capacity();
  }

  /// The child at edge `index`, read in place: what a writer stored there, or
  /// null for an edge none has (a zeroed page's).
  BBTREE_RACING_READ static Ptr child(InternalNode node, std::size_t index) noexcept {
    BBTREE_ASSERT(index <= InternalNode::capacity());
    return Ptr::from_tagged(__atomic_load_n(node.node_ptr().edge_word(index), __ATOMIC_RELAXED));
  }

  /// The leftmost leaf under `node`, down first edges read in place; null if
  /// the walk meets a null edge.
  static Ptr leftmost_leaf(Ptr node) noexcept {
    while (!node.is_null() && !node.is_leaf()) node = child(InternalNode::at(node), 0);
    return node;
  }

 private:
  /// A binary search among the separators that tie, each key read in its
  /// leaf's head (D49): past the inline keys at `target`'s window if
  /// `ByWindow`, by whole keys otherwise.
  template <bool ByWindow, class Q>
  BBTREE_NOINLINE static std::optional<Routed> route_tied(InternalNode node, IndexRange candidates,
                                                          std::size_t len, Window target, Version version,
                                                          const SearchKey<K, Q>& key) noexcept {
    std::size_t low = candidates.start, high = candidates.end;
    while (low < high) {
      const std::size_t middle = low + (high - low) / 2;
      const Ptr below = child(node, middle + 1);
      if (below.is_null()) return std::nullopt;
      if (!node.node_ptr().latch().is_unchanged(version)) return std::nullopt;
      const Ptr leaf = leftmost_leaf(below);
      if (leaf.is_null()) return std::nullopt;
      const std::optional<Ordering> order =
          cmp_separator_head<ByWindow>(node, LeafNode::at(leaf), version, key, target);
      if (!order) return std::nullopt;
      switch (*order) {
        case Ordering::Less: high = middle; break;
        case Ordering::Greater: low = middle + 1; break;
        case Ordering::Equal: return Routed{middle + 1, true, len};
      }
    }
    return Routed{low, false, len};
  }

  /// How `key` orders against the key of a separator of `node`, the head key
  /// of `leaf`: read with the leaf latched shared, or as a copy for keys
  /// ordered by their own bytes. Stands only if `node`, read at `version`,
  /// is unchanged once the head is read.
  template <bool ByWindow, class Q>
  static std::optional<Ordering> cmp_separator_head(InternalNode node, LeafNode leaf, Version version,
                                                    const SearchKey<K, Q>& key, Window target) noexcept {
    using Kind = typename InternalNode::Kind;
    auto cmp = [&](const K& stored) {
      if constexpr (ByWindow) {
        return key.template cmp_tied<Kind::width>(target, stored);
      } else {
        return key.cmp_full(stored);
      }
    };
    if constexpr (Inliner::orders_by_own_bytes) {
      // Without the latch: the head changes only while `node` is held.
      RacingCopy<K> head;
      if (!load_head(leaf, &head, nullptr)) return std::nullopt;
      const Ordering ordering = cmp(head.assume_read());
      if (!node.node_ptr().latch().is_unchanged(version)) return std::nullopt;
      return ordering;
    } else {
      NodeLatch& latch = leaf.node_ptr().latch();
      latch.lock_shared();
      std::optional<Ordering> ordering;
      if (node.node_ptr().latch().is_unchanged(version) && leaf.has_head()) {
        ordering = cmp(leaf.head_key());
      }
      latch.unlock_shared();
      return ordering;
    }
  }
};

}  // namespace bbtree::detail
