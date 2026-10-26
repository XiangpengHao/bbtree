// A leaf's annex (`leaf_annex.rs`, `tree/node/annexed_leaf.rs`): data a
// shared `TreeMap` keeps in each of its leaves, right after the leaf's
// header, for the code that owns where its keys and values point, such as a
// store's chain of heap pages. That code reads and changes it while an insert
// or a removal holds the leaf: `update_or_insert_with_leaf` and
// `remove_with_leaf` hand their closures an `AnnexedLeaf`.
//
// An annex type `Annex` is default-constructible, trivially relocatable (it
// moves with its leaf's bytes, PORTING.md P2), and has
//
//   void split(Annex& right) noexcept;       // its leaf split; `right`'s leaf,
//                                            // new, took the entries right of it
//   void merge(Annex&& right) noexcept;      // `right`'s leaf merged into this
//                                            // one's, and goes
//   template <class A>
//   void release(A& alloc) noexcept;         // its leaf goes with the map, which
//                                            // gives the map's allocator
//
// The map keeps it with its leaf: a new leaf gets a default one; a split
// gives the new leaf, right of the one that splits, a default one, then
// calls `split`; a merge calls `merge` on the leaf that stays; destroying or
// clearing the map calls `release` on each. Entries also move between
// neighbouring leaves without a call, as a removal rebalances them. Only a
// shared map keeps annexes; `NoLeafAnnex`, the default, takes no room.
#pragma once

#include <cstddef>

#include "detail/node/leaf.hpp"

namespace shoal {

/// A shared map's leaf that an insert or a removal holds exclusively while
/// it runs its caller's closure: the leaf's annex, and its keys. No other
/// thread reads or changes them meanwhile; the view is valid during the
/// closure only.
template <class K, class V, class S>
class AnnexedLeaf {
 public:
  explicit AnnexedLeaf(detail::Leaf<K, V, S> leaf) noexcept : leaf_(leaf) {}

  /// The leaf's annex.
  typename S::annex& annex() const noexcept { return leaf_.annex(); }

  /// How many keys the leaf holds, its head's included.
  std::size_t key_count() const noexcept { return leaf_.len() + (leaf_.has_head() ? 1 : 0); }

  /// Hands the leaf's keys to `visit(key)` in order: its head's first, if it
  /// has one, the separator's left of it (D49).
  template <class Visit>
  void for_each_key(Visit visit) const noexcept {
    if (leaf_.has_head()) visit(static_cast<const K&>(leaf_.head_key()));
    for (std::size_t index = 0; index < leaf_.len(); ++index) visit(static_cast<const K&>(leaf_.key(index)));
  }

 private:
  detail::Leaf<K, V, S> leaf_;
};

}  // namespace shoal
