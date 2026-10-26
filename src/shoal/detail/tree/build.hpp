// Building, cutting and copying whole trees (`tree/build.rs`, `tree/cut.rs`,
// `tree/clone.rs`):
//
// - `TreeBuilder` builds a tree from entries in ascending key order,
//   comparing no keys: each node fills before the next starts beside it, and
//   `finish` tops up the right border past the minimum from the full left
//   siblings (D25).
// - `TreeReshape::split_off_at` cuts a tree in two where a search went: each
//   node on the node stack keeps what lies left of it, what lies right goes
//   into a new node; then both sides of the cut are brought back to the
//   minimum, from the top down (`fix_border`).
// - `TreeReshape::clone_in` copies a tree node by node, same shape, calling
//   no `operator<` and no `InlineKey`.
#pragma once

#include <cstddef>
#include <utility>

#include "../config.hpp"
#include "teardown.hpp"
#include "tree.hpp"

namespace shoal::detail {

template <class K, class V, class S>
class TreeReshape {
 public:
  using TreeType = Tree<K, V, S>;
  using LeafNode = Leaf<K, V, S>;
  using InternalNode = Internal<K, V, S>;
  using AnyNode = Node<K, V, S>;
  using Stack = MutStack<K, V, S>;
  using Shape = NodeShape<K, V, S>;

  // Borders (`cut.rs`).

  /// Brings the nodes along the tree's `End` edge up to their minimum (D22),
  /// from the top down, reading no key: each border node gets one key more
  /// than its minimum, by a merge with its sibling or keys moved over from
  /// it, so that a merge below leaves it at its minimum.
  template <class End, class Pages>
  static void fix_border(TreeType& tree, const Pages& pages) noexcept {
    lower_while_root_empty(tree, pages);
    AnyNode root = tree.root_mut();
    if (!root.is_leaf()) {
      InternalNode node = root.internal();
      for (;;) {
        const std::size_t len = node.len();
        SHOAL_CHECK(len > 0);
        const std::size_t separator = End::is_last ? len - 1 : 0;
        if (node.has_leaf_children()) {
          stock_border_child<End, LeafNode>(Siblings<K, V, S, LeafNode>::of(node, separator), pages);
          break;
        }
        node = stock_border_child<End, InternalNode>(Siblings<K, V, S, InternalNode>::of(node, separator),
                                                     pages);
      }
    }
    lower_while_root_empty(tree, pages);
  }

  template <class Pages>
  static void lower_while_root_empty(TreeType& tree, const Pages& pages) noexcept {
    while (!tree.root().is_leaf() && tree.root().len() == 0) tree.lower().deallocate(pages);
  }

  // Cuts (`cut.rs`).

  /// Cuts `tree` at the leaf edge on `side` of `key`: `tree` keeps what lies
  /// left, the returned tree what lies right. Both keep what every key
  /// shared, and their borders get their minimum and their siblings' prefixes
  /// fit; `lengths` tells how many entries each holds.
  template <class Q, class Pages>
  static TreeType split_off_at(TreeType& tree, const Q& key, KeySide side, const Pages& pages) noexcept {
    const SharedPrefix shared = tree.shared_prefix();
    AnyNode upper_top;
    {
      Stack stack(tree);
      stack.search_to_edge(SearchKey<K, Q>(key), side);
      upper_top = cut(stack, pages);
    }
    TreeType upper = TreeType::from_node(upper_top, shared, pages);
    fix_border<LastEdge>(tree, pages);
    fix_border<FirstEdge>(upper, pages);
    tree.template refit_border_siblings<LastEdge>();
    upper.template refit_border_siblings<FirstEdge>();
    return upper;
  }

  /// The two trees' lengths after a cut of a tree of `length` entries: the
  /// shorter tree's entries counted a node at a time, as std's do.
  static std::pair<std::size_t, std::size_t> lengths(const TreeType& lower, const TreeType& upper,
                                                     std::size_t length) noexcept {
    if (lower.leaf_depth() < upper.leaf_depth()) {
      const std::size_t lower_length = count_entries(lower.root());
      return {lower_length, length - lower_length};
    }
    const std::size_t upper_length = count_entries(upper.root());
    return {length - upper_length, upper_length};
  }

  // Copies (`clone.rs`).

  /// A copy of `tree`, node by node, keys and values copied by their copy
  /// constructors. If one throws, what was copied is taken apart, and the
  /// exception goes on.
  template <class Pages>
  static TreeType clone_in(const TreeType& tree, const Pages& pages) {
    if (!tree.has_root()) return TreeType();
    const AnyNode top = clone_nodes(tree.root(), pages);
    return TreeType::from_node(top, tree.shared_prefix(), pages);
  }

 private:
  /// Brings the border child of `siblings` past its minimum: merges the two
  /// if they fit, else moves keys over until it has one more than its
  /// minimum. Returns the border child.
  template <class End, class C, class Pages>
  static C stock_border_child(Siblings<K, V, S, C> siblings, const Pages& pages) noexcept {
    const std::size_t target = C::min_len() + 1;
    auto border_len = [&] { return End::is_last ? siblings.right_len() : siblings.left_len(); };
    if (border_len() < target) {
      if (siblings.can_merge()) {
        const C merged = siblings.left();
        siblings.merge().deallocate(pages);
        return merged;
      }
      while (border_len() < target) {
        const auto moved = End::is_last ? siblings.steal_left() : siblings.steal_right();
        SHOAL_CHECK((moved == Siblings<K, V, S, C>::Steal::Moved));
      }
    }
    return End::is_last ? siblings.right() : siblings.left();
  }

  /// Cuts each node up the stack at its edge, keeping the left part; returns
  /// the top of the right parts.
  template <class Pages>
  static AnyNode cut(Stack& stack, const Pages& pages) noexcept {
    stack.move_prefixes_back_to_root();
    AnyNode right(stack.leaf().cut_off_at(stack.index(), LeafNode::Page::allocate(pages)));
    while (!stack.is_at_root()) {
      const InternalNode node = stack.ascend();
      right = AnyNode(node.cut_off_at(stack.index(), right, InternalNode::Page::allocate(pages)));
    }
    return right;
  }

  static std::size_t count_entries(AnyNode node) noexcept {
    if (node.is_leaf()) return node.len();
    const InternalNode internal = node.internal();
    std::size_t count = internal.len();
    for (std::size_t edge = 0; edge <= internal.len(); ++edge) count += count_entries(internal.child(edge));
    return count;
  }

  /// Takes apart a partial copy if a copy constructor throws.
  template <class Pages>
  struct PartialCopy {
    AnyNode top;
    const Pages* pages;
    bool armed = true;
    ~PartialCopy() {
      if (armed) Teardown<K, V, S>::destroy_subtree(top, *pages);
    }
  };

  template <class Pages>
  static AnyNode clone_nodes(AnyNode node, const Pages& pages) {
    if (node.is_leaf()) {
      const LeafNode leaf = node.leaf();
      const LeafNode copy = LeafNode::make(LeafNode::Page::allocate(pages), leaf.prefix_len());
      PartialCopy<Pages> guard{AnyNode(copy), &pages};
      if (leaf.has_head()) {
        V value(leaf.head_value());
        if constexpr (Shape::has_inline_keys) {
          K key(leaf.head_key());
          copy.put_head(&key, std::move(value));
        } else {
          copy.put_head(nullptr, std::move(value));
        }
      }
      for (std::size_t index = 0; index < leaf.len(); ++index) {
        K key(leaf.key(index));
        V value(leaf.value(index));
        copy.insert_fit(index, WindowedKey<K>(std::move(key), leaf.window(index)), std::move(value));
      }
      guard.armed = false;
      return AnyNode(copy);
    }
    const InternalNode internal = node.internal();
    const AnyNode first = clone_nodes(internal.child(0), pages);
    PartialCopy<Pages> guard{first, &pages};
    const InternalNode copy = InternalNode::make(InternalNode::Page::allocate(pages), first, internal.prefix_len());
    guard.top = AnyNode(copy);
    for (std::size_t index = 0; index < internal.len(); ++index) {
      const Window window = internal.window(index);
      const AnyNode child = clone_nodes(internal.child(index + 1), pages);
      if constexpr (Shape::has_inline_keys) {
        copy.insert_fit(index, typename InternalNode::Sep(window), child);
      } else {
        K key(internal.separator_key(index));
        copy.insert_fit(index, typename InternalNode::Sep(WindowedKey<K>(std::move(key), window)), child);
      }
    }
    guard.armed = false;
    return AnyNode(copy);
  }
};

/// A tree being built from entries in ascending key order (D25).
template <class K, class V, class S, class Pages>
class TreeBuilder {
 public:
  using TreeType = Tree<K, V, S>;
  using LeafNode = Leaf<K, V, S>;
  using InternalNode = Internal<K, V, S>;
  using AnyNode = Node<K, V, S>;

  explicit TreeBuilder(const Pages& pages) noexcept : pages_(pages) {}
  /// The builder keeps a reference to `pages`: a temporary would dangle.
  explicit TreeBuilder(const Pages&&) = delete;
  TreeBuilder(const TreeBuilder&) = delete;
  TreeBuilder& operator=(const TreeBuilder&) = delete;
  /// Takes apart a tree that was not finished.
  ~TreeBuilder() { Teardown<K, V, S>::destroy(tree_, pages_); }

  /// Appends an entry whose key is greater than every key so far. The order
  /// is not checked: entries out of order make a tree whose searches give
  /// wrong answers, but that is memory-safe.
  void push(K&& key, V&& value) noexcept {
    PrefixedKey<K> prefixed = tree_.shrink_shared_prefix_for(std::move(key), pages_);
    const std::size_t shared_len = tree_.shared_prefix().size();
    // On the right border, whose prefix is what every key shares.
    const LeafNode leaf = rightmost_leaf();
    const std::size_t len = leaf.len();
    if (len < LeafNode::capacity()) {
      leaf.insert_fit(len, std::move(prefixed).window_at(shared_len), std::move(value));
      ++length_;
      return;
    }
    // Past the full leaf: it goes up into the right border.
    Stack stack = start_leaf(std::move(prefixed).window_at(shared_len), std::move(value));
    ++length_;
    // Fit the nodes the new leaf closed, now off the border: the separator
    // is their upper fence, and no key joins them again.
    const std::size_t edge = stack.index();
    stack.template refit_border_below<LastEdge>(edge);
    const std::size_t edges[1] = {edge};
    stack.refit_children_to_fences(edges);
    rightmost_ = LeafNode();
  }

  /// The tree and its number of entries, its right border topped up.
  std::pair<TreeType, std::size_t> finish() noexcept {
    if (tree_.has_root()) {
      TreeReshape<K, V, S>::template fix_border<LastEdge>(tree_, pages_);
      tree_.template refit_border_siblings<LastEdge>();
    }
    TreeType tree = tree_.take();
    return {tree, length_};
  }

 private:
  using Stack = MutStack<K, V, S>;

  TreeType tree_;
  std::size_t length_ = 0;
  const Pages& pages_;
  /// The rightmost leaf, found once per leaf.
  LeafNode rightmost_ = LeafNode();

  LeafNode rightmost_leaf() noexcept {
    if (rightmost_.node_ptr().is_null()) {
      rightmost_ = NodeStack<K, V, S>::template border_leaf<LastEdge>(tree_.root_mut());
    }
    return rightmost_;
  }

  /// Starts a new rightmost leaf after a full one, its head `value` and with
  /// inline keys `separator`'s key (D49), going in after `separator` in the
  /// rightmost internal node one level up if it has room, else in a new node
  /// there, and so on up, raising the root if need be. Returns the node stack
  /// up the right border, at the node that took the separator, at the edge
  /// left of it.
  Stack start_leaf(WindowedKey<K>&& separator, V&& value) noexcept {
    const std::size_t shared_len = tree_.shared_prefix().size();
    const LeafNode leaf = LeafNode::make(LeafNode::Page::allocate(pages_), shared_len);
    typename InternalNode::Sep kept = leaf.put_separator_head(std::move(separator), std::move(value));
    AnyNode child(leaf);
    Stack stack(tree_);
    stack.descend_to_last_key();
    for (;;) {
      const InternalNode node = stack.ascend_raising(pages_);
      const std::size_t len = node.len();
      if (len < InternalNode::capacity()) {
        node.insert_fit(len, std::move(kept), child);
        return stack;
      }
      // Its new levels along the right border take what every key shares.
      child = AnyNode(InternalNode::make(InternalNode::Page::allocate(pages_), child, shared_len));
    }
  }
};

}  // namespace shoal::detail
