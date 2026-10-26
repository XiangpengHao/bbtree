// Removing from a tree (`tree/remove.rs`). The entry comes out where a node
// stack to it ends: out of a leaf, or out of an internal node as a separator.
// Going back up, each node fixes the child it leads to if the removal left it
// underfull: it merges it with a sibling, or moves one entry over (D22). A
// merge takes a separator out of the node itself, for its parent to check in
// turn. A root left without separators lowers the tree.
//
// Removing a separator takes its head out; the first key of the head's leaf,
// its successor, takes its place, and the successor's value becomes the new
// head (D17). Once the entry is out and counted, the nodes whose fences
// changed are fit to them; those along the right border left of a removed
// separator are fit before anything changes.
#pragma once

#include <cstddef>
#include <cstdint>
#include <optional>
#include <utility>

#include "../config.hpp"
#include "tree.hpp"

namespace shoal::detail {

/// What fixing a child that a removal may have left underfull did.
template <class K, class V, class S>
enum class MutStack<K, V, S>::ChildFix : std::uint8_t {
  /// It wasn't underfull.
  Nothing,
  /// It merged into its left sibling.
  MergedIntoLeft,
  /// Its right sibling merged into it.
  MergedRightIn,
  /// It took an entry from its left sibling, at its front.
  TookFromLeft,
  /// It took an entry from its right sibling, at its end.
  TookFromRight,
};

/// Where a removal's fixing changed fences, for fitting the prefixes there
/// once the removal is done, and where the entry after the removed one went.
template <class K, class V, class S>
struct MutStack<K, V, S>::ChangedFences {
  static constexpr std::size_t no_donor = SIZE_MAX;

  /// The way back down: at each depth the fixing climbed to, the edge to the
  /// child it came from; at the leaf's depth, the slot of the entry after
  /// the removed one.
  std::size_t indexes[max_depth + 1];
  /// At each depth, the edge to the sibling that gave the child an entry.
  std::size_t donors[max_depth + 1];
  /// The depth of the leaf the fixing went up from.
  std::size_t leaf_depth;
  /// The child of every node from this depth down to the leaf's parent has
  /// new fences; none if `nullopt`.
  std::optional<std::size_t> top_depth;

  ChangedFences(std::size_t leaf, std::size_t next_slot, std::optional<std::size_t> top) noexcept
      : leaf_depth(leaf), top_depth(top) {
    for (std::size_t depth = 0; depth <= max_depth; ++depth) {
      indexes[depth] = 0;
      donors[depth] = no_donor;
    }
    indexes[leaf] = next_slot;
  }

  /// Whether the fixing changed the root's children.
  bool reached_root() const noexcept { return top_depth && *top_depth == 0; }

  /// The tree lowered: each node is a depth higher.
  void lower() noexcept {
    for (std::size_t depth = 0; depth < max_depth; ++depth) {
      indexes[depth] = indexes[depth + 1];
      donors[depth] = donors[depth + 1];
    }
    --leaf_depth;
    top_depth = leaf_depth > 0 ? std::optional<std::size_t>(0) : std::nullopt;
  }

  /// Records what fixing the child of the node at `depth` did; `left_len` is
  /// the left sibling's length before a merge into it.
  void record(std::size_t depth, ChildFix fix, std::size_t left_len) noexcept {
    const std::size_t child = indexes[depth];
    switch (fix) {
      case ChildFix::Nothing:
        return;
      case ChildFix::MergedIntoLeft:
        indexes[depth] = child - 1;
        // The child's edges follow the left sibling's keys and the separator.
        indexes[depth + 1] += left_len + 1;
        break;
      case ChildFix::MergedRightIn:
        break;
      case ChildFix::TookFromLeft:
        indexes[depth + 1] += 1;
        donors[depth] = child - 1;
        break;
      case ChildFix::TookFromRight:
        donors[depth] = child + 1;
        break;
    }
    top_depth = top_depth && *top_depth < depth ? *top_depth : depth;
  }
};

template <class K, class V, class S>
template <class Pages>
std::pair<K, V> MutStack<K, V, S>::remove(const Pages& pages, std::size_t& length) noexcept {
  // Most removals take an entry out of a leaf that keeps its minimum, or is
  // the root: nothing to fix, fit or lower.
  const AnyNode bottom = this->node();
  if (bottom.is_leaf()) {
    const LeafNode leaf = bottom.leaf();
    if (leaf.keeps_its_minimum() || this->is_at_root()) {
      std::pair<K, V> entry = leaf.remove(this->index());
      --length;
      this->detach();
      return entry;
    }
  }
  auto taken = take_out(pages, length);
  lower_and_refit_changed_fences(taken.second, pages);
  this->detach();
  return std::move(taken.first);
}

template <class K, class V, class S>
template <class Pages>
std::pair<K, V> MutStack<K, V, S>::remove_and_go_to_next(const Pages& pages,
                                                         std::size_t& length) noexcept {
  auto taken = take_out(pages, length);
  ChangedFences& changed = taken.second;
  // The way down to the next entry: as the node stack leads above the nodes
  // the fixing changed, and as the fixing tracked it from there.
  for (std::size_t depth = 0; depth < this->depth(); ++depth) {
    changed.indexes[depth] = this->index_at(depth);
  }
  lower_and_refit_changed_fences(changed, pages);
  start_at_root();
  for (std::size_t depth = 0; depth < changed.leaf_depth; ++depth) {
    this->set_index(changed.indexes[depth]);
    this->descend();
  }
  go_to_entry_at_slot(changed.indexes[changed.leaf_depth]);
  return std::move(taken.first);
}

template <class K, class V, class S>
template <class Pages>
std::pair<std::pair<K, V>, typename MutStack<K, V, S>::ChangedFences> MutStack<K, V, S>::take_out(
    const Pages& pages, std::size_t& length) noexcept {
  const std::size_t entry_depth = this->depth();
  const std::size_t entry_index = this->index();
  const AnyNode bottom = this->node();
  if (bottom.is_leaf()) {
    std::pair<K, V> entry = bottom.leaf().remove(entry_index);
    // The next entry takes the removed one's place.
    ChangedFences changed =
        fix_up_the_stack(entry_index + 1, std::nullopt, pages, [](InternalNode) {});
    --length;
    return {std::move(entry), changed};
  }
  refit_left_of_separator();
  // Go down to the separator's head.
  this->set_index(entry_index + 1);
  this->template descend_to_leaf<FirstEdge>();
  auto succession = this->leaf().take_head_and_first_key();
  SHOAL_CHECK(succession.has_value());
  std::optional<typename InternalNode::Sep> successor(std::move(succession->successor));
  std::optional<typename InternalNode::Sep> replaced;
  // The successor takes the separator's place, just left of the leaf.
  ChangedFences changed = fix_up_the_stack(0, entry_depth, pages, [&](InternalNode node) {
    SHOAL_CHECK(successor.has_value());
    replaced.emplace(node.replace_separator_from_below(entry_index, std::move(*successor)));
    successor.reset();
  });
  SHOAL_CHECK(replaced.has_value());
  --length;
  if constexpr (Shape::has_inline_keys) {
    return {std::pair<K, V>(std::move(*succession->removed_key), std::move(succession->value)),
            changed};
  } else {
    return {std::pair<K, V>(std::move(*replaced).into_windowed_key().into_key(),
                            std::move(succession->value)),
            changed};
  }
}

template <class K, class V, class S>
template <class Pages, class AtMustReach>
typename MutStack<K, V, S>::ChangedFences MutStack<K, V, S>::fix_up_the_stack(
    std::size_t next_slot, std::optional<std::size_t> must_reach, const Pages& pages,
    AtMustReach at_must_reach) noexcept {
  ChangedFences changed(this->depth(), next_slot, must_reach);
  bool child_is_underfull = this->leaf().is_underfull();
  while (!this->is_at_root() && (child_is_underfull || (must_reach && this->depth() > *must_reach))) {
    const InternalNode node = this->ascend();
    const std::size_t depth = this->depth(), index = this->index();
    changed.indexes[depth] = index;
    if (must_reach && *must_reach == depth) at_must_reach(node);
    if (child_is_underfull) {
      std::size_t left_len = 0;
      const ChildFix fix = node.has_leaf_children()
                               ? fix_underfull_child<LeafNode>(node, index, pages, left_len)
                               : fix_underfull_child<InternalNode>(node, index, pages, left_len);
      const bool merged = fix == ChildFix::MergedIntoLeft || fix == ChildFix::MergedRightIn;
      changed.record(depth, fix, left_len);
      child_is_underfull = merged && node.is_underfull();
    }
  }
  return changed;
}

template <class K, class V, class S>
template <class Pages>
void MutStack<K, V, S>::lower_and_refit_changed_fences(ChangedFences& changed,
                                                       const Pages& pages) noexcept {
  if (lower_an_emptied_root(changed.reached_root(), pages)) changed.lower();
  if (!changed.top_depth) return;
  const std::size_t top_depth = *changed.top_depth;
  while (this->depth() + 1 < changed.leaf_depth) {
    this->set_index(changed.indexes[this->depth()]);
    this->descend();
  }
  for (;;) {
    const std::size_t depth = this->depth();
    const std::size_t child = changed.indexes[depth];
    this->set_index(child);
    if (changed.donors[depth] != ChangedFences::no_donor) {
      const std::size_t edges[2] = {child, changed.donors[depth]};
      refit_children_to_fences(edges);
    } else {
      const std::size_t edges[1] = {child};
      refit_children_to_fences(edges);
    }
    if (depth == top_depth) return;
    this->ascend();
  }
}

template <class K, class V, class S>
template <class C, class Pages>
typename MutStack<K, V, S>::ChildFix MutStack<K, V, S>::fix_underfull_child(
    InternalNode node, std::size_t index, const Pages& pages, std::size_t& left_len) noexcept {
  if (C::child_of(node, index).len() >= C::min_len()) return ChildFix::Nothing;
  const std::size_t sibling = index > 0 ? index - 1 : index + 1;
  SHOAL_CHECK(sibling <= node.len());
  const std::size_t separator_index = index < sibling ? index : sibling;
  const bool child_is_right = sibling < index;
  const Siblings<K, V, S, C> siblings = Siblings<K, V, S, C>::of(node, separator_index);
  left_len = siblings.left_len();
  if (siblings.can_merge()) {
    siblings.merge().deallocate(pages);
    return child_is_right ? ChildFix::MergedIntoLeft : ChildFix::MergedRightIn;
  }
  const auto stolen = child_is_right ? siblings.steal_left() : siblings.steal_right();
  SHOAL_CHECK((stolen == Siblings<K, V, S, C>::Steal::Moved));
  return child_is_right ? ChildFix::TookFromLeft : ChildFix::TookFromRight;
}

/// The tree's removals.
template <class K, class V, class S>
class TreeRemoval {
 public:
  using TreeType = Tree<K, V, S>;

  /// Removes the entry whose key equals `key` and returns it, counting it
  /// out of `length`; `nullopt` if there is none.
  template <class Q, class Pages>
  static std::optional<std::pair<K, V>> remove_entry(TreeType& tree, const Q& key,
                                                     const Pages& pages,
                                                     std::size_t& length) noexcept {
    if (!tree.has_root()) return std::nullopt;
    MutStack<K, V, S> stack(tree);
    if (!stack.search(SearchKey<K, Q>(key))) return std::nullopt;
    return stack.remove(pages, length);
  }

  /// Removes the entry with the smallest key; `nullopt` if empty.
  template <class Pages>
  static std::optional<std::pair<K, V>> pop_first(TreeType& tree, const Pages& pages,
                                                  std::size_t& length) noexcept {
    if (!tree.has_root()) return std::nullopt;
    MutStack<K, V, S> stack(tree);
    if (!stack.descend_to_first_key()) return std::nullopt;
    return stack.remove(pages, length);
  }

  /// Removes the entry with the largest key; `nullopt` if empty.
  template <class Pages>
  static std::optional<std::pair<K, V>> pop_last(TreeType& tree, const Pages& pages,
                                                 std::size_t& length) noexcept {
    if (!tree.has_root()) return std::nullopt;
    MutStack<K, V, S> stack(tree);
    if (!stack.descend_to_last_key()) return std::nullopt;
    return stack.remove(pages, length);
  }
};

}  // namespace shoal::detail
