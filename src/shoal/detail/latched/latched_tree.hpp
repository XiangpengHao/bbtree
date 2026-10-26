// A tree that threads share (OG-design/09-concurrency.md, OG-D54,
// `tree/latched/*.rs`): the unsync tree, with a latch word in each node,
// behind an atomic pointer to the root page. It is the unsync tree in every
// way but how threads share it: same nodes, shared prefix, prefix fits,
// splits, merges and appends.
//
// - **Descent** is optimistic through the internal nodes, each read in place
//   and checked by its version; the leaf is latched, shared or exclusive, or
//   read at a version (OG-D57).
// - **Operations** run the caller's closure on an entry while its leaf is
//   latched, and let go before returning.
// - **Changes to more than a leaf** (a split, a rebalancing removal, a
//   shrinking of the shared prefix) take, without waiting, every node the
//   unsync tree's operation will change or read a separator of, start over if
//   another thread holds one, then run that very operation along a node stack
//   from the topmost node held (`change_held`).
// - **Pages are type-stable** (`pages.hpp`).
// - **The last leaf** is where an append goes, by a pointer only a thread
//   holding that leaf changes.
#pragma once

#include <atomic>
#include <cstddef>
#include <cstdint>
#include <optional>
#include <type_traits>
#include <utility>

#include "../../insert_error.hpp"
#include "../../leaf_annex.hpp"
#include "../config.hpp"
#include "../tree/invariants.hpp"
#include "../tree/remove.hpp"
#include "../tree/teardown.hpp"
#include "../tree/tree.hpp"
#include "held.hpp"
#include "pages.hpp"
#include "racing.hpp"

namespace shoal::detail {

/// A descent that keeps no path.
struct NoPath {
  void clear() noexcept {}
  template <class... Args>
  void record(Args&&...) noexcept {}
};

/// A value on cache lines of its own: writing it takes no line from threads
/// that read what sits beside it. Two lines, which CPUs fetch in pairs.
template <class T>
struct alignas(128) CachePadded {
  T value;
};

// An insert's policy says what it does where the tree holds an equal key,
// `occupied(leaf, key, value, stored, stored_value)`, and where it holds none,
// `vacant(leaf, key, value)`, with the leaf held exclusively; each returns a
// `LeafOutcome`: the insert's output, or a refusal that leaves the map as it
// was. A refused insert keeps `key` and `value`; `vacant` runs before the
// entry goes in.

/// Replaces the value through a closure that takes the new one, and returns
/// the closure's result, `nullopt` for an entry inserted (Rust's
/// `UpdateOrInsert`).
template <class K, class V, class F>
struct UpdateOrInsert {
  using R = typename Returned<F, const K&, V&, V&&>::type;
  using Output = std::optional<R>;
  F f;

  template <class Leaf>
  LeafOutcome<Output> occupied(Leaf, K&, V& value, const K& stored, V& stored_value) {
    return LeafOutcome<Output>::done(Output(Returned<F, const K&, V&, V&&>::call(f, stored, stored_value, std::move(value))));
  }
  template <class Leaf>
  LeafOutcome<Output> vacant(Leaf, const K&, const V&) {
    return LeafOutcome<Output>::done(std::nullopt);
  }
};

/// Runs a closure on the key's entry: the stored one, left as it is, or the
/// one inserted (Rust's `ReadOrInsert`).
template <class K, class V, class F>
struct ReadOrInsert {
  using Output = typename Returned<F, const K&, const V&>::type;
  F f;

  template <class Leaf>
  LeafOutcome<Output> occupied(Leaf, K&, V&, const K& stored, V& stored_value) {
    return LeafOutcome<Output>::done(Returned<F, const K&, const V&>::call(f, stored, stored_value));
  }
  template <class Leaf>
  LeafOutcome<Output> vacant(Leaf, const K& key, const V& value) {
    return LeafOutcome<Output>::done(Returned<F, const K&, const V&>::call(f, key, value));
  }
};

/// The result type `R` of a closure that returns `LeafOutcome<R>`.
template <class Outcome>
struct LeafOutcomeOf;
template <class R>
struct LeafOutcomeOf<LeafOutcome<R>> {
  using type = R;
};

/// Hands the held leaf to the caller's closures, which may refuse the insert
/// (Rust's `WithLeaf`): `occupied(leaf, stored, stored_value)` and
/// `vacant(leaf, key, value)`, each returning `LeafOutcome<R>`.
template <class K, class V, class S, class Occupied, class Vacant>
struct WithLeaf {
  using Output = typename LeafOutcomeOf<std::invoke_result_t<Occupied&, AnnexedLeaf<K, V, S>, const K&, V&>>::type;
  Occupied on_occupied;
  Vacant on_vacant;

  LeafOutcome<Output> occupied(AnnexedLeaf<K, V, S> leaf, K&, V&, const K& stored, V& stored_value) {
    return on_occupied(leaf, stored, stored_value);
  }
  LeafOutcome<Output> vacant(AnnexedLeaf<K, V, S> leaf, const K& key, const V& value) {
    return on_vacant(leaf, key, value);
  }
};

/// A removal's leaf callback that does nothing.
struct NoLeafCallback {
  template <class Leaf, class K, class V>
  void operator()(Leaf, const K&, const V&) const noexcept {}
};

/// The order a scan visits entries in.
struct Ascending {
  static constexpr bool descending = false;
};
struct Descending {
  static constexpr bool descending = true;
};

template <class K, class V, class S>
class LatchedBatch;

template <class K, class V, class S>
class LatchedTree {
  /// The batches run the descent's steps themselves (`batch.hpp`).
  friend class LatchedBatch<K, V, S>;

 public:
  using Shape = NodeShape<K, V, S>;
  using Ptr = NodePtr<K, V, S>;
  using LeafNode = Leaf<K, V, S>;
  using InternalNode = Internal<K, V, S>;
  using AnyNode = Node<K, V, S>;
  using Root = RootPage<K, V, S>;
  using TreeType = Tree<K, V, S>;
  using Race = Racing<K, V, S>;
  using Path = TreePath<K, V, S>;
  template <class Mode>
  using Held = HeldLeaf<K, V, S, Mode>;

  static_assert(S::latched, "a latched tree's strategy keeps latch words");
  static_assert(Shape::has_inline_keys, "a TreeMap's keys need inline keys");

  LatchedTree() noexcept = default;
  LatchedTree(const LatchedTree&) = delete;
  LatchedTree& operator=(const LatchedTree&) = delete;

  bool has_root() const noexcept { return root_.load(std::memory_order_acquire) != 0; }

  // Lookups (`lookup.rs`).

  /// Runs `f(key, value)` on the entry whose key equals `key` while its leaf
  /// is latched shared; `nullopt` without one, which the leaf's inline keys
  /// may tell unlatched.
  template <class Q, class F>
  SHOAL_ALWAYS_INLINE std::optional<typename Returned<F, const K&, const V&>::type> read(
      const Q& key, F&& f) const noexcept {
    using Out = typename Returned<F, const K&, const V&>::type;
    const SearchKey<K, Q> search(key);
    LeafFind<false> found = find_optimistic<false>(search);
    switch (found.kind) {
      case FindKind::Absent:
        return std::nullopt;
      case FindKind::Held: {
        const auto entry = entry_at(found.held.leaf(), found.place);
        Out out = Returned<F, const K&, const V&>::call(f, *entry.first, *entry.second);
        return out;
      }
      default:
        return read_latched(search, f);
    }
  }

  /// A copy of the value of `key`'s entry: unlatched where the leaf settles
  /// it, as a sequence lock reads (OG-D57).
  template <class Q>
  SHOAL_ALWAYS_INLINE std::optional<V> get_copied(const Q& key) const noexcept {
    static_assert(std::is_trivially_copyable_v<V>, "get_copied copies values read unlatched");
    const SearchKey<K, Q> search(key);
    LeafFind<true> found = find_optimistic<true>(search);
    switch (found.kind) {
      case FindKind::Absent:
        return std::nullopt;
      case FindKind::Copied:
        return found.copy.assume_read();
      case FindKind::Held:
        return *entry_at(found.held.leaf(), found.place).second;
      default:
        return read_latched(search, [](const K&, const V& value) { return value; });
    }
  }

  template <class Q>
  SHOAL_ALWAYS_INLINE bool contains_key(const Q& key) const noexcept {
    const SearchKey<K, Q> search(key);
    LeafFind<true> found = find_optimistic<true>(search);
    switch (found.kind) {
      case FindKind::Absent: return false;
      case FindKind::Copied:
      case FindKind::Held: return true;
      default: return read_latched(search, [](const K&, const V&) {}).has_value();
    }
  }

  /// Runs `f(key, value)` on the entry whose key equals `key`, its value to
  /// change, while its leaf is latched exclusively.
  template <class Q, class F>
  std::optional<typename Returned<F, const K&, V&>::type> update(const Q& key, F&& f) const noexcept {
    const SearchKey<K, Q> search(key);
    NoPath path;
    Descent<ExclusiveMode> descent = descend<ExclusiveMode>(search, path);
    if (descent.outside) return std::nullopt;
    const LeafNode leaf = descent.held.leaf();
    if (descent.at_separator) {
      return Returned<F, const K&, V&>::call(f, leaf.head_key(), leaf.head_value());
    }
    const std::optional<std::size_t> index = leaf.template find<void>(search);
    if (!index) return std::nullopt;
    return Returned<F, const K&, V&>::call(f, leaf.key(*index), leaf.value(*index));
  }

  // Inserts (`insert.rs`, `append.rs`).

  /// Inserts `key` and `value`, or, if an equal key is there, keeps it and
  /// does what `policy` does; returns what `policy` returns. If the
  /// allocator has no page to give, the tree is unchanged, and the entry
  /// comes back. Inlined into its caller, as Rust's is (D17).
  template <class Policy, class A>
  SHOAL_ALWAYS_INLINE InsertOutcome<typename Policy::Output, K, V> insert(K&& key, V&& value, Policy policy,
                                                                           A& alloc) const noexcept {
    using Outcome = InsertOutcome<typename Policy::Output, K, V>;
    if (!has_root()) {
      Layout refused{0, 0};
      if (!start_tree(alloc, refused)) {
        return Outcome::failed(InsertError<K, V>(std::move(key), std::move(value), refused));
      }
    }
    std::optional<Layout> refused;
    LeafOutcome<typename Policy::Output> appended = try_append(key, value, policy, refused);
    if (appended.ok()) return Outcome::done(std::move(appended.value()));
    if (refused) return Outcome::failed(InsertError<K, V>(std::move(key), std::move(value), *refused));
    for (;;) {
      NoPath path;
      Descent<ExclusiveMode> descent = descend<ExclusiveMode>(SearchKey<K, K>(key), path);
      if (descent.outside) {
        key = shrink_shared_prefix_for(std::move(key), alloc);
        continue;
      }
      std::size_t edge = 0;
      LeafOutcome<typename Policy::Output> outcome = insert_into(descent.held, descent.at_separator, key, value,
                                                                  policy, MapPages<A>(alloc), edge, refused);
      if (outcome.ok()) return Outcome::done(std::move(outcome.value()));
      if (refused) return Outcome::failed(InsertError<K, V>(std::move(key), std::move(value), *refused));
      descent.held.release();
      return insert_splitting(std::move(key), std::move(value), policy, alloc);
    }
  }

  // Removals (`remove.rs`).

  /// Removes the entry whose key equals `key` if `condition(key, value)`
  /// holds of it, and returns it with the key as stored. `condition` runs
  /// once per try, while the entry's leaf is latched exclusively;
  /// `on_remove(leaf, key, value)` once, as the removal commits, before the
  /// entry comes out.
  template <class Q, class Condition, class OnRemove, class A>
  std::optional<std::pair<K, V>> remove_entry(const Q& key, Condition condition, OnRemove on_remove,
                                              A& alloc) const noexcept {
    const SearchKey<K, Q> search(key);
    {
      NoPath path;
      Descent<ExclusiveMode> descent = descend<ExclusiveMode>(search, path);
      if (descent.outside) return std::nullopt;
      const LeafNode leaf = descent.held.leaf();
      if (descent.at_separator) {
        if (!condition(leaf.head_key(), leaf.head_value())) return std::nullopt;
      } else {
        const std::optional<std::size_t> index = leaf.template find<void>(search);
        if (!index) return std::nullopt;
        if (!condition(leaf.key(*index), leaf.value(*index))) return std::nullopt;
        if (leaf.keeps_its_minimum() || is_root(leaf)) {
          on_remove(AnnexedLeaf<K, V, S>(leaf), leaf.key(*index), leaf.value(*index));
          return leaf.remove(*index);
        }
      }
    }
    return remove_rebalancing(search, condition, on_remove, alloc);
  }

  // Scans (`scan.rs`).

  /// Runs `visit(key, value)` on every entry within `lower` and `upper`, in
  /// key order, or reversed with `Order` `Descending`, each while its leaf is
  /// latched shared, until `visit` returns false. Returns whether it did. Not
  /// a snapshot: an entry present for the whole scan is visited once.
  template <class Order, class L, class U, class Visit>
  bool scan(const ::shoal::Bound<L>& lower, const ::shoal::Bound<U>& upper, Visit&& visit) const noexcept {
    if (!has_root()) return false;
    if constexpr (Order::descending) {
      return scan_in<Order>(upper, lower, visit);
    } else {
      return scan_in<Order>(lower, upper, visit);
    }
  }

  // Owner's access, once no other thread reaches the tree.

  /// The tree, as the unsync tree's operations take it, for an owner that
  /// has it alone (`owner.rs`).
  TreeType owned_tree() const noexcept {
    const Ptr root = root_ptr();
    return root.is_null() ? TreeType() : TreeType::of_root_page(Root::of_ptr(root));
  }

  /// Takes the tree apart, the pages on its free lists too.
  template <class A>
  void destroy(A& alloc) noexcept {
    const Ptr root = root_ptr();
    if (!root.is_null()) {
      root_.store(0, std::memory_order_relaxed);
      rightmost_leaf_.value.store(0, std::memory_order_relaxed);
      TreeType tree = TreeType::of_root_page(Root::of_ptr(root));
      Teardown<K, V, S>::destroy(tree, MapPages<A>(alloc));
    }
    free_.value.deallocate_all(alloc, Shape::leaf_layout.page, Shape::internal_layout.page);
  }

  /// Checks the tree's invariants, and that appends go to its last leaf: for
  /// tests, once no other thread changes it.
  void check_invariants(std::size_t length) const {
    const Ptr root = root_ptr();
    if (root.is_null()) {
      SHOAL_CHECK(length == 0);
      return;
    }
    const TreeType tree = TreeType::of_root_page(Root::of_ptr(root));
    TreeChecker<K, V, S>::check_invariants(tree, length);
    const LeafNode last = NodeStack<K, V, S>::template border_leaf<LastEdge>(tree.root());
    SHOAL_CHECK(rightmost_leaf_.value.load(std::memory_order_relaxed) == last.node_ptr().tagged());
  }

 private:
  /// Read by every operation, written when the root changes kind.
  mutable std::atomic<std::uintptr_t> root_{0};
  /// The tree's last leaf, tagged, where an append goes.
  mutable CachePadded<std::atomic<std::uintptr_t>> rightmost_leaf_{{0}};
  /// Pages of nodes a merge emptied.
  mutable CachePadded<FreeLists> free_;

  // The root and the pages.

  Ptr root_ptr() const noexcept { return Ptr::from_tagged(root_.load(std::memory_order_acquire)); }
  Root root_page() const noexcept { return Root::of_ptr(root_ptr()); }

  /// Whether `leaf` is the root, which has no minimum (OG-D22).
  bool is_root(LeafNode leaf) const noexcept {
    return root_.load(std::memory_order_acquire) == leaf.node_ptr().tagged();
  }

  template <class A>
  NodePages<A> pages(A& alloc) const noexcept {
    return NodePages<A>(free_.value, alloc, Shape::leaf_layout.page, Shape::internal_layout.page);
  }

  /// Makes the tree, unless another thread did: a zeroed root page with an
  /// empty leaf, which is the last leaf too. False, with the layout asked
  /// for, if the allocator has no page to give.
  template <class A>
  SHOAL_COLD bool start_tree(A& alloc, Layout& refused) const noexcept {
    const MapPages<A> map_pages(alloc);
    const Layout layout = Root::Page::layout();
    std::byte* raw = map_pages.try_allocate_page(PageKind::Root, layout);
    if (raw == nullptr) {
      refused = layout;
      return false;
    }
    const Root root_page = Root::make(typename Root::Page(raw));
    const LeafNode root = root_page.node().leaf();
    // Held until the leaf is the last leaf too: a split of it reads that.
    const Version version = root.node_ptr().latch().lock_exclusive();
    std::uintptr_t expected = 0;
    const std::uintptr_t tagged = root.node_ptr().tagged();
    if (root_.compare_exchange_strong(expected, tagged, std::memory_order_acq_rel,
                                      std::memory_order_acquire)) {
      rightmost_leaf_.value.store(tagged, std::memory_order_release);
      root.node_ptr().latch().unlock_exclusive(version);
    } else {
      map_pages.deallocate_page(PageKind::Root, raw, layout);
    }
    return true;
  }

  /// Runs `operation`, one of the unsync tree's own, on the tree under the
  /// root page; the caller holds every node it changes or reads a separator
  /// of. If it changed the root's kind, retags the root pointer while the
  /// caller still holds the root.
  template <class Operation>
  decltype(auto) change_held(Operation&& operation) const noexcept {
    const Root before = root_page();
    TreeType tree = TreeType::of_root_page(before);
    auto retag = [&] {
      const std::uintptr_t tagged = tree.root_page().node_ptr().tagged();
      if (tagged != before.node_ptr().tagged()) root_.store(tagged, std::memory_order_release);
    };
    if constexpr (std::is_void_v<decltype(operation(tree))>) {
      operation(tree);
      retag();
    } else {
      auto result = operation(tree);
      retag();
      return result;
    }
  }

  // The descent (`descent.rs`).

  /// Where a descent ends: the leaf where its key belongs, latched in mode
  /// `Mode`, and whether the key is its head; for a key that lacks what every
  /// key shares, where it sorts against them all instead.
  template <class Mode>
  struct Descent {
    Held<Mode> held;
    bool at_separator = false;
    std::optional<Ordering> outside;
  };

  /// The leaf where `key` belongs, latched in mode `Mode`, `path` keeping the
  /// internal nodes the descent went through.
  template <class Mode, class P, class Q>
  SHOAL_ALWAYS_INLINE Descent<Mode> descend(const SearchKey<K, Q>& key, P& path) const noexcept {
    Descent<Mode> descent;
    while (!try_descend<Mode>(key, path, descent)) {
    }
    return descent;
  }

  /// One try of `descend`, into `descent`; false to start over. Filling the
  /// caller's descent rather than returning an optional one spares a move
  /// and a destructor of the latch it holds on every descent (D18).
  template <class Mode, class P, class Q>
  SHOAL_ALWAYS_INLINE bool try_descend(const SearchKey<K, Q>& key, P& path,
                                        Descent<Mode>& descent) const noexcept {
    path.clear();
    const Root root_page = this->root_page();
    // No root yet: the key is in no node. Every caller reads `outside`
    // as absent here (a scan checks for a root before it descends), so
    // lookups load the root pointer once, in this descent, not twice (D21).
    if (root_page.is_null()) {
      descent.outside = Ordering::Less;
      return true;
    }
    const AnyNode root = root_page.node();
    if (root.is_leaf()) {
      Held<Mode> held = Held<Mode>::lock(root.leaf());
      // A split that raised the tree changed the tag first.
      if (!is_root(root.leaf())) return false;
      std::optional<Ordering> outside;
      if constexpr (Mode::latches) {
        outside = root_page.shared_prefix().place_outside(key);
      } else {
        outside = root_page.place_outside_racing(key);
        if (!held.is_unchanged()) return false;
      }
      if (outside) {
        descent.outside = outside;
        return true;
      }
      descent.held = std::move(held);
      return true;
    }
    InternalNode node = root.internal();
    Version version(0);
    if (!root_version(node, version)) return false;
    if (const std::optional<Ordering> outside = root_page.place_outside_racing(key)) {
      if (!node.node_ptr().latch().is_unchanged(version)) return false;
      descent.outside = outside;
      return true;
    }
    std::uint8_t at_separator = 0;
    bool at_root = true;
    for (;;) {
      const std::optional<Routed> routed = Race::route(node, key, version);
      if (!routed) return false;
      const Ptr child = Race::child(node, routed->edge);
      if (child.is_null()) return false;
      // An edge read from the root page is followed only once the root's
      // version vouches for it: the page may hold a leaf now.
      if (at_root && !node.node_ptr().latch().is_unchanged(version)) return false;
      at_root = false;
      at_separator |= routed->at_separator;
      path.record(node, version, *routed);
      if (child.is_leaf()) {
        const LeafNode leaf = LeafNode::at(child);
        Held<Mode> held = Held<Mode>::lock(leaf);
        if (!node.node_ptr().latch().is_unchanged(version)) return false;
        // The separator matched above may have been removed since, its
        // successor the head now; an unlatched caller checks itself.
        if (Mode::latches && at_separator && !is_head(leaf, key)) return false;
        descent.held = std::move(held);
        descent.at_separator = at_separator != 0;
        return true;
      }
      const InternalNode next = InternalNode::at(child);
      const Version next_version = next.node_ptr().latch().read_version();
      if (!node.node_ptr().latch().is_unchanged(version)) return false;
      node = next;
      version = next_version;
    }
  }

  /// `root`'s version, if the root pointer still tags `root` internal once
  /// the version is read: a lowering to a leaf may have finished between.
  bool root_version(InternalNode root, Version& version) const noexcept {
    version = root.node_ptr().latch().read_version();
    return root_.load(std::memory_order_acquire) == root.node_ptr().tagged();
  }

  /// Whether `leaf`, latched, has `key` as its head's key.
  template <class Q>
  SHOAL_COLD static bool is_head(LeafNode leaf, const SearchKey<K, Q>& key) noexcept {
    return leaf.has_head() && key.cmp_full(leaf.head_key()) == Ordering::Equal;
  }

  // Lookups, internals.

  enum class FindKind : std::uint8_t { Absent, Copied, Held, Changed };

  /// Where an entry is in its leaf: its head (`SIZE_MAX`), or key `index`.
  static constexpr std::size_t head_place = SIZE_MAX;

  template <bool Copies>
  struct LeafFind {
    FindKind kind = FindKind::Absent;
    Held<SharedMode> held;
    std::size_t place = 0;
    RacingCopy<V> copy;
  };

  static std::pair<const K*, const V*> entry_at(LeafNode leaf, std::size_t place) noexcept {
    if (place == head_place) return {&leaf.head_key(), &leaf.head_value()};
    return {&leaf.key(place), &leaf.value(place)};
  }

  /// Where `key`'s entry is, by an optimistic descent and a read of its leaf
  /// in place: settled unlatched where the inline keys rule the key out, or,
  /// if `Copies`, where the leaf settles a candidate, whose value it copies.
  /// Otherwise found in the leaf, latched shared if still at the version the
  /// descent read.
  template <bool Copies, class Q>
  SHOAL_ALWAYS_INLINE LeafFind<Copies> find_optimistic(const SearchKey<K, Q>& key) const noexcept {
    NoPath path;
    Descent<OptimisticMode> at = descend<OptimisticMode>(key, path);
    LeafFind<Copies> found;
    if (at.outside) return found;
    const LeafNode leaf = at.held.leaf();
    std::optional<std::pair<std::size_t, std::size_t>> candidate;
    // What the leaf read unlatched settles, once its version vouches for it.
    enum class Racy : std::uint8_t { None, Absent, Value, Entry };
    Racy racy = Racy::None;
    RacingCopy<K> stored;
    if (at.at_separator) {
      if constexpr (Copies && KeyInliner<K>::orders_by_own_bytes) {
        if (Race::load_head(leaf, &stored, &found.copy)) racy = Racy::Entry;
      }
    } else {
      const auto [line, prefix_len] = Race::candidate_line(leaf, key);
      if (line.kind == CandidateLine::Kind::Absent) {
        racy = Racy::Absent;
      } else if (line.kind == CandidateLine::Kind::One) {
        candidate = std::pair<std::size_t, std::size_t>(line.index, prefix_len);
        if constexpr (Copies) {
          if constexpr (KeyInliner<K>::orders_by_own_bytes) {
            Race::load_key(leaf, line.index, stored);
            Race::load_value(leaf, line.index, found.copy);
            racy = Racy::Entry;
          } else if (const std::optional<bool> is = Race::candidate_is(leaf, key, line.index, prefix_len)) {
            if (*is) {
              Race::load_value(leaf, line.index, found.copy);
              racy = Racy::Value;
            } else {
              racy = Racy::Absent;
            }
          }
        }
      }
    }
    if (racy != Racy::None) {
      if (!at.held.is_unchanged()) {
        found.kind = FindKind::Changed;
        return found;
      }
      switch (racy) {
        case Racy::Absent:
          found.kind = FindKind::Absent;
          return found;
        case Racy::Value:
          found.kind = FindKind::Copied;
          return found;
        default:
          if (key.cmp_full(stored.assume_read()) == Ordering::Equal) {
            found.kind = FindKind::Copied;
          } else {
            found.kind = at.at_separator ? FindKind::Changed : FindKind::Absent;
          }
          return found;
      }
    }
    // A leaf that changed since the descent may have merged and been freed.
    found.held = Held<SharedMode>::lock(leaf);
    if (!leaf.node_ptr().latch().is_unchanged(at.held.version())) {
      found.held.release();
      found.kind = FindKind::Changed;
      return found;
    }
    if (at.at_separator) {
      if (is_head(leaf, key)) {
        found.kind = FindKind::Held;
        found.place = head_place;
      } else {
        found.held.release();
        found.kind = FindKind::Changed;
      }
      return found;
    }
    const std::optional<std::size_t> index =
        candidate ? leaf.find_candidate(key, candidate->first, candidate->second)
                  : leaf.template find<void>(key);
    if (index) {
      found.kind = FindKind::Held;
      found.place = *index;
    } else {
      found.held.release();
      found.kind = FindKind::Absent;
    }
    return found;
  }

  /// `read` the latched way: the leaf latched shared at the end of the
  /// descent, which waits for a writer that holds it.
  template <class Q, class F>
  SHOAL_NOINLINE std::optional<typename Returned<F, const K&, const V&>::type> read_latched(
      const SearchKey<K, Q>& key, F&& f) const noexcept {
    NoPath path;
    Descent<SharedMode> descent = descend<SharedMode>(key, path);
    if (descent.outside) return std::nullopt;
    const LeafNode leaf = descent.held.leaf();
    if (descent.at_separator) {
      return Returned<F, const K&, const V&>::call(f, leaf.head_key(), leaf.head_value());
    }
    const std::optional<std::size_t> index = leaf.template find<void>(key);
    if (!index) return std::nullopt;
    return Returned<F, const K&, const V&>::call(f, leaf.key(*index), leaf.value(*index));
  }

  // Inserts, internals.

  /// Inserts into `held`, the leaf where `key` belongs, if it holds `key` or
  /// has room, and returns what `policy` returned, as the policy's own
  /// `LeafOutcome` (no second optional around it: that layer cost
  /// instructions on every insert, where Rust's `Result` is one enum, D18).
  /// Keeping the entry instead: with `refused` set if the policy refused,
  /// or else a refusal of no layout, the leaf full and `edge` where `key`
  /// goes.
  template <class Policy, class Pages>
  SHOAL_ALWAYS_INLINE LeafOutcome<typename Policy::Output> insert_into(
      const Held<ExclusiveMode>& held, bool at_separator, K& key, V& value, Policy& policy,
      const Pages& pages, std::size_t& edge, std::optional<Layout>& refused) const noexcept {
    using Output = typename Policy::Output;
    const LeafNode leaf = held.leaf();
    const AnnexedLeaf<K, V, S> annexed(leaf);
    if (at_separator) {
      return settle(policy.occupied(annexed, key, value, leaf.head_key(), leaf.head_value()), refused);
    }
    const SearchResult found = leaf.search_always_inlined(SearchKey<K, K>(key));
    if (found.found) {
      return settle(policy.occupied(annexed, key, value, leaf.key(found.index), leaf.value(found.index)), refused);
    }
    if (leaf.len() < LeafNode::capacity()) {
      LeafOutcome<Output> outcome = settle(policy.vacant(annexed, key, value), refused);
      if (!outcome.ok()) return outcome;
      PrefixedKey<K> prefixed = this->prefixed(std::move(key), leaf, pages);
      leaf.insert_fit(found.index, std::move(prefixed).window_at(leaf.prefix_len()), std::move(value));
      return outcome;
    }
    edge = found.index;
    return LeafOutcome<Output>::refused(Layout{0, 0});
  }

  /// A policy's outcome, recording a refusal's layout in `refused`.
  template <class Output>
  SHOAL_ALWAYS_INLINE static LeafOutcome<Output> settle(LeafOutcome<Output>&& outcome,
                                                         std::optional<Layout>& refused) noexcept {
    if (SHOAL_UNLIKELY(!outcome.ok())) refused = outcome.layout();
    return std::move(outcome);
  }

  /// `key`, bound for `leaf`, which the caller holds exclusively. A root leaf
  /// lies on every border, so holding it holds the shared prefix: the unsync
  /// tree's shrink runs there, as on every insert.
  template <class Pages>
  PrefixedKey<K> prefixed(K&& key, LeafNode leaf, const Pages& pages) const noexcept {
    if (is_root(leaf)) {
      return change_held([&](TreeType& tree) { return tree.shrink_shared_prefix_for(std::move(key), pages); });
    }
    return PrefixedKey<K>::checked_by_descent(std::move(key));
  }

  /// Appends to the last leaf, if it has room, `key` goes after its last
  /// key, and `key` shares what every key shares; returns what `policy`
  /// returned. Otherwise a refusal of no layout, the entry kept (as
  /// `insert_into`'s full leaf; a policy refusal sets `refused`).
  template <class Policy>
  SHOAL_ALWAYS_INLINE LeafOutcome<typename Policy::Output> try_append(
      K& key, V& value, Policy& policy, std::optional<Layout>& refused) const noexcept {
    using Output = typename Policy::Output;
    // Not const. Returning this moves it, and a move-only value (a `unique_ptr`,
    // D7) makes `LeafOutcome` movable but not copyable. A const local would
    // be copied on return, so `insert` would not compile for that value.
    LeafOutcome<Output> not_appended = LeafOutcome<Output>::refused(Layout{0, 0});
    const Ptr last = Ptr::from_tagged(rightmost_leaf_.value.load(std::memory_order_acquire));
    if (last.is_null()) return not_appended;
    const LeafNode leaf = LeafNode::at(last);
    if (!Race::may_go_last(leaf, SearchKey<K, K>(key))) return not_appended;
    const Held<ExclusiveMode> held = Held<ExclusiveMode>::lock(leaf);
    if (!is_rightmost_leaf(leaf)) return not_appended;
    const std::size_t len = leaf.len();
    if (len >= LeafNode::capacity() || len == 0) return not_appended;
    if (SearchKey<K, K>(key).cmp_full(leaf.key(len - 1)) != Ordering::Greater) return not_appended;
    if (!root_page().shared_prefix().is_shared_by(key)) return not_appended;
    LeafOutcome<Output> outcome = settle(policy.vacant(AnnexedLeaf<K, V, S>(leaf), key, value), refused);
    if (!outcome.ok()) return outcome;
    leaf.insert_fit(len, WindowedKey<K>::at(std::move(key), leaf.prefix_len()), std::move(value));
    return outcome;
  }

  bool is_rightmost_leaf(LeafNode leaf) const noexcept {
    return rightmost_leaf_.value.load(std::memory_order_relaxed) == leaf.node_ptr().tagged();
  }

  void point_appends_to(LeafNode leaf) const noexcept {
    rightmost_leaf_.value.store(leaf.node_ptr().tagged(), std::memory_order_release);
  }

  /// `insert` where the leaf was full: searches again keeping its path, and
  /// splits, until it inserts or the allocator has no page for the split.
  template <class Policy, class A>
  SHOAL_NOINLINE InsertOutcome<typename Policy::Output, K, V> insert_splitting(
      K&& key, V&& value, Policy& policy, A& alloc) const noexcept {
    using Outcome = InsertOutcome<typename Policy::Output, K, V>;
    // Destroyed last, after the nodes each split held: see `NodePages`.
    NodePages<A> pages = this->pages(alloc);
    for (;;) {
      Path path;
      Descent<ExclusiveMode> descent = descend<ExclusiveMode>(SearchKey<K, K>(key), path);
      if (descent.outside) {
        key = shrink_shared_prefix_for(std::move(key), alloc);
        continue;
      }
      std::size_t edge = 0;
      std::optional<Layout> refused;
      LeafOutcome<typename Policy::Output> outcome = insert_into(descent.held, descent.at_separator, key, value,
                                                                  policy, MapPages<A>(alloc), edge, refused);
      if (outcome.ok()) return Outcome::done(std::move(outcome.value()));
      if (refused) return Outcome::failed(InsertError<K, V>(std::move(key), std::move(value), *refused));
      switch (split(path, descent.held, edge, key, value, policy, pages, outcome, refused)) {
        case SplitResult::Inserted:
          return Outcome::done(std::move(outcome.value()));
        case SplitResult::NoPage:
          return Outcome::failed(InsertError<K, V>(std::move(key), std::move(value), *refused));
        case SplitResult::StartOver:
          break;
      }
      // Another thread holds a node the split needs.
      descent.held.release();
      Backoff().spin();
    }
  }

  enum class SplitResult : std::uint8_t { Inserted, NoPage, StartOver };

  /// Splits `held`, the full leaf `path` leads to, and inserts `key` and
  /// `value` at its `edge`, with the unsync tree's `insert_prefixed`, after
  /// taking every node it changes and every page it allocates; or changes
  /// nothing, to try again or for want of a page.
  template <class Policy, class A>
  SplitResult split(const Path& path, const Held<ExclusiveMode>& held, std::size_t edge, K& key,
                    V& value, Policy& policy, NodePages<A>& pages,
                    LeafOutcome<typename Policy::Output>& outcome,
                    std::optional<Layout>& refused) const noexcept {
    // The lowest node with room takes the separator; every node below it
    // splits. Without one, the root splits, and the tree grows.
    std::optional<std::size_t> room;
    for (std::size_t level = path.depth(); level-- > 0;) {
      if (path.level(level).routed.len < InternalNode::capacity()) {
        room = level;
        break;
      }
    }
    const std::size_t top = path.fences_top(room.value_or(0));
    HeldNodes<K, V, S> held_nodes;
    if (!held_nodes.take_path(path, top)) return SplitResult::StartOver;
    // One page for the leaf's right half, one for each internal node below
    // the one with room; a root that splits also moves out of the root page.
    const std::size_t splits = path.depth() - (room ? *room + 1 : 0);
    std::size_t leaves = 1, internals = splits;
    if (!room && path.depth() == 0) {
      leaves = 2;
      internals = 0;
    } else if (!room) {
      internals = splits + 1;
    }
    Layout no_page{0, 0};
    if (!pages.reserve(leaves, internals, no_page)) {
      refused = no_page;
      return SplitResult::NoPage;
    }
    const LeafNode leaf = held.leaf();
    outcome = settle(policy.vacant(AnnexedLeaf<K, V, S>(leaf), key, value), refused);
    if (!outcome.ok()) {
      // The policy refused: the split takes no page.
      pages.unreserve();
      return SplitResult::NoPage;
    }
    const bool was_last_leaf = is_rightmost_leaf(leaf);
    PrefixedKey<K> prefixed = this->prefixed(std::move(key), leaf, pages);
    change_held([&](TreeType& tree) {
      MutStack<K, V, S> stack(tree);
      path.stack_to_leaf(stack, top, leaf, edge);
      // A latched tree keeps no entry count.
      std::size_t uncounted = 0;
      stack.insert_prefixed(std::move(prefixed), std::move(value), pages, uncounted);
    });
    if (was_last_leaf) {
      // The new last leaf is the last under the node that took the
      // separator, or the root: the split holds every node down to it.
      const AnyNode took = room ? AnyNode(path.level(*room).node) : root_page().node();
      point_appends_to(NodeStack<K, V, S>::template border_leaf<LastEdge>(took));
    }
    return SplitResult::Inserted;
  }

  // The shared prefix (`shared_prefix.rs`).

  /// Shrinks what every key shares to what `key` shares too, holding the root
  /// and both borders, and returns `key`.
  template <class A>
  SHOAL_NOINLINE K shrink_shared_prefix_for(K&& key, A& alloc) const noexcept {
    for (;;) {
      HeldNodes<K, V, S> held_nodes;
      if (take_borders(held_nodes)) {
        return change_held([&](TreeType& tree) {
          return tree.shrink_shared_prefix_for(std::move(key), MapPages<A>(alloc)).into_key();
        });
      }
      Backoff().spin();
    }
  }

  bool take_borders(HeldNodes<K, V, S>& held_nodes) const noexcept {
    const AnyNode root = root_page().node();
    if (!held_nodes.take_node(root)) return false;
    // The root page may have changed kind before it was taken.
    if (root_.load(std::memory_order_acquire) != root.node_ptr().tagged()) return false;
    return take_border<FirstEdge>(root, held_nodes) && take_border<LastEdge>(root, held_nodes);
  }

  template <class End>
  static bool take_border(AnyNode node, HeldNodes<K, V, S>& held_nodes) noexcept {
    while (!node.is_leaf()) {
      node = node.internal().child(End::edge(node));
      if (!held_nodes.take_node(node)) return false;
    }
    return true;
  }

  // Removals, internals.

  template <class Q, class Condition, class OnRemove, class A>
  SHOAL_NOINLINE std::optional<std::pair<K, V>> remove_rebalancing(const SearchKey<K, Q>& key,
                                                                    Condition& condition, OnRemove& on_remove,
                                                                    A& alloc) const noexcept {
    NodePages<A> pages = this->pages(alloc);
    for (;;) {
      bool start_over = false;
      std::optional<std::pair<K, V>> removed =
          try_remove_rebalancing(key, condition, on_remove, pages, start_over);
      if (!start_over) return removed;
      Backoff().spin();
    }
  }

  template <class Q, class Condition, class OnRemove, class A>
  std::optional<std::pair<K, V>> try_remove_rebalancing(const SearchKey<K, Q>& key, Condition& condition,
                                                        OnRemove& on_remove, NodePages<A>& pages,
                                                        bool& start_over) const noexcept {
    Path path;
    Descent<ExclusiveMode> descent = descend<ExclusiveMode>(key, path);
    if (descent.outside) return std::nullopt;
    const LeafNode leaf = descent.held.leaf();
    const std::size_t depth = path.depth();
    HeldNodes<K, V, S> held_nodes;
    std::optional<std::size_t> separator_level;
    std::size_t leaf_index = 0;
    if (descent.at_separator) {
      if (!condition(leaf.head_key(), leaf.head_value())) return std::nullopt;
      for (std::size_t level = 0; level < depth; ++level) {
        if (path.level(level).routed.at_separator) {
          separator_level = level;
          break;
        }
      }
      SHOAL_CHECK(separator_level.has_value());
    } else {
      const std::optional<std::size_t> index = leaf.template find<void>(key);
      if (!index) return std::nullopt;
      if (!condition(leaf.key(*index), leaf.value(*index))) return std::nullopt;
      // Another thread may have filled the leaf since.
      if (leaf.keeps_its_minimum() || is_root(leaf)) {
        on_remove(AnnexedLeaf<K, V, S>(leaf), leaf.key(*index), leaf.value(*index));
        return leaf.remove(*index);
      }
      leaf_index = *index;
    }
    // Either way, the leaf loses an entry.
    std::size_t fixes = 0;
    if (!take_fixes(path, leaf.len() - 1, held_nodes, fixes)) {
      start_over = true;
      return std::nullopt;
    }
    std::size_t top_changed = depth - fixes;
    if (separator_level) {
      const auto& level = path.level(*separator_level);
      if (!held_nodes.take_path(path, *separator_level) ||
          !take_left_border(level.node, level.routed.edge - 1, held_nodes)) {
        start_over = true;
        return std::nullopt;
      }
      if (*separator_level < top_changed) top_changed = *separator_level;
    }
    const std::size_t top = path.fences_top(top_changed);
    if (!held_nodes.take_path(path, top)) {
      start_over = true;
      return std::nullopt;
    }
    // Every node the removal changes is held: it commits.
    if (separator_level) {
      on_remove(AnnexedLeaf<K, V, S>(leaf), leaf.head_key(), leaf.head_value());
    } else {
      on_remove(AnnexedLeaf<K, V, S>(leaf), leaf.key(leaf_index), leaf.value(leaf_index));
    }
    std::pair<K, V> removed = change_held([&](TreeType& tree) {
      MutStack<K, V, S> stack(tree);
      if (separator_level) {
        path.stack_to_separator(stack, top, *separator_level, path.level(*separator_level).routed.edge - 1);
      } else {
        path.stack_to_leaf(stack, top, leaf, leaf_index);
      }
      // A latched tree keeps no entry count: `remove` takes this one out of a
      // count of its own.
      std::size_t uncounted = 1;
      return stack.remove(pages, uncounted);
    });
    // A freed last leaf merged into the leaf left of it, under the topmost
    // node fixed, or moved into the root page.
    const Ptr last = Ptr::from_tagged(rightmost_leaf_.value.load(std::memory_order_relaxed));
    if (!last.is_null() && pages.has_freed(last.page())) {
      const AnyNode top_fixed =
          depth - fixes == 0 ? root_page().node() : AnyNode(path.level(depth - fixes).node);
      point_appends_to(NodeStack<K, V, S>::template border_leaf<LastEdge>(top_fixed));
    }
    return removed;
  }

  /// Takes the nodes that fixing the leaf of `path` changes, once a removal
  /// leaves it with `leaf_len` entries: for each level left underfull, its
  /// parent and the sibling the fixing pairs it with. Counts the levels to
  /// fix in `fixes`; false if a node is held or changed.
  bool take_fixes(const Path& path, std::size_t leaf_len, HeldNodes<K, V, S>& held_nodes,
                  std::size_t& fixes) const noexcept {
    std::size_t child_len = leaf_len;
    for (;;) {
      const std::size_t level = path.depth() - fixes;
      const std::size_t min_len = fixes == 0 ? LeafNode::min_len() : InternalNode::min_len();
      if (child_len >= min_len || level == 0) return true;
      const auto& above = path.level(level - 1);
      const InternalNode parent = above.node;
      if (!held_nodes.take_internal_at(parent, above.version)) return false;
      const std::size_t routed_edge = above.routed.edge;
      const std::size_t sibling_edge = routed_edge > 0 ? routed_edge - 1 : routed_edge + 1;
      bool merges;
      if (fixes == 0) {
        const LeafNode sibling = parent.leaf_child(sibling_edge);
        if (!held_nodes.take_leaf(sibling)) return false;
        merges = LeafNode::fit_merged(child_len, sibling.len());
      } else {
        const InternalNode sibling = parent.internal_child(sibling_edge);
        if (!held_nodes.take_internal(sibling)) return false;
        merges = InternalNode::fit_merged(child_len, sibling.len());
      }
      ++fixes;
      // The child takes an entry from its sibling: the parent keeps its
      // separators.
      if (!merges) return true;
      child_len = parent.len() - 1;
    }
  }

  /// Takes the nodes along the right border of `node`'s child at `edge`,
  /// down to its last leaf; the caller holds `node`.
  static bool take_left_border(InternalNode node, std::size_t edge, HeldNodes<K, V, S>& held_nodes) noexcept {
    AnyNode child = node.child(edge);
    for (;;) {
      if (child.is_leaf()) return held_nodes.take_leaf(child.leaf());
      const InternalNode internal = child.internal();
      if (!held_nodes.take_internal(internal)) return false;
      child = internal.child(internal.len());
    }
  }

  // Scans, internals.

  /// Visits a leaf's entries in order `Order`, its head first in key order,
  /// from `cursor`, the number of entries before where the scan stands,
  /// until a key passes `to` or `visit` stops. Returns 0 to go on, 1 past the
  /// end, 2 if `visit` stopped.
  template <class Order, class T, class Visit>
  static int visit_leaf(LeafNode leaf, std::size_t cursor, const ::shoal::Bound<T>& to,
                        Visit& visit) noexcept {
    const bool has_head = leaf.has_head();
    const std::size_t count = static_cast<std::size_t>(has_head) + leaf.len();
    // How the bound orders against a key, `order`, says whether the key is
    // past it.
    auto past_end = [&](const K& key) {
      switch (to.kind()) {
        case ::shoal::Bound<T>::Kind::Included: {
          const Ordering order = SearchKey<K, T>(to.key()).cmp_full(key);
          return Order::descending ? order == Ordering::Greater : order == Ordering::Less;
        }
        case ::shoal::Bound<T>::Kind::Excluded: {
          const Ordering order = SearchKey<K, T>(to.key()).cmp_full(key);
          return Order::descending ? order != Ordering::Less : order != Ordering::Greater;
        }
        default:
          return false;
      }
    };
    auto entry = [&](std::size_t position) -> std::pair<const K*, const V*> {
      if (has_head && position == 0) return {&leaf.head_key(), &leaf.head_value()};
      const std::size_t index = position - static_cast<std::size_t>(has_head);
      return {&leaf.key(index), &leaf.value(index)};
    };
    if constexpr (Order::descending) {
      for (std::size_t position = cursor; position-- > 0;) {
        const auto [key, value] = entry(position);
        if (past_end(*key)) return 1;
        if (!visit(*key, *value)) return 2;
      }
    } else {
      for (std::size_t position = cursor; position < count; ++position) {
        const auto [key, value] = entry(position);
        if (past_end(*key)) return 1;
        if (!visit(*key, *value)) return 2;
      }
    }
    return 0;
  }

  /// Where a scan starts in `leaf` from `key`, visiting its entry if
  /// `inclusive`; `at_head` if the descent found the key at the head.
  template <class Order, class Q>
  static std::size_t cursor_at(LeafNode leaf, const SearchKey<K, Q>& key, bool at_head,
                               bool inclusive) noexcept {
    std::size_t position = 0;
    bool found = true;
    if (!at_head) {
      const SearchResult result = leaf.search(key);
      position = static_cast<std::size_t>(leaf.has_head()) + result.index;
      found = result.found;
    }
    if constexpr (Order::descending) {
      return position + (found && inclusive ? 1 : 0);
    } else {
      return position + (found && !inclusive ? 1 : 0);
    }
  }

  static std::size_t entry_count(LeafNode leaf) noexcept {
    return static_cast<std::size_t>(leaf.has_head()) + leaf.len();
  }

  template <class Order>
  static std::size_t whole_leaf(LeafNode leaf) noexcept {
    return Order::descending ? entry_count(leaf) : 0;
  }

  template <class Order, class F, class T, class Visit>
  bool scan_in(const ::shoal::Bound<F>& from, const ::shoal::Bound<T>& to, Visit& visit) const noexcept {
    Path path;
    Held<SharedMode> held;
    std::size_t cursor = 0;
    if (!scan_start<Order>(from, path, held, cursor)) return false;
    for (;;) {
      const LeafNode leaf = held.leaf();
      const int visited = visit_leaf<Order>(leaf, cursor, to, visit);
      if (visited == 2) return true;
      if (visited == 1) return false;
      Held<SharedMode> next;
      const NextLeaf found = next_leaf<Order>(path, next);
      if (found == NextLeaf::None) return false;
      if (found == NextLeaf::Latched) {
        held = std::move(next);
        cursor = whole_leaf<Order>(held.leaf());
        continue;
      }
      // A node on the way changed: search again for the last key visited,
      // still holding its leaf, and go on past it.
      const K* last = last_visited<Order>(leaf);
      if (last == nullptr) return false;
      const SearchKey<K, K> search(*last);
      Descent<SharedMode> again = descend<SharedMode>(search, path);
      SHOAL_CHECK(!again.outside);
      cursor = cursor_at<Order>(again.held.leaf(), search, again.at_separator, false);
      held = std::move(again.held);
    }
  }

  template <class Order>
  static const K* last_visited(LeafNode leaf) noexcept {
    if constexpr (Order::descending) {
      if (leaf.has_head()) return &leaf.head_key();
      return leaf.len() > 0 ? &leaf.key(0) : nullptr;
    } else {
      if (leaf.len() > 0) return &leaf.key(leaf.len() - 1);
      return leaf.has_head() ? &leaf.head_key() : nullptr;
    }
  }

  /// The leaf where a scan from `from` starts, latched shared, and the place
  /// in it; false if the range has no key in the tree.
  template <class Order, class F>
  bool scan_start(const ::shoal::Bound<F>& from, Path& path, Held<SharedMode>& held,
                  std::size_t& cursor) const noexcept {
    if (from.kind() == ::shoal::Bound<F>::Kind::Unbounded) {
      descend_to_first_leaf<Order>(path, held);
      cursor = whole_leaf<Order>(held.leaf());
      return true;
    }
    const SearchKey<K, F> key(from.key());
    const bool inclusive = from.kind() == ::shoal::Bound<F>::Kind::Included;
    for (;;) {
      Descent<SharedMode> descent = descend<SharedMode>(key, path);
      if (!descent.outside) {
        cursor = cursor_at<Order>(descent.held.leaf(), key, descent.at_separator, inclusive);
        held = std::move(descent.held);
        return true;
      }
      // The bound came before every key in the scan's order: start at the
      // first leaf if it still does once that leaf is latched.
      const Ordering before = Order::descending ? Ordering::Greater : Ordering::Less;
      if (*descent.outside != before) return false;
      descend_to_first_leaf<Order>(path, held);
      if (root_page().shared_prefix().place_outside(key) == std::optional<Ordering>(before)) {
        cursor = whole_leaf<Order>(held.leaf());
        return true;
      }
      held.release();
    }
  }

  template <class Order>
  void descend_to_first_leaf(Path& path, Held<SharedMode>& held) const noexcept {
    for (;;) {
      path.clear();
      const AnyNode root = root_page().node();
      if (root.is_leaf()) {
        held = Held<SharedMode>::lock(root.leaf());
        if (is_root(root.leaf())) return;
        held.release();
        continue;
      }
      Version version(0);
      if (!root_version(root.internal(), version)) continue;
      if (walk_down<Order>(root.internal(), version, std::nullopt, path, held)) return;
    }
  }

  enum class NextLeaf : std::uint8_t { None, Latched, StartOver };

  /// The leaf after the one `path` leads to, in order `Order`, latched
  /// shared before the held one is let go; `path` leads to it then.
  template <class Order>
  NextLeaf next_leaf(Path& path, Held<SharedMode>& next) const noexcept {
    const std::size_t depth = path.depth();
    std::size_t level = depth;
    std::size_t edge = 0;
    for (std::size_t at = depth; at-- > 0;) {
      const Routed routed = path.level(at).routed;
      if constexpr (Order::descending) {
        if (routed.edge > 0) {
          level = at;
          edge = routed.edge - 1;
          break;
        }
      } else {
        if (routed.edge < routed.len) {
          level = at;
          edge = routed.edge + 1;
          break;
        }
      }
    }
    if (level == depth) return NextLeaf::None;
    const auto& from = path.level(level);
    Path next_path;
    next_path.copy_levels_from(path, level);
    if (!walk_down<Order>(from.node, from.version, edge, next_path, next)) return NextLeaf::StartOver;
    // The nodes below still end at the held leaf, which can't split while
    // held: the next leaf is its neighbor.
    for (std::size_t below = level + 1; below < depth; ++below) {
      if (!path.level(below).node.node_ptr().latch().is_unchanged(path.level(below).version)) {
        next.release();
        return NextLeaf::StartOver;
      }
    }
    path.copy_from(next_path);
    return NextLeaf::Latched;
  }

  /// Walks down from `node`, read at `version`, to a leaf, and latches it
  /// shared: leaving `node` through `edge`, or else where `Order` enters it,
  /// and each node below where `Order` enters it, each checked hand over
  /// hand. False if a check fails.
  template <class Order>
  bool walk_down(InternalNode node, Version version, std::optional<std::size_t> edge, Path& path,
                 Held<SharedMode>& held) const noexcept {
    for (;;) {
      const std::size_t len = Race::len(node);
      const std::size_t taken = edge ? *edge : (Order::descending ? len : 0);
      edge.reset();
      const Ptr child = Race::child(node, taken);
      if (child.is_null()) return false;
      if (!node.node_ptr().latch().is_unchanged(version)) return false;
      path.record(node, version, Routed{taken, false, len});
      if (child.is_leaf()) {
        held = Held<SharedMode>::lock(LeafNode::at(child));
        if (node.node_ptr().latch().is_unchanged(version)) return true;
        held.release();
        return false;
      }
      const InternalNode next = InternalNode::at(child);
      const Version next_version = next.node_ptr().latch().read_version();
      if (!node.node_ptr().latch().is_unchanged(version)) return false;
      node = next;
      version = next_version;
    }
  }
};

}  // namespace shoal::detail
