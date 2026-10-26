// The root page (`tree/node/root_page.rs`): the root node, of either kind, in
// room for the larger, and after it the root area, which holds what every key
// shares (D50). A tree keeps its root page for life; the root node's kind
// changes in place (D52). Raising the tree moves the root node to a fresh
// page of its kind and makes the root internal above it; lowering it moves
// the root's only child up and frees the child's page. The tree's pointer to
// its root page is the root's edge: its tag tells the root's kind.
#pragma once

#include <cstddef>
#include <cstdint>
#include <cstring>
#include <optional>
#include <utility>

#include "../config.hpp"
#include "internal.hpp"
#include "page.hpp"
#include "shared_prefix.hpp"

namespace shoal::detail {

template <class K, class V, class S>
class RootPage {
 public:
  using Shape = NodeShape<K, V, S>;
  using Ptr = NodePtr<K, V, S>;
  using LeafNode = Leaf<K, V, S>;
  using InternalNode = Internal<K, V, S>;
  using AnyNode = Node<K, V, S>;
  using Page = FreshPage<K, V, S, PageKind::Root>;

  RootPage() noexcept = default;

  /// A new tree's root page, in `page`: an empty leaf, sharing nothing yet.
  static RootPage make(Page&& page) noexcept {
    std::byte* raw = std::move(page).into_raw();
    const RootPage root(Ptr::template initialize<typename LeafNode::Kind>(raw));
    ::new (static_cast<void*>(root.area())) SharedPrefix(SharedPrefix::none());
    return root;
  }

  /// Makes `page` the root page for `top`, the top node of a tree made on
  /// plain pages whose keys share `shared_prefix`, moving `top` there.
  /// Returns it, with `top`'s emptied page.
  static std::pair<RootPage, AnyFreshPage> from_node(Page&& page, AnyNode top,
                                                     const SharedPrefix& shared_prefix) noexcept {
    std::byte* raw = std::move(page).into_raw();
    auto [node, emptied] = move_in(raw, top);
    const RootPage root(node);
    ::new (static_cast<void*>(root.area())) SharedPrefix(shared_prefix);
    return {root, emptied};
  }

  /// The root page `node`, a tree's root node, is in.
  static RootPage of_node(AnyNode node) noexcept { return RootPage(node.node_ptr()); }
  static RootPage of_ptr(Ptr node) noexcept { return RootPage(node); }

  /// Where the root area starts: past room for a node of either kind.
  static constexpr std::size_t area_offset() noexcept { return Shape::root_area_offset; }

  bool is_null() const noexcept { return node_.is_null(); }
  Ptr node_ptr() const noexcept { return node_; }

  /// The root node, of the kind the tag tells.
  AnyNode node() const noexcept { return AnyNode(node_); }

  /// What every key of the tree shares (D50).
  SharedPrefix& shared_prefix() const noexcept { return *area(); }

  /// The shared prefix of a latched tree, read while a thread holding the
  /// root may be changing it: a copy, which the root's version vouches for
  /// once checked. `nullopt` if its length, read first, is 0.
  SHOAL_RACING_READ std::optional<SharedPrefix> load_racing_shared_prefix() const noexcept {
    SharedPrefix* shared = area();
    if (RacingLoad::relaxed(&shared->len) == 0) return std::nullopt;
    SharedPrefix copy;
    std::memcpy(&copy, shared, sizeof(copy));
    return copy;
  }

  /// Where `key` sorts against every key of the tree, read in place while
  /// a thread holding the root may be changing the prefix: `place_outside`
  /// without the 65-byte copy above. The bytes read may be mid-change;
  /// the caller's version check vouches for them, as it does for a copy.
  template <class Q>
  SHOAL_RACING_READ std::optional<Ordering> place_outside_racing(
      const SearchKey<K, Q>& key) const noexcept {
    SharedPrefix* shared = area();
    const std::size_t len = RacingLoad::relaxed(&shared->len);
    if (len == 0) return std::nullopt;
    const std::optional<Ordering> order = key.cmp_shared_prefix(shared->bytes, len);
    if (order && *order == Ordering::Equal) return std::nullopt;
    return order;
  }

  /// Makes the tree a level taller: moves the root node to a fresh page of
  /// its kind and makes the root internal above it, at the tree's shared
  /// prefix length (D50). Returns the page's new handle.
  template <class Pages>
  RootPage raise(const Pages& pages) const noexcept {
    const AnyNode child = move_node_out(pages);
    const Ptr node = Ptr::template initialize<typename InternalNode::Kind>(node_.page());
    const InternalNode root = InternalNode::at(node);
    root.put_only_child(child);
    root.set_empty_prefix_len(shared_prefix().size());
    return RootPage(node);
  }

  /// Makes the tree a level shorter: moves the root's only child up into the
  /// root page. Returns the page's new handle and the child's emptied page.
  std::pair<RootPage, AnyFreshPage> lower() const noexcept {
    SHOAL_CHECK(!node_.is_leaf());
    const InternalNode root = InternalNode::at(node_);
    SHOAL_ASSERT(root.len() == 0);
    auto [node, emptied] = move_in(node_.page(), root.child(0));
    return {RootPage(node), emptied};
  }

  /// Gives the page back. Every key, value and edge of the root node has
  /// moved out or been freed.
  Page free() const noexcept { return Page(node_.page()); }

 private:
  Ptr node_{};

  explicit RootPage(Ptr node) noexcept : node_(node) {}

  SharedPrefix* area() const noexcept {
    return std::launder(reinterpret_cast<SharedPrefix*>(node_.page() + area_offset()));
  }

  /// Moves `node`, on a plain page of its kind, whole into the root page at
  /// `page`: returns the node there, and `node`'s emptied page.
  static std::pair<Ptr, AnyFreshPage> move_in(std::byte* page, AnyNode node) noexcept {
    Ptr moved;
    if (node.is_leaf()) {
      moved = node.node_ptr().template move_node_to<typename LeafNode::Kind>(page);
    } else {
      moved = node.node_ptr().template move_node_to<typename InternalNode::Kind>(page);
    }
    return {moved, node.free()};
  }

  /// Moves the root node to a fresh page of its kind, and returns it there.
  template <class Pages>
  AnyNode move_node_out(const Pages& pages) const noexcept {
    if (node_.is_leaf()) {
      std::byte* page = std::move(LeafNode::Page::allocate(pages)).into_raw();
      return AnyNode(node_.template move_node_to<typename LeafNode::Kind>(page));
    }
    std::byte* page = std::move(InternalNode::Page::allocate(pages)).into_raw();
    return AnyNode(node_.template move_node_to<typename InternalNode::Kind>(page));
  }
};

}  // namespace shoal::detail
