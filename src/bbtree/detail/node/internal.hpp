// Internal nodes: the nodes that route (`tree/node/internal.rs`). An internal
// node's page holds its children, one more than its separators (D12, D32),
// and no values (D17). With inline keys it holds only its separators' inline
// keys and its edges, in a 512 B page by default; each separator's key is the
// head key of the leftmost leaf right of it, read only on a tie (D49).
// Without inline keys, the node keeps its separator keys.
#pragma once

#include <cstddef>
#include <optional>
#include <utility>

#include "../config.hpp"
#include "child.hpp"
#include "leaf.hpp"
#include "page.hpp"
#include "slots.hpp"

namespace bbtree::detail {

template <class K, class V, class S>
class Internal;

/// A node, of the kind its pointer's tag tells: what an edge or the root
/// leads to. A traversal decides the kind once, where it reads the pointer
/// (D28).
template <class K, class V, class S>
class Node {
 public:
  using Ptr = NodePtr<K, V, S>;

  Node() noexcept = default;
  explicit Node(Ptr node) noexcept : node_(node) {}
  Node(Leaf<K, V, S> leaf) noexcept : node_(leaf.node_ptr()) {}
  Node(Internal<K, V, S> internal) noexcept;

  Ptr node_ptr() const noexcept { return node_; }
  bool is_leaf() const noexcept { return node_.is_leaf(); }
  Leaf<K, V, S> leaf() const noexcept { return Leaf<K, V, S>::at(node_); }
  Internal<K, V, S> internal() const noexcept;
  std::size_t len() const noexcept { return node_.len(); }
  std::size_t prefix_len() const noexcept { return node_.prefix_len(); }

  AnyFreshPage free() const noexcept;

 private:
  Ptr node_;
};

template <class K, class V, class S>
class Internal : public ChildBase<Internal<K, V, S>, K, KindShape<K, V, S, false>> {
  using Base = ChildBase<Internal<K, V, S>, K, KindShape<K, V, S, false>>;
  friend Base;

 public:
  using Kind = KindShape<K, V, S, false>;
  using Shape = NodeShape<K, V, S>;
  using Ptr = NodePtr<K, V, S>;
  using Page = FreshPage<K, V, S, PageKind::Internal>;
  using Sep = Separator<K, !Shape::has_inline_keys>;
  using Windowed = WindowedKey<K>;
  using LeafNode = Leaf<K, V, S>;
  using AnyNode = Node<K, V, S>;
  static constexpr bool has_inline_keys = Shape::has_inline_keys;

  Internal() noexcept = default;

  static Internal at(Ptr node) noexcept {
    BBTREE_ASSERT(!node.is_leaf());
    return Internal(node);
  }

  /// A new internal node in `page`, with no separators and `first_child` as
  /// its only child, at `prefix_len`.
  static Internal make(Page&& page, AnyNode first_child, std::size_t prefix_len) noexcept {
    const Internal node(Ptr::template initialize<Kind>(std::move(page).into_raw()));
    node.put_only_child(first_child);
    node.set_empty_prefix_len(prefix_len);
    return node;
  }

  /// Makes `child` this node's edge 0 and only child: a new node, or the root
  /// page turned internal.
  void put_only_child(AnyNode child) const noexcept {
    BBTREE_ASSERT(len() == 0);
    node_.edge_slots().put(0, child.node_ptr());
  }

  Page free() const noexcept { return Page(node_.page()); }

  Ptr node_ptr() const noexcept { return node_; }
  bool operator==(Internal other) const noexcept { return node_ == other.node_; }
  std::size_t len() const noexcept { return node_.len(); }
  std::size_t prefix_len() const noexcept { return node_.prefix_len(); }
  bool is_underfull() const noexcept { return len() < Kind::min_len; }

  /// The header's line, for a batch's next internal search.
  const void* search_line() const noexcept { return node_.page(); }
  /// Edge `index`'s line, for a batch's follow stage; reads nothing.
  const void* edge_line(std::size_t index) const noexcept { return node_.edge_word(index); }

  // Children.

  /// Whether the node's children are leaves, as its first edge's tag tells.
  bool has_leaf_children() const noexcept { return edge(0).is_leaf(); }

  /// Edge `index`: the child's tagged pointer. `index <= len`.
  Ptr edge(std::size_t index) const noexcept {
    BBTREE_ASSERT(index <= len());
    return *node_.edge_slots().slot(index);
  }

  AnyNode child(std::size_t index) const noexcept {
    BBTREE_CHECK(index <= len());
    return AnyNode(edge(index));
  }

  AnyNode child_unchecked(std::size_t index) const noexcept { return AnyNode(edge(index)); }

  LeafNode leaf_child(std::size_t index) const noexcept {
    BBTREE_ASSERT(has_leaf_children());
    BBTREE_CHECK(index <= len());
    return LeafNode::at(edge(index));
  }

  Internal internal_child(std::size_t index) const noexcept {
    BBTREE_ASSERT(!has_leaf_children());
    BBTREE_CHECK(index <= len());
    return Internal::at(edge(index));
  }

  /// The leftmost leaf right of separator `index`, whose head is that
  /// separator's (D17, D49): down edge `index + 1`, then first edges.
  LeafNode head_leaf(std::size_t index) const noexcept {
    Ptr node = child(index + 1).node_ptr();
    while (!node.is_leaf()) node = Internal::at(node).edge(0);
    return LeafNode::at(node);
  }

  // Separators.

  /// Separator `index`'s key: in this node (D17), or with inline keys the
  /// head key of its leaf, which this walks down to (D49).
  const K& separator_key(std::size_t index) const noexcept {
    BBTREE_CHECK(index < len());
    if constexpr (has_inline_keys) {
      return head_leaf(index).head_key();
    } else {
      return keys()[index];
    }
  }

  /// Separator `index`'s key and value: one walk down finds both.
  std::pair<const K*, V*> separator_key_value(std::size_t index) const noexcept {
    const LeafNode head = head_leaf(index);
    if constexpr (has_inline_keys) {
      return {&head.head_key(), &head.head_value()};
    } else {
      return {&keys()[index], &head.head_value()};
    }
  }

  /// Where separator `index`'s entry is, `head_leaf` the leftmost leaf right
  /// of it.
  EntrySlots<K, V> separator_entry_slots(std::size_t index, LeafNode head_leaf) const noexcept {
    V* value = head_leaf.head_value_slot();
    if constexpr (has_inline_keys) {
      return {head_leaf.head_key_slot(), value};
    } else {
      BBTREE_CHECK(index < len());
      return {key_slots().keys().slot(index), value};
    }
  }

  /// The node's separator keys, which it keeps only without inline keys.
  NodeKeys<K> keys() const noexcept {
    static_assert(!has_inline_keys);
    return NodeKeys<K>(node_.page() + Kind::keys.keys_offset, Kind::keys.key_stride, len());
  }

  /// Separator `index`'s window at the node's prefix length. A node keeping
  /// fewer bytes than a leaf reads it from the key again (D43).
  Window window(std::size_t index) const noexcept {
    BBTREE_CHECK(index < len());
    if constexpr (has_inline_keys) {
      const std::size_t prefix = prefix_len();
      if constexpr (Kind::plan.rehydrates_windows) {
        return Window(prefix, KeyInliner<K>::inline_key_at(separator_key(index), prefix));
      } else {
        return Window(prefix, this->inline_keys().inline_key(index));
      }
    } else {
      return Window::none();
    }
  }

  /// The window of a separator beside edge `index`: the one left of it if
  /// any, else the one right of it; `nullopt` without separators.
  std::optional<Window> fence_window(std::size_t index) const noexcept {
    const std::size_t count = len();
    BBTREE_CHECK(index <= count);
    if (index > 0) return window(index - 1);
    if (index < count) return window(index);
    return std::nullopt;
  }

  /// Replaces separator `index` with `separator`, whose window is at this
  /// node's prefix length, and returns the old one as kept.
  Sep replace_separator(std::size_t index, Sep&& separator) const noexcept {
    BBTREE_CHECK(index < len());
    return separator_slots().replace(index, std::move(separator));
  }

  /// `replace_separator` for a separator from below, whose window may be at
  /// a longer prefix: it moves back to this node's, filled from separator
  /// `index`'s window now.
  Sep replace_separator_from_below(std::size_t index, Sep&& separator) const noexcept {
    Sep moved = std::move(separator).moved_back_to(window(index));
    return replace_separator(index, std::move(moved));
  }

  /// Replaces separator `index`'s key and its inline key, returning the old
  /// key: in this node, or with inline keys in its leaf's head (D49).
  K replace_key(std::size_t index, Windowed&& key) const noexcept {
    if constexpr (!has_inline_keys) {
      return replace_separator_from_below(index, Sep(std::move(key))).into_windowed_key().into_key();
    } else {
      const Window key_window = key.window();
      K old = head_leaf(index).replace_head_key(std::move(key.key()));
      replace_separator_from_below(index, Sep(key_window));
      return old;
    }
  }

  // Routing a key.

  /// `key` searched for among the separators: the separator equal to it, or
  /// the edge to the child it belongs under. With inline keys, over those
  /// first, reading a separator's key only on a tie (D49).
  template <class Q>
  BBTREE_ALWAYS_INLINE SearchResult search(const SearchKey<K, Q>& key) const noexcept {
    if constexpr (Kind::plan.inlines_node_search) {
      return search_always_inlined(key);
    } else {
      return search_maybe_inlined(key);
    }
  }

  template <class Q>
  SearchResult search_maybe_inlined(const SearchKey<K, Q>& key) const noexcept {
    return search_always_inlined(key);
  }

  template <class Q>
  BBTREE_ALWAYS_INLINE SearchResult search_always_inlined(const SearchKey<K, Q>& key) const noexcept {
    if constexpr (has_inline_keys && SearchKey<K, Q>::has_inline_target) {
      const Window target = key.inline_target(prefix_len());
      const IndexRange candidates = this->inline_keys().equal_range(target.inline_key());
      // Most searches tie with no separator: they stop here, without a call
      // to the tie search, which reads keys in leaf heads (D49).
      if (candidates.empty()) return SearchResult::edge(candidates.start);
      return search_candidates(candidates, key, target);
    } else if constexpr (has_inline_keys) {
      return KeySearcher::search<Kind::plan.all_keys_search>(
          HeadKeys(*this, 0, len()), key.key(),
          [&](const K& stored) { return key.cmp_full(stored); });
    } else {
      return KeySearcher::search<Kind::plan.all_keys_search>(
          keys(), key.key(), [&](const K& stored) { return key.cmp_full(stored); });
    }
  }

  // Changing separators and children.

  /// Inserts `separator` at edge `index` with `right_child` as the edge to
  /// its right. The node has room, and `right_child` is of its children's
  /// kind, one level down; its leftmost leaf has the separator as its head.
  void insert_fit(std::size_t index, Sep&& separator, AnyNode right_child) const noexcept {
    const std::size_t count = len();
    BBTREE_CHECK(index <= count && count < Kind::capacity);
    BBTREE_CHECK(right_child.is_leaf() == has_leaf_children());
    insert_fit_unchecked(index, std::move(separator), right_child);
  }

  /// Inserts `separator` at edge `index` with `right_child` right of it,
  /// splitting the node first if it is full, the right half in a page from
  /// `pages`: how a child's split lands in its parent. Returns whether it
  /// split, and the split in `split`.
  template <class Pages>
  bool insert(std::size_t index, Sep&& separator, AnyNode right_child, const Pages& pages,
              std::optional<Split<Sep, Internal>>& split) const noexcept {
    if (len() < Kind::capacity) {
      insert_fit(index, std::move(separator), right_child);
      return false;
    }
    split.emplace(split_insert(index, std::move(separator), right_child, Page::allocate(pages)));
    return true;
  }

  /// Splits the full node and inserts `separator`, with `right_child` right
  /// of it, at edge `index`, on the side where that edge ended up. The middle
  /// separator goes up; no leaf changes.
  Split<Sep, Internal> split_insert(std::size_t index, Sep&& separator, AnyNode right_child,
                                    Page&& right_page) const noexcept {
    BBTREE_CHECK(right_child.is_leaf() == has_leaf_children());
    BBTREE_ASSERT(len() == Kind::capacity);
    InsertionSide inserted;
    const std::size_t separator_index =
        InsertionSide::of_split(Kind::capacity, index, Kind::min_len, inserted);
    const Internal right(Ptr::template initialize<Kind>(std::move(right_page).into_raw()));
    right.node_.set_prefix_len(prefix_len());
    Sep middle = split_around(separator_index, right);
    if (inserted.right) {
      right.insert_fit_unchecked(inserted.index, std::move(separator), right_child);
    } else {
      insert_fit_unchecked(inserted.index, std::move(separator), right_child);
    }
    return Split<Sep, Internal>{std::move(middle), right, inserted};
  }

  /// Removes separator `index` and the edge to its right, returning both.
  std::pair<Sep, AnyNode> remove_with_right_child(std::size_t index) const noexcept {
    const std::size_t count = len();
    BBTREE_CHECK(index < count);
    Sep separator = separator_slots().remove(index, count);
    const Ptr child = node_.edge_slots().remove(index + 1, count + 1);
    set_len(count - 1);
    return {std::move(separator), AnyNode(child)};
  }

  /// Cuts the node at edge `index` and returns the new right node, in `page`:
  /// the separators from `index` on and the edges right of them, after
  /// `first_child`, the part of the child at `index` that went right.
  Internal cut_off_at(std::size_t index, AnyNode first_child, Page&& page) const noexcept {
    const std::size_t count = len();
    BBTREE_CHECK(index <= count);
    BBTREE_CHECK(first_child.is_leaf() == has_leaf_children());
    const std::size_t moved = count - index;
    const Internal right = Internal::make(std::move(page), first_child, prefix_len());
    separator_slots().move_run_to(index, moved, right.separator_slots(), 0);
    node_.edge_slots().move_run_to(index + 1, moved, right.node_.edge_slots(), 1);
    right.set_len(moved);
    set_len(index);
    return right;
  }

  // Balancing with a sibling (`Child`).

  static Internal child_of(Internal parent, std::size_t index) noexcept {
    return parent.internal_child(index);
  }

  bool has_first_trailing_slot() const noexcept { return true; }

  const K& key(std::size_t index) const noexcept { return separator_key(index); }

  /// No further than its children's prefixes.
  std::size_t allowed_prefix_len(std::size_t target) const noexcept {
    if (target <= prefix_len()) return target;
    std::size_t allowed = target;
    for (std::size_t index = 0; index <= len(); ++index) {
      const std::size_t child_prefix = child(index).prefix_len();
      if (child_prefix < allowed) allowed = child_prefix;
    }
    return allowed;
  }

  AnyFreshPage free_page() const noexcept {
    return AnyFreshPage{PageKind::Internal, node_.page(), Kind::page};
  }

  static void steal_left_unchecked(const Siblings<K, V, S, Internal>& siblings,
                                   Window fill) noexcept {
    const Internal parent = siblings.parent();
    const std::size_t index = siblings.index();
    const Internal left = siblings.left(), right = siblings.right();
    const std::size_t left_len = left.len(), right_len = right.len();
    Sep separator = left.separator_slots().remove(left_len - 1, left_len);
    Sep old = parent.replace_separator(index, std::move(separator).moved_back_to(fill));
    right.separator_slots().insert(0, right_len, std::move(old));
    const Ptr edge = left.node_.edge_slots().remove(left_len, left_len + 1);
    right.node_.edge_slots().insert(0, right_len + 1, Ptr(edge));
    left.set_len(left_len - 1);
    right.set_len(right_len + 1);
  }

  static void steal_right_unchecked(const Siblings<K, V, S, Internal>& siblings,
                                    Window fill) noexcept {
    const Internal parent = siblings.parent();
    const std::size_t index = siblings.index();
    const Internal left = siblings.left(), right = siblings.right();
    const std::size_t left_len = left.len(), right_len = right.len();
    Sep separator = right.separator_slots().remove(0, right_len);
    Sep old = parent.replace_separator(index, std::move(separator).moved_back_to(fill));
    left.separator_slots().insert(left_len, left_len, std::move(old));
    const Ptr edge = right.node_.edge_slots().remove(0, right_len + 1);
    left.node_.edge_slots().insert(left_len + 1, left_len + 1, Ptr(edge));
    left.set_len(left_len + 1);
    right.set_len(right_len - 1);
  }

  static void append(Internal left, Window, Sep&& separator, Internal right) noexcept {
    const std::size_t left_len = left.len(), right_len = right.len();
    left.separator_slots().write(left_len, std::move(separator));
    right.separator_slots().move_run_to(0, right_len, left.separator_slots(), left_len + 1);
    right.node_.edge_slots().move_run_to(0, right_len + 1, left.node_.edge_slots(), left_len + 1);
    left.set_len(left_len + 1 + right_len);
  }

  SeparatorSlots<K, Kind> separator_slots() const noexcept { return node_.separator_slots(); }

  void set_len(std::size_t new_len) const noexcept {
    node_.template set_len_refilling<Kind>(new_len);
  }

 private:
  Ptr node_;

  explicit Internal(Ptr node) noexcept : node_(node) {}

  KeySlots<K, Kind> key_slots() const noexcept { return node_.template key_slots<Kind>(); }

  /// The keys of separators `start..start + len`, with inline keys: each a
  /// head key, which getting walks down to (D49).
  class HeadKeys {
   public:
    HeadKeys(Internal node, std::size_t start, std::size_t len) noexcept
        : node_(node), start_(start), len_(len) {}
    std::size_t len() const noexcept { return len_; }
    const K& operator[](std::size_t index) const noexcept {
      return node_.head_leaf(start_ + index).head_key();
    }
    HeadKeys slice(std::size_t start, std::size_t end) const noexcept {
      BBTREE_CHECK(start <= end && end <= len_);
      return HeadKeys(node_, start_ + start, end - start);
    }

   private:
    Internal node_;
    std::size_t start_;
    std::size_t len_;
  };

  /// `separator`, from a split of the child at edge `index`, with its window
  /// moved back to this node's prefix length. A node without separators is a
  /// new root, at the shared prefix's length as its only child (D50).
  Sep separator_from_child(std::size_t index, Sep&& separator) const noexcept {
    if (const std::optional<Window> fill = fence_window(index)) {
      return std::move(separator).moved_back_to(*fill);
    }
    BBTREE_ASSERT(!has_inline_keys || separator.window().prefix_len() == prefix_len());
    return std::move(separator);
  }

  void insert_fit_unchecked(std::size_t index, Sep&& separator, AnyNode right_child) const noexcept {
    const std::size_t count = len();
    BBTREE_ASSERT(index <= count && count < Kind::capacity);
    Sep moved = separator_from_child(index, std::move(separator));
    separator_slots().insert(index, count, std::move(moved));
    node_.edge_slots().insert(index + 1, count + 1, right_child.node_ptr());
    node_.set_len(count + 1);
  }

  /// Moves the separators and edges right of `separator_index` into `right`,
  /// and takes out separator `separator_index` for the parent.
  Sep split_around(std::size_t separator_index, Internal right) const noexcept {
    const std::size_t count = len();
    BBTREE_ASSERT(separator_index < count);
    const std::size_t right_len = count - separator_index - 1;
    separator_slots().move_run_to(separator_index + 1, right_len, right.separator_slots(), 0);
    node_.edge_slots().move_run_to(separator_index + 1, right_len + 1, right.node_.edge_slots(), 0);
    right.set_len(right_len);
    Sep separator = separator_slots().read(separator_index);
    set_len(separator_index);
    return separator;
  }

  /// The search among the separators whose inline keys tie with `key`'s.
  template <class Q>
  SearchResult search_candidates(IndexRange candidates, const SearchKey<K, Q>& key,
                                 Window target) const noexcept {
    return KeySearcher::search<Kind::plan.candidate_search>(
               HeadKeys(*this, candidates.start, candidates.size()), key.key(),
               [&](const K& stored) { return key.template cmp_tied<Kind::width>(target, stored); })
        .offset_by(candidates.start);
  }
};

template <class K, class V, class S>
Node<K, V, S>::Node(Internal<K, V, S> internal) noexcept : node_(internal.node_ptr()) {}

template <class K, class V, class S>
Internal<K, V, S> Node<K, V, S>::internal() const noexcept {
  return Internal<K, V, S>::at(node_);
}

template <class K, class V, class S>
AnyFreshPage Node<K, V, S>::free() const noexcept {
  return is_leaf() ? leaf().free_page() : internal().free_page();
}

}  // namespace bbtree::detail
