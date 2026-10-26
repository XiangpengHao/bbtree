// Ranges and iterators over an unsync map (`unsync/map/iter.rs`): a range is
// a walk (D23) between two positions, in key order or reversed. Iterating one
// with range-for never copies its walk: `begin()` hands out a cursor into it.
// A map's own `begin()` returns an iterator that holds its walk, built in
// place.
#pragma once

#include <cstddef>
#include <optional>
#include <type_traits>
#include <utility>

#include "../detail/tree/walk.hpp"

namespace shoal::unsync {

/// A walk's end, which an iterator compares with to stop.
struct RangeEnd {};

/// The entries of a map between two bounds, forward or `Reverse`, values
/// `const` if `Const`. Its keys are const; with `Const` false it hands out
/// values to change, as Rust's `RangeMut`.
template <class K, class V, class S, bool Const, bool Reverse>
class Range {
  using Walk = detail::Walk<K, V, S, Reverse>;
  using Entry = detail::EntrySlots<K, V>;

 public:
  using value_ref = std::conditional_t<Const, const V&, V&>;
  using item = std::pair<const K&, value_ref>;

  /// The entries of `tree` within `lower` and `upper`, the walk made in
  /// place: a range is returned as it is made, never moved (D23).
  template <class L, class U>
  Range(const detail::Tree<K, V, S>& tree, const Bound<L>& lower, const Bound<U>& upper) noexcept {
    walk_.start_range(tree, lower, upper);
  }

  /// Every entry of `tree`.
  explicit Range(const detail::Tree<K, V, S>& tree) noexcept { walk_.start_all(tree); }

  Range(const Range&) = delete;
  Range& operator=(const Range&) = delete;

  /// The next entry, if any. Always inlined, with the walk's crossing: then
  /// the walk's fields stay in registers through a caller's loop.
  SHOAL_ALWAYS_INLINE std::optional<item> next() noexcept {
    Entry entry;
    if (!walk_.next(entry)) return std::nullopt;
    return item(*entry.key, *entry.value);
  }

  /// Hands every entry left to `visit(key, value)`, a counted loop per leaf.
  template <class Visit>
  void for_each(Visit visit) noexcept {
    walk_.for_each([&](Entry entry) { visit(*static_cast<const K*>(entry.key), static_cast<value_ref>(*entry.value)); });
  }

  /// A cursor into the range, at its next entry, for range-for.
  class iterator {
   public:
    explicit iterator(Range* range) noexcept : range_(range) { valid_ = range_->walk_.next(entry_); }
    item operator*() const noexcept { return item(*entry_.key, *entry_.value); }
    SHOAL_ALWAYS_INLINE iterator& operator++() noexcept {
      valid_ = range_->walk_.next(entry_);
      return *this;
    }
    bool operator!=(RangeEnd) const noexcept { return valid_; }
    bool operator==(RangeEnd) const noexcept { return !valid_; }

   private:
    Range* range_;
    Entry entry_{nullptr, nullptr};
    bool valid_;
  };

  iterator begin() noexcept { return iterator(this); }
  RangeEnd end() const noexcept { return RangeEnd(); }

 private:
  Walk walk_;
};

/// An iterator that holds its walk: what a map's `begin()` returns, built in
/// place. It can't be copied.
template <class K, class V, class S, bool Const>
class MapIterator {
  using Walk = detail::Walk<K, V, S, false>;
  using Entry = detail::EntrySlots<K, V>;

 public:
  using value_ref = std::conditional_t<Const, const V&, V&>;
  using item = std::pair<const K&, value_ref>;

  explicit MapIterator(const detail::Tree<K, V, S>& tree) noexcept {
    walk_.start_all(tree);
    valid_ = walk_.next(entry_);
  }
  MapIterator(const MapIterator&) = delete;
  MapIterator& operator=(const MapIterator&) = delete;

  item operator*() const noexcept { return item(*entry_.key, *entry_.value); }
  SHOAL_ALWAYS_INLINE MapIterator& operator++() noexcept {
    valid_ = walk_.next(entry_);
    return *this;
  }
  bool operator!=(RangeEnd) const noexcept { return valid_; }
  bool operator==(RangeEnd) const noexcept { return !valid_; }

 private:
  Walk walk_;
  Entry entry_{nullptr, nullptr};
  bool valid_;
};

}  // namespace shoal::unsync
