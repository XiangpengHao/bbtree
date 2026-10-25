// Iterating over a map by value (`unsync/map/into_iter.rs`): the map's
// entries, keys or values moved out in key order, from either end.
//
// Two walks, one from each end, move entries out of their slots: nothing
// shifts and nothing is rebalanced. They stop when the count left reaches
// zero, so neither reaches an entry the other took. Rust's iterator frees
// each node once both ends are past it; this one gives every page back when
// it is destroyed, after destroying the entries left between the ends in
// ascending order, as destroying the map does (PORTING.md).
#pragma once

#include <cstddef>
#include <cstdint>
#include <optional>
#include <type_traits>
#include <utility>

#include "../detail/tree/teardown.hpp"
#include "../detail/tree/walk.hpp"
#include "range.hpp"

namespace bbtree::unsync {

/// What a consuming iterator hands out of each entry.
enum class IntoPart : std::uint8_t { Entries, Keys, Values };

/// A map's entries by value, in key order, from either end: `Part` says
/// whether it hands out entries, keys or values. Made in place by the map's
/// `into_iter`, `into_keys` or `into_values`, never moved.
template <class K, class V, class S, class A, IntoPart Part = IntoPart::Entries>
class IntoIter {
  using TreeType = detail::Tree<K, V, S>;
  using Pages = detail::MapPages<A>;
  using Entry = detail::EntrySlots<K, V>;

 public:
  using item = std::conditional_t<Part == IntoPart::Entries, std::pair<K, V>,
                                  std::conditional_t<Part == IntoPart::Keys, K, V>>;

  /// The entries of `tree`, `length` of them, which the iterator owns, with
  /// the allocator their pages came from.
  IntoIter(TreeType tree, std::size_t length, A alloc) noexcept
      : alloc_(std::move(alloc)), tree_(tree), remaining_(length) {
    front_.start_all(tree_);
    back_.start_all(tree_);
  }

  IntoIter(const IntoIter&) = delete;
  IntoIter& operator=(const IntoIter&) = delete;

  ~IntoIter() {
    if constexpr (!std::is_trivially_destructible_v<K> || !std::is_trivially_destructible_v<V>) {
      while (next()) {
      }
    }
    detail::Teardown<K, V, S>::free_emptied(tree_, Pages(alloc_));
  }

  /// How many entries are left.
  std::size_t size() const noexcept { return remaining_; }

  /// The first entry left, moved out, if any.
  std::optional<item> next() noexcept {
    Entry entry;
    if (!take_front(entry)) return std::nullopt;
    return take(entry);
  }

  /// The last entry left, moved out, if any.
  std::optional<item> next_back() noexcept {
    // The last entry left may be the one `peek` holds: the count then
    // reaches zero, and nothing reads what `peek` held again.
    if (remaining_ == 0) return std::nullopt;
    Entry entry;
    back_.next(entry);
    --remaining_;
    return take(entry);
  }

  /// The first entry left, where it is, if any: for merging two iterators.
  std::optional<std::pair<const K&, const V&>> peek() noexcept {
    if (remaining_ == 0) return std::nullopt;
    if (!has_peeked_) {
      front_.next(peeked_);
      has_peeked_ = true;
    }
    return std::pair<const K&, const V&>(*peeked_.key, *peeked_.value);
  }

  /// A cursor at the next entry, for range-for: each step moves one out.
  class iterator {
   public:
    explicit iterator(IntoIter* entries) noexcept : entries_(entries), current_(entries->next()) {}
    item& operator*() noexcept { return *current_; }
    iterator& operator++() noexcept {
      current_ = entries_->next();
      return *this;
    }
    bool operator!=(RangeEnd) const noexcept { return current_.has_value(); }
    bool operator==(RangeEnd) const noexcept { return !current_.has_value(); }

   private:
    IntoIter* entries_;
    std::optional<item> current_;
  };

  iterator begin() noexcept { return iterator(this); }
  RangeEnd end() const noexcept { return RangeEnd(); }

 private:
  A alloc_;
  TreeType tree_;
  std::size_t remaining_;
  detail::Walk<K, V, S, false> front_;
  detail::Walk<K, V, S, true> back_;
  Entry peeked_{nullptr, nullptr};
  bool has_peeked_ = false;

  /// The front entry's slots, the one `peek` saw if it looked.
  bool take_front(Entry& entry) noexcept {
    if (remaining_ == 0) return false;
    if (has_peeked_) {
      entry = peeked_;
      has_peeked_ = false;
    } else {
      front_.next(entry);
    }
    --remaining_;
    return true;
  }

  /// Moves the entry out of its slots and destroys what is left there: the
  /// pages are given back later without touching them.
  static item take(Entry entry) noexcept {
    K key(std::move(*entry.key));
    V value(std::move(*entry.value));
    entry.key->~K();
    entry.value->~V();
    if constexpr (Part == IntoPart::Entries) {
      return item(std::move(key), std::move(value));
    } else if constexpr (Part == IntoPart::Keys) {
      return key;
    } else {
      return value;
    }
  }
};

}  // namespace bbtree::unsync
