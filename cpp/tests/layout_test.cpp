// The node layouts, against the numbers `tree/node/layout.rs`'s tests work out
// by hand from D13, D15, D18, D21, D29, D36 and D49.
#include <array>
#include <cstdint>

#include "bbtree/detail/node/plan.hpp"
#include "test_harness.hpp"

using bbtree::detail::KeySearch;
using bbtree::detail::NodeShape;
using bbtree::detail::Strategy;

namespace {

/// A 64-bit key without inline keys.
struct PlainU64 {
  std::uint64_t value;
  bool operator<(const PlainU64& other) const { return value < other.value; }
};

/// A 24-byte key without inline keys, as a `String` without them.
struct Plain24 {
  std::uint64_t words[3];
  bool operator<(const Plain24& other) const { return words[0] < other.words[0]; }
};

struct Unit {};

/// Rust's `String` as the layouts see it: 24 bytes, a byte string that
/// points to its bytes. The numbers below are Rust's, whatever size this
/// standard library gives `std::string` (libc++ 24 bytes, MSVC's 32).
struct RustString {
  const std::uint8_t* bytes;
  std::size_t len;
  std::size_t capacity;
  bool operator<(const RustString& other) const { return len < other.len; }
};

}  // namespace

template <>
struct bbtree::InlineKey<RustString> : bbtree::ByteStringInlineKey<RustString, false> {
  static bbtree::ByteView bytes(const RustString& key) noexcept { return {key.bytes, key.len}; }
};

namespace {

struct ExpectedNode {
  std::size_t size, align, capacity, keys_offset, trailing_offset;

  static ExpectedNode one_page(std::size_t capacity, std::size_t keys_offset,
                               std::size_t trailing_offset) {
    return {1024, 1024, capacity, keys_offset, trailing_offset};
  }
  static ExpectedNode keyless(std::size_t capacity, std::size_t keys_offset,
                              std::size_t trailing_offset) {
    return {512, 512, capacity, keys_offset, trailing_offset};
  }
};

template <class K, class V, class S = Strategy<>>
void check_leaf(ExpectedNode expected) {
  constexpr auto layout = NodeShape<K, V, S>::leaf_layout;
  CHECK_EQ(layout.page.size, expected.size);
  CHECK_EQ(layout.page.align, expected.align);
  CHECK_EQ(layout.keys.capacity, expected.capacity);
  CHECK_EQ(layout.keys.keys_offset, expected.keys_offset);
  CHECK_EQ(layout.head_offset, expected.trailing_offset);
}

template <class K, class V, class S = Strategy<>>
void check_internal(ExpectedNode expected) {
  constexpr auto layout = NodeShape<K, V, S>::internal_layout;
  CHECK_EQ(layout.page.size, expected.size);
  CHECK_EQ(layout.page.align, expected.align);
  CHECK_EQ(layout.keys.capacity, expected.capacity);
  CHECK_EQ(layout.keys.keys_offset, expected.keys_offset);
  CHECK_EQ(layout.edges_offset, expected.trailing_offset);
}

}  // namespace

TEST(u64_to_u64_without_inline_keys_fills_one_page) {
  check_leaf<PlainU64, std::uint64_t>(ExpectedNode::one_page(63, 8, 512));
  check_internal<PlainU64, std::uint64_t>(ExpectedNode::one_page(63, 8, 512));
}

TEST(string_sized_keys_without_inline_keys_fill_one_page) {
  check_leaf<Plain24, std::uint64_t>(ExpectedNode::one_page(31, 8, 752));
  check_internal<Plain24, std::uint64_t>(ExpectedNode::one_page(31, 8, 752));
}

TEST(u64_to_u64_with_two_byte_inline_keys) {
  check_leaf<std::uint64_t, std::uint64_t>(ExpectedNode::one_page(55, 144, 136));
  check_internal<std::uint64_t, std::uint64_t>(ExpectedNode::keyless(49, 106, 112));
  using Shape = NodeShape<std::uint64_t, std::uint64_t, Strategy<>>;
  CHECK_EQ(Shape::leaf_plan.inline_width, 2u);
  CHECK(Shape::leaf_plan.all_keys_search == KeySearch::Branchless);
  CHECK(!Shape::leaf_plan.hash_tags);
}

#if defined(__SIZEOF_INT128__)
TEST(u128_to_u64_with_two_byte_inline_keys) {
  check_leaf<unsigned __int128, std::uint64_t>(ExpectedNode::one_page(28, 96, 80));
  check_internal<unsigned __int128, std::uint64_t>(ExpectedNode::keyless(49, 106, 112));
}
#endif

TEST(set_of_i64_with_two_byte_inline_keys) {
  check_leaf<std::int64_t, Unit>(ExpectedNode::one_page(100, 216, 216));
}

TEST(string_to_u64_with_hash_tags_in_the_padding) {
  using Shape = NodeShape<RustString, std::uint64_t, Strategy<>>;
  check_leaf<RustString, std::uint64_t>(ExpectedNode::one_page(24, 256, 248));
  CHECK_EQ(Shape::leaf_layout.keys.key_stride, 32u);
  CHECK(Shape::leaf_plan.hash_tags);
  CHECK(Shape::leaf_layout.has_hash_tags());
  CHECK_EQ(Shape::leaf_plan.inline_width, 8u);
  // Heap byte strings keep 4 bytes inside (D43).
  check_internal<RustString, std::uint64_t>(ExpectedNode::keyless(41, 172, 176));
  CHECK(Shape::internal_plan.rehydrates_windows);
  CHECK(!Shape::internal_plan.inlines_node_search);
}

TEST(string_to_string_gives_up_an_entry_for_hash_tags) {
  check_leaf<RustString, RustString>(ExpectedNode::one_page(16, 200, 176));
  using Shape = NodeShape<RustString, RustString, Strategy<>>;
  CHECK_EQ(Shape::leaf_layout.keys.key_stride, 48u);
}

TEST(sets_of_strings_have_no_room_for_hash_tags) {
  using Shape = NodeShape<RustString, Unit, Strategy<>>;
  CHECK(!Shape::leaf_plan.hash_tags);
}

TEST(byte_arrays_keep_two_bytes_in_both_kinds) {
  using Key = std::array<std::uint8_t, 32>;
  using Shape = NodeShape<Key, std::uint64_t, Strategy<>>;
  CHECK_EQ(Shape::leaf_plan.inline_width, 2u);
  CHECK_EQ(Shape::internal_plan.inline_width, 2u);
  CHECK(Shape::leaf_plan.whole_key_equality);
  CHECK(Shape::leaf_plan.candidate_search == KeySearch::EarlyExit);
  check_internal<Key, std::uint64_t>(ExpectedNode::keyless(49, 106, 112));
}

TEST(latched_string_leaves_give_up_an_entry_for_hash_tags) {
  using Shape = NodeShape<RustString, std::uint64_t, Strategy<bbtree::DefaultNodeConfig,
                                                               bbtree::NoLeafAnnex, true>>;
  CHECK(Shape::leaf_plan.hash_tags);
  CHECK_EQ(Shape::leaf_layout.keys.capacity, 23u);
  CHECK_EQ(Shape::leaf_layout.keys.inline_keys_offset, 16u);
}

TEST_MAIN()
