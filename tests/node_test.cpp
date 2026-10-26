// Single nodes: a leaf's inserts, searches, splits and heads, against a
// sorted vector; windows moving back; the inline keys' encodings.
#include <algorithm>
#include <cstdint>
#include <string>
#include <vector>

#include "shoal/allocator.hpp"
#include "shoal/detail/node/root_page.hpp"
#include "shoal/detail/node/siblings.hpp"
#include "test_harness.hpp"

using namespace shoal;
using namespace shoal::detail;

namespace {

using U64Leaf = Leaf<std::uint64_t, std::uint64_t, Strategy<>>;
using StringLeaf = Leaf<std::string, std::uint64_t, Strategy<>>;

template <class LeafType>
LeafType new_leaf(const MapPages<Global>& pages) {
  return LeafType::make(LeafType::Page::allocate(pages), 0);
}

template <class LeafType, class K>
void insert_sorted(LeafType leaf, K key, std::uint64_t value) {
  SearchKey<K, K> search(key);
  const SearchResult found = leaf.search(search);
  CHECK(!found.found);
  leaf.insert_fit(found.index, WindowedKey<K>::at(std::move(key), leaf.prefix_len()),
                  std::move(value));
}

}  // namespace

TEST(byte_windows_are_big_endian_and_zero_padded) {
  auto window = [](const char* text, std::size_t offset) {
    return ByteWindows(ByteView{reinterpret_cast<const std::uint8_t*>(text), std::strlen(text)})
        .inline_key_at(offset);
  };
  CHECK_EQ(window("", 0), 0u);
  CHECK_EQ(window("a", 0), 0x6100000000000000ull);
  CHECK_EQ(window("abc", 0), 0x6162630000000000ull);
  CHECK_EQ(window("abcdefgh", 0), 0x6162636465666768ull);
  CHECK_EQ(window("abcdefghij", 0), window("abcdefgh", 0));
  CHECK_EQ(window("abcdefghij", 2), window("cdefghij", 0));
  CHECK_EQ(window("abc", 1), 0x6263000000000000ull);
  CHECK_EQ(window("abc", 3), 0u);
  CHECK_EQ(window("abc", SIZE_MAX), 0u);
  // Every window of every short string is its bytes from the offset.
  std::uint8_t bytes[24];
  for (int index = 0; index < 24; ++index) bytes[index] = static_cast<std::uint8_t>(index + 1);
  for (std::size_t len = 0; len <= 24; ++len) {
    for (std::size_t offset = 0; offset < len + 12; ++offset) {
      InlineWord expected = 0;
      for (std::size_t index = 0; index < 8; ++index) {
        const std::size_t at = offset + index;
        expected = (expected << 8) | (at < len ? bytes[at] : 0);
      }
      CHECK_EQ(ByteWindows(ByteView{bytes, len}).inline_key_at(offset), expected);
    }
  }
}

TEST(integer_windows_are_big_endian_bytes) {
  CHECK_EQ(InlineKey<std::uint64_t>::inline_key_at(0x0102030405060708ull, 2), 0x0304050607080000ull);
  CHECK_EQ(InlineKey<std::int64_t>::inline_key_at(-1, 0), 0x7fffffffffffffffull);
  CHECK_EQ(InlineKey<std::int64_t>::inline_key_at(0, 0), 0x8000000000000000ull);
  CHECK_EQ(InlineKey<std::uint64_t>::inline_key_at(5, 8), 0u);
}

TEST(windows_move_back_from_their_fill) {
  const Window window(3, 0x1122334455667788ull);
  const Window fill(1, 0xaabbcc0000000000ull);
  CHECK_EQ(window.moved_back_to(fill).inline_key(), 0xaabb112233445566ull);
  CHECK_EQ(window.moved_back_to(fill).prefix_len(), 1u);
  CHECK_EQ(window.moved_back_to(Window(3, 0)).inline_key(), window.inline_key());
}

TEST(byte_strings_compare_from_where_they_differ) {
  shoal_test::Rng rng(41);
  const std::uint8_t alphabet[] = {0, 1, 'a', 0xff};
  for (int round = 0; round < 20000; ++round) {
    std::vector<std::uint8_t> a(rng.below(20)), b(rng.below(20));
    for (auto& byte : a) byte = alphabet[rng.below(4)];
    for (auto& byte : b) byte = alphabet[rng.below(4)];
    const Ordering expected = a < b ? Ordering::Less : (b < a ? Ordering::Greater : Ordering::Equal);
    std::size_t shared = 0;
    while (shared < a.size() && shared < b.size() && a[shared] == b[shared]) ++shared;
    const ByteWindows query(ByteView{a.data(), a.size()});
    for (std::size_t equal_len = 0; equal_len <= shared; ++equal_len) {
      CHECK(query.cmp_from(ByteView{b.data(), b.size()}, equal_len) == expected);
    }
    // Against a prefix: the bytes, zero-padded, in its first bytes.
    std::vector<std::uint8_t> padded(b.size());
    for (std::size_t index = 0; index < b.size(); ++index) padded[index] = index < a.size() ? a[index] : 0;
    const Ordering against_prefix =
        padded < b ? Ordering::Less : (b < padded ? Ordering::Greater : Ordering::Equal);
    CHECK(query.cmp_prefix(b.data(), b.size()) == against_prefix);
  }
}

TEST(u64_leaf_inserts_and_finds_in_order) {
  Global global;
  MapPages<Global> pages(global);
  const U64Leaf leaf = new_leaf<U64Leaf>(pages);
  shoal_test::Rng rng(7);
  std::vector<std::uint64_t> expected;
  while (leaf.len() < U64Leaf::capacity()) {
    const std::uint64_t key = rng.below(1000);
    if (std::find(expected.begin(), expected.end(), key) != expected.end()) continue;
    insert_sorted(leaf, key, key * 10);
    expected.insert(std::upper_bound(expected.begin(), expected.end(), key), key);
  }
  CHECK(leaf.inline_keys().pads_with_greatest());
  for (std::size_t index = 0; index < expected.size(); ++index) {
    CHECK_EQ(leaf.key(index), expected[index]);
    CHECK_EQ(leaf.value(index), expected[index] * 10);
  }
  for (std::uint64_t probe = 0; probe < 1000; ++probe) {
    SearchKey<std::uint64_t, std::uint64_t> search(probe);
    const auto found = leaf.template find<void>(search);
    const auto at = std::lower_bound(expected.begin(), expected.end(), probe);
    const bool present = at != expected.end() && *at == probe;
    CHECK_EQ(found.has_value(), present);
    if (present) CHECK_EQ(*found, static_cast<std::size_t>(at - expected.begin()));
    const SearchResult ordered = leaf.search(search);
    CHECK_EQ(ordered.found, present);
    CHECK_EQ(ordered.index, static_cast<std::size_t>(at - expected.begin()));
  }
  // Split it at either end and in the middle; the halves hold everything.
  const U64Leaf right_page = new_leaf<U64Leaf>(pages);
  (void)right_page;
  auto split = leaf.split_insert(leaf.len(), WindowedKey<std::uint64_t>::at(5000, 0), 50000,
                                 U64Leaf::Page::allocate(pages));
  CHECK(split.inserted.right);
  CHECK(split.right.has_head());
  CHECK_EQ(leaf.len() + split.right.len() + 1, expected.size() + 1);
  CHECK_EQ(split.right.key(split.right.len() - 1), 5000u);
  CHECK(leaf.inline_keys().pads_with_greatest());
  CHECK(split.right.inline_keys().pads_with_greatest());
  // Free the pages; u64 needs no destructor.
  std::move(split.right.free()).deallocate(pages);
  std::move(right_page.free()).deallocate(pages);
  std::move(leaf.free()).deallocate(pages);
}

TEST(string_leaf_keeps_hash_tags_and_finds_by_view) {
  Global global;
  MapPages<Global> pages(global);
  const StringLeaf leaf = new_leaf<StringLeaf>(pages);
  std::vector<std::string> expected;
  shoal_test::Rng rng(9);
  while (leaf.len() < StringLeaf::capacity()) {
    std::string key = "/usr/share/" + std::string(rng.below(3), 'x') + std::to_string(rng.below(500));
    if (std::find(expected.begin(), expected.end(), key) != expected.end()) continue;
    expected.insert(std::upper_bound(expected.begin(), expected.end(), key), key);
    insert_sorted(leaf, std::move(key), 1);
  }
  for (std::size_t index = 0; index < expected.size(); ++index) CHECK(leaf.key(index) == expected[index]);
  for (const std::string& key : expected) {
    std::string_view view(key);
    SearchKey<std::string, std::string_view> search(view);
    const auto found = leaf.template find<void>(search);
    CHECK(found.has_value());
    CHECK(leaf.key(*found) == key);
  }
  std::string missing = "/usr/share/xx999999";
  SearchKey<std::string, std::string> absent(missing);
  CHECK(!leaf.template find<void>(absent).has_value());
  // Take the entries out, which destroys the strings.
  while (leaf.len() > 0) leaf.remove(0);
  std::move(leaf.free()).deallocate(pages);
}

TEST_MAIN()
