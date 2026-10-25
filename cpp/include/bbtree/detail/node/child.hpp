// What code working at either level needs of a node (`tree/node/mod.rs`,
// `Child`): to balance it with its siblings, or to fit its prefix to its
// fences (`tree/prefix.rs`). `Leaf` and `Internal` derive from `ChildBase`,
// which holds what both kinds do alike, over what each does its own way:
//
//     NodePtr node_ptr(); std::size_t len(); std::size_t prefix_len();
//     const K& key(std::size_t);            // a leaf's, or a separator's
//     std::size_t allowed_prefix_len(std::size_t);
//
// and its `Kind` (`KindShape`).
#pragma once

#include <cstddef>

#include "../config.hpp"
#include "inline_keys.hpp"
#include "page.hpp"

namespace bbtree::detail {

/// Where a split leaves the new entry: the half, and its index there.
struct InsertionSide {
  bool right;
  std::size_t index;

  /// Where a full node of `capacity` entries splits for an insertion at edge
  /// `insertion_index`, and where the new entry goes then. An insertion at
  /// either end leaves that side `min(2 * minimum, capacity / 2)` entries and
  /// the other denser; one in the middle divides evenly (D32). Returns the
  /// separator's index.
  static constexpr std::size_t of_split(std::size_t capacity, std::size_t insertion_index,
                                        std::size_t minimum, InsertionSide& side) noexcept {
    const std::size_t arriving_len = 2 * minimum < capacity / 2 ? 2 * minimum : capacity / 2;
    const std::size_t separator_index = insertion_index == 0           ? arriving_len - 1
                                        : insertion_index == capacity ? capacity - arriving_len
                                                                      : capacity / 2;
    if (insertion_index <= separator_index) {
      side = InsertionSide{false, insertion_index};
    } else {
      side = InsertionSide{true, insertion_index - separator_index - 1};
    }
    return separator_index;
  }
};

/// A full node split in two: the node keeps its lower entries, `separator`
/// goes up to the parent, `right` is the new node with the upper entries,
/// whose leftmost leaf has the separator as its head (D17, D49).
template <class Sep, class Handle>
struct Split {
  Sep separator;
  Handle right;
  InsertionSide inserted;
};

template <class Derived, class K, class Kind>
class ChildBase {
 public:
  /// The most keys a node of this kind holds.
  static constexpr std::size_t capacity() noexcept { return Kind::capacity; }
  /// The fewest keys a node below the root holds between removals (D22).
  static constexpr std::size_t min_len() noexcept { return Kind::min_len; }
  /// Whether two siblings of `left_len` and `right_len` keys and their
  /// separator fit in one node: then a removal's fixing merges them.
  static constexpr bool fit_merged(std::size_t left_len, std::size_t right_len) noexcept {
    return left_len + 1 + right_len <= Kind::capacity;
  }

  /// The node's inline keys, read in place (D18).
  typename Kind::InlineKeys inline_keys() const noexcept {
    static_assert(Kind::has_inline_keys);
    return typename Kind::InlineKeys(columns().inline_key_slots(), self().len());
  }

  /// Moves the node's prefix length back to `fill`'s, and every inline key
  /// with it (`Window::moved_back_to`). `fill` is the window, at that length,
  /// of a key whose encoding shares the node's prefix. Reads no key. Does
  /// nothing if the prefix is no longer than `fill`'s.
  void move_prefix_back_to(Window fill) const noexcept {
    if constexpr (Kind::has_inline_keys) {
      const std::size_t prefix_len = self().prefix_len();
      if (prefix_len <= fill.prefix_len()) return;
      const std::size_t len = self().len();
      const KeyColumns<Kind> slots = columns();
      for (std::size_t index = 0; index < len; ++index) {
        const Window window =
            Window(prefix_len, slots.read_inline_key(index)).moved_back_to(fill);
        slots.write_inline_key(index, window.inline_key());
      }
      self().node_ptr().set_prefix_len(fill.prefix_len());
    }
  }

  /// Sets the node's inline keys to `window_at(i)` for each key `i`, which
  /// must be its keys' windows at `prefix_len`, then its prefix length.
  template <class WindowAt>
  void set_windows(std::size_t prefix_len, WindowAt window_at) const noexcept {
    if constexpr (Kind::has_inline_keys) {
      const std::size_t len = self().len();
      const KeyColumns<Kind> slots = columns();
      for (std::size_t index = 0; index < len; ++index) {
        slots.write_inline_key(index, window_at(index));
      }
      self().node_ptr().set_prefix_len(prefix_len);
    }
  }

  /// Sets the prefix length of a node with no keys: how a copied node, or a
  /// new root, starts.
  void set_empty_prefix_len(std::size_t prefix_len) const noexcept {
    BBTREE_CHECK(self().len() == 0);
    self().node_ptr().set_prefix_len(prefix_len);
  }

  /// Whether the node's first and last keys differ in the first byte past its
  /// prefix, as their inline keys tell: then its fences share no more than
  /// its prefix, and fitting it can skip reading them.
  bool keys_differ_past_prefix() const noexcept {
    if constexpr (Kind::has_inline_keys) {
      const std::size_t len = self().len();
      if (len == 0) return false;
      const auto keys = inline_keys();
      return Bits::leading_zeros(keys.inline_key(0) ^ keys.inline_key(len - 1)) < 8;
    } else {
      return false;
    }
  }

  /// Sets the node's prefix length to `prefix_len`, at most what its fences
  /// share, as far as its kind lets it grow; every inline key becomes its
  /// key's window there, read from the key.
  void refit_prefix(std::size_t prefix_len) const noexcept {
    if constexpr (Kind::has_inline_keys) {
      const std::size_t allowed = self().allowed_prefix_len(prefix_len);
      if (allowed == self().prefix_len()) return;
      set_windows(allowed, [&](std::size_t index) {
        return KeyInliner<K>::inline_key_at(self().key(index), allowed);
      });
    }
  }

  /// Shrinks the node's prefix length to `prefix_len` if it is longer.
  void shrink_prefix_to(std::size_t prefix_len) const noexcept {
    if (self().prefix_len() > prefix_len) refit_prefix(prefix_len);
  }

 protected:
  KeyColumns<Kind> columns() const noexcept {
    return KeyColumns<Kind>(self().node_ptr().page());
  }

 private:
  const Derived& self() const noexcept { return static_cast<const Derived&>(*this); }
};

}  // namespace bbtree::detail
