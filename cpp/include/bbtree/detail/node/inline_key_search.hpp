// A node's inline keys, read in place for a search
// (`tree/node/inline_key_search.rs`), and a leaf's hash tags
// (`tree/node/hash_tags.rs`). A search finds the keys whose inline keys equal
// its own by counting over every slot, never by a binary search: the count's
// loads don't depend on one another, so the lines of a node that comes from
// memory arrive together.
#pragma once

#include <cstddef>
#include <cstdint>
#include <optional>

#include "../config.hpp"
#include "inline_keys.hpp"
#include "vector_count.hpp"

namespace bbtree::detail {

/// A node's inline keys at `Width` bytes, every slot up to `Capacity`, of
/// which the first `len` are live; the rest hold the greatest inline key
/// (invariant 4). Read through their pointer with plain loads: a latched
/// tree's optimistic reader may read a node a writer is changing, and gets
/// some answer in bounds, which its version check keeps or throws away.
template <std::size_t Width, std::size_t Capacity>
class NodeInlineKeys {
 public:
  using Slot = InlineSlot<Width>;
  using Word = typename Slot::Word;

  NodeInlineKeys(const Word* slots, std::size_t len) noexcept : slots_(slots), len_(len) {}

  std::size_t len() const noexcept { return len_; }

  /// Inline key `index`: the bytes the node keeps of key `index`'s window,
  /// zeros after them.
  InlineWord inline_key(std::size_t index) const noexcept {
    BBTREE_ASSERT(index < len_);
    return Slot::inline_key(slots_[index]);
  }

  /// Whether inline key `index` is above the bytes the node keeps of
  /// `inline_key`: a key with that inline key sorts before key `index`.
  bool is_above(std::size_t index, InlineWord inline_key) const noexcept {
    BBTREE_ASSERT(index < len_);
    return slots_[index] > Slot::of_inline_key(inline_key);
  }

  /// The positions of the inline keys equal to the bytes the node keeps of
  /// `inline_key`: exactly those for sorted inline keys, a range within
  /// `0..len` for any. If none equals it, the empty range at the first
  /// greater one.
  inline IndexRange equal_range(InlineWord inline_key) const noexcept {
    const Word target = Slot::of_inline_key(inline_key);
    if (auto range = VectorCount<Word>::template equal_range<Capacity>(slots_, len_, target)) {
      return *range;
    }
    return equal_range_by_count(target);
  }

  /// The slots equal to the bytes the node keeps of `inline_key`, as a mask,
  /// where the build target has vectors for it.
  inline std::optional<U64EqualityMask> equality_mask(InlineWord inline_key) const noexcept {
    return VectorCount<Word>::template equality_mask<Capacity>(slots_,
                                                               Slot::of_inline_key(inline_key));
  }

  /// Whether every slot past `len` holds the greatest inline key.
  bool pads_with_greatest() const noexcept {
    const Word greatest = Slot::of_inline_key(~InlineWord{0});
    for (std::size_t index = len_; index < Capacity; ++index) {
      if (slots_[index] != greatest) return false;
    }
    return true;
  }

 private:
  const Word* slots_;
  std::size_t len_;

  /// One pass over every slot counts those below `target` and those equal.
  /// The slots past `len` hold the greatest inline key, so they never count
  /// below; the cuts at `len` keep the range in bounds for any slots. Its
  /// trip count is the capacity, a constant, so the compiler vectorizes it.
  inline IndexRange equal_range_by_count(Word target) const noexcept {
    std::size_t below = 0, equal = 0;
    for (std::size_t index = 0; index < Capacity; ++index) below += slots_[index] < target;
    for (std::size_t index = 0; index < Capacity; ++index) equal += slots_[index] == target;
    if (below > len_) below = len_;
    std::size_t end = below + equal;
    if (end > len_) end = len_;
    return IndexRange{below, end};
  }
};

/// A leaf's hash tags (D36, invariant 5), every slot up to `Capacity`, each
/// live one its key's tag. Tags never order keys.
template <std::size_t Capacity>
class HashTags {
 public:
  /// The tag of every byte string of 255 bytes or more.
  static constexpr std::uint8_t saturated_length = 255;

  explicit HashTags(const std::uint8_t* slots) noexcept : slots_(slots) {}

  /// Whether `tag`, a byte string's, is its exact length.
  static bool is_exact_length(std::uint8_t tag) noexcept { return tag < saturated_length; }

  /// The positions among `candidates`, live keys, whose tag is `target`.
  inline SlotMatches matching(IndexRange candidates, std::uint8_t target) const noexcept {
    if (auto matches = U8EqualitySearch::matching<Capacity>(slots_, candidates, target)) {
      return *matches;
    }
    std::size_t found = 0, lower = 0, upper = 0;
    for (std::size_t index = candidates.start; index < candidates.end; ++index) {
      if (slots_[index] != target) continue;
      if (found == 0) lower = index;
      else if (found == 1) upper = index;
      else return SlotMatches::many();
      ++found;
    }
    if (found == 0) return SlotMatches::none();
    if (found == 1) return SlotMatches::one(lower);
    return SlotMatches::two(lower, upper);
  }

 private:
  const std::uint8_t* slots_;
};

}  // namespace bbtree::detail
