// Checks that a tree is well formed (`tree/invariants.rs`): its shape, order
// and counts, and on every node that its inline keys and hash tags match its
// keys and its prefix length is what its fences share, or along the borders,
// what every key shares (OG-D50). For tests; a broken invariant ends the
// program with the condition that failed.
#pragma once

#include <cstddef>
#include <optional>

#include "../config.hpp"
#include "tree.hpp"

namespace shoal::detail {

template <class K, class V, class S>
class TreeChecker {
 public:
  using TreeType = Tree<K, V, S>;
  using AnyNode = Node<K, V, S>;
  using LeafNode = Leaf<K, V, S>;
  using InternalNode = Internal<K, V, S>;
  using Shape = NodeShape<K, V, S>;

  /// Checks that `tree` is a well-formed B+-tree of `length` entries (OG-D17):
  /// keys strictly increase in order, within their separators' bounds;
  /// leaves at one depth; nodes below the root at least at their minimum;
  /// every leaf but the leftmost with a head; inline keys the keys' windows
  /// at each node's prefix, which is what its fences share.
  static void check_invariants(const TreeType& tree, std::size_t length) {
    if (!tree.has_root()) {
      SHOAL_CHECK(length == 0);
      return;
    }
    const AnyNode root = tree.root();
    if (length > 0 || !root.is_leaf()) SHOAL_CHECK(root.len() > 0);
    Walk walk{&tree.shared_prefix(), nullptr, 0, 0, 0, std::nullopt};
    walk.check_nodes_under(root, nullptr, nullptr, 0);
    SHOAL_CHECK(walk.separators + 1 == walk.leaves);
    SHOAL_CHECK(walk.entries == length);
    check_prefix_lens(tree);
  }

  /// Checks that no node's prefix is shorter than its parent's (OG-D44), and
  /// every node along the borders has the shared prefix (OG-D50). Calls
  /// neither `operator<` nor any encoding.
  static void check_prefix_lens(const TreeType& tree) {
    if (!tree.has_root()) return;
    const std::size_t shared_len = tree.shared_prefix().size();
    check_prefix_lens_in(tree.root(), shared_len, shared_len, true, true);
  }

  /// Checks that two trees hold the same nodes, keys and prefixes: batched
  /// inserts must make the same splits as individual ones.
  static void check_same_nodes(const TreeType& left, const TreeType& right) {
    SHOAL_CHECK(left.has_root() == right.has_root());
    if (!left.has_root()) return;
    SHOAL_CHECK(left.shared_prefix().size() == right.shared_prefix().size());
    check_same_nodes_in(left.root(), right.root());
  }

 private:
  struct Walk {
    const SharedPrefix* shared_prefix;
    const K* previous_key;
    std::size_t entries;
    std::size_t leaves;
    std::size_t separators;
    std::optional<std::size_t> leaf_depth;

    void check_nodes_under(AnyNode node, const K* lower, const K* upper, std::size_t depth) {
      if (node.is_leaf()) {
        const LeafNode leaf = node.leaf();
        check_node(leaf, lower, upper, depth);
        if (!leaf_depth) leaf_depth = depth;
        SHOAL_CHECK(*leaf_depth == depth);
        SHOAL_CHECK(leaf.has_head() == (leaves != 0));
        ++leaves;
        for (std::size_t index = 0; index < leaf.len(); ++index) visit(leaf.key(index));
        return;
      }
      const InternalNode internal = node.internal();
      check_node(internal, lower, upper, depth);
      for (std::size_t index = 0; index <= internal.len(); ++index) {
        const K* child_lower = index > 0 ? &internal.separator_key(index - 1) : lower;
        const K* child_upper = index < internal.len() ? &internal.separator_key(index) : upper;
        check_nodes_under(internal.child(index), child_lower, child_upper, depth + 1);
        if (index < internal.len()) {
          ++separators;
          visit(internal.separator_key(index));
        }
      }
    }

    template <class C>
    void check_node(C node, const K* lower, const K* upper, std::size_t depth) {
      check_inline_keys_match(node);
      std::size_t fence_prefix_len = 0;
      if constexpr (Shape::has_inline_keys) {
        fence_prefix_len = KeyInliner<K>::fence_prefix_len(lower, upper, shared_prefix->size());
      }
      SHOAL_CHECK(node.prefix_len() == fence_prefix_len);
      if (depth > 0) SHOAL_CHECK(node.len() >= C::min_len());
      for (std::size_t index = 0; index < node.len(); ++index) {
        const K& key = node.key(index);
        if (lower != nullptr) SHOAL_CHECK(*lower < key);
        if (upper != nullptr) SHOAL_CHECK(key < *upper);
        SHOAL_CHECK(shared_prefix->is_shared_by(key));
      }
    }

    template <class C>
    static void check_inline_keys_match(C node) {
      using Kind = typename C::Kind;
      if constexpr (Kind::has_inline_keys) {
        const auto inline_keys = node.inline_keys();
        SHOAL_CHECK(inline_keys.pads_with_greatest());
        const unsigned dropped_bits = 8 * static_cast<unsigned>(8 - Kind::width);
        for (std::size_t index = 0; index < node.len(); ++index) {
          InlineWord expected = KeyInliner<K>::inline_key_at(node.key(index), node.prefix_len());
          if (dropped_bits != 0) expected = expected >> dropped_bits << dropped_bits;
          SHOAL_CHECK(inline_keys.inline_key(index) == expected);
        }
        if constexpr (Kind::has_hash_tags) {
          for (std::size_t index = 0; index < node.len(); ++index) {
            SHOAL_CHECK(node.hash_tags()[index] == KeyInliner<K>::hash_tag(node.key(index)));
          }
        }
      } else {
        SHOAL_CHECK(node.prefix_len() == 0);
      }
    }

    void visit(const K& key) {
      if (previous_key != nullptr) SHOAL_CHECK(*previous_key < key);
      previous_key = &key;
      ++entries;
    }
  };

  static void check_prefix_lens_in(AnyNode node, std::size_t shared_len,
                                   std::size_t parent_prefix_len, bool on_left, bool on_right) {
    const std::size_t prefix_len = node.prefix_len();
    SHOAL_CHECK(prefix_len >= parent_prefix_len);
    SHOAL_CHECK(prefix_len == shared_len || !(on_left || on_right));
    if (node.is_leaf()) return;
    const InternalNode internal = node.internal();
    const std::size_t len = internal.len();
    for (std::size_t index = 0; index <= len; ++index) {
      check_prefix_lens_in(internal.child(index), shared_len, prefix_len, on_left && index == 0,
                           on_right && index == len);
    }
  }

  static void check_same_nodes_in(AnyNode left, AnyNode right) {
    SHOAL_CHECK(left.prefix_len() == right.prefix_len());
    SHOAL_CHECK(left.len() == right.len());
    SHOAL_CHECK(left.is_leaf() == right.is_leaf());
    if (left.is_leaf()) {
      const LeafNode a = left.leaf(), b = right.leaf();
      SHOAL_CHECK(a.has_head() == b.has_head());
      for (std::size_t index = 0; index < a.len(); ++index) {
        SHOAL_CHECK(!(a.key(index) < b.key(index)) && !(b.key(index) < a.key(index)));
      }
      return;
    }
    const InternalNode a = left.internal(), b = right.internal();
    for (std::size_t edge = 0; edge <= a.len(); ++edge) {
      check_same_nodes_in(a.child(edge), b.child(edge));
    }
  }
};

}  // namespace shoal::detail
