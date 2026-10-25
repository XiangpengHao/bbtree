// Batched lookups and inserts in a tree that threads share (D55,
// `tree/latched/batch.rs`): each key descends in its own lane, which
// prefetches the line it reads next before its next step, so that one lane's
// wait for memory overlaps the others'. Lanes step round-robin, one step each
// per round, as the unsync batch's levels do (`tree/batch.hpp`); unlike there,
// another thread's split may change the tree's height under a lane, so each
// lane keeps its own stage.
//
// A lane holds no latch between its steps. It reads the internal nodes and its
// leaf's inline keys optimistically, at versions it checks; it latches the leaf
// only in the step that searches it, and lets go in that step. A lane that
// another thread's change sends back falls back to the single-key operation
// once the batch is done.
#pragma once

#include <cstddef>
#include <cstdint>
#include <optional>
#include <utility>

#include "../config.hpp"
#include "../tree/batch.hpp"
#include "latched_tree.hpp"

namespace bbtree::detail {

template <class K, class V, class S>
class LatchedBatch {
 public:
  using TreeType = LatchedTree<K, V, S>;
  using LeafNode = Leaf<K, V, S>;
  using InternalNode = Internal<K, V, S>;
  using Ptr = NodePtr<K, V, S>;
  using Race = Racing<K, V, S>;

  /// Copies each key's value, `values[i]` for `keys[i]`, as separate
  /// `get_copied`s give them.
  template <class Q>
  static void get_many_copied(const TreeType& tree, const Q* keys, std::size_t count,
                              std::optional<V>* values) noexcept {
    CopyValue visit{values};
    look_up_many(tree, keys, count, visit);
  }

  /// Runs `f(index, key, value)` on each key's entry, latched shared, as
  /// separate `read`s do, in the order the lanes reach their entries.
  template <class Q, class F>
  static void read_many(const TreeType& tree, const Q* keys, std::size_t count, F& f) noexcept {
    ReadWith<F> visit{&f};
    look_up_many(tree, keys, count, visit);
  }

  /// Inserts the entries in input order, as separate `insert`s do: lanes
  /// find every key's leaf first, then each entry goes into its leaf if the
  /// leaf is still at the version its lane read, as moved on by the batch's
  /// own inserts there; otherwise as a single insert. `previous[i]` gets the
  /// value replaced; a failed allocation sets `failed[i]` and gives the entry
  /// back in `keys[i]` and `values[i]`.
  template <class A>
  static void insert_many(const TreeType& tree, K* keys, V* values, std::size_t count,
                          std::optional<V>* previous, bool* failed, A& alloc) noexcept {
    for (std::size_t start = 0; start < count; start += batch_lanes) {
      const std::size_t lanes = count - start < batch_lanes ? count - start : batch_lanes;
      insert_lanes(tree, keys + start, values + start, lanes, previous + start, failed + start, alloc);
    }
  }

 private:
  /// Copies each entry's value out.
  struct CopyValue {
    std::optional<V>* values;
    static constexpr bool copies = true;
    void miss(std::size_t index) noexcept { values[index] = std::nullopt; }
    void visit(std::size_t index, const K&, const V& value) noexcept { values[index] = value; }
    void copied(std::size_t index, const V& value) noexcept { values[index] = value; }
    template <class Q>
    void look_up(const TreeType& tree, std::size_t index, const Q& key) noexcept {
      values[index] = tree.get_copied(key);
    }
  };

  /// Runs the caller's closure on each entry, latched shared.
  template <class F>
  struct ReadWith {
    F* f;
    static constexpr bool copies = false;
    void miss(std::size_t) noexcept {}
    void visit(std::size_t index, const K& key, const V& value) noexcept { (*f)(index, key, value); }
    void copied(std::size_t, const V&) noexcept {}
    template <class Q>
    void look_up(const TreeType& tree, std::size_t index, const Q& key) noexcept {
      tree.read(key, [&](const K& stored, const V& value) { (*f)(index, stored, value); });
    }
  };

  /// Whether the tree is large enough for batches to pay for their lanes:
  /// three internal levels, about an L2 of leaves (doc 12, D55); read
  /// optimistically, so a hint.
  static bool batches(const TreeType& tree) noexcept {
    if (!tree.has_root()) return false;
    const Ptr root = tree.root_ptr();
    if (root.is_leaf()) return false;
#if defined(BBTREE_TEST_BATCHES)
    // Tests batch on every tree with an internal root, to reach the lanes.
    return true;
#endif
    Version version(0);
    if (!tree.root_version(InternalNode::at(root), version)) return false;
    const Ptr child = Race::child(InternalNode::at(root), 0);
    if (!InternalNode::at(root).node_ptr().latch().is_unchanged(version)) return false;
    // Pages below the root keep their kind: an edge read from one is null or
    // a node of the kind its tag says.
    if (child.is_null() || child.is_leaf()) return false;
    const Ptr grandchild = Race::child(InternalNode::at(child), 0);
    return !grandchild.is_null() && !grandchild.is_leaf();
  }

  /// A lane's stage: what it does in its next step, the line it reads there
  /// prefetched in the step before.
  enum class Stage : std::uint8_t {
    Route,         // route through `node`, then fetch the chosen edge
    Follow,        // read the edge, then fetch the child's header
    ChildVersion,  // read the internal child's version
    LeafVersion,   // read the leaf's version
    Leaf,          // search the leaf's inline keys, fetch the candidate
    Candidate,     // settle the candidate, or fetch its key's bytes
    Bytes,         // compare the candidate, latched
    Done,
    Fallback,      // another thread's change: a single-key operation after
  };

  /// One key's lookup: all a lane holds between its steps.
  template <class Q>
  struct Lane {
    InternalNode node;
    Ptr child;
    Version version;
    Stage stage;
    std::uint8_t at_separator;  // 0 or 1, ORed from `Routed`'s as it comes
    bool at_root;
    std::uint16_t index;
    std::uint16_t prefix_len;
  };

  template <class Q, class Visit>
  static void look_up_many(const TreeType& tree, const Q* keys, std::size_t count, Visit& visit) noexcept {
    if (count < 2 || !batches(tree)) {
      for (std::size_t index = 0; index < count; ++index) visit.look_up(tree, index, keys[index]);
      return;
    }
    for (std::size_t start = 0; start < count; start += batch_lanes) {
      const std::size_t lanes = count - start < batch_lanes ? count - start : batch_lanes;
      look_up_lanes(tree, keys + start, lanes, visit, start);
    }
  }

  /// Starts every lane at the root, read once at a version for the batch.
  /// False if the root is a leaf, or changed: every key goes alone.
  template <class Q>
  static bool start(const TreeType& tree, LaneArray<SearchKey<K, Q>, batch_lanes>& search_keys,
                    Lane<Q>* lanes, const Q* keys, std::size_t count) noexcept {
    const Ptr root = tree.root_ptr();
    if (root.is_leaf()) return false;
    const InternalNode node = InternalNode::at(root);
    Version version(0);
    if (!tree.root_version(node, version)) return false;
    const std::optional<SharedPrefix> shared = tree.root_page().load_racing_shared_prefix();
    const std::size_t prefix_len = node.node_ptr().load_racing_header().second;
    for (std::size_t lane = 0; lane < count; ++lane) {
      search_keys.emplace(lane, keys[lane]);
      lanes[lane] = Lane<Q>{node, Ptr(), version, Stage::Route, 0, true, 0, 0};
      if (shared && shared->place_outside(search_keys[lane])) {
        // Outside every key, if the root vouches for the prefix read.
        lanes[lane].stage = node.node_ptr().latch().is_unchanged(version) ? Stage::Done : Stage::Fallback;
        continue;
      }
      Prefetcher::hint(search_keys[lane].window_line(prefix_len));
    }
    return true;
  }

  /// Routes the key through `lane.node`, read at `lane.version`, and fetches
  /// the chosen edge.
  template <class Q>
  BBTREE_ALWAYS_INLINE static void route_step(Lane<Q>& lane, const SearchKey<K, Q>& key) noexcept {
    const std::optional<Routed> routed = Race::route(lane.node, key, lane.version);
    if (!routed) {
      lane.stage = Stage::Fallback;
      return;
    }
    lane.at_separator |= routed->at_separator;
    lane.index = static_cast<std::uint16_t>(routed->edge);
    Prefetcher::hint(lane.node.edge_line(routed->edge));
    lane.stage = Stage::Follow;
  }

  /// Reads the chosen edge, and fetches the child's header. An edge of the
  /// root page is followed only once the root's version vouches for it.
  template <class Q>
  BBTREE_ALWAYS_INLINE static void follow_step(Lane<Q>& lane) noexcept {
    const Ptr child = Race::child(lane.node, lane.index);
    if (child.is_null() || (lane.at_root && !lane.node.node_ptr().latch().is_unchanged(lane.version))) {
      lane.stage = Stage::Fallback;
      return;
    }
    lane.at_root = false;
    lane.child = child;
    Prefetcher::hint(child.page());
    lane.stage = child.is_leaf() ? Stage::LeafVersion : Stage::ChildVersion;
  }

  /// Reads the child's version, its header arrived, and checks the parent's:
  /// the child was still the parent's child then. The lane's fields are read
  /// first: what the code reads after an acquire it must read again.
  template <class Q>
  BBTREE_ALWAYS_INLINE static void version_step(Lane<Q>& lane) noexcept {
    const InternalNode parent = lane.node;
    const Version parent_version = lane.version;
    const Ptr child = lane.child;
    const bool child_is_internal = lane.stage == Stage::ChildVersion;
    const Version child_version = child.latch().read_version();
    if (!parent.node_ptr().latch().is_unchanged(parent_version)) {
      lane.stage = Stage::Fallback;
      return;
    }
    lane.version = child_version;
    if (child_is_internal) {
      lane.node = InternalNode::at(child);
      lane.stage = Stage::Route;
    } else {
      lane.stage = Stage::Leaf;
    }
  }

  /// The live lanes of a batch, by index, compacted after each phase.
  struct LiveLanes {
    std::uint8_t lanes[batch_lanes];
    std::size_t len = 0;
  };

  /// Keeps the lanes in `stage`; a live lane in another, which another
  /// thread's change put there, goes alone after the batch.
  template <class Q>
  BBTREE_ALWAYS_INLINE static void keep(LiveLanes& live, Lane<Q>* lanes, Stage stage) noexcept {
    std::size_t kept = 0;
    for (std::size_t at = 0; at < live.len; ++at) {
      Lane<Q>& state = lanes[live.lanes[at]];
      if (state.stage == stage) {
        live.lanes[kept++] = live.lanes[at];
      } else if (state.stage != Stage::Done) {
        state.stage = Stage::Fallback;
      }
    }
    live.len = kept;
  }

  /// Takes the live lanes, at the root, down to their leaves level by level,
  /// as the unsync batch does (`tree/batch.hpp`): each phase one loop over
  /// them, each step ending with the prefetch of what the next reads. Every
  /// leaf lies as deep, so the first live lane's child tells the level.
  template <class Q>
  BBTREE_ALWAYS_INLINE static void descend_lanes(Lane<Q>* lanes,
                                                 LaneArray<SearchKey<K, Q>, batch_lanes>& keys,
                                                 LiveLanes& live) noexcept {
    // The count and each lane's index are locals: the lanes' fences would
    // make the code read them again from `live` after each.
    std::size_t count = live.len;
    for (std::size_t at = 0; at < count; ++at) {
      const std::size_t id = live.lanes[at];
      route_step(lanes[id], keys[id]);
    }
    // Each phase steps the lanes in its stage and skips the others, which
    // fell back; the live lanes are compacted once a level.
    while (count != 0) {
      Stage next = Stage::Fallback;
      for (std::size_t at = 0; at < count; ++at) {
        Lane<Q>& lane = lanes[live.lanes[at]];
        if (lane.stage != Stage::Follow) continue;
        follow_step(lane);
        if (next == Stage::Fallback) next = lane.stage;
      }
      if (next == Stage::Fallback) {
        live.len = 0;
        return;
      }
      for (std::size_t at = 0; at < count; ++at) {
        const std::size_t id = live.lanes[at];
        Lane<Q>& lane = lanes[id];
        if (lane.stage != next) continue;
        version_step(lane);
        // The child's version, and the next level's route, in one step.
        if (lane.stage == Stage::Route) route_step(lane, keys[id]);
      }
      if (next == Stage::LeafVersion) {
        keep(live, lanes, Stage::Leaf);
        return;
      }
      keep(live, lanes, Stage::Follow);
      count = live.len;
    }
  }

  template <class Q, class Visit>
  static void look_up_lanes(const TreeType& tree, const Q* keys, std::size_t count, Visit& visit,
                            std::size_t base) noexcept {
    LaneArray<SearchKey<K, Q>, batch_lanes> search_keys;
    Lane<Q> lanes[batch_lanes];
    if (!start(tree, search_keys, lanes, keys, count)) {
      for (std::size_t index = 0; index < count; ++index) visit.look_up(tree, base + index, keys[index]);
      return;
    }
    LiveLanes live;
    for (std::size_t lane = 0; lane < count; ++lane) {
      if (lanes[lane].stage == Stage::Done) {
        visit.miss(base + lane);
      } else if (lanes[lane].stage == Stage::Route) {
        live.lanes[live.len++] = static_cast<std::uint8_t>(lane);
      }
    }
    descend_lanes(lanes, search_keys, live);
    // The leaves: each stage a loop over the lanes still in it.
    for (std::size_t at = 0; at < live.len; ++at) {
      leaf_step(lanes[live.lanes[at]], search_keys[live.lanes[at]], visit, base + live.lanes[at]);
    }
    for (std::size_t at = 0; at < live.len; ++at) {
      Lane<Q>& lane = lanes[live.lanes[at]];
      if (lane.stage == Stage::Candidate) candidate_step(lane, search_keys[live.lanes[at]], visit, base + live.lanes[at]);
    }
    for (std::size_t at = 0; at < live.len; ++at) {
      Lane<Q>& lane = lanes[live.lanes[at]];
      if (lane.stage == Stage::Bytes) read_in_leaf(lane, search_keys[live.lanes[at]], visit, base + live.lanes[at]);
    }
    // Every lane another thread's change sent back, whatever stage it was
    // left in, goes alone.
    for (std::size_t lane = 0; lane < count; ++lane) {
      if (lanes[lane].stage != Stage::Done) visit.look_up(tree, base + lane, keys[lane]);
    }
  }

  /// A lane at its leaf, which it read at `lane.version`: rules the key out,
  /// or fetches its lone candidate's entry; the head and ties latched.
  template <class Q, class Visit>
  BBTREE_ALWAYS_INLINE static void leaf_step(Lane<Q>& lane, const SearchKey<K, Q>& key, Visit& visit,
                                             std::size_t index) noexcept {
    const LeafNode leaf = LeafNode::at(lane.child);
    NodeLatch& latch = leaf.node_ptr().latch();
    {
      {
        if (lane.at_separator) {
          if constexpr (Visit::copies && KeyInliner<K>::orders_by_own_bytes) {
            RacingCopy<K> head_key;
            RacingCopy<V> head_value;
            if (Race::load_head(leaf, &head_key, &head_value) && latch.is_unchanged(lane.version) &&
                key.cmp_full(head_key.assume_read()) == Ordering::Equal) {
              visit.copied(index, head_value.assume_read());
              lane.stage = Stage::Done;
              return;
            }
          }
          // The head, latched: the separator may have been removed since.
          latch.lock_shared();
          const bool head = latch.is_unchanged(lane.version) && leaf.has_head() &&
                            key.cmp_full(leaf.head_key()) == Ordering::Equal;
          if (head) visit.visit(index, leaf.head_key(), leaf.head_value());
          latch.unlock_shared();
          lane.stage = head ? Stage::Done : Stage::Fallback;
          return;
        }
        const auto [line, prefix_len] = Race::candidate_line(leaf, key);
        if (line.kind == CandidateLine::Kind::Absent) {
          if (latch.is_unchanged(lane.version)) {
            visit.miss(index);
            lane.stage = Stage::Done;
          } else {
            lane.stage = Stage::Fallback;
          }
          return;
        }
        if (line.kind == CandidateLine::Kind::Unknown) {
          lane.index = UINT16_MAX;
          read_in_leaf(lane, key, visit, index);
          return;
        }
        lane.index = static_cast<std::uint16_t>(line.index);
        lane.prefix_len = static_cast<std::uint16_t>(prefix_len);
        Prefetcher::hint(line.line);
        lane.stage = Stage::Candidate;
        return;
      }
    }
  }

  /// A lane whose candidate's entry arrived: settles a copy unlatched where
  /// the leaf tells, or fetches the key's bytes, or compares it latched.
  template <class Q, class Visit>
  BBTREE_ALWAYS_INLINE static void candidate_step(Lane<Q>& lane, const SearchKey<K, Q>& key, Visit& visit,
                                                  std::size_t index) noexcept {
    const LeafNode leaf = LeafNode::at(lane.child);
    NodeLatch& latch = leaf.node_ptr().latch();
    {
      {
        if constexpr (Visit::copies) {
          if constexpr (KeyInliner<K>::orders_by_own_bytes) {
            RacingCopy<K> stored;
            RacingCopy<V> value;
            Race::load_key(leaf, lane.index, stored);
            Race::load_value(leaf, lane.index, value);
            if (!latch.is_unchanged(lane.version)) {
              lane.stage = Stage::Fallback;
              return;
            }
            if (key.cmp_full(stored.assume_read()) == Ordering::Equal) {
              visit.copied(index, value.assume_read());
            } else {
              visit.miss(index);
            }
            lane.stage = Stage::Done;
            return;
          } else if (const std::optional<bool> is =
                         Race::candidate_is(leaf, key, lane.index, lane.prefix_len)) {
            RacingCopy<V> value;
            if (*is) Race::load_value(leaf, lane.index, value);
            if (!latch.is_unchanged(lane.version)) {
              lane.stage = Stage::Fallback;
              return;
            }
            if (*is) {
              visit.copied(index, value.assume_read());
            } else {
              visit.miss(index);
            }
            lane.stage = Stage::Done;
            return;
          }
        }
        if constexpr (KeyInliner<K>::is_heap_byte_string) {
          // The key's bytes' address, read latched a moment; a prefetch of a
          // stale one is harmless.
          latch.lock_shared();
          if (!latch.is_unchanged(lane.version)) {
            latch.unlock_shared();
            lane.stage = Stage::Fallback;
            return;
          }
          Prefetcher::hint(leaf.candidate_bytes_line(key, lane.index, lane.prefix_len));
          latch.unlock_shared();
          lane.stage = Stage::Bytes;
          return;
        }
        read_in_leaf(lane, key, visit, index);
        return;
      }
    }
  }

  /// Searches the leaf latched shared, if still at the lane's version, and
  /// visits the entry: the lane's candidate, or the leaf's own search.
  template <class Q, class Visit>
  static void read_in_leaf(Lane<Q>& lane, const SearchKey<K, Q>& key, Visit& visit, std::size_t index) noexcept {
    const LeafNode leaf = LeafNode::at(lane.child);
    NodeLatch& latch = leaf.node_ptr().latch();
    latch.lock_shared();
    if (!latch.is_unchanged(lane.version)) {
      latch.unlock_shared();
      lane.stage = Stage::Fallback;
      return;
    }
    const std::optional<std::size_t> found =
        lane.index == UINT16_MAX ? leaf.template find<BatchSearch>(key)
                                 : leaf.find_candidate(key, lane.index, lane.prefix_len);
    if (found) {
      visit.visit(index, leaf.key(*found), leaf.value(*found));
    } else {
      visit.miss(index);
    }
    latch.unlock_shared();
    lane.stage = Stage::Done;
  }

  /// The policy of `BBTreeMap::insert`: replace the value, return the old.
  struct ReplaceValue {
    V operator()(const K&, V& stored, V&& value) const noexcept {
      V old = std::move(stored);
      stored = std::move(value);
      return old;
    }
  };
  using Replacing = UpdateOrInsert<K, V, ReplaceValue>;
  static Replacing replacing() noexcept { return Replacing{ReplaceValue{}}; }

  /// Where a batched insert's key goes: the leaf and the version its lane
  /// read it at, and whether the key matched a separator (the leaf's head).
  struct Destination {
    LeafNode leaf;
    Version version;
    bool at_separator;
    bool valid;
  };

  /// Inserts `key` and `value` into the leaf `destination` names, if that
  /// leaf is still at the version its lane read and has room or the key:
  /// latched exclusively at that version, it is at the next after. False,
  /// changing nothing and keeping the entry, otherwise.
  template <class A>
  static bool insert_at_version(const TreeType& tree, const Destination& destination, K& key, V& value,
                                std::optional<V>& previous, A& alloc) noexcept {
    using Held = HeldLeaf<K, V, S, ExclusiveMode>;
    const Held held = Held::try_lock_at(destination.leaf, destination.version);
    if (!held.holds()) return false;
    if (destination.at_separator && !TreeType::is_head(destination.leaf, SearchKey<K, K>(key))) return false;
    Replacing policy = replacing();
    std::size_t edge = 0;
    std::optional<Layout> refused;  // Replacing a value refuses nothing.
    std::optional<std::optional<V>> output =
        tree.insert_into(held, destination.at_separator, key, value, policy, MapPages<A>(alloc), edge, refused);
    if (!output) return false;
    previous = std::move(*output);
    return true;
  }


  template <class A>
  static void insert_lanes(const TreeType& tree, K* keys, V* values, std::size_t count,
                           std::optional<V>* previous, bool* failed, A& alloc) noexcept {
    for (std::size_t index = 0; index < count; ++index) failed[index] = false;
    auto insert_one = [&](std::size_t index) {
      auto outcome = tree.insert(std::move(keys[index]), std::move(values[index]), replacing(), alloc);
      if (outcome.ok()) {
        previous[index] = std::move(outcome.value());
      } else {
        failed[index] = true;
        keys[index] = std::move(outcome.error().key());
        values[index] = std::move(outcome.error().value());
      }
    };
    if (count < 2 || !batches(tree)) {
      for (std::size_t index = 0; index < count; ++index) insert_one(index);
      return;
    }
    // Ascending keys: if the first appends, the rest go one by one.
    Replacing first_policy = replacing();
    std::optional<Layout> refused;  // Replacing a value refuses nothing.
    if (std::optional<std::optional<V>> appended = tree.try_append(keys[0], values[0], first_policy, refused)) {
      previous[0] = std::move(*appended);
      for (std::size_t index = 1; index < count; ++index) insert_one(index);
      return;
    }
    Destination destinations[batch_lanes];
    {
      LaneArray<SearchKey<K, K>, batch_lanes> search_keys;
      Lane<K> lanes[batch_lanes];
      if (!start(tree, search_keys, lanes, keys, count)) {
        for (std::size_t index = 0; index < count; ++index) insert_one(index);
        return;
      }
      LiveLanes live;
      for (std::size_t lane = 0; lane < count; ++lane) {
        if (lanes[lane].stage == Stage::Route) live.lanes[live.len++] = static_cast<std::uint8_t>(lane);
      }
      descend_lanes(lanes, search_keys, live);
      for (std::size_t lane = 0; lane < count; ++lane) {
        const Lane<K>& state = lanes[lane];
        destinations[lane] = Destination{LeafNode(), Version(0), false, state.stage == Stage::Leaf};
        if (!destinations[lane].valid) continue;
        const LeafNode leaf = LeafNode::at(state.child);
        destinations[lane] = Destination{leaf, state.version, state.at_separator != 0, true};
        // The insertion shifts the entries from there: hint that line.
        Prefetcher::hint(Race::insertion_line(leaf, search_keys[lane]));
      }
    }
    for (std::size_t lane = 0; lane < count; ++lane) {
      const Destination destination = destinations[lane];
      if (!destination.valid) {
        insert_one(lane);
        continue;
      }
      const LeafNode leaf = destination.leaf;
      if (insert_at_version(tree, destination, keys[lane], values[lane], previous[lane], alloc)) {
        // Later keys whose lanes read the leaf as this insert found it trust
        // it as it is now.
        for (std::size_t later = lane + 1; later < count; ++later) {
          Destination& next = destinations[later];
          if (next.valid && next.leaf == leaf && next.version == destination.version) {
            next.version = destination.version.next();
          }
        }
        continue;
      }
      // The leaf changed, or splits: no later key trusts it.
      for (std::size_t later = lane + 1; later < count; ++later) {
        if (destinations[later].valid && destinations[later].leaf == leaf) destinations[later].valid = false;
      }
      insert_one(lane);
    }
  }
};

}  // namespace bbtree::detail
