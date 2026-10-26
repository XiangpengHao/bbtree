// What every key of a tree shares, from its first byte (OG-D50,
// `tree/shared_prefix.rs`). It is the prefix of the nodes along the tree's
// borders, which lack a fence on one side, and of its root, which lacks both.
// A search checks a query against it once, at the top, so that every node may
// skip its prefix unchecked. The root page keeps it, after the root node.
#pragma once

#include <cstddef>
#include <cstdint>
#include <cstring>
#include <optional>

#include "../config.hpp"
#include "inline_keys.hpp"
#include "plan.hpp"

namespace shoal::detail {

/// The leading bytes of the encoding of every key of a tree, as many as they
/// all share, at most `max_len`. None for a tree whose keys have no inline
/// keys, share no byte, or that held no key yet.
///
/// Invariant: the encoding of every key starts with these bytes, and a
/// byte-string key is at least as long. Every node along the borders has this
/// prefix length.
struct SharedPrefix {
  /// The most bytes a tree keeps.
  static constexpr std::size_t max_len = 64;

  std::uint8_t len;
  std::uint8_t bytes[max_len];

  /// Nothing shared: what an empty tree starts with.
  static SharedPrefix none() noexcept {
    SharedPrefix shared;
    std::memset(&shared, 0, sizeof(shared));
    return shared;
  }

  /// What a tree of `key` alone shares: its encoding's first bytes.
  template <class K>
  static SharedPrefix of_key(const K& key) noexcept {
    SharedPrefix shared = none();
    shared.len = static_cast<std::uint8_t>(
        KeyInliner<K>::write_leading_bytes(key, shared.bytes, max_len));
    return shared;
  }

  std::size_t size() const noexcept { return len; }

  /// How many of these bytes `key`'s encoding starts with: what a tree that
  /// takes it shares.
  template <class K>
  std::size_t len_shared_with(const K& key) const noexcept {
    if (len == 0) return 0;
    return KeyInliner<K>::common_prefix_len_with_bytes(key, bytes, len);
  }

  /// Shrinks to the first `new_len` bytes, if there are more.
  void shrink_to(std::size_t new_len) noexcept {
    if (new_len < len) len = static_cast<std::uint8_t>(new_len);
  }

  /// The window of these bytes at `prefix_len`, at most their length: what a
  /// node whose prefix shrinks from these bytes' length fills its windows
  /// from.
  Window window_at(std::size_t prefix_len) const noexcept {
    SHOAL_ASSERT(prefix_len <= len);
    return Window(prefix_len, ByteWindows(ByteView{bytes, len}).inline_key_at(prefix_len));
  }

  /// Where `key` sorts against every key of the tree if it lacks these bytes:
  /// before all of them, or after. `nullopt` if it has them, or if the nodes
  /// can't compare its encoding.
  template <class K, class Q>
  SHOAL_ALWAYS_INLINE std::optional<Ordering> place_outside(
      const SearchKey<K, Q>& key) const noexcept {
    if (len == 0) return std::nullopt;
    const std::optional<Ordering> order = key.cmp_shared_prefix(bytes, len);
    if (order && *order == Ordering::Equal) return std::nullopt;
    return order;
  }

  /// Whether `key`'s encoding starts with these bytes.
  template <class K>
  bool is_shared_by(const K& key) const noexcept {
    return len_shared_with(key) == len;
  }
};

static_assert(sizeof(SharedPrefix) == 65 && offsetof(SharedPrefix, len) == 0);

}  // namespace shoal::detail
