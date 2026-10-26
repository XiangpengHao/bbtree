// Operations on two sets (`unsync/set/{merge,lookups,difference,
// intersection,union,symmetric_difference,subset}.rs`): std's algorithms on
// the sets' elements, each a lazy sequence that hands out elements of the
// sets in place.
//
// - **Cursors** take a set's keys from its walk a run at a time: all the keys
//   left in a leaf, which the walk moves past at once, or, where the walk
//   crosses into another leaf, the separator alone. Stepping through a run
//   touches neither the walk nor its count. A fold (`for_each`) keeps both
//   runs in locals, which live in registers, and touches each walk once per
//   run.
// - **Strategies**, picked once as std picks them: when the sets' ranges
//   don't overlap, an answer without walking; when one set has at least 16
//   times the elements of the other, a lookup in the large one for each
//   element of the small one, 32 at a time as a batch (OG-doc 12); otherwise a
//   walk of both side by side.
#pragma once

#include <cstddef>
#include <cstdint>

#include "../detail/tree/batch.hpp"
#include "../detail/tree/walk.hpp"
#include "../insert_error.hpp"
#include "range.hpp"

namespace shoal::detail {

/// A set's elements left, a run of its walk at a time: `remaining` counts the
/// elements past the current run that the cursor still hands out.
template <class K, class S>
class SetCursor {
  using TreeType = Tree<K, Unit, S>;
  using SetWalk = Walk<K, Unit, S, false>;

 public:
  /// The elements of `tree`, which holds `length`; none, without walking,
  /// for a `length` of 0.
  SetCursor(const TreeType& tree, std::size_t length) noexcept : remaining_(length) {
    if (length != 0) walk_.start_all(tree);
  }

  SetCursor(const SetCursor&) = delete;
  SetCursor& operator=(const SetCursor&) = delete;

  std::size_t len() const noexcept { return run_.len() + remaining_; }

  SHOAL_ALWAYS_INLINE const K* peek() noexcept {
    if (const K* element = run_.peek()) return element;
    const Refill next = refill(walk_, remaining_);
    run_ = next.run;
    remaining_ = next.remaining;
    return run_.peek();
  }

  /// Past the element `peek` gave.
  SHOAL_ALWAYS_INLINE void advance() noexcept { run_.advance(); }

  SHOAL_ALWAYS_INLINE const K* next() noexcept {
    const K* element = peek();
    if (element != nullptr) advance();
    return element;
  }

  /// Leaves out the last element: the cursor ends before it. It has one.
  void drop_last() noexcept {
    if (remaining_ > 0) {
      --remaining_;
    } else {
      run_.truncate(run_.len() - 1);
    }
  }

  /// A cursor's next run and the elements past it.
  struct Refill {
    KeyRun<K> run;
    std::size_t remaining;
  };

  /// The next run from `walk`, cut to `remaining`, and the elements left
  /// past it; an empty run at the end. By value: a fold's run stays in
  /// registers only if its address goes to no call.
  SHOAL_NOINLINE static Refill refill(SetWalk& walk, std::size_t remaining) noexcept {
    KeyRun<K> run;
    if (remaining == 0 || !walk.next_key_run(run)) return Refill{KeyRun<K>(), 0};
    if (run.len() > remaining) run.truncate(remaining);
    return Refill{run, remaining - run.len()};
  }

  /// Hands every element left to `on(element)` while it returns true; false
  /// if it stopped. The run stays in locals.
  template <class On>
  bool for_each_while(On& on) noexcept {
    KeyRun<K> run = run_;
    std::size_t remaining = remaining_;
    for (;;) {
      const K* element = run.peek();
      if (element == nullptr) {
        const Refill next = refill(walk_, remaining);
        run = next.run;
        remaining = next.remaining;
        if ((element = run.peek()) == nullptr) break;
      }
      run.advance();
      if (!on(*element)) {
        run_ = run;
        remaining_ = remaining;
        return false;
      }
    }
    run_ = KeyRun<K>();
    remaining_ = 0;
    return true;
  }

  SetWalk& walk() noexcept { return walk_; }
  KeyRun<K>& run() noexcept { return run_; }
  std::size_t& remaining() noexcept { return remaining_; }

 private:
  SetWalk walk_;
  KeyRun<K> run_;
  std::size_t remaining_;
};

/// Where a merged element is: in the left set, the right one, or both.
enum class MergeSide : std::uint8_t { Left, Right, Both };

/// Two sets' elements merged in order.
template <class K, class S>
class MergeSides {
  using TreeType = Tree<K, Unit, S>;
  using Cursor = SetCursor<K, S>;

 public:
  /// The two sets' elements; a side given a length of 0 has none, and costs
  /// no walk: what a strategy that walks one set alone makes.
  MergeSides(const TreeType& left, std::size_t left_len, const TreeType& right, std::size_t right_len) noexcept
      : left_(left, left_len), right_(right, right_len) {}

  Cursor& left() noexcept { return left_; }
  Cursor& right() noexcept { return right_; }

  /// The next element in order and where it is, or null at the end of both.
  SHOAL_ALWAYS_INLINE const K* next_step(MergeSide& side) noexcept {
    const K* left = left_.peek();
    const K* right = right_.peek();
    if (left != nullptr && right != nullptr) {
      switch (SearchKey<K, K>(*left).cmp_full(*right)) {
        case Ordering::Less:
          left_.advance();
          side = MergeSide::Left;
          return left;
        case Ordering::Greater:
          right_.advance();
          side = MergeSide::Right;
          return right;
        case Ordering::Equal:
          left_.advance();
          right_.advance();
          side = MergeSide::Both;
          return left;
      }
    }
    if (left != nullptr) {
      left_.advance();
      side = MergeSide::Left;
      return left;
    }
    if (right != nullptr) {
      right_.advance();
      side = MergeSide::Right;
      return right;
    }
    return nullptr;
  }

  /// Hands every step to `on(side, element)` while it returns true; false if
  /// it stopped. With `LeftTail`, the left set's elements past the right
  /// set's end are steps too, and with `RightTail` the right's past the
  /// left's; without, the merge ends with the first side. Both runs stay in
  /// locals.
  template <bool LeftTail, bool RightTail, class On>
  bool for_each_step(On on) noexcept {
    KeyRun<K> left_run = left_.run(), right_run = right_.run();
    std::size_t left_remaining = left_.remaining(), right_remaining = right_.remaining();
    for (;;) {
      const K* left = left_run.peek();
      if (left == nullptr) {
        const auto next = Cursor::refill(left_.walk(), left_remaining);
        left_run = next.run;
        left_remaining = next.remaining;
        left = left_run.peek();
      }
      if (left == nullptr) {
        if constexpr (RightTail) {
          auto on_right = [&](const K& element) { return on(MergeSide::Right, element); };
          right_.run() = right_run;
          right_.remaining() = right_remaining;
          return right_.for_each_while(on_right);
        } else {
          return true;
        }
      }
      const K* right = right_run.peek();
      if (right == nullptr) {
        const auto next = Cursor::refill(right_.walk(), right_remaining);
        right_run = next.run;
        right_remaining = next.remaining;
        right = right_run.peek();
      }
      if (right == nullptr) {
        if constexpr (LeftTail) {
          auto on_left = [&](const K& element) { return on(MergeSide::Left, element); };
          left_.run() = left_run;
          left_.remaining() = left_remaining;
          return left_.for_each_while(on_left);
        } else {
          return true;
        }
      }
      MergeSide side;
      const K* element;
      switch (SearchKey<K, K>(*left).cmp_full(*right)) {
        case Ordering::Less:
          left_run.advance();
          side = MergeSide::Left;
          element = left;
          break;
        case Ordering::Greater:
          right_run.advance();
          side = MergeSide::Right;
          element = right;
          break;
        default:
          left_run.advance();
          right_run.advance();
          side = MergeSide::Both;
          element = left;
          break;
      }
      if (!on(side, *element)) {
        left_.run() = left_run;
        left_.remaining() = left_remaining;
        right_.run() = right_run;
        right_.remaining() = right_remaining;
        return false;
      }
    }
  }

 private:
  Cursor left_;
  Cursor right_;
};

/// Looks up a cursor's elements in a large set a batch at a time, for the
/// operations that search one set for the other's elements: a batch's
/// lookups overlap their waits for memory (OG-doc 12). The cursor's next 32
/// elements wait in `ahead`, found or not, handed out one at a time: a walk
/// is never copied to read ahead, as Rust clones its iterator (D16).
template <class K, class S>
class LookupsAhead {
  using TreeType = Tree<K, Unit, S>;
  using Batch = TreeBatch<K, Unit, S>;

 public:
  static constexpr std::size_t batch = 32;

  /// The cursor's next element, and in `found` whether `set`, of `set_len`
  /// elements, holds it; null at the cursor's end.
  SHOAL_ALWAYS_INLINE const K* next(SetCursor<K, S>& elements, const TreeType& set, std::size_t set_len,
                                     bool& found) noexcept {
    if (looked_up_ == 0 && elements.len() >= batch && Batch::batches(set_len)) look_up(elements, set, set_len);
    if (looked_up_ == 0) {
      const K* element = elements.next();
      if (element != nullptr) found = set.find(*element).has_value();
      return element;
    }
    const K* element = ahead_[batch - looked_up_];
    found = (found_ & 1) != 0;
    found_ >>= 1;
    --looked_up_;
    return element;
  }

 private:
  const K* ahead_[batch];
  std::uint32_t found_ = 0;
  std::uint8_t looked_up_ = 0;

  SHOAL_NOINLINE void look_up(SetCursor<K, S>& elements, const TreeType& set, std::size_t set_len) noexcept {
    for (std::size_t index = 0; index < batch; ++index) ahead_[index] = elements.next();
    const Unit* values[batch];
    Batch::find_many(set, PointedKeys<K>(ahead_), batch, values, set_len);
    found_ = 0;
    for (std::size_t index = 0; index < batch; ++index) {
      if (values[index] != nullptr) found_ |= std::uint32_t{1} << index;
    }
    looked_up_ = batch;
  }
};

/// The first and last elements of two sets, both non-empty, compared: where
/// the sets' ranges lie against each other.
struct SetEnds {
  Ordering self_min_against_other_max;
  Ordering self_max_against_other_min;

  template <class K, class S>
  static SetEnds of(const Tree<K, Unit, S>& self, const Tree<K, Unit, S>& other) noexcept {
    return SetEnds{SearchKey<K, K>(*self.first_key_value()->first).cmp_full(*other.last_key_value()->first),
                   SearchKey<K, K>(*self.last_key_value()->first).cmp_full(*other.first_key_value()->first)};
  }
};

/// Whether looking up `lookups` elements in a set of `len` one by one beats
/// walking it alongside them: at least 16 times as many, std's estimate. It
/// must be std's: it decides which of two equal elements an intersection
/// hands out.
constexpr bool prefers_lookups(std::size_t len, std::size_t lookups) noexcept { return lookups <= len / 16; }

/// What the set operations share: `next` and `for_each` over the elements,
/// and range-for.
template <class Operation, class K>
class SetSequence {
 public:
  class iterator {
   public:
    explicit iterator(Operation* operation) noexcept : operation_(operation), current_(operation->next()) {}
    const K& operator*() const noexcept { return *current_; }
    iterator& operator++() noexcept {
      current_ = operation_->next();
      return *this;
    }
    bool operator!=(::shoal::unsync::RangeEnd) const noexcept { return current_ != nullptr; }
    bool operator==(::shoal::unsync::RangeEnd) const noexcept { return current_ == nullptr; }

   private:
    Operation* operation_;
    const K* current_;
  };

  iterator begin() noexcept { return iterator(static_cast<Operation*>(this)); }
  ::shoal::unsync::RangeEnd end() const noexcept { return ::shoal::unsync::RangeEnd(); }
};

}  // namespace shoal::detail

namespace shoal::unsync {

/// The elements of one set not in another, in order.
template <class K, class S>
class Difference : public detail::SetSequence<Difference<K, S>, K> {
  using TreeType = detail::Tree<K, Unit, S>;

 public:
  Difference(const TreeType& self, std::size_t self_len, const TreeType& other, std::size_t other_len) noexcept
      : strategy_(pick(self, self_len, other, other_len)),
        sides_(self, self_len, other, strategy_ == Strategy::Stitch ? other_len : 0),
        other_(&other),
        other_len_(other_len) {
    if (strategy_ == Strategy::IterateButFirst) sides_.left().next();
    if (strategy_ == Strategy::IterateButLast) sides_.left().drop_last();
  }

  Difference(const Difference&) = delete;
  Difference& operator=(const Difference&) = delete;

  /// The next element, or null at the end.
  SHOAL_ALWAYS_INLINE const K* next() noexcept {
    auto& self = sides_.left();
    switch (strategy_) {
      case Strategy::Stitch: {
        // std's steps: this set's next element taken out, then the other's
        // peeked at, and passed over while not greater.
        auto& other = sides_.right();
        const K* self_next = self.next();
        while (self_next != nullptr) {
          const K* other_next = other.peek();
          const Ordering order =
              other_next == nullptr ? Ordering::Less : detail::SearchKey<K, K>(*self_next).cmp_full(*other_next);
          if (order == Ordering::Less) return self_next;
          if (order == Ordering::Equal) self_next = self.next();
          other.advance();
        }
        return nullptr;
      }
      case Strategy::Search:
        for (;;) {
          bool found = false;
          const K* element = lookups_.next(self, *other_, other_len_, found);
          if (element == nullptr || !found) return element;
        }
      default:
        return self.next();
    }
  }

  /// Hands every element left to `visit(element)`, in order.
  template <class Visit>
  void for_each(Visit visit) noexcept {
    if (strategy_ == Strategy::Stitch) {
      sides_.template for_each_step<true, false>([&](detail::MergeSide side, const K& element) {
        if (side == detail::MergeSide::Left) visit(element);
        return true;
      });
      return;
    }
    while (const K* element = next()) visit(*element);
  }

 private:
  enum class Strategy : std::uint8_t { Stitch, Search, Iterate, IterateButFirst, IterateButLast };
  using MergeSides = detail::MergeSides<K, S>;

  Strategy strategy_;
  MergeSides sides_;
  const TreeType* other_;
  std::size_t other_len_;
  detail::LookupsAhead<K, S> lookups_;

  static Strategy pick(const TreeType& self, std::size_t self_len, const TreeType& other,
                       std::size_t other_len) noexcept {
    if (self_len == 0 || other_len == 0) return Strategy::Iterate;
    const detail::SetEnds ends = detail::SetEnds::of(self, other);
    if (ends.self_min_against_other_max == Ordering::Greater || ends.self_max_against_other_min == Ordering::Less) {
      return Strategy::Iterate;
    }
    if (ends.self_min_against_other_max == Ordering::Equal) return Strategy::IterateButFirst;
    if (ends.self_max_against_other_min == Ordering::Equal) return Strategy::IterateButLast;
    if (detail::prefers_lookups(other_len, self_len)) return Strategy::Search;
    return Strategy::Stitch;
  }
};

/// The elements in both of two sets, in order: the first set's where it
/// walks both, the smaller set's where it looks them up in the larger.
template <class K, class S>
class Intersection : public detail::SetSequence<Intersection<K, S>, K> {
  using TreeType = detail::Tree<K, Unit, S>;

 public:
  Intersection(const TreeType& self, std::size_t self_len, const TreeType& other, std::size_t other_len) noexcept
      : strategy_(pick(self, self_len, other, other_len)),
        // The left side walks the small set when looking up in the large.
        sides_(strategy_ == Strategy::SearchSelf ? other : self,
               strategy_ == Strategy::SearchSelf    ? other_len
               : strategy_ == Strategy::SearchOther || strategy_ == Strategy::Stitch ? self_len
                                                                                      : 0,
               other, strategy_ == Strategy::Stitch ? other_len : 0),
        large_(strategy_ == Strategy::SearchSelf ? &self : &other),
        large_len_(strategy_ == Strategy::SearchSelf ? self_len : other_len) {
    if (strategy_ == Strategy::AnswerFirst) answer_ = self.first_key_value()->first;
    if (strategy_ == Strategy::AnswerLast) answer_ = self.last_key_value()->first;
  }

  Intersection(const Intersection&) = delete;
  Intersection& operator=(const Intersection&) = delete;

  SHOAL_ALWAYS_INLINE const K* next() noexcept {
    switch (strategy_) {
      case Strategy::Stitch: {
        // Each side's next element is taken out before comparing, as in
        // std's: the one left over when the other side ends is gone.
        auto& left = sides_.left();
        auto& right = sides_.right();
        const K* a = left.next();
        const K* b = a != nullptr ? right.next() : nullptr;
        while (a != nullptr && b != nullptr) {
          switch (detail::SearchKey<K, K>(*a).cmp_full(*b)) {
            case Ordering::Less: a = left.next(); break;
            case Ordering::Greater: b = right.next(); break;
            case Ordering::Equal: return a;
          }
        }
        return nullptr;
      }
      case Strategy::SearchOther:
      case Strategy::SearchSelf:
        for (;;) {
          bool found = false;
          const K* element = lookups_.next(sides_.left(), *large_, large_len_, found);
          if (element == nullptr || found) return element;
        }
      default: {
        const K* answer = answer_;
        answer_ = nullptr;
        return answer;
      }
    }
  }

  template <class Visit>
  void for_each(Visit visit) noexcept {
    if (strategy_ == Strategy::Stitch) {
      sides_.template for_each_step<false, false>([&](detail::MergeSide side, const K& element) {
        if (side == detail::MergeSide::Both) visit(element);
        return true;
      });
      return;
    }
    while (const K* element = next()) visit(*element);
  }

 private:
  enum class Strategy : std::uint8_t { Stitch, SearchOther, SearchSelf, AnswerNone, AnswerFirst, AnswerLast };
  using MergeSides = detail::MergeSides<K, S>;

  Strategy strategy_;
  MergeSides sides_;
  const TreeType* large_;
  std::size_t large_len_;
  const K* answer_ = nullptr;
  detail::LookupsAhead<K, S> lookups_;

  static Strategy pick(const TreeType& self, std::size_t self_len, const TreeType& other,
                       std::size_t other_len) noexcept {
    if (self_len == 0 || other_len == 0) return Strategy::AnswerNone;
    const detail::SetEnds ends = detail::SetEnds::of(self, other);
    if (ends.self_min_against_other_max == Ordering::Greater || ends.self_max_against_other_min == Ordering::Less) {
      return Strategy::AnswerNone;
    }
    if (ends.self_min_against_other_max == Ordering::Equal) return Strategy::AnswerFirst;
    if (ends.self_max_against_other_min == Ordering::Equal) return Strategy::AnswerLast;
    if (detail::prefers_lookups(other_len, self_len)) return Strategy::SearchOther;
    if (detail::prefers_lookups(self_len, other_len)) return Strategy::SearchSelf;
    return Strategy::Stitch;
  }
};

/// The elements in either of two sets, in order, the first set's where both
/// hold one.
template <class K, class S>
class Union : public detail::SetSequence<Union<K, S>, K> {
  using TreeType = detail::Tree<K, Unit, S>;

 public:
  Union(const TreeType& self, std::size_t self_len, const TreeType& other, std::size_t other_len) noexcept
      : sides_(self, self_len, other, other_len) {}

  Union(const Union&) = delete;
  Union& operator=(const Union&) = delete;

  SHOAL_ALWAYS_INLINE const K* next() noexcept {
    detail::MergeSide side;
    return sides_.next_step(side);
  }

  template <class Visit>
  void for_each(Visit visit) noexcept {
    sides_.template for_each_step<true, true>([&](detail::MergeSide, const K& element) {
      visit(element);
      return true;
    });
  }

 private:
  detail::MergeSides<K, S> sides_;
};

/// The elements in exactly one of two sets, in order.
template <class K, class S>
class SymmetricDifference : public detail::SetSequence<SymmetricDifference<K, S>, K> {
  using TreeType = detail::Tree<K, Unit, S>;

 public:
  SymmetricDifference(const TreeType& self, std::size_t self_len, const TreeType& other,
                      std::size_t other_len) noexcept
      : sides_(self, self_len, other, other_len) {}

  SymmetricDifference(const SymmetricDifference&) = delete;
  SymmetricDifference& operator=(const SymmetricDifference&) = delete;

  SHOAL_ALWAYS_INLINE const K* next() noexcept {
    detail::MergeSide side;
    for (;;) {
      const K* element = sides_.next_step(side);
      if (element == nullptr || side != detail::MergeSide::Both) return element;
    }
  }

  template <class Visit>
  void for_each(Visit visit) noexcept {
    sides_.template for_each_step<true, true>([&](detail::MergeSide side, const K& element) {
      if (side != detail::MergeSide::Both) visit(element);
      return true;
    });
  }

 private:
  detail::MergeSides<K, S> sides_;
};

}  // namespace shoal::unsync

namespace shoal::detail {

/// Whether every element of one set is in another (`subset.rs`): both ends
/// settle first, by the sets' first and last elements; then, as for a
/// difference, a lookup in `other` for each element left if `other` is much
/// larger, or a walk of both side by side, giving up at the first element
/// missing.
template <class K, class S>
struct SetSubset {
  static bool holds(const Tree<K, Unit, S>& self, std::size_t self_len, const Tree<K, Unit, S>& other,
                    std::size_t other_len) noexcept {
    if (self_len > other_len) return false;
    if (self_len == 0) return true;
    const Ordering at_first =
        SearchKey<K, K>(*self.first_key_value()->first).cmp_full(*other.first_key_value()->first);
    const Ordering at_last = SearchKey<K, K>(*self.last_key_value()->first).cmp_full(*other.last_key_value()->first);
    if (at_first == Ordering::Less || at_last == Ordering::Greater) return false;
    // The elements left once the ends that equal `other`'s are dropped:
    // neither of `other`'s ends is among them.
    const bool drops_first = at_first == Ordering::Equal;
    const bool drops_last = at_last == Ordering::Equal && (self_len > 1 || !drops_first);
    const std::size_t left = self_len - drops_first - drops_last;
    if (prefers_lookups(other_len, left)) {
      SetCursor<K, S> elements(self, self_len);
      trim(elements, drops_first, drops_last);
      LookupsAhead<K, S> lookups;
      for (;;) {
        bool found = false;
        if (lookups.next(elements, other, other_len, found) == nullptr) return true;
        if (!found) return false;
      }
    }
    MergeSides<K, S> sides(self, self_len, other, other_len);
    trim(sides.left(), drops_first, drops_last);
    trim(sides.right(), true, true);
    // An element of `self` alone, or left once `other` ends, is missing.
    return sides.template for_each_step<true, false>(
        [](MergeSide side, const K&) { return side != MergeSide::Left; });
  }

 private:
  static void trim(SetCursor<K, S>& cursor, bool first, bool last) noexcept {
    if (first && cursor.len() > 0) cursor.next();
    if (last && cursor.len() > 0) cursor.drop_last();
  }
};

}  // namespace shoal::detail
