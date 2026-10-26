// `shoal::TreeMap`: the ordered map that threads share (`map/mod.rs`,
// D54, design/09-concurrency.md): the unsync tree, shared through optimistic
// lock coupling.
//
// Closures, not handles: an operation that exposes a key or value runs the
// caller's closure while the entry's leaf is latched, lets go, and returns
// what the closure returned; or hands out a copy. No reference or latch
// outlives the call, so a thread never deadlocks with itself. The closures
// must not call into the same map: a leaf latched by the call stays latched.
//
// One entry at a time: no operation on two entries is atomic, and there is no
// `size()`: a shared counter would cost every insert a write to one line all
// threads share. Count entries from what `insert` returns.
#pragma once

#include <cstddef>
#include <optional>
#include <type_traits>
#include <utility>

#include "allocator.hpp"
#include "bound.hpp"
#include "detail/latched/batch.hpp"
#include "detail/latched/latched_tree.hpp"
#include "insert_error.hpp"
#include "leaf_annex.hpp"
#include "node_config.hpp"
#include "relocatable.hpp"
#include "unsync/range.hpp"

namespace shoal {

/// An ordered map of keys `K` to values `V` that threads share. Keys need
/// inline keys (`InlineKey<K>`, D49): integers, byte strings, or a key type of
/// your own that specializes it. `Annex` is what each leaf keeps for the code
/// that owns where keys and values point (`leaf_annex.hpp`); none by default.
template <class K, class V, class Config = DefaultNodeConfig, class A = Global, class Annex = NoLeafAnnex>
class TreeMap {
  static_assert(is_trivially_relocatable_v<K> && is_trivially_relocatable_v<V>,
                "shoal moves keys and values as bytes: specialize shoal::is_trivially_relocatable");
  static_assert(is_trivially_relocatable_v<Annex> && std::is_nothrow_default_constructible_v<Annex>,
                "a leaf annex moves with its leaf's bytes, and a new leaf makes one");
  static_assert(std::is_nothrow_move_constructible_v<K> && std::is_nothrow_move_constructible_v<V>,
                "keys and values move without throwing");

  using Strategy = detail::Strategy<Config, Annex, true>;
  using TreeType = detail::LatchedTree<K, V, Strategy>;
  template <class Q>
  using Query = typename detail::Query<K>::template type<Q>;

 public:
  using key_type = K;
  using mapped_type = V;
  using allocator_type = A;

  TreeMap() noexcept(std::is_nothrow_default_constructible_v<A>) = default;
  explicit TreeMap(A alloc) noexcept : alloc_(std::move(alloc)) {}
  TreeMap(const TreeMap&) = delete;
  TreeMap& operator=(const TreeMap&) = delete;
  ~TreeMap() { tree_.destroy(alloc_); }

  // Lookups.

  /// Runs `f(key, value)` on the entry whose key equals `key` while its leaf
  /// is latched shared, and returns what `f` returned; `nullopt` without one.
  template <class Q, class F>
  SHOAL_ALWAYS_INLINE auto read(const Q& key, F&& f) const noexcept {
    return tree_.read(detail::Query<K>::of(key), std::forward<F>(f));
  }

  /// A copy of the value, read without a latch where the leaf settles the
  /// key (D57): for trivially copyable values.
  template <class Q>
  SHOAL_ALWAYS_INLINE std::optional<V> get_copied(const Q& key) const noexcept {
    return tree_.get_copied(detail::Query<K>::of(key));
  }

  /// A copy of the value, made while the leaf is latched shared.
  template <class Q>
  std::optional<V> get_cloned(const Q& key) const {
    return tree_.read(detail::Query<K>::of(key), [](const K&, const V& value) { return V(value); });
  }

  template <class Q>
  SHOAL_ALWAYS_INLINE bool contains_key(const Q& key) const noexcept {
    return tree_.contains_key(detail::Query<K>::of(key));
  }

  /// Runs `f(key, value)` on the entry whose key equals `key`, its value to
  /// change, while its leaf is latched exclusively.
  template <class Q, class F>
  auto update(const Q& key, F&& f) const noexcept {
    return tree_.update(detail::Query<K>::of(key), std::forward<F>(f));
  }

  // Inserts.

  /// Inserts `key` and `value`; if an equal key is there, keeps it, replaces
  /// the value and returns the old one. If the allocator has no page to give,
  /// the map is unchanged and the entry comes back.
  SHOAL_ALWAYS_INLINE InsertOutcome<std::optional<V>, K, V> insert(K key, V value) const noexcept {
    return update_or_insert(std::move(key), std::move(value), [](const K&, V& stored, V&& value) {
      V old = std::move(stored);
      stored = std::move(value);
      return old;
    });
  }

  /// Runs `f(key, stored, value)` on the entry and `value`, which it takes,
  /// while its leaf is latched exclusively, and returns what `f` returned; or
  /// inserts `key` and `value` if there is none, and returns `nullopt`.
  template <class F>
  SHOAL_ALWAYS_INLINE auto update_or_insert(K key, V value, F f) const noexcept {
    return tree_.insert(std::move(key), std::move(value), detail::UpdateOrInsert<K, V, F>{std::move(f)},
                        alloc_);
  }

  /// Runs `f(key, value)` on the entry, inserting `key` and `value` first if
  /// there is none, while its leaf is latched exclusively.
  template <class F>
  SHOAL_ALWAYS_INLINE auto get_or_insert(K key, V value, F f) const noexcept {
    return tree_.insert(std::move(key), std::move(value), detail::ReadOrInsert<K, V, F>{std::move(f)},
                        alloc_);
  }

  // The leaf and its annex (`leaf_annex.hpp`, `map/with_leaf.rs`).

  /// The leaf an insert or a removal holds, as its closure sees it.
  using annexed_leaf = AnnexedLeaf<K, V, Strategy>;

  /// Inserts `key` and `value`, handing the closure the leaf held
  /// exclusively: `occupied(leaf, stored, stored_value)` if an equal key is
  /// there, which keeps it; `vacant(leaf, key, value)`, before the entry goes
  /// in, if not. Each returns `LeafOutcome<R>`: `R`, which the insert
  /// returns, or a refusal, which leaves the map as it was and gives the
  /// entry back in the `InsertError`. Either may run again if the insert
  /// starts over for another thread, but only one's result stands.
  template <class Occupied, class Vacant>
  auto update_or_insert_with_leaf(K key, V value, Occupied occupied, Vacant vacant) const noexcept {
    return tree_.insert(std::move(key), std::move(value),
                        detail::WithLeaf<K, V, Strategy, Occupied, Vacant>{std::move(occupied), std::move(vacant)},
                        alloc_);
  }

  /// Removes the entry whose key equals `key`, and returns it, its key as
  /// stored; `f(leaf, key, value)` runs once, with the leaf held
  /// exclusively, right before the entry comes out.
  template <class Q, class F>
  std::optional<std::pair<K, V>> remove_with_leaf(const Q& key, F f) const noexcept {
    return tree_.remove_entry(detail::Query<K>::of(key), [](const K&, const V&) { return true; }, std::move(f),
                              alloc_);
  }

  // Removals.

  template <class Q>
  std::optional<V> remove(const Q& key) const noexcept {
    if (auto entry = remove_entry(key)) return std::move(entry->second);
    return std::nullopt;
  }

  /// Removes the entry whose key equals `key` and returns it, its key as
  /// stored.
  template <class Q>
  std::optional<std::pair<K, V>> remove_entry(const Q& key) const noexcept {
    return tree_.remove_entry(detail::Query<K>::of(key), [](const K&, const V&) { return true; },
                              detail::NoLeafCallback(), alloc_);
  }

  /// Removes the entry if `condition(key, value)` holds of it, latched
  /// exclusively: only the entry the caller saw.
  template <class Q, class Condition>
  std::optional<std::pair<K, V>> remove_if(const Q& key, Condition condition) const noexcept {
    return tree_.remove_entry(detail::Query<K>::of(key), condition, detail::NoLeafCallback(), alloc_);
  }

  // Batches (D55): each key in a lane, the lanes' waits for memory
  // overlapped, `batch_lanes` keys at a time. No lane holds a latch while
  // another runs.

  /// A copy of each key's value, `values[i]` for `keys[i]`, as separate
  /// `get_copied`s give them.
  template <class Q>
  void get_many_copied(const Q* keys, std::size_t count, std::optional<V>* values) const noexcept {
    detail::LatchedBatch<K, V, Strategy>::get_many_copied(tree_, keys, count, values);
  }

  /// Runs `f(i, key, value)` on the entry of each `keys[i]` the map holds,
  /// latched shared, in the order the lanes reach them.
  template <class Q, class F>
  void read_many(const Q* keys, std::size_t count, F f) const noexcept {
    detail::LatchedBatch<K, V, Strategy>::read_many(tree_, keys, count, f);
  }

  /// Inserts `keys[i]` and `values[i]` in order, as separate `insert`s do;
  /// `previous[i]` gets the value each replaced. If the allocator had no page
  /// for an entry, `failed[i]` is set and the entry is back in `keys[i]` and
  /// `values[i]`. The other keys and values are moved from.
  void insert_many(K* keys, V* values, std::size_t count, std::optional<V>* previous,
                   bool* failed) const noexcept {
    detail::LatchedBatch<K, V, Strategy>::insert_many(tree_, keys, values, count, previous, failed, alloc_);
  }

  // Scans.

  /// Visits the entries within `lower` and `upper` in key order, each while
  /// its leaf is latched shared, until `visit(key, value)` returns false.
  /// Returns whether it did. Not a snapshot: an entry present for the whole
  /// scan is visited exactly once.
  template <class L, class U, class Visit>
  bool scan(const Bound<L>& lower, const Bound<U>& upper, Visit visit) const noexcept {
    return tree_.template scan<detail::Ascending>(lower, upper, visit);
  }

  /// `scan` in reverse key order.
  template <class L, class U, class Visit>
  bool scan_rev(const Bound<L>& lower, const Bound<U>& upper, Visit visit) const noexcept {
    return tree_.template scan<detail::Descending>(lower, upper, visit);
  }

  /// Whether the map has no entry.
  bool empty() const noexcept {
    return !scan(Bound<K>::unbounded(), Bound<K>::unbounded(), [](const K&, const V&) { return false; });
  }

  // The owner's, with the map alone: no latch.

  /// The value of the entry whose key equals `key`, to change.
  template <class Q>
  V* get_mut(const Q& key) noexcept {
    const auto found = tree_.owned_tree().find(detail::Query<K>::of(key));
    return found ? found->second : nullptr;
  }

  using mut_range = unsync::Range<K, V, Strategy, false, false>;
  using const_range = unsync::Range<K, V, Strategy, true, false>;

  /// Every entry in key order, values to change.
  mut_range iter_mut() noexcept { return mut_range(tree_.owned_tree()); }

  /// Removes every entry, and gives every page back.
  void clear() noexcept {
    tree_.destroy(alloc_);
    tree_.~TreeType();
    ::new (static_cast<void*>(&tree_)) TreeType();
  }

  /// Checks the tree's invariants for a map of `length` entries: for tests,
  /// with no other thread on the map.
  void check_invariants(std::size_t length) const { tree_.check_invariants(length); }

 private:
  TreeType tree_;
  mutable A alloc_;
};

}  // namespace shoal
