// Taking an owned tree apart (`tree/teardown.rs`): every key and value
// destroyed once, in ascending key order, each key before its value, as
// std's `BTreeMap` drops them, and every page given back once. A separator's
// value is its head (OG-D17), destroyed right after the separator's key. Keys
// and values without destructors are skipped a leaf at a time (OG-D23). A
// shared map's leaf annexes are released to its allocator with their leaves.
#pragma once

#include <cstddef>
#include <type_traits>

#include "../config.hpp"
#include "tree.hpp"

namespace shoal::detail {

template <class K, class V, class S>
class Teardown {
 public:
  using TreeType = Tree<K, V, S>;
  using AnyNode = Node<K, V, S>;
  using LeafNode = Leaf<K, V, S>;
  using InternalNode = Internal<K, V, S>;
  using Shape = NodeShape<K, V, S>;

  /// Takes `tree` apart, if there is one, leaving none.
  template <class Pages>
  static void destroy(TreeType& tree, const Pages& pages) noexcept {
    if (!tree.has_root()) return;
    const auto root_page = tree.root_page();
    destroy_entries_and_children(root_page.node(), pages);
    std::move(root_page.free()).deallocate(pages);
    tree = TreeType();
  }

  /// Takes apart `node`, on a plain page, and every node under it.
  template <class Pages>
  static void destroy_subtree(AnyNode node, const Pages& pages) noexcept {
    destroy_entries_and_children(node, pages);
    node.free().deallocate(pages);
  }

  /// Gives back every page of `tree`, whose entries were all moved out and
  /// destroyed already (a map consumed by its `IntoIter`), leaving none.
  template <class Pages>
  static void free_emptied(TreeType& tree, const Pages& pages) noexcept {
    if (!tree.has_root()) return;
    const auto root_page = tree.root_page();
    free_children(root_page.node(), pages);
    std::move(root_page.free()).deallocate(pages);
    tree = TreeType();
  }

 private:
  /// Frees every page below `node`, destroying nothing in them.
  template <class Pages>
  static void free_children(AnyNode node, const Pages& pages) noexcept {
    if (node.is_leaf()) return;
    const InternalNode internal = node.internal();
    for (std::size_t edge = 0; edge <= internal.len(); ++edge) {
      const AnyNode child = internal.child(edge);
      free_children(child, pages);
      child.free().deallocate(pages);
    }
  }

  static constexpr bool entries_need_destroying =
      !std::is_trivially_destructible_v<K> || !std::is_trivially_destructible_v<V>;

  /// Destroys every entry of `node` and its subtree, and frees every page
  /// below `node`, but not `node`'s own.
  template <class Pages>
  static void destroy_entries_and_children(AnyNode node, const Pages& pages) noexcept {
    if (node.is_leaf()) {
      if constexpr (entries_need_destroying) destroy_leaf_entries(node.leaf());
      if constexpr (!std::is_empty_v<typename S::annex>) node.leaf().release_annex(pages.allocator());
      return;
    }
    const InternalNode internal = node.internal();
    const std::size_t len = internal.len();
    for (std::size_t edge = 0; edge <= len; ++edge) {
      destroy_subtree(internal.child(edge), pages);
      if constexpr (!Shape::has_inline_keys && !std::is_trivially_destructible_v<K>) {
        if (edge < len) internal.separator_slots().read(edge);
      }
    }
  }

  static void destroy_leaf_entries(LeafNode leaf) noexcept {
    if (leaf.has_head()) leaf.take_head();
    const std::size_t len = leaf.len();
    for (std::size_t index = 0; index < len; ++index) {
      auto entry = leaf.entry_slots(index);
      entry.key->~K();
      entry.value->~V();
    }
  }
};

}  // namespace shoal::detail
