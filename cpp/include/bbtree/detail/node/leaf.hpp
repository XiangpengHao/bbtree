// Leaves: the nodes that hold the entries (`tree/node/leaf.rs`). A leaf's
// page holds its keys and values, paired with inline keys (D29), the inline
// keys a search runs over first (D18), and in a leaf of heap byte strings the
// keys' hash tags (D36). It also holds a head: the value of the separator just
// left of the leaf, and with inline keys that separator's key (D17, D49).
// Every leaf but the tree's leftmost has one.
//
// `Leaf` is a handle with no access of its own, as every node handle: the
// operation holding it keeps the contracts of `tree/node/mod.rs`.
#pragma once

#include <cstddef>
#include <optional>
#include <utility>

#include "../config.hpp"
#include "child.hpp"
#include "inline_key_search.hpp"
#include "inline_keys.hpp"
#include "page.hpp"
#include "slots.hpp"

namespace bbtree::detail {

template <class K, class V, class S>
class Internal;

template <class K, class V, class S, class C>
class Siblings;

/// Where an entry's key and value are, for a walk to hand out as its borrow
/// allows: a leaf's key and the value beside it, or a separator's key and its
/// head's value.
template <class K, class V>
struct EntrySlots {
  K* key;
  V* value;
};

/// What an inline-key search alone tells a batch about a key: that the leaf
/// lacks it, the one candidate entry, or nothing.
struct CandidateLine {
  enum class Kind : std::uint8_t { Absent, One, Unknown };
  Kind kind;
  std::size_t index;
  const void* line;
};

/// What the leaf right of a separator being removed gives up and hands up
/// (D17): its head's value, with inline keys its key, and its first key as
/// the separator that takes the removed one's place.
template <class K, class V, class Sep>
struct HeadSuccession {
  std::optional<K> removed_key;
  V value;
  Sep successor;
};

template <class K, class V, class S>
class Leaf : public ChildBase<Leaf<K, V, S>, K, KindShape<K, V, S, true>> {
  using Base = ChildBase<Leaf<K, V, S>, K, KindShape<K, V, S, true>>;
  friend Base;

 public:
  using Kind = KindShape<K, V, S, true>;
  using Shape = NodeShape<K, V, S>;
  using Ptr = NodePtr<K, V, S>;
  using Page = FreshPage<K, V, S, PageKind::Leaf>;
  using Sep = Separator<K, !Shape::has_inline_keys>;
  using Windowed = WindowedKey<K>;
  static constexpr bool has_inline_keys = Shape::has_inline_keys;

  Leaf() noexcept = default;

  /// The leaf at `node`, which must hold one.
  static Leaf at(Ptr node) noexcept {
    BBTREE_ASSERT(node.is_leaf());
    return Leaf(node);
  }

  /// A new, empty leaf in `page`, without a head, at `prefix_len`.
  static Leaf make(Page&& page, std::size_t prefix_len) noexcept {
    const Leaf leaf(Ptr::template initialize<Kind>(std::move(page).into_raw()));
    leaf.set_empty_prefix_len(prefix_len);
    return leaf;
  }

  /// Gives the leaf's page back. Every key, value and head has moved out, and
  /// nothing uses the leaf again.
  Page free() const noexcept { return Page(node_.page()); }

  Ptr node_ptr() const noexcept { return node_; }
  bool operator==(Leaf other) const noexcept { return node_ == other.node_; }
  std::size_t len() const noexcept { return node_.len(); }
  bool has_head() const noexcept { return node_.has_head(); }
  std::size_t prefix_len() const noexcept { return node_.prefix_len(); }

  bool is_underfull() const noexcept { return len() < Kind::min_len; }
  bool keeps_its_minimum() const noexcept { return len() > Kind::min_len; }

  /// The header's line, for a batch's next leaf search.
  const void* search_line() const noexcept { return node_.page(); }
  /// Key `index`'s line, which a batch's pending shift reads.
  const void* entry_line(std::size_t index) const noexcept { return &key(index); }

  // Reading entries.

  NodeKeys<K> keys() const noexcept {
    return NodeKeys<K>(node_.page() + Kind::keys.keys_offset, Kind::keys.key_stride, len());
  }

  const K& key(std::size_t index) const noexcept {
    BBTREE_ASSERT(index < len());
    return *key_slots().keys().slot(index);
  }
  K& key_mut(std::size_t index) const noexcept {
    BBTREE_ASSERT(index < len());
    return *key_slots().keys().slot(index);
  }

  V& value(std::size_t index) const noexcept {
    BBTREE_ASSERT(index < len());
    return *node_.value_slots().slot(index);
  }

  EntrySlots<K, V> entry_slots(std::size_t index) const noexcept {
    BBTREE_ASSERT(index < len());
    return {key_slots().keys().slot(index), node_.value_slots().slot(index)};
  }

  /// The keys' hash tags, if the plan keeps them: every slot up to capacity.
  const std::uint8_t* hash_tags() const noexcept {
    static_assert(Kind::has_hash_tags);
    return this->columns().hash_tag_slots();
  }

  /// Key `index`'s window at the leaf's prefix length.
  Window window(std::size_t index) const noexcept {
    BBTREE_CHECK(index < len());
    if constexpr (has_inline_keys) {
      return Window(prefix_len(), this->inline_keys().inline_key(index));
    } else {
      return Window::none();
    }
  }

  // Searching.

  /// `key` searched for among the leaf's keys: the key equal to it, or the
  /// edge where it belongs; over the inline keys first, if any (D18). In
  /// bounds for any `operator<` and inline keys.
  template <class Q>
  BBTREE_ALWAYS_INLINE SearchResult search(const SearchKey<K, Q>& key) const noexcept {
    if constexpr (Kind::plan.inlines_node_search) {
      return search_always_inlined(key);
    } else {
      return search_maybe_inlined(key);
    }
  }

  /// `search`, inlined or not as the compiler decides (D41).
  template <class Q>
  SearchResult search_maybe_inlined(const SearchKey<K, Q>& key) const noexcept {
    return search_always_inlined(key);
  }

  /// `search`, always inlined: an insert's search uses it (D41).
  template <class Q>
  BBTREE_ALWAYS_INLINE SearchResult search_always_inlined(const SearchKey<K, Q>& key) const noexcept {
    if constexpr (has_inline_keys && SearchKey<K, Q>::has_inline_target) {
      const Window target = key.inline_target(prefix_len());
      const IndexRange candidates = this->inline_keys().equal_range(target.inline_key());
      // Most keys an insert searches for are absent: they stop here.
      if (candidates.empty()) return SearchResult::edge(candidates.start);
      return search_candidates(candidates, key, target);
    } else {
      return search_all_keys(key);
    }
  }

  /// The index of the key equal to `key`, if the leaf holds one. As an
  /// exact-key lookup it may reject candidates of other lengths (D36), and
  /// confirm a lone candidate by equality alone (D38). `CallSite` compiles
  /// the single-key and batch lookups apart (D53).
  template <class CallSite, class Q>
  std::optional<std::size_t> find(const SearchKey<K, Q>& key) const noexcept {
    if constexpr (has_inline_keys && SearchKey<K, Q>::has_inline_target) {
      const Window target = key.inline_target(prefix_len());
      const auto inline_keys = this->inline_keys();
      IndexRange candidates;
      if (auto mask = inline_keys.equality_mask(target.inline_key())) {
        auto range = mask->equal_range(len());
        if (!range) return std::nullopt;
        candidates = *range;
      } else {
        candidates = inline_keys.equal_range(target.inline_key());
      }
      if (candidates.empty()) return std::nullopt;
      if (candidates.size() == 1) {
        if (matches(key, target, keys()[candidates.start])) return candidates.start;
        return std::nullopt;
      }
      if constexpr (Kind::has_hash_tags && SearchKey<K, Q>::has_hash_tag) {
        const SlotMatches tagged =
            HashTags<Kind::capacity>(hash_tags()).matching(candidates, key.hash_tag());
        const NodeKeys<K> all = keys();
        switch (tagged.kind) {
          case SlotMatches::Kind::None:
            return std::nullopt;
          case SlotMatches::Kind::One:
            if (key.template cmp_tied<Kind::width>(target, all[tagged.lower]) == Ordering::Equal) {
              return tagged.lower;
            }
            return std::nullopt;
          case SlotMatches::Kind::Two:
            // The upper key is the midpoint of a binary search over the two:
            // the other candidates' keys are never read.
            switch (key.template cmp_tied<Kind::width>(target, all[tagged.upper])) {
              case Ordering::Equal: return tagged.upper;
              case Ordering::Less:
                if (key.template cmp_tied<Kind::width>(target, all[tagged.lower]) ==
                    Ordering::Equal) {
                  return tagged.lower;
                }
                return std::nullopt;
              default: return std::nullopt;
            }
          case SlotMatches::Kind::Many:
            break;
        }
      }
      const SearchResult found = search_candidates(candidates, key, target);
      if (found.found) return found.index;
      return std::nullopt;
    } else {
      const SearchResult found = search(key);
      if (found.found) return found.index;
      return std::nullopt;
    }
  }

  /// What an inline-key search alone tells a batch about `key`.
  template <class Q>
  BBTREE_ALWAYS_INLINE CandidateLine candidate_line(const SearchKey<K, Q>& key) const noexcept {
    if constexpr (has_inline_keys && SearchKey<K, Q>::has_inline_target) {
      return candidate_line_among(key.inline_target(prefix_len()), this->inline_keys(), len());
    } else {
      return CandidateLine{CandidateLine::Kind::Unknown, 0, nullptr};
    }
  }

  /// `candidate_line` for `target` among `inline_keys`, `len` live.
  BBTREE_ALWAYS_INLINE CandidateLine candidate_line_among(Window target,
                                                         typename Kind::InlineKeys inline_keys,
                                                         std::size_t live) const noexcept {
    std::size_t index;
    if (auto mask = inline_keys.equality_mask(target.inline_key())) {
      const SlotMatches unique = mask->unique(live);
      if (unique.kind == SlotMatches::Kind::None) return {CandidateLine::Kind::Absent, 0, nullptr};
      if (unique.kind != SlotMatches::Kind::One) return {CandidateLine::Kind::Unknown, 0, nullptr};
      index = unique.lower;
    } else {
      const IndexRange candidates = inline_keys.equal_range(target.inline_key());
      if (candidates.empty()) return {CandidateLine::Kind::Absent, 0, nullptr};
      if (candidates.size() != 1) return {CandidateLine::Kind::Unknown, 0, nullptr};
      index = candidates.start;
    }
    return {CandidateLine::Kind::One, index, key_slots().keys().slot(index)};
  }

  /// The first heap line a unique candidate's comparison reads.
  template <class Q>
  const void* candidate_bytes_line(const SearchKey<K, Q>& key, std::size_t index,
                                   std::size_t saved_prefix_len) const noexcept {
    return key.template tied_bytes_line<Kind::width>(saved_prefix_len,
                                                     *key_slots().keys().slot(index));
  }

  /// Confirms the unique candidate a batch's earlier mask step kept, at the
  /// prefix length it saved.
  template <class Q>
  std::optional<std::size_t> find_candidate(const SearchKey<K, Q>& key, std::size_t index,
                                            std::size_t saved_prefix_len) const noexcept {
    if constexpr (has_inline_keys && SearchKey<K, Q>::has_inline_target) {
      const Window target = key.inline_target(saved_prefix_len);
      if (matches(key, target, *key_slots().keys().slot(index))) return index;
    }
    return std::nullopt;
  }

  // The head: the entry of the separator just left of the leaf (D17, D49).

  V& head_value() const noexcept {
    BBTREE_CHECK(has_head());
    return *node_.head_and_value_slots().slot(0);
  }

  V* head_value_slot() const noexcept {
    BBTREE_CHECK(has_head());
    return node_.head_and_value_slots().slot(0);
  }

  /// The head's key, which it has only with inline keys (D49).
  const K& head_key() const noexcept {
    BBTREE_CHECK(has_head());
    return *node_.head_key_slot();
  }

  K* head_key_slot() const noexcept { return node_.head_key_slot(); }

  /// Sets a headless leaf's head to `value`, and its key with inline keys.
  void put_head(K* key, V&& value) const noexcept {
    BBTREE_CHECK(!has_head());
    BBTREE_CHECK((key != nullptr) == has_inline_keys);
    if constexpr (has_inline_keys) ::new (static_cast<void*>(node_.head_key_slot())) K(std::move(*key));
    node_.head_and_value_slots().put(0, std::move(value));
    node_.set_has_head(true);
  }

  /// Makes `separator`, the separator just left of this headless leaf, its
  /// head; returns the separator as its internal node keeps it.
  Sep put_separator_head(Windowed&& separator, V&& value) const noexcept {
    if constexpr (has_inline_keys) {
      const Window window = separator.window();
      put_head(&separator.key(), std::move(value));
      return Sep(window);
    } else {
      put_head(nullptr, std::move(value));
      return Sep(std::move(separator));
    }
  }

  /// Takes the head out of the leaf, which then has none.
  std::pair<std::optional<K>, V> take_head() const noexcept {
    BBTREE_CHECK(has_head());
    std::optional<K> key;
    if constexpr (has_inline_keys) {
      K* slot = node_.head_key_slot();
      key.emplace(std::move(*slot));
      slot->~K();
    }
    V value = node_.head_and_value_slots().take(0);
    node_.set_has_head(false);
    return {std::move(key), std::move(value)};
  }

  /// Replaces the head's key with `key` and returns the old one (D49).
  K replace_head_key(K&& key) const noexcept {
    BBTREE_CHECK(has_head());
    K* slot = node_.head_key_slot();
    K old(std::move(*slot));
    slot->~K();
    ::new (static_cast<void*>(slot)) K(std::move(key));
    return old;
  }

  /// How the leaf right of a separator being removed gives up its head and
  /// hands up its first key to take the separator's place (D17); `nullopt`,
  /// changing nothing, if the leaf has no keys.
  std::optional<HeadSuccession<K, V, Sep>> take_head_and_first_key() const noexcept {
    BBTREE_CHECK(has_head());
    const std::size_t count = len();
    if (count == 0) return std::nullopt;
    Windowed first_key = key_slots().remove(0, count);
    V head = node_.head_and_value_slots().remove(0, count + 1);
    std::optional<HeadSuccession<K, V, Sep>> succession;
    if constexpr (has_inline_keys) {
      const Window window = first_key.window();
      K removed = replace_head_key(std::move(first_key.key()));
      succession.emplace(HeadSuccession<K, V, Sep>{std::optional<K>(std::move(removed)),
                                                   std::move(head), Sep(window)});
    } else {
      succession.emplace(
          HeadSuccession<K, V, Sep>{std::nullopt, std::move(head), Sep(std::move(first_key))});
    }
    set_len(count - 1);
    return succession;
  }

  // Changing entries.

  /// Inserts `key` and `value` at edge `index`, shifting later entries right.
  /// The leaf has room: `len < capacity`, and `index <= len`.
  inline void insert_fit(std::size_t index, Windowed&& key, V&& value) const noexcept {
    const std::size_t count = len();
    BBTREE_ASSERT(index <= count && count < Kind::capacity);
    if constexpr (Shape::paired_leaf_entries) {
      key_slots().insert_shifting_entries(index, count, std::move(key));
      node_.value_slots().put(index, std::move(value));
    } else {
      key_slots().insert(index, count, std::move(key));
      node_.value_slots().insert(index, count, std::move(value));
    }
    // The slots past it already hold the greatest inline key.
    node_.set_len(count + 1);
  }

  /// Splits the full leaf and inserts `key`/`value` at edge `index`, on the
  /// side where that edge ended up. The separator goes up, and its entry
  /// becomes the right leaf's head (D17, D49).
  Split<Sep, Leaf> split_insert(std::size_t index, Windowed&& key, V&& value,
                                Page&& right_page) const noexcept {
    BBTREE_ASSERT(len() == Kind::capacity);
    InsertionSide inserted;
    const std::size_t separator_index =
        InsertionSide::of_split(Kind::capacity, index, Kind::min_len, inserted);
    const Leaf right = Leaf::make(std::move(right_page), prefix_len());
    Sep separator = split_around(separator_index, right);
    if (inserted.right) {
      right.insert_fit(inserted.index, std::move(key), std::move(value));
    } else {
      insert_fit(inserted.index, std::move(key), std::move(value));
    }
    return Split<Sep, Leaf>{std::move(separator), right, inserted};
  }

  /// Removes key/value `index`, shifting later ones left, and returns it.
  std::pair<K, V> remove(std::size_t index) const noexcept {
    const std::size_t count = len();
    BBTREE_CHECK(index < count);
    if constexpr (Shape::paired_leaf_entries) {
      V value = node_.value_slots().take(index);
      K key = key_slots().remove_shifting_entries(index, count).into_key();
      set_len(count - 1);
      return {std::move(key), std::move(value)};
    } else {
      K key = key_slots().remove(index, count).into_key();
      V value = node_.value_slots().remove(index, count);
      set_len(count - 1);
      return {std::move(key), std::move(value)};
    }
  }

  /// Replaces key `index` and its inline key, returning the old key.
  K replace_key(std::size_t index, Windowed&& key) const noexcept {
    BBTREE_CHECK(index < len());
    return key_slots().replace(index, std::move(key)).into_key();
  }

  /// Cuts the leaf at edge `index`: moves the entries from `index` on into a
  /// new leaf in `page`, without a head, and returns it.
  Leaf cut_off_at(std::size_t index, Page&& page) const noexcept {
    const std::size_t count = len();
    BBTREE_CHECK(index <= count);
    const std::size_t moved = count - index;
    const Leaf right = Leaf::make(std::move(page), prefix_len());
    key_slots().move_run_to(index, moved, right.key_slots(), 0);
    node_.value_slots().move_run_to(index, moved, right.node_.value_slots(), 0);
    right.set_len(moved);
    set_len(index);
    return right;
  }

  // Balancing with a sibling (`Child`).

  /// The child at edge `index` of `parent`, a leaf.
  static Leaf child_of(Internal<K, V, S> parent, std::size_t index) noexcept {
    return parent.leaf_child(index);
  }

  bool has_first_trailing_slot() const noexcept { return has_head(); }

  /// As far as its fences share: nothing below a leaf bounds it.
  std::size_t allowed_prefix_len(std::size_t prefix_len) const noexcept { return prefix_len; }

  AnyFreshPage free_page() const noexcept {
    return AnyFreshPage{PageKind::Leaf, node_.page(), Kind::page};
  }

  /// Moves the left leaf's last entry to the right leaf's front, through the
  /// parent's separator and the right leaf's head. The left leaf has an
  /// entry, the right one room, at `fill`'s prefix length.
  static void steal_left_unchecked(const Siblings<K, V, S, Leaf>& siblings, Window fill) noexcept {
    const Internal<K, V, S> parent = siblings.parent();
    const std::size_t index = siblings.index();
    const Leaf left = siblings.left(), right = siblings.right();
    const std::size_t left_len = left.len(), right_len = right.len();
    Windowed key = left.key_slots().remove(left_len - 1, left_len);
    V value = left.node_.head_and_value_slots().remove(left_len, left_len + 1);
    right.node_.head_and_value_slots().insert(0, right_len + 1, std::move(value));
    Windowed separator = right.replace_separator_key(parent, index, std::move(key), fill);
    right.key_slots().insert(0, right_len, std::move(separator));
    left.set_len(left_len - 1);
    right.set_len(right_len + 1);
  }

  /// Moves the right leaf's first entry to the left leaf's end, mirrored.
  static void steal_right_unchecked(const Siblings<K, V, S, Leaf>& siblings, Window fill) noexcept {
    const Internal<K, V, S> parent = siblings.parent();
    const std::size_t index = siblings.index();
    const Leaf left = siblings.left(), right = siblings.right();
    const std::size_t left_len = left.len(), right_len = right.len();
    Windowed key = right.key_slots().remove(0, right_len);
    V value = right.node_.head_and_value_slots().remove(0, right_len + 1);
    left.node_.head_and_value_slots().insert(left_len + 1, left_len + 1, std::move(value));
    Windowed separator = right.replace_separator_key(parent, index, std::move(key), fill);
    left.key_slots().insert(left_len, left_len, std::move(separator));
    left.set_len(left_len + 1);
    right.set_len(right_len - 1);
  }

  /// Puts the separator at position `len` of `left`, and every key and value
  /// of `right` after it; `right`'s head value joins the separator's key as
  /// an entry. Both are at `fill`'s prefix length; they fit in one node.
  static void append(Leaf left, Window fill, Sep&& separator, Leaf right) noexcept {
    const std::size_t left_len = left.len(), right_len = right.len();
    if constexpr (has_inline_keys) {
      K* head_key = right.node_.head_key_slot();
      Windowed key(std::move(*head_key), fill);
      head_key->~K();
      left.key_slots().write(left_len, std::move(key));
    } else {
      left.key_slots().write(left_len, std::move(separator).into_windowed_key());
    }
    right.key_slots().move_run_to(0, right_len, left.key_slots(), left_len + 1);
    right.node_.head_and_value_slots().move_run_to(0, right_len + 1,
                                                    left.node_.head_and_value_slots(), left_len + 1);
    left.set_len(left_len + 1 + right_len);
    left.merge_annex(right);
  }

  /// The leaf's annex (leaf_annex.rs).
  typename S::annex& annex() const noexcept { return *node_.leaf_annex(); }

  /// The leaf split into itself and `right`: their annexes split.
  void split_annex(Leaf right) const noexcept {
    if constexpr (!std::is_empty_v<typename S::annex>) annex().split(right.annex());
  }

  /// `right` merged into this leaf, and goes: their annexes merge.
  void merge_annex(Leaf right) const noexcept {
    if constexpr (!std::is_empty_v<typename S::annex>) {
      annex().merge(std::move(right.annex()));
      right.annex().~annex_type();
    }
  }

  using annex_type = typename S::annex;

  /// The leaf goes with the map: its annex gives what it holds back to
  /// `alloc`, the map's allocator, and is destroyed.
  template <class Alloc>
  void release_annex(Alloc& alloc) const noexcept {
    if constexpr (!std::is_empty_v<annex_type>) {
      annex().release(alloc);
      annex().~annex_type();
    }
  }

  KeySlots<K, Kind> key_slots() const noexcept { return node_.template key_slots<Kind>(); }

  /// # Contract: keys and values `0..len` are initialized at this call.
  void set_len(std::size_t new_len) const noexcept {
    node_.template set_len_refilling<Kind>(new_len);
  }

 private:
  Ptr node_;

  explicit Leaf(Ptr node) noexcept : node_(node) {}

  /// Moves the entries right of `separator_index` into `right`, a fresh leaf
  /// at this leaf's prefix length. Takes out key `separator_index`, whose
  /// entry becomes `right`'s head (D49), and returns it as the parent's
  /// separator. Then the leaves' annexes split.
  Sep split_around(std::size_t separator_index, Leaf right) const noexcept {
    const std::size_t count = len();
    BBTREE_ASSERT(separator_index < count);
    const std::size_t right_len = count - separator_index - 1;
    key_slots().move_run_to(separator_index + 1, right_len, right.key_slots(), 0);
    node_.head_and_value_slots().move_run_to(separator_index + 1, right_len + 1,
                                             right.node_.head_and_value_slots(), 0);
    Windowed key = key_slots().read(separator_index);
    std::optional<Sep> separator;
    if constexpr (has_inline_keys) {
      ::new (static_cast<void*>(right.node_.head_key_slot())) K(std::move(key.key()));
      separator.emplace(key.window());
    } else {
      separator.emplace(std::move(key));
    }
    right.node_.set_has_head(true);
    right.set_len(right_len);
    set_len(separator_index);
    split_annex(right);
    return std::move(*separator);
  }

  /// Whether the lone candidate `stored` is `key`: all of its bytes at once
  /// for byte arrays (D38), past the tie otherwise.
  template <class Q>
  BBTREE_ALWAYS_INLINE bool matches(const SearchKey<K, Q>& key, Window target,
                                    const K& stored) const noexcept {
    if constexpr (Kind::plan.whole_key_equality && SearchKey<K, Q>::by_bytes) {
      return key.equals_whole_bytes(stored);
    } else {
      return key.template cmp_tied<Kind::width>(target, stored) == Ordering::Equal;
    }
  }

  /// The search among `candidates`, the keys whose inline keys equal
  /// `key`'s: the others differ in their inline keys, which order them.
  template <class Q>
  SearchResult search_candidates(IndexRange candidates, const SearchKey<K, Q>& key,
                                 Window target) const noexcept {
    const NodeKeys<K> tied = keys().slice(candidates.start, candidates.end);
    return KeySearcher::search<Kind::plan.candidate_search>(
               tied, key.key(),
               [&](const K& stored) { return key.template cmp_tied<Kind::width>(target, stored); })
        .offset_by(candidates.start);
  }

  /// The search over every key, without an inline key to narrow with.
  template <class Q>
  BBTREE_ALWAYS_INLINE SearchResult search_all_keys(const SearchKey<K, Q>& key) const noexcept {
    return KeySearcher::search<Kind::plan.all_keys_search>(
        keys(), key.key(), [&](const K& stored) { return key.cmp_full(stored); });
  }

  /// Puts `key`, leaving a sibling, in place of the key of the parent's
  /// separator `index`, and returns the old key with `fill` as its window.
  /// This leaf is the leftmost leaf right of that separator, and has a head.
  Windowed replace_separator_key(Internal<K, V, S> parent, std::size_t index, Windowed&& key,
                                 Window fill) const noexcept {
    if constexpr (has_inline_keys) {
      const Window window = key.window();
      K old_head_key = replace_head_key(std::move(key.key()));
      parent.replace_separator(index, Sep(window.moved_back_to(fill)));
      return Windowed(std::move(old_head_key), fill);
    } else {
      return parent.replace_separator(index, Sep(std::move(key).moved_back_to(fill)))
          .into_windowed_key();
    }
  }
};

}  // namespace bbtree::detail
