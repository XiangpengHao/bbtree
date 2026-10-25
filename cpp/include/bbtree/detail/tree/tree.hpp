// The tree under the maps (`tree/root.rs`, `tree/search.rs`,
// `tree/insert.rs`, `tree/prefix.rs`): a root page, if there is one, which
// owns every node under it, and the last leaf an append went to (D45).
//
// - `Tree`: lookups, inserts, and the shared prefix's upkeep (D50).
// - `MutStack`: an exclusive node stack, which inserts and removals climb,
//   and the prefix fits that follow a change (D26).
// - `PrefixedKey`: a key that shares what every key of its tree shares.
//
// Removal is `remove.hpp`'s, teardown `teardown.hpp`'s.
#pragma once

#include <cstddef>
#include <cstdint>
#include <optional>
#include <utility>

#include "../config.hpp"
#include "../node/root_page.hpp"
#include "../node/siblings.hpp"
#include "node_stack.hpp"

namespace bbtree::detail {

/// A key that shares what every key of its tree shares (D50): a search that
/// skips each node's prefix unchecked needs one. Only the tree makes one, by
/// shrinking the shared prefix for it or by checking it.
template <class K>
class PrefixedKey {
 public:
  /// `key`, which a latched tree's descent found shares what every key
  /// shares, on its way into the leaf it holds.
  static PrefixedKey checked_by_descent(K&& key) noexcept { return PrefixedKey(std::move(key)); }

  static PrefixedKey shrunk_for(K&& key) noexcept { return PrefixedKey(std::move(key)); }

  const K& key() const noexcept { return key_; }
  K& key() noexcept { return key_; }
  K into_key() && noexcept { return std::move(key_); }

  /// The key with its window at `prefix_len`, the prefix length of the node
  /// it goes into.
  WindowedKey<K> window_at(std::size_t prefix_len) && noexcept {
    return WindowedKey<K>::at(std::move(key_), prefix_len);
  }

 private:
  K key_;
  explicit PrefixedKey(K&& key) noexcept : key_(std::move(key)) {}
};

/// Where a search found a key: in a leaf beside its value, or as a separator
/// in an internal node, whose value is its head (D17).
template <class K, class V, class S>
class Found {
 public:
  using LeafNode = Leaf<K, V, S>;
  using InternalNode = Internal<K, V, S>;

  static Found in_leaf(LeafNode leaf, std::size_t index) noexcept {
    return Found(leaf.node_ptr(), index);
  }
  static Found separator(InternalNode internal, std::size_t index) noexcept {
    return Found(internal.node_ptr(), index);
  }

  bool is_in_leaf() const noexcept { return node_.is_leaf(); }
  std::size_t index() const noexcept { return index_; }
  NodePtr<K, V, S> node_ptr() const noexcept { return node_; }

  BBTREE_ALWAYS_INLINE std::pair<const K*, V*> key_value() const noexcept {
    if (node_.is_leaf()) {
      const LeafNode leaf = LeafNode::at(node_);
      return {&leaf.key(index_), &leaf.value(index_)};
    }
    return separator_key_value(InternalNode::at(node_), index_);
  }

  BBTREE_ALWAYS_INLINE V& value() const noexcept { return *key_value().second; }

 private:
  NodePtr<K, V, S> node_;
  std::size_t index_;

  Found(NodePtr<K, V, S> node, std::size_t index) noexcept : node_(node), index_(index) {}

  /// Out of line: a search rarely ends at a separator, and the walk down to
  /// its head would otherwise sit in every descent's loop (D41).
  BBTREE_NOINLINE static std::pair<const K*, V*> separator_key_value(InternalNode internal,
                                                                     std::size_t index) noexcept {
    return internal.separator_key_value(index);
  }
};

template <class K, class V, class S>
class MutStack;

/// A tree: its root page, if it has one, which the tree owns with every node
/// under it, and the last leaf an append went to. It has no destructor:
/// dropping a `Tree` leaks it; `destroy` takes it apart (`teardown.hpp`).
///
/// Invariant: every key of the tree shares its shared prefix; every node along
/// its borders has the shared prefix's length as its prefix length.
/// `rightmost_leaf`, if set, is the tree's last leaf; every exclusive use of
/// the tree but the owner's fitting append clears it first.
template <class K, class V, class S>
class Tree {
 public:
  using Shape = NodeShape<K, V, S>;
  using Ptr = NodePtr<K, V, S>;
  using LeafNode = Leaf<K, V, S>;
  using InternalNode = Internal<K, V, S>;
  using AnyNode = Node<K, V, S>;
  using Root = RootPage<K, V, S>;
  using Stack = MutStack<K, V, S>;

  /// No tree: what a new map has, allocating nothing.
  Tree() noexcept = default;

  /// The tree on `root_page`, with every node under it: for a latched tree,
  /// to run one of the tree's operations on the nodes it holds.
  static Tree of_root_page(Root root_page) noexcept {
    Tree tree;
    tree.root_page_ = root_page;
    return tree;
  }

  /// A new tree, on a root page from `pages`: an empty root leaf.
  template <class Pages>
  static Tree make(const Pages& pages) noexcept {
    return of_root_page(Root::make(Root::Page::allocate(pages)));
  }

  /// The tree of `top` and the nodes under it, on plain pages, whose keys
  /// share `shared_prefix`: `top` moves into a root page from `pages`.
  template <class Pages>
  static Tree from_node(AnyNode top, const SharedPrefix& shared_prefix,
                        const Pages& pages) noexcept {
    auto [root, emptied] = Root::from_node(Root::Page::allocate(pages), top, shared_prefix);
    emptied.deallocate(pages);
    return of_root_page(root);
  }

  bool has_root() const noexcept { return !root_page_.is_null(); }

  /// The root page, to read the tree from.
  Root root_page() const noexcept { return root_page_; }

  /// The root page, to change the tree from: it ends the owner's permission
  /// to append to the remembered leaf.
  Root root_page_mut() noexcept {
    forget_rightmost_leaf();
    return root_page_;
  }

  /// The tree, leaving none here.
  Tree take() noexcept {
    Tree taken = *this;
    *this = Tree();
    return taken;
  }

  AnyNode root() const noexcept {
    BBTREE_CHECK(has_root());
    return root_page_.node();
  }

  AnyNode root_mut() noexcept {
    forget_rightmost_leaf();
    return root();
  }

  /// How many edges lead down from the root to each leaf.
  std::size_t leaf_depth() const noexcept {
    AnyNode node = root();
    std::size_t depth = 0;
    while (!node.is_leaf()) {
      const InternalNode internal = node.internal();
      node = internal.child(internal.len());
      ++depth;
    }
    return depth;
  }

  /// What every key of the tree shares (D50).
  SharedPrefix& shared_prefix() const noexcept {
    BBTREE_CHECK(has_root());
    return root_page_.shared_prefix();
  }

  void forget_rightmost_leaf() noexcept { rightmost_leaf_ = Ptr(); }
  bool remembers_rightmost_leaf() const noexcept { return !rightmost_leaf_.is_null(); }

  /// Remembers `leaf`, the tree's physically last leaf, for the next insert
  /// to append to (D45).
  void remember_rightmost_leaf(LeafNode leaf) noexcept { rightmost_leaf_ = leaf.node_ptr(); }

  /// The root page; without a tree, that of a new one from `pages`.
  template <class Pages>
  Root root_page_or_new(const Pages& pages) noexcept {
    if (root_page_.is_null()) root_page_ = Root::make(Root::Page::allocate(pages));
    return root_page_;
  }

  /// Makes the tree a level taller: the root node moves to a page of its
  /// kind, under the root, which is internal now.
  template <class Pages>
  void raise(const Pages& pages) noexcept {
    forget_rightmost_leaf();
    root_page_ = root_page_.raise(pages);
  }

  /// Makes the tree a level shorter: the root's only child moves up into the
  /// root page. Returns the child's emptied page.
  AnyFreshPage lower() noexcept {
    forget_rightmost_leaf();
    auto [root, emptied] = root_page_.lower();
    root_page_ = root;
    return emptied;
  }

  // Lookups (`search.rs`).

  /// The first entry, if the tree has one.
  std::optional<std::pair<const K*, V*>> first_key_value() const noexcept {
    if (!has_root()) return std::nullopt;
    const LeafNode leaf = NodeStack<K, V, S>::template border_leaf<FirstEdge>(root());
    if (leaf.len() == 0) return std::nullopt;
    return std::pair<const K*, V*>{&leaf.key(0), &leaf.value(0)};
  }

  /// The last entry, if the tree has one.
  std::optional<std::pair<const K*, V*>> last_key_value() const noexcept {
    if (!has_root()) return std::nullopt;
    const LeafNode leaf = NodeStack<K, V, S>::template border_leaf<LastEdge>(root());
    if (leaf.len() == 0) return std::nullopt;
    return std::pair<const K*, V*>{&leaf.key(leaf.len() - 1), &leaf.value(leaf.len() - 1)};
  }

  /// The entry whose key equals `key`, if the tree holds one. A key that
  /// lacks what every key shares (D50) is in none of its nodes.
  template <class Q>
  BBTREE_ALWAYS_INLINE std::optional<std::pair<const K*, V*>> find(const Q& key) const noexcept {
    if (!has_root()) return std::nullopt;
    const SearchKey<K, Q> search(key);
    if (root_page_.shared_prefix().place_outside(search)) return std::nullopt;
    if (const std::optional<Found<K, V, S>> found = find_from(root(), search)) {
      return found->key_value();
    }
    return std::nullopt;
  }

  /// Searches down from `node` for the entry whose key equals `key`,
  /// keeping only the current node. `key` must share what every key shares.
  /// `CallSite` compiles the single-key and batch lookups apart (D53).
  template <class CallSite = void, class Q>
  BBTREE_ALWAYS_INLINE static std::optional<Found<K, V, S>> find_from(
      AnyNode node, const SearchKey<K, Q>& key) noexcept {
    Ptr at = node.node_ptr();
    while (!at.is_leaf()) {
      const InternalNode internal = InternalNode::at(at);
      const SearchResult found = internal.search(key);
      if (found.found) return Found<K, V, S>::separator(internal, found.index);
      at = internal.edge(found.index);
    }
    const LeafNode leaf = LeafNode::at(at);
    if (const std::optional<std::size_t> index = leaf.template find<CallSite>(key)) {
      return Found<K, V, S>::in_leaf(leaf, *index);
    }
    return std::nullopt;
  }

  /// What a search for an insertion finds: the existing key, or the leaf and
  /// the edge of it where the key belongs.
  struct InsertSearch {
    std::optional<Found<K, V, S>> found;
    LeafNode leaf;
    std::size_t edge;
  };

  /// Searches for `key` in key order, from `node`. Unlike `find_from`, the
  /// leaf search establishes where an absent key belongs.
  template <class Q>
  BBTREE_ALWAYS_INLINE static InsertSearch search_for_insert(AnyNode node,
                                                             const SearchKey<K, Q>& key) noexcept {
    Ptr at = node.node_ptr();
    while (!at.is_leaf()) {
      const InternalNode internal = InternalNode::at(at);
      const SearchResult found = internal.search(key);
      if (found.found) return {Found<K, V, S>::separator(internal, found.index), LeafNode(), 0};
      at = internal.edge(found.index);
    }
    const LeafNode leaf = LeafNode::at(at);
    const SearchResult found = leaf.search_always_inlined(key);
    if (found.found) return {Found<K, V, S>::in_leaf(leaf, found.index), leaf, found.index};
    return {std::nullopt, leaf, found.index};
  }

  // Inserts (`insert.rs`).

  /// Inserts `key`/`value` and returns `nullopt`, counting the entry in
  /// `length`; or, if an equal key is there, keeps that key, replaces its
  /// value and returns the old one, as `std::map::insert_or_assign` and
  /// Rust's `BTreeMap::insert` do. Pages come from `pages`.
  template <class Pages>
  BBTREE_ALWAYS_INLINE std::optional<V> insert(K&& key, V&& value, const Pages& pages,
                                               std::size_t& length) noexcept {
    if (try_rightmost_append(key, value)) {
      ++length;
      return std::nullopt;
    }
    // Shrink first: the search skips every node's prefix unchecked.
    PrefixedKey<K> prefixed = shrink_shared_prefix_for(std::move(key), pages);
    const InsertSearch search = search_for_insert(root_mut(), SearchKey<K, K>(prefixed.key()));
    if (search.found) {
      V& slot = search.found->value();
      V old = std::move(slot);
      slot = std::move(value);
      return old;
    }
    if (search.leaf.len() < LeafNode::capacity()) {
      search.leaf.insert_fit(search.edge, std::move(prefixed).window_at(search.leaf.prefix_len()),
                             std::move(value));
      ++length;
      return std::nullopt;
    }
    return insert_with_stack(std::move(prefixed), std::move(value), pages, length);
  }

  /// Inserts into a full leaf, which needs a node stack to split. Outlined to
  /// keep the node stack out of fitting inserts; it searches again.
  template <class Pages>
  BBTREE_NOINLINE std::optional<V> insert_with_stack(PrefixedKey<K>&& key, V&& value,
                                                     const Pages& pages,
                                                     std::size_t& length) noexcept;

  /// Appends `key` and `value` to the remembered last leaf, if it has room
  /// and `key` is greater than its last key and shares what every key
  /// shares (D45). Otherwise false, the key and value untouched. Forgets the
  /// leaf before comparing.
  BBTREE_ALWAYS_INLINE bool try_rightmost_append(K& key, V& value) noexcept {
    if (rightmost_leaf_.is_null()) return false;
    const LeafNode leaf = LeafNode::at(rightmost_leaf_);
    rightmost_leaf_ = Ptr();
    const std::size_t count = leaf.len();
    if (count >= LeafNode::capacity() || count == 0) return false;
    if (SearchKey<K, K>(key).cmp_full(leaf.key(count - 1)) != Ordering::Greater) return false;
    // A key that shares less goes the general way, which shrinks the shared
    // prefix for it.
    if (!shared_prefix().is_shared_by(key)) return false;
    leaf.insert_fit(count, WindowedKey<K>::at(std::move(key), leaf.prefix_len()), std::move(value));
    rightmost_leaf_ = leaf.node_ptr();
    return true;
  }

  // The shared prefix (`prefix.rs`).

  /// Shrinks what every key shares to what `key` shares too, before the tree
  /// takes it, and returns `key`. Without a tree, it first makes one. An
  /// empty tree takes `key`'s first bytes as what every key shares.
  template <class Pages>
  BBTREE_ALWAYS_INLINE PrefixedKey<K> shrink_shared_prefix_for(K&& key, const Pages& pages) noexcept {
    const Root root_page = root_page_or_new(pages);
    if constexpr (Shape::has_inline_keys) {
      const AnyNode root = root_page.node();
      if (root.is_leaf() && root.len() == 0) {
        start_shared_prefix(root.leaf(), key);
      } else {
        shrink_shared_prefix_to(root_page.shared_prefix().len_shared_with(key));
      }
    }
    return PrefixedKey<K>::shrunk_for(std::move(key));
  }

  /// Fits the prefix of the node beside each node along the `End` border to
  /// its fences, from the root down: after a border fix.
  template <class End>
  void refit_border_siblings() noexcept;

 private:
  Root root_page_{};
  Ptr rightmost_leaf_{};

  /// Makes `key`'s first bytes what every key shares, for a tree with no keys
  /// yet, and the prefix of `root`, its empty root leaf.
  BBTREE_COLD void start_shared_prefix(LeafNode root, const K& key) noexcept {
    const SharedPrefix shared = SharedPrefix::of_key(key);
    root.set_empty_prefix_len(shared.size());
    shared_prefix() = shared;
  }

  BBTREE_ALWAYS_INLINE void shrink_shared_prefix_to(std::size_t len) noexcept {
    if (len < shared_prefix().size()) shrink_borders_to(len);
  }

  /// Shrinks what every key shares to its first `len` bytes, and the border
  /// nodes' prefixes with it, reading no key.
  BBTREE_COLD void shrink_borders_to(std::size_t len) noexcept {
    const Window fill = shared_prefix().window_at(len);
    shared_prefix().shrink_to(len);
    move_border_back_to<FirstEdge>(fill);
    move_border_back_to<LastEdge>(fill);
  }

  template <class End>
  void move_border_back_to(Window fill) noexcept {
    AnyNode node = root_mut();
    for (;;) {
      if (node.is_leaf()) {
        node.leaf().move_prefix_back_to(fill);
        return;
      }
      const InternalNode internal = node.internal();
      internal.move_prefix_back_to(fill);
      node = internal.child(End::edge(node));
    }
  }
};

/// An exclusive node stack, from the root down: what an entry keeps, and what
/// inserts and removals go back up. It changes the nodes below the root
/// through its bottom node, and the root only by raising or lowering it.
template <class K, class V, class S>
class MutStack : public NodeStack<K, V, S> {
  using Base = NodeStack<K, V, S>;

 public:
  using TreeType = Tree<K, V, S>;
  using LeafNode = Leaf<K, V, S>;
  using InternalNode = Internal<K, V, S>;
  using AnyNode = Node<K, V, S>;
  using Shape = NodeShape<K, V, S>;

  /// A detached node stack down `tree`. It ends the owner's permission to
  /// append to the remembered leaf.
  explicit MutStack(TreeType& tree) noexcept : tree_(&tree) { tree.forget_rightmost_leaf(); }

  TreeType& tree() const noexcept { return *tree_; }

  void start_at_root() noexcept { this->attach(tree_->root()); }

  /// Starts at the root and searches down; see `NodeStack::search`.
  template <class Q>
  bool search(const SearchKey<K, Q>& key) noexcept {
    start_at_root();
    return Base::search(key, tree_->shared_prefix());
  }

  template <class Q>
  void search_to_edge(const SearchKey<K, Q>& key, KeySide side) noexcept {
    start_at_root();
    Base::search_to_edge(key, side, tree_->shared_prefix());
  }

  bool descend_to_first_key() noexcept {
    start_at_root();
    return Base::descend_to_first_key();
  }

  bool descend_to_last_key() noexcept {
    start_at_root();
    return Base::descend_to_last_key();
  }

  /// Starts at the root and goes to the first entry not below `lower`, as a
  /// range from it starts; detached if there is none.
  template <class Q>
  void go_to_first_entry_from(const ::bbtree::Bound<Q>& lower) noexcept {
    start_at_root();
    this->go_to_lower_edge(lower, tree_->shared_prefix());
    go_to_entry_at_slot(this->index() + 1);
  }

  /// Goes up to the bottom node's parent; at the root, first raises the tree
  /// a level, and ends at the new root's only edge.
  template <class Pages>
  InternalNode ascend_raising(const Pages& pages) noexcept {
    if (!this->is_at_root()) return this->ascend();
    tree_->raise(pages);
    this->attach(tree_->root());
    return this->internal_at(0);
  }

  /// The bottom node's fences, low and high: the nearest keys left and right
  /// of the node stack in the nodes above it; null where there is none.
  std::pair<const K*, const K*> fences() const noexcept {
    const K* low = nullptr;
    const K* high = nullptr;
    for (std::size_t depth = this->depth(); depth-- > 0;) {
      const InternalNode node = this->internal_at(depth);
      const std::size_t index = this->index_at(depth);
      if (low == nullptr && index > 0) low = &node.separator_key(index - 1);
      if (high == nullptr && index < node.len()) high = &node.separator_key(index);
      if (low != nullptr && high != nullptr) break;
    }
    return {low, high};
  }

  /// The key the node stack ends at.
  const K& key() const noexcept {
    const AnyNode node = this->node();
    if (node.is_leaf()) return node.leaf().key(this->index());
    return node.internal().separator_key(this->index());
  }

  /// The entry the node stack ends at, as a search finds it.
  Found<K, V, S> found() const noexcept {
    const AnyNode node = this->node();
    BBTREE_CHECK(this->index() < node.len());
    if (node.is_leaf()) return Found<K, V, S>::in_leaf(node.leaf(), this->index());
    return Found<K, V, S>::separator(node.internal(), this->index());
  }

  V& value() const noexcept { return found().value(); }

  /// Puts `key` in place of the key the node stack ends at, and returns the
  /// old key. Keeping the keys in order is the caller's business.
  K replace_key(K&& key) noexcept {
    const AnyNode node = this->node();
    if (node.is_leaf()) {
      const LeafNode leaf = node.leaf();
      return leaf.replace_key(this->index(), WindowedKey<K>::at(std::move(key), leaf.prefix_len()));
    }
    const InternalNode internal = node.internal();
    return internal.replace_key(this->index(),
                                WindowedKey<K>::at(std::move(key), internal.prefix_len()));
  }

  /// Finishes a plain insertion: remembers its leaf when the node stack shows
  /// the new entry is physically last (D45).
  void finish_insertion() noexcept {
    const LeafNode leaf = this->leaf();
    if (this->index() + 1 != leaf.len()) return;
    for (std::size_t depth = 0; depth < this->depth(); ++depth) {
      if (this->index_at(depth) != this->internal_at(depth).len()) return;
    }
    tree_->remember_rightmost_leaf(leaf);
  }

  /// Goes from the bottom node, a leaf, to the entry at `slot` in key order
  /// around it: 0 the separator just left of it, `i + 1` key `i`, `len + 1`
  /// the separator just right. Detached past the tree's last entry.
  void go_to_entry_at_slot(std::size_t slot) noexcept {
    if (slot >= 1 && slot <= this->leaf().len()) {
      this->set_index(slot - 1);
      return;
    }
    while (!this->is_at_root()) {
      const InternalNode node = this->ascend();
      const std::size_t edge = this->index();
      if (slot > 0 && edge < node.len()) return;
      if (slot == 0 && edge > 0) {
        this->set_index(edge - 1);
        return;
      }
    }
    this->detach();
  }

  /// Moves past the entry the node stack ends at, to the next in key order;
  /// detached past the last. Compares no keys.
  void go_past_entry() noexcept {
    const std::size_t index = this->index();
    if (this->ends_at_leaf()) {
      go_to_entry_at_slot(index + 2);
    } else {
      this->set_index(index + 1);
      this->template descend_to_leaf<FirstEdge>();
      go_to_entry_at_slot(1);
    }
  }

  /// Lowers the tree if `fixed_root` and merges left the root internal
  /// without keys, and starts the node stack at the root then.
  template <class Pages>
  bool lower_an_emptied_root(bool fixed_root, const Pages& pages) noexcept {
    if (!fixed_root) return false;
    const AnyNode root = tree_->root();
    if (root.is_leaf() || root.len() != 0) return false;
    this->detach();
    tree_->lower().deallocate(pages);
    start_at_root();
    return true;
  }

  // Inserts (`insert.rs`).

  /// Inserts `key` and `value` at the leaf edge the node stack ends at,
  /// first shrinking the shared prefix for `key`; see `insert_prefixed`.
  template <class Pages>
  void insert(K&& key, V&& value, const Pages& pages, std::size_t& length) noexcept {
    PrefixedKey<K> prefixed = tree_->shrink_shared_prefix_for(std::move(key), pages);
    insert_prefixed(std::move(prefixed), std::move(value), pages, length);
  }

  /// Inserts a key that shares what every key shares at the leaf edge the
  /// node stack ends at, counting it, fitting the prefixes of the nodes that
  /// split, and moving the node stack to the new entry. Compares no keys.
  template <class Pages>
  void insert_prefixed(PrefixedKey<K>&& key, V&& value, const Pages& pages,
                       std::size_t& length) noexcept {
    const std::size_t index = this->index();
    const LeafNode leaf = this->leaf();
    BBTREE_CHECK(index <= leaf.len());
    if (leaf.len() < LeafNode::capacity()) {
      leaf.insert_fit(index, std::move(key).window_at(leaf.prefix_len()), std::move(value));
      ++length;
    } else {
      insert_splitting(std::move(key), std::move(value), pages, length);
    }
  }

  // Prefix fits (`prefix.rs`).

  /// Fits the prefixes of the children at `edges` of the bottom node, an
  /// internal node, to what their fences share.
  template <std::size_t N>
  void refit_children_to_fences(const std::size_t (&edges)[N]) noexcept {
    if constexpr (Shape::has_inline_keys) {
      if (this->internal().has_leaf_children()) {
        refit_children_of_kind<LeafNode>(edges);
      } else {
        refit_children_of_kind<InternalNode>(edges);
      }
    }
  }

  /// Fits the prefixes of the nodes below the bottom node's child at `edge`,
  /// down that child's `End` border, from the leaf up.
  template <class End>
  void refit_border_below(std::size_t edge) noexcept {
    if constexpr (Shape::has_inline_keys) {
      const std::size_t start_depth = this->depth();
      this->set_index(edge);
      this->template descend_to_leaf<End>();
      this->ascend();
      while (this->depth() > start_depth) {
        const std::size_t child[1] = {this->index()};
        refit_children_to_fences(child);
        this->ascend();
      }
    }
  }

  /// Prepares the separator the node stack ends at to give way to its
  /// successor, the first key of its head's leaf: shrinks the prefixes along
  /// the right border under the edge left of it to what they will share with
  /// the successor, from the top down, before anything changes.
  void refit_left_of_separator() noexcept {
    if constexpr (Shape::has_inline_keys) {
      const std::size_t index = this->index();
      const InternalNode node = this->internal();
      const LeafNode head = node.head_leaf(index);
      BBTREE_CHECK(head.len() > 0);
      std::size_t prefix_len =
          KeyInliner<K>::common_prefix_len(node.separator_key(index), head.key(0));
      if (prefix_len < node.prefix_len()) prefix_len = node.prefix_len();
      AnyNode left = node.child(index);
      for (;;) {
        if (left.is_leaf()) {
          left.leaf().shrink_prefix_to(prefix_len);
          return;
        }
        const InternalNode internal = left.internal();
        internal.shrink_prefix_to(prefix_len);
        left = internal.child(internal.len());
      }
    }
  }

  /// Moves the prefix of every node on the node stack back to the root's,
  /// from the root down: for a cut along the node stack (`cut.hpp`).
  void move_prefixes_back_to_root() noexcept {
    if constexpr (Shape::has_inline_keys) {
      for (std::size_t depth = 0; depth < this->depth(); ++depth) {
        const std::optional<Window> fill =
            this->internal_at(depth).fence_window(this->index_at(depth));
        BBTREE_CHECK(fill.has_value());
        const AnyNode child = this->node_at(depth + 1);
        if (child.is_leaf()) {
          child.leaf().move_prefix_back_to(*fill);
        } else {
          child.internal().move_prefix_back_to(*fill);
        }
      }
    }
  }

  // Removal (`remove.hpp`).

  /// Removes the entry the node stack ends at and returns it, counting it out
  /// of `length`; fixes the nodes up the node stack and leaves it detached.
  template <class Pages>
  std::pair<K, V> remove(const Pages& pages, std::size_t& length) noexcept;

  /// Removes the entry the node stack ends at and ends at the entry after
  /// it, wherever the fixing moved it; detached past the last.
  template <class Pages>
  std::pair<K, V> remove_and_go_to_next(const Pages& pages, std::size_t& length) noexcept;

 private:
  TreeType* tree_;

  template <class Pages>
  BBTREE_NOINLINE void insert_splitting(PrefixedKey<K>&& key, V&& value, const Pages& pages,
                                        std::size_t& length) noexcept;

  /// Fits the prefixes of the nodes an insert split to their fences, from the
  /// leaves up. The node stack ends at the new entry before and after.
  void refit_split_halves(const std::size_t (&separator_indexes)[max_depth + 1],
                          std::size_t levels_climbed) noexcept {
    if constexpr (Shape::has_inline_keys) {
      std::size_t indexes[max_depth + 1];
      indexes[0] = this->index();
      for (std::size_t climbed = 1; climbed <= levels_climbed; ++climbed) {
        this->ascend();
        indexes[climbed] = this->index();
        const std::size_t separator_index = separator_indexes[climbed];
        const std::size_t edges[2] = {separator_index, separator_index + 1};
        refit_children_to_fences(edges);
      }
      for (std::size_t climbed = levels_climbed; climbed-- > 0;) {
        this->descend();
        this->set_index(indexes[climbed]);
      }
    }
  }

  template <class C, std::size_t N>
  void refit_children_of_kind(const std::size_t (&edges)[N]) noexcept {
    const std::size_t shared_len = tree_->shared_prefix().size();
    const auto [low, high] = fences();
    const InternalNode node = this->internal();
    for (const std::size_t edge : edges) {
      const C child = C::child_of(node, edge);
      if (child.keys_differ_past_prefix()) continue;
      const K* child_low = edge > 0 ? &node.separator_key(edge - 1) : low;
      const K* child_high = edge < node.len() ? &node.separator_key(edge) : high;
      // A lying encoding may report less than the parent's prefix: keep the
      // parent/child invariant (D44).
      std::size_t prefix_len = KeyInliner<K>::fence_prefix_len(child_low, child_high, shared_len);
      if (prefix_len < node.prefix_len()) prefix_len = node.prefix_len();
      child.refit_prefix(prefix_len);
    }
  }

  // Removal internals, defined in `remove.hpp`.
  struct ChangedFences;
  enum class ChildFix : std::uint8_t;
  template <class Pages>
  std::pair<std::pair<K, V>, ChangedFences> take_out(const Pages& pages, std::size_t& length) noexcept;
  template <class Pages, class AtMustReach>
  ChangedFences fix_up_the_stack(std::size_t next_slot, std::optional<std::size_t> must_reach,
                                 const Pages& pages, AtMustReach at_must_reach) noexcept;
  template <class Pages>
  void lower_and_refit_changed_fences(ChangedFences& changed, const Pages& pages) noexcept;
  template <class C, class Pages>
  static ChildFix fix_underfull_child(InternalNode node, std::size_t index, const Pages& pages,
                                      std::size_t& left_len) noexcept;
};

template <class K, class V, class S>
template <class Pages>
std::optional<V> Tree<K, V, S>::insert_with_stack(PrefixedKey<K>&& key, V&& value,
                                                  const Pages& pages,
                                                  std::size_t& length) noexcept {
  Stack stack(*this);
  if (stack.search(SearchKey<K, K>(key.key()))) {
    V& slot = stack.value();
    V old = std::move(slot);
    slot = std::move(value);
    return old;
  }
  stack.insert_prefixed(std::move(key), std::move(value), pages, length);
  stack.finish_insertion();
  return std::nullopt;
}

template <class K, class V, class S>
template <class Pages>
void MutStack<K, V, S>::insert_splitting(PrefixedKey<K>&& key, V&& value, const Pages& pages,
                                         std::size_t& length) noexcept {
  using LeafSplit = Split<typename LeafNode::Sep, LeafNode>;
  using InternalSplit = Split<typename InternalNode::Sep, InternalNode>;
  const std::size_t index = this->index();
  const LeafNode leaf = this->leaf();
  // The key's window at the leaf's prefix, which both halves keep.
  WindowedKey<K> windowed = std::move(key).window_at(leaf.prefix_len());
  LeafSplit split =
      leaf.split_insert(index, std::move(windowed), std::move(value), LeafNode::Page::allocate(pages));
  // Indexed by how many levels the split has climbed from the leaf: in each
  // node that split, the index leading to the new entry in the half that
  // took it, and above each, where the separator between its halves went.
  std::size_t indexes_in_halves[max_depth + 1];
  std::size_t separator_indexes[max_depth + 1];
  indexes_in_halves[0] = split.inserted.index;
  std::optional<typename InternalNode::Sep> separator(std::move(split.separator));
  AnyNode right(split.right);
  bool entry_is_right = split.inserted.right;
  std::size_t climbed = 0;
  for (;;) {
    const InternalNode node = ascend_raising(pages);
    ++climbed;
    const std::size_t edge = this->index();
    std::optional<InternalSplit> node_split;
    typename InternalNode::Sep carried = std::move(*separator);
    separator.reset();
    if (!node.insert(edge, std::move(carried), right, pages, node_split)) {
      separator_indexes[climbed] = edge;
      this->set_index(edge + (entry_is_right ? 1 : 0));
      break;
    }
    separator_indexes[climbed] = node_split->inserted.index;
    indexes_in_halves[climbed] = node_split->inserted.index + (entry_is_right ? 1 : 0);
    separator.emplace(std::move(node_split->separator));
    right = AnyNode(node_split->right);
    entry_is_right = node_split->inserted.right;
  }
  const std::size_t levels_climbed = climbed;
  // Back down to the new entry, through the halves that took it.
  for (std::size_t level = levels_climbed; level-- > 0;) {
    this->descend();
    this->set_index(indexes_in_halves[level]);
  }
  ++length;
  refit_split_halves(separator_indexes, levels_climbed);
}

template <class K, class V, class S>
template <class End>
void Tree<K, V, S>::refit_border_siblings() noexcept {
  if constexpr (Shape::has_inline_keys) {
    Stack stack(*this);
    stack.start_at_root();
    while (!stack.ends_at_leaf()) {
      const std::size_t len = stack.node().len();
      BBTREE_ASSERT(len > 0);
      const std::size_t sibling[1] = {End::is_last ? len - 1 : 1};
      stack.refit_children_to_fences(sibling);
      stack.set_index(End::edge(stack.node()));
      stack.descend();
    }
  }
}

}  // namespace bbtree::detail
