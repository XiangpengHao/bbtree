// What a latched tree's operations hold (`tree/latched/descent.rs`,
// `tree/latched/held.rs`):
//
// - `HeldLeaf`: the leaf a descent ends at, latched shared or exclusive, or
//   read at a version (optimistic), let go when destroyed.
// - `TreePath`: the internal nodes a descent went through, each at the
//   version it read, for a split or a removal to take.
// - `HeldNodes`: every node an operation that changes more than its leaf
//   takes, without waiting, before it changes any: let go together.
#pragma once

#include <cstddef>
#include <utility>

#include "../config.hpp"
#include "../node/internal.hpp"
#include "../tree/node_stack.hpp"
#include "racing.hpp"

namespace bbtree::detail {

/// How a leaf is latched: shared, exclusive, or not at all (read at a
/// version, checked after). `Held` is what letting go needs, as Rust's
/// `LatchMode::Held`: nothing for a shared hold, the version otherwise.
struct SharedMode {
  struct Held {};
  static constexpr bool latches = true;
  static Held lock(NodeLatch& latch) noexcept {
    latch.lock_shared();
    return Held{};
  }
  static void unlock(NodeLatch& latch, Held) noexcept { latch.unlock_shared(); }
  static bool is_unchanged(NodeLatch&, Held) noexcept { return true; }
};

struct ExclusiveMode {
  using Held = Version;
  static constexpr bool latches = true;
  static Held lock(NodeLatch& latch) noexcept { return latch.lock_exclusive(); }
  static void unlock(NodeLatch& latch, Held version) noexcept { latch.unlock_exclusive(version); }
  static bool is_unchanged(NodeLatch&, Held) noexcept { return true; }
};

/// Not latched: the leaf is read in place at the version it was at, which
/// the reader checks after (`lookup.rs`). A miss settled so writes nothing.
struct OptimisticMode {
  using Held = Version;
  static constexpr bool latches = false;
  static Held lock(NodeLatch& latch) noexcept { return latch.read_version(); }
  static void unlock(NodeLatch&, Held) noexcept {}
  static bool is_unchanged(NodeLatch& latch, Held version) noexcept {
    return latch.is_unchanged(version);
  }
};

/// A leaf latched in mode `Mode` until destroyed or let go.
template <class K, class V, class S, class Mode>
class HeldLeaf {
 public:
  using LeafNode = Leaf<K, V, S>;

  HeldLeaf() noexcept : leaf_(), held_(), owns_(false) {}

  /// Latches `leaf`, waiting for the other mode's holders.
  static HeldLeaf lock(LeafNode leaf) noexcept {
    return HeldLeaf(leaf, Mode::lock(leaf.node_ptr().latch()));
  }

  /// Latches `leaf` exclusively if it is still at `version` with no holder;
  /// an empty hold otherwise.
  static HeldLeaf try_lock_at(LeafNode leaf, Version version) noexcept {
    static_assert(std::is_same_v<Mode, ExclusiveMode>);
    if (!leaf.node_ptr().latch().try_lock_at(version)) return HeldLeaf();
    return HeldLeaf(leaf, version);
  }

  HeldLeaf(HeldLeaf&& other) noexcept : leaf_(other.leaf_), held_(other.held_), owns_(other.owns_) {
    other.owns_ = false;
  }
  HeldLeaf& operator=(HeldLeaf&& other) noexcept {
    if (this != &other) {
      release();
      leaf_ = other.leaf_;
      held_ = other.held_;
      owns_ = other.owns_;
      other.owns_ = false;
    }
    return *this;
  }
  HeldLeaf(const HeldLeaf&) = delete;
  HeldLeaf& operator=(const HeldLeaf&) = delete;
  ~HeldLeaf() { release(); }

  bool holds() const noexcept { return owns_; }
  LeafNode leaf() const noexcept { return leaf_; }
  /// The version the leaf was read at, optimistically.
  typename Mode::Held version() const noexcept { return held_; }

  /// Whether the leaf is still as the descent found it.
  bool is_unchanged() const noexcept { return Mode::is_unchanged(leaf_.node_ptr().latch(), held_); }

  /// Lets go now.
  void release() noexcept {
    if (owns_) {
      Mode::unlock(leaf_.node_ptr().latch(), held_);
      owns_ = false;
    }
  }

 private:
  LeafNode leaf_;
  typename Mode::Held held_;
  bool owns_;

  HeldLeaf(LeafNode leaf, typename Mode::Held held) noexcept : leaf_(leaf), held_(held), owns_(true) {}
};

/// An internal node a descent went through, at the version it read, and
/// where the key went from it.
template <class K, class V, class S>
struct PathLevel {
  Internal<K, V, S> node;
  Version version;
  Routed routed;
};

/// The internal nodes from the root down to a leaf's parent: level 0 is the
/// root, the leaf at level `depth()`. Levels past the depth are not
/// initialized, so that making a path writes nothing.
template <class K, class V, class S>
class TreePath {
 public:
  using Level = PathLevel<K, V, S>;

  TreePath() noexcept : depth_(0) {}

  std::size_t depth() const noexcept { return depth_; }
  void clear() noexcept { depth_ = 0; }

  void record(Internal<K, V, S> node, Version version, Routed routed) noexcept {
    BBTREE_ASSERT(depth_ < max_depth);
    new (&storage_[depth_]) Level{node, version, routed};
    ++depth_;
  }

  const Level& level(std::size_t index) const noexcept {
    BBTREE_CHECK(index < depth_);
    return *std::launder(reinterpret_cast<const Level*>(&storage_[index]));
  }

  /// Makes levels `0..depth` of `other` this path's, in place.
  void copy_levels_from(const TreePath& other, std::size_t depth) noexcept {
    BBTREE_CHECK(depth <= other.depth_);
    for (std::size_t index = 0; index < depth; ++index) storage_[index] = other.storage_[index];
    depth_ = depth;
  }

  void copy_from(const TreePath& other) noexcept { copy_levels_from(other, other.depth_); }

  /// The topmost level read by an operation that fits the prefixes of the
  /// children of the node at `level` to their fences: the nearest levels
  /// above with a separator left of the path and right of it, which hold the
  /// node's fences; on a side without one, the tree's border.
  std::size_t fences_top(std::size_t at) const noexcept {
    std::size_t top = at;
    bool found_low = false, found_high = false;
    for (std::size_t above = at; above-- > 0;) {
      const Routed routed = level(above).routed;
      if (!found_low && routed.edge > 0) {
        found_low = true;
        if (above < top) top = above;
      }
      if (!found_high && routed.edge < routed.len) {
        found_high = true;
        if (above < top) top = above;
      }
      if (found_low && found_high) break;
    }
    return top;
  }

  /// A node stack down `tree` along the path, from level `top` to `leaf`,
  /// ending at `index` in it, for the unsync tree's operations on the nodes
  /// the caller holds from `top` down.
  template <class Stack>
  void stack_to_leaf(Stack& stack, std::size_t top, Leaf<K, V, S> leaf, std::size_t index) const noexcept {
    std::size_t depth = 0;
    for (std::size_t at = top; at < depth_; ++at, ++depth) {
      stack.write_level(depth, level(at).node.node_ptr(), level(at).routed.edge);
    }
    stack.write_level(depth, leaf.node_ptr(), index);
    stack.set_levels(depth + 1);
  }

  /// A node stack along the path from level `top` to the node at `at`,
  /// ending at its separator `index`.
  template <class Stack>
  void stack_to_separator(Stack& stack, std::size_t top, std::size_t at, std::size_t index) const noexcept {
    std::size_t depth = 0;
    for (std::size_t above = top; above < at; ++above, ++depth) {
      stack.write_level(depth, level(above).node.node_ptr(), level(above).routed.edge);
    }
    stack.write_level(depth, level(at).node.node_ptr(), index);
    stack.set_levels(depth + 1);
  }

 private:
  struct alignas(Level) Slot {
    unsigned char bytes[sizeof(Level)];
  };
  Slot storage_[max_depth];
  std::size_t depth_;
};

/// The nodes an operation holds exclusively, besides the leaf its descent
/// latched: let go together when destroyed, each at the next version, so
/// that a reader that read one meanwhile starts over.
template <class K, class V, class S>
class HeldNodes {
 public:
  using InternalNode = Internal<K, V, S>;
  using LeafNode = Leaf<K, V, S>;

  HeldNodes() noexcept = default;
  HeldNodes(const HeldNodes&) = delete;
  HeldNodes& operator=(const HeldNodes&) = delete;

  ~HeldNodes() {
    for (std::size_t index = 0; index < internals_len_; ++index) {
      internals_[index].node.node_ptr().latch().unlock_exclusive(internals_[index].version);
    }
    for (std::size_t index = 0; index < leaves_len_; ++index) {
      leaves_[index].node.node_ptr().latch().unlock_exclusive(leaves_[index].version);
    }
  }

  /// Takes `node` exclusively if it is still at `version`; false if it
  /// changed or another thread holds it. A node held already stays so.
  bool take_internal_at(InternalNode node, Version version) noexcept {
    if (holds(internals_, internals_len_, node.node_ptr())) return true;
    if (!node.node_ptr().latch().try_lock_at(version)) return false;
    internals_[internals_len_++] = Taken<InternalNode>{node, version};
    return true;
  }

  /// Takes `node` at the version it is at, unless another thread holds it:
  /// for a node reached through nodes the caller holds.
  bool take_internal(InternalNode node) noexcept {
    if (holds(internals_, internals_len_, node.node_ptr())) return true;
    Version version(0);
    return node.node_ptr().latch().try_read_version(version) && take_internal_at(node, version);
  }

  /// Latches `leaf` exclusively, unless another thread holds it.
  bool take_leaf(LeafNode leaf) noexcept {
    if (holds(leaves_, leaves_len_, leaf.node_ptr())) return true;
    Version version(0);
    if (!leaf.node_ptr().latch().try_lock_exclusive(version)) return false;
    leaves_[leaves_len_++] = Taken<LeafNode>{leaf, version};
    return true;
  }

  /// Takes `node`, of either kind, at the version it is at.
  bool take_node(Node<K, V, S> node) noexcept {
    return node.is_leaf() ? take_leaf(node.leaf()) : take_internal(node.internal());
  }

  /// Takes the internal nodes of `path` from level `top` down to the leaf's
  /// parent, each at the version the descent read it at.
  bool take_path(const TreePath<K, V, S>& path, std::size_t top) noexcept {
    for (std::size_t level = top; level < path.depth(); ++level) {
      if (!take_internal_at(path.level(level).node, path.level(level).version)) return false;
    }
    return true;
  }

 private:
  /// The most internal nodes one operation holds: a removal's path, the
  /// sibling of each node on it and the border under a separator it removes;
  /// a shrinking of the shared prefix the root and both borders.
  static constexpr std::size_t max_internals = 3 * max_depth + 2;
  /// The most leaves besides its own: a sibling and the last leaf left of a
  /// removed separator; or the two border leaves.
  static constexpr std::size_t max_leaves = 2;

  template <class N>
  struct Taken {
    N node;
    Version version;
  };

  Taken<InternalNode> internals_[max_internals];
  std::size_t internals_len_ = 0;
  Taken<LeafNode> leaves_[max_leaves];
  std::size_t leaves_len_ = 0;

  template <class N>
  static bool holds(const Taken<N>* taken, std::size_t len, NodePtr<K, V, S> node) noexcept {
    for (std::size_t index = 0; index < len; ++index) {
      if (taken[index].node.node_ptr() == node) return true;
    }
    return false;
  }
};

}  // namespace bbtree::detail
