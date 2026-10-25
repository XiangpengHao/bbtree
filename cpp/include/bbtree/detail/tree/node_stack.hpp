// Node stacks (`tree/node_stack.rs`, D11, D24): the nodes from a tree's root
// down to a position in one of them, and the index taken at each. Iterators
// move them through the tree, inserts and removals climb the one that led to
// the key, splitting or fixing the nodes on it.
//
// A node stack has a slot for every level of the tallest possible tree,
// about 250 bytes, the root's first. Nothing records how tall a tree is: a
// node stack counts the levels it goes down. It is made detached and started
// in place, writing only the levels in use, and never moved once written: a
// copy's wide loads would wait on the narrow stores that wrote it (D23).
#pragma once

#include <cstddef>
#include <cstdint>
#include <optional>

#include "../config.hpp"
#include "../node/internal.hpp"
#include "../node/shared_prefix.hpp"

namespace bbtree::detail {

/// The deepest a leaf can be below the root: a non-root internal node has at
/// least 5 children, and a leaf takes at least 1 KiB (D22, D59).
inline constexpr std::size_t max_depth = 23;

/// An end of a node, where a node stack goes down to its first or last
/// entries: a type, so that the code for one end holds no choice (D28).
struct FirstEdge {
  static constexpr bool is_last = false;
  template <class AnyNode>
  static std::size_t edge(AnyNode) noexcept { return 0; }
};

struct LastEdge {
  static constexpr bool is_last = true;
  template <class AnyNode>
  static std::size_t edge(AnyNode node) noexcept { return node.len(); }
};

/// Where a node stack stands relative to a key it looked up.
enum class KeySide : std::uint8_t {
  /// Just before the key, or where it would be.
  Before,
  /// Just after the key, or where it would be.
  After,
};

/// A node stack from a tree's root down to a position in one of its nodes.
///
/// Invariant: `levels <= max_depth + 1`, 0 if detached; for every depth below
/// `levels`, `nodes[depth]` and `indexes[depth]` are written; above the
/// bottom, each node is internal, its index an edge, and the next node the
/// child there. The bottom's index is a position in it, an edge or a key.
template <class K, class V, class S>
class NodeStack {
 public:
  using Ptr = NodePtr<K, V, S>;
  using LeafNode = Leaf<K, V, S>;
  using InternalNode = Internal<K, V, S>;
  using AnyNode = Node<K, V, S>;

  /// A node stack that reaches no node: writes nothing but its levels. Its
  /// slots stay uninitialized until a level is written, as Rust's
  /// `MaybeUninit` slots: zeroing them would cost every range and entry 25
  /// stores.
  NodeStack() noexcept : levels_(0) {}

  /// The leaf at the `End` border of the subtree under `node`.
  template <class End>
  static LeafNode border_leaf(AnyNode node) noexcept {
    while (!node.is_leaf()) node = node.internal().child(End::edge(node));
    return node.leaf();
  }

  /// Makes the node stack just `root`, ending at its first edge.
  void attach(AnyNode root) noexcept {
    write_level(0, root.node_ptr(), 0);
    levels_ = 1;
  }

  /// Makes the node stack reach no node.
  void detach() noexcept { levels_ = 0; }

  /// How far below the root the bottom node is; past every slot if detached.
  std::size_t depth() const noexcept { return static_cast<std::size_t>(levels_) - 1; }
  std::size_t levels() const noexcept { return levels_; }
  bool is_at_root() const noexcept { return levels_ == 1; }
  bool is_attached() const noexcept { return levels_ > 0; }

  Ptr node_ptr(std::size_t depth) const noexcept {
    BBTREE_CHECK(depth < levels_);
    return Ptr::from_tagged(nodes_[depth]);
  }

  AnyNode node_at(std::size_t depth) const noexcept {
    const AnyNode node(node_ptr(depth));
    BBTREE_ASSERT(depth == this->depth() || !node.is_leaf());
    return node;
  }

  InternalNode internal_at(std::size_t depth) const noexcept {
    return InternalNode::at(node_ptr(depth));
  }

  /// The bottom node.
  AnyNode node() const noexcept { return node_at(depth()); }

  LeafNode leaf() const noexcept {
    BBTREE_CHECK(ends_at_leaf());
    return LeafNode::at(Ptr::from_tagged(nodes_[depth()]));
  }

  InternalNode internal() const noexcept { return InternalNode::at(node_ptr(depth())); }

  bool ends_at_leaf() const noexcept { return node_ptr(depth()).is_leaf(); }

  /// The leaf and edge the node stack ends at, read without checks: for a
  /// started walk's cursor.
  BBTREE_ALWAYS_INLINE LeafNode cursor_leaf() const noexcept {
    BBTREE_ASSERT(is_attached() && Ptr::from_tagged(nodes_[levels_ - 1]).is_leaf());
    return LeafNode::at(Ptr::from_tagged(nodes_[levels_ - 1]));
  }
  BBTREE_ALWAYS_INLINE std::size_t cursor_index() const noexcept { return indexes_[levels_ - 1]; }
  BBTREE_ALWAYS_INLINE void set_cursor_index(std::size_t index) noexcept {
    BBTREE_ASSERT(is_attached());
    indexes_[levels_ - 1] = static_cast<std::uint16_t>(index);
  }

  std::size_t index_at(std::size_t depth) const noexcept {
    BBTREE_CHECK(depth < levels_);
    return indexes_[depth];
  }

  /// The position the node stack ends at in its bottom node.
  std::size_t index() const noexcept { return index_at(depth()); }
  void set_index(std::size_t index) noexcept { write_index(depth(), index); }

  /// Goes up to the bottom node's parent, at the edge that leads to it.
  InternalNode ascend() noexcept {
    BBTREE_CHECK(levels_ > 1);
    --levels_;
    return internal_at(levels_ - 1);
  }

  /// Goes down the edge the node stack ends at, to the child's first edge.
  void descend() noexcept {
    const std::size_t at = depth();
    BBTREE_CHECK(!node_ptr(at).is_leaf());
    const Ptr child = internal_at(at).child(indexes_[at]).node_ptr();
    write_level(at + 1, child, 0);
    ++levels_;
  }

  /// Goes down from the edge the node stack ends at to a leaf, along the
  /// `End` edge of every node below, ending at that leaf's `End` edge.
  template <class End>
  void descend_to_leaf() noexcept {
    std::size_t at = depth();
    std::size_t edge = index();
    Ptr node = node_ptr(at);
    while (!node.is_leaf()) {
      node = InternalNode::at(node).child(edge).node_ptr();
      ++at;
      edge = End::edge(AnyNode(node));
      write_level(at, node, edge);
    }
    levels_ = static_cast<std::uint8_t>(at + 1);
  }

  /// Goes down from the bottom node to its `End` end, to a leaf.
  template <class End>
  void descend_to_end() noexcept {
    set_index(End::edge(node()));
    descend_to_leaf<End>();
  }

  /// Goes down to the first key under the bottom node; false, at that leaf's
  /// edge, if the leaf has none (only an empty tree's root).
  bool descend_to_first_key() noexcept {
    descend_to_end<FirstEdge>();
    return node().len() > 0;
  }

  /// Goes down to the last key under the bottom node; false if there is none.
  bool descend_to_last_key() noexcept {
    descend_to_end<LastEdge>();
    if (index() == 0) return false;
    set_index(index() - 1);
    return true;
  }

  /// Goes down from the bottom node, the root of a tree whose keys share
  /// `shared` (D50), searching each node for `key`. If it is there, the node
  /// stack ends at it, in whichever node holds it; if not, at the leaf edge
  /// where it belongs. A key outside what every key shares belongs at the
  /// tree's first or last edge, reached without searching.
  template <class Q>
  bool search(const SearchKey<K, Q>& key, const SharedPrefix& shared) noexcept {
    if (const std::optional<Ordering> outside = shared.place_outside(key)) {
      if (*outside == Ordering::Less) {
        descend_to_end<FirstEdge>();
      } else {
        descend_to_end<LastEdge>();
      }
      return false;
    }
    std::size_t at = depth();
    Ptr node = Ptr::from_tagged(nodes_[at]);
    while (!node.is_leaf()) {
      const InternalNode internal = InternalNode::at(node);
      const SearchResult found = internal.search(key);
      write_index(at, found.index);
      if (found.found) {
        levels_ = static_cast<std::uint8_t>(at + 1);
        return true;
      }
      node = internal.edge(found.index);
      ++at;
      nodes_[at] = node.tagged();
    }
    const SearchResult found = LeafNode::at(node).search(key);
    write_index(at, found.index);
    levels_ = static_cast<std::uint8_t>(at + 1);
    return found.found;
  }

  /// Goes down to the leaf edge just before or after `key`, or where it would
  /// be. A separator stands between the end of the leaf left of it and the
  /// start of the leaf right of it (D17).
  template <class Q>
  void search_to_edge(const SearchKey<K, Q>& key, KeySide side, const SharedPrefix& shared) noexcept {
    if (!search(key, shared)) return;
    if (side == KeySide::Before) {
      descend_to_leaf<LastEdge>();
    } else {
      set_index(index() + 1);
      descend_to_leaf<FirstEdge>();
    }
  }

  /// Whether both node stacks are attached and end at the same position of
  /// the same node.
  bool is_at(const NodeStack& other) const noexcept {
    return is_attached() && levels_ == other.levels_ && index() == other.index() &&
           nodes_[depth()] == other.nodes_[other.depth()];
  }

  /// How the position this node stack ends at compares with `other`'s, in
  /// the same tree, both at the leaves: by the indexes from the root down.
  Ordering cmp_position(const NodeStack& other) const noexcept {
    const std::size_t shared_levels = levels_ < other.levels_ ? levels_ : other.levels_;
    for (std::size_t at = 0; at < shared_levels; ++at) {
      if (indexes_[at] != other.indexes_[at]) {
        return indexes_[at] < other.indexes_[at] ? Ordering::Less : Ordering::Greater;
      }
    }
    if (levels_ != other.levels_) return levels_ < other.levels_ ? Ordering::Less : Ordering::Greater;
    return Ordering::Equal;
  }

  /// Writes level `depth` directly: for a stack made along a kept path
  /// (`tree/latched`).
  void write_level(std::size_t depth, Ptr node, std::size_t index) noexcept {
    BBTREE_ASSERT(depth <= max_depth);
    nodes_[depth] = node.tagged();
    write_index(depth, index);
  }

  void set_levels(std::size_t levels) noexcept { levels_ = static_cast<std::uint8_t>(levels); }

 private:
  /// Each level's node, tagged, as `NodePtr::tagged` stores it.
  std::uintptr_t nodes_[max_depth + 1];
  std::uint16_t indexes_[max_depth + 1];
  std::uint8_t levels_;

  void write_index(std::size_t depth, std::size_t index) noexcept {
    BBTREE_ASSERT(index <= UINT16_MAX);
    indexes_[depth] = static_cast<std::uint16_t>(index);
  }
};

}  // namespace bbtree::detail
