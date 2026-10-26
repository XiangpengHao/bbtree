// Removing the entries a predicate picks (`unsync/map/extract_if.rs`):
// `ExtractIf`, and the map's `retain` on its walk.
//
// The walk is a node stack at the next entry to visit. It starts at the
// range's start and stops at the first entry past its end, comparing each
// entry it visits with the end. An entry the predicate passes over stays
// where it is; one it picks comes out as a removal takes it, and the walk
// goes on from where the entry after it went (`tree/remove.hpp`). The map is
// whole between steps.
//
// The walk takes the predicate at each step, so that a set's `extract_if`,
// whose predicate sees keys alone, shares it.
#pragma once

#include <cstddef>
#include <optional>
#include <utility>

#include "../bound.hpp"
#include "../detail/tree/remove.hpp"
#include "../detail/tree/tree.hpp"
#include "range.hpp"

namespace shoal::unsync {

/// The walk of an extraction over the entries of a map between two bounds:
/// it hands out the entries a predicate picks, removed.
template <class K, class V, class S, class A, class U>
class ExtractionWalk {
  using TreeType = detail::Tree<K, V, S>;
  using Pages = detail::MapPages<A>;

 public:
  /// A walk over the entries of `tree` within `lower` and `upper`; `length`
  /// counts the entries left, and removed nodes go back to `pages`.
  /// The walk keeps a copy of the upper bound's key: a `Bound` points to its
  /// key only while a range is made, and the walk outlives that.
  template <class L>
  ExtractionWalk(TreeType& tree, std::size_t& length, Pages pages, const Bound<L>& lower,
                 const Bound<U>& upper) noexcept
      : stack_(tree), length_(&length), pages_(pages), upper_kind_(upper.kind()) {
    if (upper_kind_ != Bound<U>::Kind::Unbounded) upper_key_.emplace(upper.key());
    if (tree.has_root()) stack_.go_to_first_entry_from(lower);
  }

  /// The next entry within the bounds that `pick(key, value)` picks,
  /// removed; `nullopt` once past the last within them.
  template <class Pick>
  std::optional<std::pair<K, V>> next(Pick& pick) noexcept {
    while (stack_.is_attached()) {
      const K& key = stack_.key();
      if (!below_upper(key)) {
        stack_.detach();
        return std::nullopt;
      }
      if (pick(key, stack_.value())) return stack_.remove_and_go_to_next(pages_, *length_);
      stack_.go_past_entry();
    }
    return std::nullopt;
  }

 private:
  detail::MutStack<K, V, S> stack_;
  std::size_t* length_;
  Pages pages_;
  typename Bound<U>::Kind upper_kind_;
  std::optional<U> upper_key_;

  /// Whether `key` is within the upper bound.
  bool below_upper(const K& key) const noexcept {
    switch (upper_kind_) {
      case Bound<U>::Kind::Included:
        return detail::SearchKey<K, U>(*upper_key_).cmp_full(key) != Ordering::Less;
      case Bound<U>::Kind::Excluded:
        return detail::SearchKey<K, U>(*upper_key_).cmp_full(key) == Ordering::Greater;
      case Bound<U>::Kind::Unbounded:
        break;
    }
    return true;
  }
};

/// The entries of a map between two bounds that `pick(key, value)` picks,
/// each removed as it is handed out, in key order. Lazy: an entry not reached
/// stays in the map. Made in place by the map's `extract_if`, never moved.
template <class K, class V, class S, class A, class U, class Pick>
class ExtractIf {
 public:
  using item = std::pair<K, V>;

  template <class L>
  ExtractIf(detail::Tree<K, V, S>& tree, std::size_t& length, detail::MapPages<A> pages,
            const Bound<L>& lower, const Bound<U>& upper, Pick pick) noexcept
      : walk_(tree, length, pages, lower, upper), pick_(std::move(pick)) {}

  ExtractIf(const ExtractIf&) = delete;
  ExtractIf& operator=(const ExtractIf&) = delete;

  /// The next entry picked, removed, if any.
  std::optional<item> next() noexcept { return walk_.next(pick_); }

  /// A cursor at the next entry picked, for range-for.
  class iterator {
   public:
    explicit iterator(ExtractIf* extraction) noexcept : extraction_(extraction), current_(extraction->next()) {}
    item& operator*() noexcept { return *current_; }
    iterator& operator++() noexcept {
      current_ = extraction_->next();
      return *this;
    }
    bool operator!=(RangeEnd) const noexcept { return current_.has_value(); }
    bool operator==(RangeEnd) const noexcept { return !current_.has_value(); }

   private:
    ExtractIf* extraction_;
    std::optional<item> current_;
  };

  iterator begin() noexcept { return iterator(this); }
  RangeEnd end() const noexcept { return RangeEnd(); }

 private:
  ExtractionWalk<K, V, S, A, U> walk_;
  Pick pick_;
};

}  // namespace shoal::unsync
