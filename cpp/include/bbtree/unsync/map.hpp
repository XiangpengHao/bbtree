// `bbtree::unsync::BBTreeMap`: the ordered map for one thread
// (`unsync/map/mod.rs`, D4), over the tree both maps use. Its API follows
// Rust's `BTreeMap`, whose semantics the Rust crate keeps (D1), in C++
// spelling where std has a name for it: `size`, `empty`, `clear`, `begin`.
#pragma once

#include <algorithm>
#include <cstddef>
#include <iterator>
#include <optional>
#include <vector>
#include <type_traits>
#include <utility>

#include "../allocator.hpp"
#include "../detail/tree/batch.hpp"
#include "../detail/tree/build.hpp"
#include "../detail/tree/invariants.hpp"
#include "../detail/tree/remove.hpp"
#include "../detail/tree/teardown.hpp"
#include "../detail/tree/tree.hpp"
#include "../node_config.hpp"
#include "../relocatable.hpp"
#include "entry.hpp"
#include "extract_if.hpp"
#include "into_iter.hpp"
#include "range.hpp"

namespace bbtree::unsync {

template <class K, class Config, class A>
class BBTreeSet;

/// An ordered map of keys `K` to values `V`, for one thread. Keys order by
/// `operator<`; `InlineKey<K>` gives them inline keys where it is specialized
/// (PORTING.md, P1). Keys and values are trivially relocatable (P2).
template <class K, class V, class Config = DefaultNodeConfig, class A = Global>
class BBTreeMap {
  static_assert(is_trivially_relocatable_v<K>,
                "bbtree moves keys as bytes: specialize bbtree::is_trivially_relocatable for K");
  static_assert(is_trivially_relocatable_v<V>,
                "bbtree moves values as bytes: specialize bbtree::is_trivially_relocatable for V");
  static_assert(std::is_nothrow_move_constructible_v<K> && std::is_nothrow_move_constructible_v<V>,
                "keys and values move without throwing");

  using Strategy = detail::Strategy<Config>;
  using TreeType = detail::Tree<K, V, Strategy>;
  using Pages = detail::MapPages<A>;

 public:
  using key_type = K;
  using mapped_type = V;
  using size_type = std::size_t;
  using allocator_type = A;

  /// An empty map, which allocates nothing until its first insert.
  BBTreeMap() noexcept(std::is_nothrow_default_constructible_v<A>) = default;
  explicit BBTreeMap(A alloc) noexcept : state_(std::move(alloc)) {}

  BBTreeMap(BBTreeMap&& other) noexcept : state_(std::move(other.state_.allocator())) {
    state_.tree = other.state_.tree.take();
    state_.length = other.state_.length;
    other.state_.length = 0;
  }

  BBTreeMap& operator=(BBTreeMap&& other) noexcept {
    if (this != &other) {
      clear();
      state_.allocator() = std::move(other.state_.allocator());
      state_.tree = other.state_.tree.take();
      state_.length = other.state_.length;
      other.state_.length = 0;
    }
    return *this;
  }

  /// A copy, node by node, of the same shape (`tree/clone.rs`): keys and
  /// values copied by their copy constructors; if one throws, the partial
  /// copy is taken apart and the exception goes on.
  BBTreeMap(const BBTreeMap& other) : state_(A(other.state_.allocator())) {
    state_.tree = detail::TreeReshape<K, V, Strategy>::clone_in(other.state_.tree, pages());
    state_.length = other.state_.length;
  }

  BBTreeMap& operator=(const BBTreeMap& other) {
    if (this != &other) {
      BBTreeMap copy(other);
      *this = std::move(copy);
    }
    return *this;
  }

  /// A map of entries whose keys strictly ascend, built node by node, every
  /// node full but along the right border (D25), comparing no keys. Keys out
  /// of order make a map whose searches give wrong answers, never undefined
  /// behavior.
  template <class Iterator>
  static BBTreeMap from_sorted(Iterator first, Iterator last, A alloc = A()) {
    BBTreeMap map(std::move(alloc));
    const Pages pages = map.pages();
    detail::TreeBuilder<K, V, Strategy, Pages> builder(pages);
    for (; first != last; ++first) {
      auto&& entry = *first;
      builder.push(K(std::move(entry.first)), V(std::move(entry.second)));
    }
    auto [tree, length] = builder.finish();
    map.state_.tree = tree;
    map.state_.length = length;
    return map;
  }

  /// A map of `entries`, in any order: sorted, the last of equal keys kept,
  /// as Rust's `collect` and `std::map` assignment keep, then built as
  /// `from_sorted` builds.
  static BBTreeMap from_entries(std::vector<std::pair<K, V>> entries, A alloc = A()) {
    std::stable_sort(entries.begin(), entries.end(),
                     [](const auto& a, const auto& b) { return a.first < b.first; });
    std::size_t kept = 0;
    for (std::size_t index = 0; index < entries.size(); ++index) {
      const bool last_of_equal =
          index + 1 == entries.size() || entries[index].first < entries[index + 1].first;
      if (last_of_equal) entries[kept++] = std::move(entries[index]);
    }
    entries.resize(kept);
    return from_sorted(std::make_move_iterator(entries.begin()), std::make_move_iterator(entries.end()),
                       std::move(alloc));
  }

  ~BBTreeMap() { clear(); }

  size_type size() const noexcept { return state_.length; }
  bool empty() const noexcept { return state_.length == 0; }
  const A& allocator() const noexcept { return state_.allocator(); }

  /// Inserts `key` and `value`. If the map had an entry with an equal key, it
  /// keeps that key, replaces the value, and returns the old one.
  std::optional<V> insert(K key, V value) noexcept {
    return state_.tree.insert(std::move(key), std::move(value), pages(), state_.length);
  }

  /// The value of the entry whose key equals `key`, if any.
  template <class Q>
  BBTREE_ALWAYS_INLINE const V* get(const Q& key) const noexcept {
    const auto found = state_.tree.find(detail::Query<K>::of(key));
    return found ? found->second : nullptr;
  }

  template <class Q>
  V* get_mut(const Q& key) noexcept {
    state_.tree.forget_rightmost_leaf();
    const auto found = state_.tree.find(detail::Query<K>::of(key));
    return found ? found->second : nullptr;
  }

  /// The entry whose key equals `key`, if any.
  template <class Q>
  std::optional<std::pair<const K&, const V&>> get_key_value(const Q& key) const noexcept {
    if (const auto found = state_.tree.find(detail::Query<K>::of(key))) {
      return std::pair<const K&, const V&>(*found->first, *found->second);
    }
    return std::nullopt;
  }

  template <class Q>
  BBTREE_ALWAYS_INLINE bool contains_key(const Q& key) const noexcept {
    return state_.tree.find(detail::Query<K>::of(key)).has_value();
  }

  /// Removes the entry whose key equals `key` and returns its value.
  template <class Q>
  std::optional<V> remove(const Q& key) noexcept {
    if (auto entry = remove_entry(key)) return std::move(entry->second);
    return std::nullopt;
  }

  /// Removes the entry whose key equals `key` and returns it, its key as
  /// stored.
  template <class Q>
  std::optional<std::pair<K, V>> remove_entry(const Q& key) noexcept {
    return detail::TreeRemoval<K, V, Strategy>::remove_entry(state_.tree, detail::Query<K>::of(key), pages(),
                                                               state_.length);
  }

  /// The entry with the smallest key, if any.
  std::optional<std::pair<const K&, const V&>> first_key_value() const noexcept {
    if (const auto found = state_.tree.first_key_value()) {
      return std::pair<const K&, const V&>(*found->first, *found->second);
    }
    return std::nullopt;
  }

  /// The entry with the largest key, if any.
  std::optional<std::pair<const K&, const V&>> last_key_value() const noexcept {
    if (const auto found = state_.tree.last_key_value()) {
      return std::pair<const K&, const V&>(*found->first, *found->second);
    }
    return std::nullopt;
  }

  /// Removes the entry with the smallest key and returns it.
  std::optional<std::pair<K, V>> pop_first() noexcept {
    return detail::TreeRemoval<K, V, Strategy>::pop_first(state_.tree, pages(), state_.length);
  }

  /// Removes the entry with the largest key and returns it.
  std::optional<std::pair<K, V>> pop_last() noexcept {
    return detail::TreeRemoval<K, V, Strategy>::pop_last(state_.tree, pages(), state_.length);
  }

  // Batches (D53, design/12-batch-api.md).

  /// Each key's value, `values[i]` for `keys[i]`, null for a key the map
  /// lacks: as `count` separate `get`s give them, with their waits for memory
  /// overlapped, `batch_lanes` keys at a time.
  template <class Q>
  void get_many(const Q* keys, size_type count, const V** values) const noexcept {
    detail::TreeBatch<K, V, Strategy>::find_many(state_.tree, keys, count, values, state_.length);
  }

  /// Inserts `keys[i]` and `values[i]` in order, as `count` separate
  /// `insert`s do, the tree node by node included; `previous[i]` gets the
  /// value each replaced. The keys and values are moved from.
  void insert_many(K* keys, V* values, size_type count, std::optional<V>* previous) noexcept {
    detail::TreeBatch<K, V, Strategy>::insert_many(state_.tree, keys, values, count, previous, pages(),
                                                   state_.length);
  }

  // Iteration.

  using const_range = Range<K, V, Strategy, true, false>;
  using mut_range = Range<K, V, Strategy, false, false>;
  using const_reverse_range = Range<K, V, Strategy, true, true>;
  using mut_reverse_range = Range<K, V, Strategy, false, true>;

  /// Every entry, in key order: `for (auto [key, value] : map)`.
  MapIterator<K, V, Strategy, true> begin() const noexcept {
    return MapIterator<K, V, Strategy, true>(state_.tree);
  }
  MapIterator<K, V, Strategy, false> begin() noexcept {
    state_.tree.forget_rightmost_leaf();
    return MapIterator<K, V, Strategy, false>(state_.tree);
  }
  RangeEnd end() const noexcept { return RangeEnd(); }

  /// Every entry, in key order, as a range.
  const_range iter() const noexcept { return const_range(state_.tree); }
  mut_range iter_mut() noexcept {
    state_.tree.forget_rightmost_leaf();
    return mut_range(state_.tree);
  }
  /// Every entry, in reverse key order.
  const_reverse_range iter_rev() const noexcept { return const_reverse_range(state_.tree); }

  /// The entries within `lower` and `upper`, in key order; none if the bounds
  /// cross.
  template <class L, class U>
  const_range range(const Bound<L>& lower, const Bound<U>& upper) const noexcept {
    return const_range(state_.tree, lower, upper);
  }
  template <class L, class U>
  mut_range range_mut(const Bound<L>& lower, const Bound<U>& upper) noexcept {
    state_.tree.forget_rightmost_leaf();
    return mut_range(state_.tree, lower, upper);
  }
  /// The entries within `lower` and `upper`, in reverse key order.
  template <class L, class U>
  const_reverse_range range_rev(const Bound<L>& lower, const Bound<U>& upper) const noexcept {
    return const_reverse_range(state_.tree, lower, upper);
  }

  /// Hands every entry to `visit(key, value)` in key order, a counted loop
  /// per leaf (D32).
  template <class Visit>
  void for_each(Visit visit) const noexcept {
    iter().for_each(visit);
  }

  /// Inserts every entry of a range, as separate `insert`s do: once the map
  /// is past the batch cutoff, sixteen at a time through `insert_many`
  /// (doc 12).
  template <class Iterator>
  void extend(Iterator first, Iterator last) {
    constexpr std::size_t chunk = 16;
    std::vector<K> keys;
    std::vector<V> values;
    std::optional<V> previous[chunk];
    while (first != last) {
      if (!detail::TreeBatch<K, V, Strategy>::batches(state_.length)) {
        auto&& entry = *first;
        insert(K(std::move(entry.first)), V(std::move(entry.second)));
        ++first;
        continue;
      }
      keys.clear();
      values.clear();
      for (; keys.size() < chunk && first != last; ++first) {
        auto&& entry = *first;
        keys.push_back(K(std::move(entry.first)));
        values.push_back(V(std::move(entry.second)));
      }
      insert_many(keys.data(), values.data(), keys.size(), previous);
    }
  }

  /// Splits the map at `key`: this map keeps the entries before it, the
  /// returned one the entries from it on (`tree/cut.rs`, D25), in
  /// O(log n) plus counting the shorter part.
  template <class Q>
  BBTreeMap split_off(const Q& key) noexcept {
    BBTreeMap upper(A(state_.allocator()));
    if (!state_.tree.has_root()) return upper;
    using Reshape = detail::TreeReshape<K, V, Strategy>;
    upper.state_.tree =
        Reshape::split_off_at(state_.tree, detail::Query<K>::of(key), detail::KeySide::Before, pages());
    const auto [lower_length, upper_length] = Reshape::lengths(state_.tree, upper.state_.tree, state_.length);
    state_.length = lower_length;
    upper.state_.length = upper_length;
    return upper;
  }

  /// Keeps only the entries for which `keep(key, value)` holds, in key
  /// order, removing the others in place (D25).
  template <class Keep>
  void retain(Keep keep) noexcept {
    auto drop = [&](const K& key, V& value) { return !keep(key, value); };
    ExtractionWalk<K, V, Strategy, A, K> walk(state_.tree, state_.length, pages(), Bound<K>::unbounded(),
                                              Bound<K>::unbounded());
    while (walk.next(drop)) {
    }
  }

  /// The entries within `lower` and `upper` that `pick(key, value)` picks,
  /// each removed as it is handed out, in key order. Lazy: entries the
  /// extraction doesn't reach stay. It keeps a copy of `upper`'s key.
  template <class L, class U, class Pick>
  ExtractIf<K, V, Strategy, A, U, Pick> extract_if(const Bound<L>& lower, const Bound<U>& upper,
                                                   Pick pick) noexcept {
    return ExtractIf<K, V, Strategy, A, U, Pick>(state_.tree, state_.length, pages(), lower, upper,
                                                 std::move(pick));
  }

  /// `extract_if` over the whole map.
  template <class Pick>
  ExtractIf<K, V, Strategy, A, K, Pick> extract_if(Pick pick) noexcept {
    return extract_if(Bound<K>::unbounded(), Bound<K>::unbounded(), std::move(pick));
  }

  /// Moves every entry of `other` into this map, `other`'s value winning
  /// where both hold a key, and leaves `other` empty: both maps' entries
  /// merged in key order and built into full nodes, in O(n + m), as std's.
  void append(BBTreeMap& other) noexcept {
    if (other.empty()) return;
    if (empty()) {
      std::swap(state_.tree, other.state_.tree);
      std::swap(state_.length, other.state_.length);
      return;
    }
    IntoIter<K, V, Strategy, A> ours(state_.tree.take(), std::exchange(state_.length, 0), state_.allocator());
    IntoIter<K, V, Strategy, A> theirs(other.state_.tree.take(), std::exchange(other.state_.length, 0),
                                       other.state_.allocator());
    const Pages map_pages = pages();
    detail::TreeBuilder<K, V, Strategy, Pages> merged(map_pages);
    for (;;) {
      const auto our_next = ours.peek();
      const auto their_next = theirs.peek();
      if (!our_next && !their_next) break;
      const Ordering order =
          !their_next ? Ordering::Less
          : !our_next ? Ordering::Greater
                      : detail::SearchKey<K, K>(our_next->first).cmp_full(their_next->first);
      if (order == Ordering::Less) {
        auto [key, value] = *ours.next();
        merged.push(std::move(key), std::move(value));
      } else if (order == Ordering::Greater) {
        auto [key, value] = *theirs.next();
        merged.push(std::move(key), std::move(value));
      } else {
        // Our key, their value: as `insert` keeps the key it has.
        K key = std::move(ours.next()->first);
        merged.push(std::move(key), std::move(theirs.next()->second));
      }
    }
    auto [tree, length] = merged.finish();
    state_.tree = tree;
    state_.length = length;
  }

  // Entries (`unsync/map/entry.rs`).

  using entry_type = Entry<K, V, Strategy, A>;
  using occupied_entry = OccupiedEntry<K, V, Strategy, A>;
  using vacant_entry = VacantEntry<K, V, Strategy, A>;

  /// `key`'s entry, to change in place. Every comparison of keys happens
  /// here; the entry's operations compare none.
  entry_type entry(K key) noexcept {
    detail::MutStack<K, V, Strategy> stack(state_.tree);
    if (!state_.tree.has_root()) {
      return entry_type(vacant_entry(std::move(key), stack, state_.length, pages(), false));
    }
    if (stack.search(detail::SearchKey<K, K>(key))) {
      return entry_type(occupied_entry(stack, state_.length, pages()));
    }
    return entry_type(vacant_entry(std::move(key), stack, state_.length, pages(), true));
  }

  /// The entry with the smallest key, if any.
  std::optional<occupied_entry> first_entry() noexcept {
    detail::MutStack<K, V, Strategy> stack(state_.tree);
    if (!state_.tree.has_root() || !stack.descend_to_first_key()) return std::nullopt;
    return occupied_entry(stack, state_.length, pages());
  }

  /// The entry with the largest key, if any.
  std::optional<occupied_entry> last_entry() noexcept {
    detail::MutStack<K, V, Strategy> stack(state_.tree);
    if (!state_.tree.has_root() || !stack.descend_to_last_key()) return std::nullopt;
    return occupied_entry(stack, state_.length, pages());
  }

  /// Puts `key` in place of an equal key and returns that one, keeping its
  /// value; or, if there is none, inserts `key` with `value` and returns
  /// `nullopt`: a set's `replace`, on one search.
  std::optional<K> replace_key(K key, V value) noexcept {
    detail::MutStack<K, V, Strategy> stack(state_.tree);
    const bool has_tree = state_.tree.has_root();
    if (has_tree && stack.search(detail::SearchKey<K, K>(key))) return stack.replace_key(std::move(key));
    vacant_entry(std::move(key), stack, state_.length, pages(), has_tree).insert_entry(std::move(value));
    return std::nullopt;
  }

  // Iteration by value (`unsync/map/into_iter.rs`).

  /// Every entry, moved out in key order, from either end; the map is left
  /// empty.
  IntoIter<K, V, Strategy, A> into_iter() && noexcept {
    return IntoIter<K, V, Strategy, A>(state_.tree.take(), std::exchange(state_.length, 0), state_.allocator());
  }

  /// Every key, moved out in key order; the map is left empty.
  IntoIter<K, V, Strategy, A, IntoPart::Keys> into_keys() && noexcept {
    return IntoIter<K, V, Strategy, A, IntoPart::Keys>(state_.tree.take(), std::exchange(state_.length, 0),
                                                       state_.allocator());
  }

  /// Every value, moved out in key order; the map is left empty.
  IntoIter<K, V, Strategy, A, IntoPart::Values> into_values() && noexcept {
    return IntoIter<K, V, Strategy, A, IntoPart::Values>(state_.tree.take(), std::exchange(state_.length, 0),
                                                         state_.allocator());
  }

  /// Removes every entry, and gives every page back.
  void clear() noexcept {
    detail::Teardown<K, V, Strategy>::destroy(state_.tree, pages());
    state_.length = 0;
  }

  /// Checks the tree's invariants, ending the program if one is broken: for
  /// tests.
  void check_invariants() const {
    detail::TreeChecker<K, V, Strategy>::check_invariants(state_.tree, state_.length);
  }

  /// Checks that `other` holds the same nodes, keys and prefixes, ending the
  /// program if not: for tests of batches, which make the same tree as
  /// single operations.
  void check_same_nodes(const BBTreeMap& other) const {
    detail::TreeChecker<K, V, Strategy>::check_same_nodes(state_.tree, other.state_.tree);
  }

 private:
  /// A set is a map of `Unit` values: it walks and batches over its tree.
  template <class, class, class>
  friend class BBTreeSet;

  /// The allocator, the tree and its count; an empty allocator takes no
  /// room, so that the map is 24 bytes, as the Rust map (D52).
  struct State : A {
    TreeType tree;
    size_type length = 0;

    State() = default;
    explicit State(A&& alloc) noexcept : A(std::move(alloc)) {}
    A& allocator() noexcept { return *this; }
    const A& allocator() const noexcept { return *this; }
  };

  mutable State state_;

  Pages pages() const noexcept { return Pages(state_.allocator()); }
};

}  // namespace bbtree::unsync
