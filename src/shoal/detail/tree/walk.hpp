// Walks over a tree in key order (`tree/walk.rs`, OG-D23): the core of every
// iterator. A walk is a node stack to a position between two entries of a
// leaf, and moving it never calls `operator<` (OG-D11).
//
// Within a leaf, the next entry is beside the position: the walk keeps the
// leaf, its index and the run's limit in its own fields, a cached run
// (OG-D33), and touches the node stack only to cross a leaf boundary. After a
// leaf's last entry comes the nearest ancestor separator to its right, whose
// value is the next leaf's head (OG-D17): the walk climbs to it and descends
// to that leaf. Walking backward mirrors this.
//
// A walk stops at a position, a leaf and an edge of it, or at the tree's end.
// Every entry has one position just before it and one just after: key `i`
// of leaf `L` lies between `(L, i)` and `(L, i + 1)`, the separator between
// leaves `L` and `R` between `(L, len)` and `(R, 0)`.
#pragma once

#include <cstddef>
#include <new>
#include <optional>

#include "../../bound.hpp"
#include "../config.hpp"
#include "node_stack.hpp"
#include "tree.hpp"

namespace shoal::detail {

/// A position between two entries: a leaf and an edge of it.
template <class K, class V, class S>
struct LeafPosition {
  NodePtr<K, V, S> leaf;
  std::size_t edge;

  bool operator==(const LeafPosition& other) const noexcept {
    return leaf == other.leaf && edge == other.edge;
  }
};

/// A walk over a tree, forward or backward (`Reverse`), from a position to a
/// stop position or the tree's end.
/// Keys side by side in a leaf, `stride` bytes apart, or one key alone: a
/// run a set's cursor steps through without touching its walk
/// (`tree/node/leaf.rs`'s `KeyRun`).
template <class K>
class KeyRun {
 public:
  KeyRun() noexcept = default;
  KeyRun(const K* first, std::size_t stride, std::size_t len) noexcept
      : next_(reinterpret_cast<const std::byte*>(first)), stride_(stride), remaining_(len) {}

  /// `key` alone.
  static KeyRun one(const K* key) noexcept { return KeyRun(key, 0, 1); }

  std::size_t len() const noexcept { return remaining_; }

  /// The run's next key, or null past its last.
  SHOAL_ALWAYS_INLINE const K* peek() const noexcept {
    return remaining_ > 0 ? std::launder(reinterpret_cast<const K*>(next_)) : nullptr;
  }

  SHOAL_ALWAYS_INLINE void advance() noexcept {
    if (remaining_ > 0) {
      --remaining_;
      // Past the last key the address stays within the leaf's page, unread.
      next_ += stride_;
    }
  }

  /// Keeps the run's first `len` keys only; `len` is at most its length.
  void truncate(std::size_t len) noexcept { remaining_ = len; }

 private:
  const std::byte* next_ = nullptr;
  std::size_t stride_ = 0;
  std::size_t remaining_ = 0;
};

template <class K, class V, class S, bool Reverse>
class Walk {
 public:
  using Stack = NodeStack<K, V, S>;
  using LeafNode = Leaf<K, V, S>;
  using InternalNode = Internal<K, V, S>;
  using AnyNode = Node<K, V, S>;
  using Ptr = NodePtr<K, V, S>;
  using Position = LeafPosition<K, V, S>;
  using Entry = EntrySlots<K, V>;
  using StartEdge = std::conditional_t<Reverse, LastEdge, FirstEdge>;

  /// A walk that hands out nothing.
  Walk() noexcept : index_(0), limit_(0), done_(true) {}

  /// Starts at the walk's end of the tree under `root`: its first position
  /// forward, its last backward.
  void start_at_end(AnyNode root) noexcept {
    stack_.attach(root);
    stack_.template descend_to_end<StartEdge>();
    load_run();
  }

  /// Starts over every entry of `tree`.
  void start_all(const Tree<K, V, S>& tree) noexcept {
    if (tree.has_root()) start_at_end(tree.root());
  }

  /// Starts over the entries of `tree` within `lower` and `upper`; hands out
  /// none if the bounds cross. The walk's own node stack goes to where it
  /// starts, in place: a node stack is never copied (OG-D23). The other end is
  /// searched only if it has a bound (OG-D32).
  template <class L, class U>
  void start_range(const Tree<K, V, S>& tree, const ::shoal::Bound<L>& lower,
                   const ::shoal::Bound<U>& upper) noexcept {
    if (!tree.has_root()) return;
    if constexpr (Reverse) {
      position_at_upper(tree, upper, stack_);
      if (lower.kind() != ::shoal::Bound<L>::Kind::Unbounded) {
        Stack stop;
        position_at_lower(tree, lower, stop);
        if (stop.cmp_position(stack_) == Ordering::Greater) return;
        stop_at(Position{stop.cursor_leaf().node_ptr(), stop.cursor_index()});
      }
    } else {
      position_at_lower(tree, lower, stack_);
      if (upper.kind() != ::shoal::Bound<U>::Kind::Unbounded) {
        Stack stop;
        position_at_upper(tree, upper, stop);
        if (stack_.cmp_position(stop) == Ordering::Greater) return;
        stop_at(Position{stop.cursor_leaf().node_ptr(), stop.cursor_index()});
      }
    }
    load_run();
  }

  /// Hands out the next entry into `entry`; false at the stop or the end.
  SHOAL_ALWAYS_INLINE bool next(Entry& entry) noexcept {
    if constexpr (Reverse) {
      if (SHOAL_LIKELY(index_ > limit_)) {
        entry = leaf_.entry_slots(--index_);
        return true;
      }
    } else {
      if (SHOAL_LIKELY(index_ < limit_)) {
        entry = leaf_.entry_slots(index_++);
        return true;
      }
    }
    return cross(entry);
  }

  /// The keys left in the current leaf's run, which the walk moves past at
  /// once; or, where it crosses into another leaf, the separator alone.
  /// False at the stop or the end. Forward walks only.
  SHOAL_ALWAYS_INLINE bool next_key_run(KeyRun<K>& run) noexcept {
    static_assert(!Reverse, "key runs go forward");
    if (index_ < limit_) {
      run = KeyRun<K>(leaf_.entry_slots(index_).key, LeafNode::Kind::keys.key_stride, limit_ - index_);
      index_ = limit_;
      return true;
    }
    Entry entry;
    if (!cross(entry)) return false;
    run = KeyRun<K>::one(entry.key);
    return true;
  }

  /// Hands every entry left to `visit(entry)`, a counted loop per leaf
  /// (OG-D32): no per-entry bookkeeping beyond the index.
  template <class Visit>
  void for_each(Visit visit) noexcept {
    Entry entry;
    for (;;) {
      if constexpr (Reverse) {
        while (index_ > limit_) visit(leaf_.entry_slots(--index_));
      } else {
        while (index_ < limit_) visit(leaf_.entry_slots(index_++));
      }
      if (!cross(entry)) return;
      visit(entry);
    }
  }

  /// The position the walk stands at.
  Position position() const noexcept { return Position{leaf_.node_ptr(), index_}; }

 private:
  Stack stack_;
  LeafNode leaf_;
  /// The edge the walk stands at in `leaf_`.
  std::size_t index_;
  /// Where the run in `leaf_` ends: the leaf's end, or the stop if it is in
  /// this leaf.
  std::size_t limit_;
  Position stop_{Ptr(), 0};
  bool has_stop_ = false;
  bool done_ = false;

  void stop_at(Position stop) noexcept {
    stop_ = stop;
    has_stop_ = true;
  }

  /// Where a range from `lower` starts (`NodeStack::go_to_lower_edge`).
  template <class Q>
  static void position_at_lower(const Tree<K, V, S>& tree, const ::shoal::Bound<Q>& lower,
                                Stack& stack) noexcept {
    stack.attach(tree.root());
    stack.go_to_lower_edge(lower, tree.shared_prefix());
  }

  /// Where a range to `upper` ends (`NodeStack::go_to_upper_edge`).
  template <class Q>
  static void position_at_upper(const Tree<K, V, S>& tree, const ::shoal::Bound<Q>& upper,
                                Stack& stack) noexcept {
    stack.attach(tree.root());
    stack.go_to_upper_edge(upper, tree.shared_prefix());
  }

  /// Takes the leaf and edge the node stack ends at as the run.
  void load_run() noexcept {
    leaf_ = stack_.cursor_leaf();
    index_ = stack_.cursor_index();
    done_ = false;
    set_limit();
  }

  void set_limit() noexcept {
    if (has_stop_ && stop_.leaf == leaf_.node_ptr()) {
      limit_ = stop_.edge;
    } else {
      limit_ = Reverse ? 0 : leaf_.len();
    }
  }

  /// Crosses a leaf boundary and hands out the separator there; false at the
  /// stop or the tree's end.
  ///
  /// Inlined into every scan loop, unlike Rust's out-of-line crossing: a call
  /// that may change the walk keeps the compiler from holding the walk's
  /// index and limit in registers, and the loop then carries each entry's
  /// index through a store and a load. Inlined, a 64-entry range scan of a
  /// 1M `uint64_t` map took 736 cycles instead of 1092 (D14).
  SHOAL_ALWAYS_INLINE bool cross(Entry& entry) noexcept {
    if (done_) return false;
    if (has_stop_ && stop_.leaf == leaf_.node_ptr() && stop_.edge == index_) {
      done_ = true;
      return false;
    }
    if constexpr (Reverse) {
      // Before the leaf's first entry: the separator left of it, whose value
      // is this leaf's head.
      std::size_t depth = stack_.depth();
      while (depth > 0 && stack_.index_at(depth - 1) == 0) --depth;
      if (depth == 0) {
        done_ = true;
        return false;
      }
      --depth;
      const InternalNode node = stack_.internal_at(depth);
      const std::size_t separator = stack_.index_at(depth) - 1;
      entry = node.separator_entry_slots(separator, leaf_);
      stack_.set_levels(depth + 1);
      stack_.set_index(separator);
      stack_.template descend_to_leaf<LastEdge>();
    } else {
      // After the leaf's last entry: the separator right of it, whose value
      // is the next leaf's head.
      std::size_t depth = stack_.depth();
      while (depth > 0 && stack_.index_at(depth - 1) == stack_.internal_at(depth - 1).len()) {
        --depth;
      }
      if (depth == 0) {
        done_ = true;
        return false;
      }
      --depth;
      const InternalNode node = stack_.internal_at(depth);
      const std::size_t separator = stack_.index_at(depth);
      stack_.set_levels(depth + 1);
      stack_.set_index(separator + 1);
      stack_.template descend_to_leaf<FirstEdge>();
      leaf_ = stack_.cursor_leaf();
      entry = node.separator_entry_slots(separator, leaf_);
    }
    load_run();
    return true;
  }
};

}  // namespace shoal::detail
