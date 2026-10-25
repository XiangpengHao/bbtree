// `bbtree::unsync::BBTreeSet`: an ordered set for one thread
// (`unsync/set/mod.rs`), with the API and behavior of Rust's `BTreeSet`
// (D1). Like std's set, it is a map of zero-sized values, a `BBTreeMap` of
// `Unit`: each method hands its work to the map's method of the same job, and
// the iterators wrap the map's walks. The operations on two sets are in
// `set_algebra.hpp`; their operators build the resulting set into full nodes
// from the ascending elements.
#pragma once

#include <algorithm>
#include <cstddef>
#include <iterator>
#include <optional>
#include <utility>
#include <vector>

#include "map.hpp"
#include "set_algebra.hpp"

namespace bbtree::unsync {

/// A set's elements between two bounds, in order or `Reverse`.
template <class K, class S, bool Reverse>
class SetRange {
  using Entries = Range<K, Unit, S, true, Reverse>;

 public:
  template <class L, class U>
  SetRange(const detail::Tree<K, Unit, S>& tree, const Bound<L>& lower, const Bound<U>& upper) noexcept
      : entries_(tree, lower, upper) {}
  explicit SetRange(const detail::Tree<K, Unit, S>& tree) noexcept : entries_(tree) {}

  SetRange(const SetRange&) = delete;
  SetRange& operator=(const SetRange&) = delete;

  /// The next element, or null at the end.
  BBTREE_ALWAYS_INLINE const K* next() noexcept {
    const auto entry = entries_.next();
    return entry ? &entry->first : nullptr;
  }

  /// Hands every element left to `visit(element)`, a counted loop per leaf.
  template <class Visit>
  void for_each(Visit visit) noexcept {
    entries_.for_each([&](const K& element, const Unit&) { visit(element); });
  }

  class iterator {
   public:
    explicit iterator(SetRange* range) noexcept : range_(range), current_(range->next()) {}
    const K& operator*() const noexcept { return *current_; }
    BBTREE_ALWAYS_INLINE iterator& operator++() noexcept {
      current_ = range_->next();
      return *this;
    }
    bool operator!=(RangeEnd) const noexcept { return current_ != nullptr; }
    bool operator==(RangeEnd) const noexcept { return current_ == nullptr; }

   private:
    SetRange* range_;
    const K* current_;
  };

  iterator begin() noexcept { return iterator(this); }
  RangeEnd end() const noexcept { return RangeEnd(); }

 private:
  Entries entries_;
};

/// An iterator over every element of a set that holds its walk: what a
/// set's `begin()` returns, built in place. It can't be copied.
template <class K, class S>
class SetIterator {
 public:
  explicit SetIterator(const detail::Tree<K, Unit, S>& tree) noexcept : entries_(tree) {}
  const K& operator*() const noexcept { return (*entries_).first; }
  BBTREE_ALWAYS_INLINE SetIterator& operator++() noexcept {
    ++entries_;
    return *this;
  }
  bool operator!=(RangeEnd end) const noexcept { return entries_ != end; }
  bool operator==(RangeEnd end) const noexcept { return entries_ == end; }

 private:
  MapIterator<K, Unit, S, true> entries_;
};

/// A set's elements between two bounds that `pick(element)` picks, each
/// removed as it is handed out, in order. Lazy: elements it doesn't reach
/// stay. Made in place by the set's `extract_if`, never moved.
template <class K, class S, class A, class U, class Pick>
class SetExtractIf {
 public:
  template <class L>
  SetExtractIf(detail::Tree<K, Unit, S>& tree, std::size_t& length, detail::MapPages<A> pages,
               const Bound<L>& lower, const Bound<U>& upper, Pick pick) noexcept
      : walk_(tree, length, pages, lower, upper), pick_(std::move(pick)) {}

  SetExtractIf(const SetExtractIf&) = delete;
  SetExtractIf& operator=(const SetExtractIf&) = delete;

  /// The next element picked, removed, if any.
  std::optional<K> next() noexcept {
    auto by_element = [&](const K& element, Unit&) { return pick_(element); };
    auto entry = walk_.next(by_element);
    if (!entry) return std::nullopt;
    return std::move(entry->first);
  }

  class iterator {
   public:
    explicit iterator(SetExtractIf* extraction) noexcept : extraction_(extraction), current_(extraction->next()) {}
    K& operator*() noexcept { return *current_; }
    iterator& operator++() noexcept {
      current_ = extraction_->next();
      return *this;
    }
    bool operator!=(RangeEnd) const noexcept { return current_.has_value(); }
    bool operator==(RangeEnd) const noexcept { return !current_.has_value(); }

   private:
    SetExtractIf* extraction_;
    std::optional<K> current_;
  };

  iterator begin() noexcept { return iterator(this); }
  RangeEnd end() const noexcept { return RangeEnd(); }

 private:
  ExtractionWalk<K, Unit, S, A, U> walk_;
  Pick pick_;
};

/// An ordered set of `K`, for one thread: keys order by `operator<`, with
/// inline keys where `InlineKey<K>` is specialized (PORTING.md, P1), and move
/// as bytes (P2).
template <class K, class Config = DefaultNodeConfig, class A = Global>
class BBTreeSet {
  using Map = BBTreeMap<K, Unit, Config, A>;
  using Strategy = detail::Strategy<Config>;
  using TreeType = detail::Tree<K, Unit, Strategy>;

 public:
  using key_type = K;
  using value_type = K;
  using size_type = std::size_t;
  using allocator_type = A;

  BBTreeSet() noexcept(std::is_nothrow_default_constructible_v<A>) = default;
  explicit BBTreeSet(A alloc) noexcept : map_(std::move(alloc)) {}

  /// A set of elements that strictly ascend, built node by node, every node
  /// full but along the right border, comparing no elements. Elements out of
  /// order make a set whose searches give wrong answers, never undefined
  /// behavior.
  template <class Iterator>
  static BBTreeSet from_sorted(Iterator first, Iterator last, A alloc = A()) {
    BBTreeSet set(std::move(alloc));
    // The builder keeps a reference to the pages: they outlive it here.
    const detail::MapPages<A> pages = set.map_.pages();
    detail::TreeBuilder<K, Unit, Strategy, detail::MapPages<A>> builder(pages);
    for (; first != last; ++first) builder.push(K(*first), Unit{});
    set.finish(builder);
    return set;
  }

  /// A set of `elements`, in any order: sorted, the last of equal elements
  /// kept, as Rust's `collect` keeps, then built as `from_sorted` builds.
  static BBTreeSet from_elements(std::vector<K> elements, A alloc = A()) {
    std::stable_sort(elements.begin(), elements.end(), [](const K& a, const K& b) { return a < b; });
    std::size_t kept = 0;
    for (std::size_t index = 0; index < elements.size(); ++index) {
      if (index + 1 == elements.size() || elements[index] < elements[index + 1]) {
        elements[kept++] = std::move(elements[index]);
      }
    }
    elements.resize(kept);
    return from_sorted(std::make_move_iterator(elements.begin()), std::make_move_iterator(elements.end()),
                       std::move(alloc));
  }

  size_type size() const noexcept { return map_.size(); }
  bool empty() const noexcept { return map_.empty(); }
  const A& allocator() const noexcept { return map_.allocator(); }

  // Single elements.

  /// Inserts `element`; false, keeping the element there, if the set holds
  /// an equal one.
  bool insert(K element) noexcept { return !map_.insert(std::move(element), Unit{}).has_value(); }

  /// Puts `element` in place of an equal element and returns that one; or
  /// inserts it and returns `nullopt`.
  std::optional<K> replace(K element) noexcept { return map_.replace_key(std::move(element), Unit{}); }

  template <class Q>
  bool contains(const Q& element) const noexcept {
    return map_.contains_key(element);
  }

  /// The element equal to `element`, as stored, if any.
  template <class Q>
  const K* get(const Q& element) const noexcept {
    const auto found = map_.get_key_value(element);
    return found ? &found->first : nullptr;
  }

  /// Removes the element equal to `element`; false if there is none.
  template <class Q>
  bool remove(const Q& element) noexcept {
    return map_.remove(element).has_value();
  }

  /// Removes the element equal to `element` and returns it, as stored.
  template <class Q>
  std::optional<K> take(const Q& element) noexcept {
    auto entry = map_.remove_entry(element);
    if (!entry) return std::nullopt;
    return std::move(entry->first);
  }

  const K* first() const noexcept {
    const auto entry = map_.first_key_value();
    return entry ? &entry->first : nullptr;
  }

  const K* last() const noexcept {
    const auto entry = map_.last_key_value();
    return entry ? &entry->first : nullptr;
  }

  std::optional<K> pop_first() noexcept {
    auto entry = map_.pop_first();
    if (!entry) return std::nullopt;
    return std::move(entry->first);
  }

  std::optional<K> pop_last() noexcept {
    auto entry = map_.pop_last();
    if (!entry) return std::nullopt;
    return std::move(entry->first);
  }

  // Batches (D53).

  /// Whether the set holds each element: `found[i]` for `elements[i]`, as
  /// `count` separate `contains` say, with their waits for memory overlapped.
  template <class Q>
  void contains_many(const Q* elements, size_type count, bool* found) const noexcept {
    constexpr std::size_t chunk = detail::batch_lanes;
    const Unit* values[chunk];
    for (std::size_t start = 0; start < count; start += chunk) {
      const std::size_t len = count - start < chunk ? count - start : chunk;
      map_.get_many(elements + start, len, values);
      for (std::size_t index = 0; index < len; ++index) found[start + index] = values[index] != nullptr;
    }
  }

  /// Inserts `elements[i]` in order, as `count` separate `insert`s do;
  /// `inserted[i]` says whether each was new. The elements are moved from.
  void insert_many(K* elements, size_type count, bool* inserted) noexcept {
    constexpr std::size_t chunk = detail::batch_lanes;
    Unit values[chunk];
    std::optional<Unit> previous[chunk];
    for (std::size_t start = 0; start < count; start += chunk) {
      const std::size_t len = count - start < chunk ? count - start : chunk;
      map_.insert_many(elements + start, values, len, previous);
      for (std::size_t index = 0; index < len; ++index) inserted[start + index] = !previous[index].has_value();
    }
  }

  // Iteration.

  using const_range = SetRange<K, Strategy, false>;
  using const_reverse_range = SetRange<K, Strategy, true>;

  /// Every element, in order: `for (const K& element : set)`.
  SetIterator<K, Strategy> begin() const noexcept { return SetIterator<K, Strategy>(tree()); }
  RangeEnd end() const noexcept { return RangeEnd(); }

  /// Every element, in order, as a range.
  const_range iter() const noexcept { return const_range(tree()); }

  /// Every element, in reverse order.
  const_reverse_range iter_rev() const noexcept { return const_reverse_range(tree()); }

  /// The elements within `lower` and `upper`, in order; none if the bounds
  /// cross.
  template <class L, class U>
  const_range range(const Bound<L>& lower, const Bound<U>& upper) const noexcept {
    return const_range(tree(), lower, upper);
  }

  template <class L, class U>
  const_reverse_range range_rev(const Bound<L>& lower, const Bound<U>& upper) const noexcept {
    return const_reverse_range(tree(), lower, upper);
  }

  /// Hands every element to `visit(element)` in order, a counted loop per
  /// leaf.
  template <class Visit>
  void for_each(Visit visit) const noexcept {
    map_.for_each([&](const K& element, const Unit&) { visit(element); });
  }

  // Changes in bulk.

  /// Keeps only the elements for which `keep(element)` holds.
  template <class Keep>
  void retain(Keep keep) noexcept {
    map_.retain([&](const K& element, Unit&) { return keep(element); });
  }

  /// The elements within `lower` and `upper` that `pick(element)` picks,
  /// each removed as it is handed out, in order. It keeps a copy of
  /// `upper`'s key.
  template <class L, class U, class Pick>
  SetExtractIf<K, Strategy, A, U, Pick> extract_if(const Bound<L>& lower, const Bound<U>& upper,
                                                   Pick pick) noexcept {
    return SetExtractIf<K, Strategy, A, U, Pick>(map_.state_.tree, map_.state_.length, map_.pages(), lower, upper,
                                                 std::move(pick));
  }

  template <class Pick>
  SetExtractIf<K, Strategy, A, K, Pick> extract_if(Pick pick) noexcept {
    return extract_if(Bound<K>::unbounded(), Bound<K>::unbounded(), std::move(pick));
  }

  /// Splits the set at `element`: this set keeps the elements before it, the
  /// returned one the elements from it on.
  template <class Q>
  BBTreeSet split_off(const Q& element) noexcept {
    BBTreeSet upper{A(allocator())};
    upper.map_ = map_.split_off(element);
    return upper;
  }

  /// Moves every element of `other` into this set, `other`'s where both hold
  /// one, and leaves `other` empty.
  void append(BBTreeSet& other) noexcept { map_.append(other.map_); }

  void clear() noexcept { map_.clear(); }

  // Operations on two sets (`set_algebra.hpp`), lazy.

  using difference_type = Difference<K, Strategy>;
  using intersection_type = Intersection<K, Strategy>;
  using union_type = Union<K, Strategy>;
  using symmetric_difference_type = SymmetricDifference<K, Strategy>;

  /// The elements of this set not in `other`.
  difference_type difference(const BBTreeSet& other) const noexcept {
    return difference_type(tree(), size(), other.tree(), other.size());
  }

  /// The elements in both sets: this set's where it walks both, the smaller
  /// set's where it looks them up in the larger, as std's.
  intersection_type intersection(const BBTreeSet& other) const noexcept {
    return intersection_type(tree(), size(), other.tree(), other.size());
  }

  /// The elements in either set, this set's where both hold one. (`union` is
  /// a keyword in C++.)
  union_type union_with(const BBTreeSet& other) const noexcept {
    return union_type(tree(), size(), other.tree(), other.size());
  }

  /// The elements in exactly one of the sets.
  symmetric_difference_type symmetric_difference(const BBTreeSet& other) const noexcept {
    return symmetric_difference_type(tree(), size(), other.tree(), other.size());
  }

  bool is_subset(const BBTreeSet& other) const noexcept {
    return detail::SetSubset<K, Strategy>::holds(tree(), size(), other.tree(), other.size());
  }

  bool is_superset(const BBTreeSet& other) const noexcept { return other.is_subset(*this); }

  bool is_disjoint(const BBTreeSet& other) const noexcept { return intersection(other).next() == nullptr; }

  /// The sets the operations make: elements copied, built into full nodes.
  friend BBTreeSet operator-(const BBTreeSet& a, const BBTreeSet& b) {
    auto elements = a.difference(b);
    return collect(elements, a.allocator());
  }
  friend BBTreeSet operator&(const BBTreeSet& a, const BBTreeSet& b) {
    auto elements = a.intersection(b);
    return collect(elements, a.allocator());
  }
  friend BBTreeSet operator|(const BBTreeSet& a, const BBTreeSet& b) {
    auto elements = a.union_with(b);
    return collect(elements, a.allocator());
  }
  friend BBTreeSet operator^(const BBTreeSet& a, const BBTreeSet& b) {
    auto elements = a.symmetric_difference(b);
    return collect(elements, a.allocator());
  }

  void check_invariants() const { map_.check_invariants(); }
  void check_same_nodes(const BBTreeSet& other) const { map_.check_same_nodes(other.map_); }

 private:
  Map map_;

  const TreeType& tree() const noexcept { return map_.state_.tree; }

  template <class Builder>
  void finish(Builder& builder) noexcept {
    auto [tree, length] = builder.finish();
    map_.state_.tree = tree;
    map_.state_.length = length;
  }

  /// A set of the ascending elements `elements` hands out, copied.
  template <class Elements>
  static BBTreeSet collect(Elements& elements, const A& alloc) {
    BBTreeSet set{A(alloc)};
    // The builder keeps a reference to the pages: they outlive it here.
    const detail::MapPages<A> pages = set.map_.pages();
    detail::TreeBuilder<K, Unit, Strategy, detail::MapPages<A>> builder(pages);
    elements.for_each([&](const K& element) { builder.push(K(element), Unit{}); });
    set.finish(builder);
    return set;
  }
};

}  // namespace bbtree::unsync
