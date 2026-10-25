// Two adjacent children of an internal node, of either kind, and the
// separator between them (`tree/node/siblings.rs`). A removal that leaves a
// child underfull fixes it here, as does a cut or a build that leaves a border
// short: they merge the two, or move entries from one to the other through
// the separator (D22).
#pragma once

#include <cstddef>
#include <utility>

#include "../config.hpp"
#include "internal.hpp"

namespace bbtree::detail {

/// Separator `index` of `parent` and the children on either side of it, of
/// kind `C` (`Leaf` or `Internal`). The right child's trailing slot 0 is
/// initialized: for leaves, its head.
template <class K, class V, class S, class C>
class Siblings {
 public:
  /// Why a steal moved nothing.
  enum class Steal { Moved, DonorEmpty, ReceiverFull };

  /// Separator `index` of `parent` and its children, of kind `C`, which the
  /// caller decided once for `parent`'s children.
  static Siblings of(Internal<K, V, S> parent, std::size_t index) noexcept {
    BBTREE_CHECK(index < parent.len());
    const C left = C::child_of(parent, index), right = C::child_of(parent, index + 1);
    BBTREE_CHECK(right.has_first_trailing_slot());
    return Siblings(parent, index, left, right);
  }

  Internal<K, V, S> parent() const noexcept { return parent_; }
  std::size_t index() const noexcept { return index_; }
  C left() const noexcept { return left_; }
  C right() const noexcept { return right_; }
  std::size_t left_len() const noexcept { return left_.len(); }
  std::size_t right_len() const noexcept { return right_.len(); }

  /// Whether both children and the separator fit in one node.
  bool can_merge() const noexcept { return C::fit_merged(left_.len(), right_.len()); }

  /// Moves the separator and all of the right child into the left child, and
  /// removes the separator and the right child's edge from the parent.
  /// Returns the right child's emptied page. The merged child takes the
  /// parent's prefix length, both children's inline keys moved back to it
  /// from the separator's window; the caller fits it to its fences.
  AnyFreshPage merge() const noexcept {
    BBTREE_CHECK(can_merge());
    const Window fill = parent_.window(index_);
    auto removed = parent_.remove_with_right_child(index_);
    left_.move_prefix_back_to(fill);
    right_.move_prefix_back_to(fill);
    C::append(left_, fill, std::move(removed.first), right_);
    return right_.free_page();
  }

  /// Rotates one entry rightwards: the left child's last key goes up, the
  /// parent's separator comes down to the front of the right child, which
  /// takes the parent's prefix length. The caller fits both children's
  /// prefixes to their new fences afterwards.
  Steal steal_left() const noexcept {
    if (left_.len() == 0) return Steal::DonorEmpty;
    if (right_.len() == C::capacity()) return Steal::ReceiverFull;
    const Window fill = parent_.window(index_);
    right_.move_prefix_back_to(fill);
    C::steal_left_unchecked(*this, fill);
    return Steal::Moved;
  }

  /// Rotates one entry leftwards, mirrored.
  Steal steal_right() const noexcept {
    if (right_.len() == 0) return Steal::DonorEmpty;
    if (left_.len() == C::capacity()) return Steal::ReceiverFull;
    const Window fill = parent_.window(index_);
    left_.move_prefix_back_to(fill);
    C::steal_right_unchecked(*this, fill);
    return Steal::Moved;
  }

 private:
  Internal<K, V, S> parent_;
  std::size_t index_;
  C left_;
  C right_;

  Siblings(Internal<K, V, S> parent, std::size_t index, C left, C right) noexcept
      : parent_(parent), index_(index), left_(left), right_(right) {}
};

}  // namespace bbtree::detail
