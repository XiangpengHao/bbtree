// Differential stress for the single-threaded ordered map and set, past the
// cases the other tests already run: byte windows against a direct encoding,
// inline-key searches against a scalar count (the SIMD path when this binary
// was built with one), every split and merge threshold, prefix boundaries,
// and copy / allocation failure.
#include <algorithm>
#include <array>
#include <cstdio>
#include <cstdint>
#include <cstring>
#include <iterator>
#include <map>
#include <memory>
#include <optional>
#include <set>
#include <stdexcept>
#include <string>
#include <string_view>
#include <type_traits>
#include <utility>
#include <vector>

#if defined(__unix__) || defined(__APPLE__)
#define SHOAL_TEST_FORK 1
#include <signal.h>
#include <sys/wait.h>
#include <unistd.h>
#endif

#include "shoal/detail/node/inline_key_search.hpp"
#include "shoal/detail/node/page.hpp"
#include "shoal/detail/node/plan.hpp"
#include "shoal/inline_key.hpp"
#include "shoal/unsync/map.hpp"
#include "shoal/unsync/set.hpp"
#include "test_harness.hpp"

using shoal::Bound;
using shoal::ByteView;
using shoal::ByteWindows;
using shoal::InlineWord;
using shoal::Ordering;
using shoal::unsync::TreeMap;
using shoal::unsync::TreeSet;

namespace {

struct NarrowNodes : shoal::DefaultNodeConfig {
  static constexpr std::size_t internal_page_size = 64;
  static constexpr std::size_t plain_internal_page_size = 64;
};

// Seven bytes, no inline keys: slot alignment and the plain-key search.
struct OddKey {
  std::uint8_t bytes[7]{};
  friend bool operator<(const OddKey& a, const OddKey& b) {
    return std::memcmp(a.bytes, b.bytes, 7) < 0;
  }
};

// Copy throws once `budget` hits zero. Moves do not. The nodes relocate it
// as bytes, so a missed destructor shows up in `alive`.
struct Bomb {
  int id = 0;
  static int budget;
  static int alive;

  Bomb() { ++alive; }
  explicit Bomb(int id) : id(id) { ++alive; }
  Bomb(const Bomb& other) : id(other.id) {
    if (budget == 0) throw std::runtime_error("copy");
    if (budget > 0) --budget;
    ++alive;
  }
  Bomb(Bomb&& other) noexcept : id(other.id) { ++alive; }
  Bomb& operator=(const Bomb&) = delete;
  Bomb& operator=(Bomb&& other) noexcept {
    id = other.id;
    return *this;
  }
  ~Bomb() { --alive; }
  friend bool operator<(const Bomb& a, const Bomb& b) { return a.id < b.id; }
};

int Bomb::budget = -1;
int Bomb::alive = 0;

struct FailAfter {
  int* left = nullptr;
  void* allocate_zeroed(shoal::Layout layout) noexcept {
    if (left == nullptr || *left <= 0) return nullptr;
    --*left;
    return shoal::Global{}.allocate_zeroed(layout);
  }
  void deallocate(void* block, shoal::Layout layout) noexcept { shoal::Global{}.deallocate(block, layout); }
};

struct CountingAlloc {
  int* live = nullptr;
  void* allocate_zeroed(shoal::Layout layout) noexcept {
    void* block = shoal::Global{}.allocate_zeroed(layout);
    if (block != nullptr && live != nullptr) ++*live;
    return block;
  }
  void deallocate(void* block, shoal::Layout layout) noexcept {
    if (live != nullptr) --*live;
    shoal::Global{}.deallocate(block, layout);
  }
};

InlineWord reference_window(const std::uint8_t* bytes, std::size_t size, std::size_t offset) {
  InlineWord word = 0;
  for (std::size_t index = 0; index < 8; ++index) {
    const std::size_t at = offset + index;
    word = (word << 8) | (at < size ? bytes[at] : 0);
  }
  return word;
}

int reference_order(const std::uint8_t* a, std::size_t a_len, const std::uint8_t* b, std::size_t b_len) {
  const std::size_t n = a_len < b_len ? a_len : b_len;
  if (n != 0) {
    const int cmp = std::memcmp(a, b, n);
    if (cmp != 0) return cmp;
  }
  if (a_len < b_len) return -1;
  if (b_len < a_len) return 1;
  return 0;
}

std::vector<std::uint8_t> pattern_bytes(std::uint64_t seed, std::size_t size) {
  shoal_test::Rng rng(seed);
  std::vector<std::uint8_t> bytes(size);
  for (std::uint8_t& byte : bytes) byte = static_cast<std::uint8_t>(rng.next());
  return bytes;
}

template <class K, class V, class Config = shoal::DefaultNodeConfig>
void check_contents(const TreeMap<K, V, Config>& map, const std::map<K, V>& model) {
  map.check_invariants();
  CHECK_EQ(map.size(), model.size());
  if (model.empty()) {
    CHECK(map.empty());
    CHECK(!map.first_key_value().has_value());
    CHECK(!map.last_key_value().has_value());
    std::size_t visited = 0;
    for (auto entry : map) {
      (void)entry;
      ++visited;
    }
    CHECK_EQ(visited, 0u);
    return;
  }
  CHECK(!(map.first_key_value()->first < model.begin()->first));
  CHECK(!(model.begin()->first < map.first_key_value()->first));
  CHECK(map.first_key_value()->second == model.begin()->second);
  const auto last = std::prev(model.end());
  CHECK(!(map.last_key_value()->first < last->first));
  CHECK(!(last->first < map.last_key_value()->first));
  CHECK(map.last_key_value()->second == last->second);

  auto at = model.begin();
  for (auto [key, value] : map) {
    CHECK(at != model.end());
    CHECK(!(key < at->first) && !(at->first < key));
    CHECK(value == at->second);
    const V* found = map.get(key);
    CHECK(found != nullptr && *found == value);
    ++at;
  }
  CHECK(at == model.end());

  at = model.end();
  auto reversed = map.iter_rev();
  while (auto entry = reversed.next()) {
    CHECK(at != model.begin());
    --at;
    CHECK(!(entry->first < at->first) && !(at->first < entry->first));
    CHECK(entry->second == at->second);
  }
  CHECK(at == model.begin());
}

template <class K, class V>
std::vector<std::pair<K, V>> model_range(const std::map<K, V>& model, const Bound<K>& lower, const Bound<K>& upper) {
  auto from = model.begin();
  if (lower.kind() == Bound<K>::Kind::Included) from = model.lower_bound(lower.key());
  if (lower.kind() == Bound<K>::Kind::Excluded) from = model.upper_bound(lower.key());
  auto to = model.end();
  if (upper.kind() == Bound<K>::Kind::Included) to = model.upper_bound(upper.key());
  if (upper.kind() == Bound<K>::Kind::Excluded) to = model.lower_bound(upper.key());
  std::vector<std::pair<K, V>> entries;
  if (from == model.end()) return entries;
  if (to != model.end() && to->first < from->first) return entries;
  for (auto at = from; at != to; ++at) entries.push_back(*at);
  return entries;
}

template <class K, class V, class Config>
void check_range(const TreeMap<K, V, Config>& map, const std::map<K, V>& model, const Bound<K>& lower,
                 const Bound<K>& upper) {
  const auto expected = model_range(model, lower, upper);
  std::size_t index = 0;
  for (auto [key, value] : map.range(lower, upper)) {
    CHECK(index < expected.size());
    CHECK(!(key < expected[index].first) && !(expected[index].first < key));
    CHECK(value == expected[index].second);
    ++index;
  }
  CHECK_EQ(index, expected.size());
  auto backward = map.range_rev(lower, upper);
  while (auto entry = backward.next()) {
    CHECK(index > 0);
    --index;
    CHECK(!(entry->first < expected[index].first) && !(expected[index].first < entry->first));
    CHECK(entry->second == expected[index].second);
  }
  CHECK_EQ(index, 0u);
}

template <class K, class V, class Config>
void check_neighbor_bounds(const TreeMap<K, V, Config>& map, const std::map<K, V>& model) {
  check_range(map, model, Bound<K>::unbounded(), Bound<K>::unbounded());
  const K* previous = nullptr;
  for (auto at = model.begin(); at != model.end(); ++at) {
    const K& key = at->first;
    check_range(map, model, Bound<K>::included(key), Bound<K>::included(key));
    check_range(map, model, Bound<K>::excluded(key), Bound<K>::excluded(key));
    check_range(map, model, Bound<K>::included(key), Bound<K>::excluded(key));
    check_range(map, model, Bound<K>::excluded(key), Bound<K>::included(key));
    check_range(map, model, Bound<K>::included(key), Bound<K>::unbounded());
    check_range(map, model, Bound<K>::excluded(key), Bound<K>::unbounded());
    check_range(map, model, Bound<K>::unbounded(), Bound<K>::included(key));
    check_range(map, model, Bound<K>::unbounded(), Bound<K>::excluded(key));
    if (previous != nullptr) check_range(map, model, Bound<K>::excluded(*previous), Bound<K>::excluded(key));
    previous = &key;
  }
}

template <std::size_t Width, std::size_t Capacity>
void check_inline_search(shoal_test::Rng& rng) {
  using Keys = shoal::detail::NodeInlineKeys<Width, Capacity>;
  using Word = typename Keys::Word;
  Word slots[Capacity];
  for (int fill = 0; fill < 12; ++fill) {
    const std::size_t len = rng.below(Capacity + 1);
    Word previous = 0;
    for (std::size_t index = 0; index < len; ++index) {
      // Sorted, with runs of ties, including the sign bit of every width.
      const Word step = static_cast<Word>(rng.below(3));
      if (previous > static_cast<Word>(~Word{0} - step)) previous = static_cast<Word>(~Word{0});
      else previous = static_cast<Word>(previous + step);
      slots[index] = previous;
    }
    const Word greatest = static_cast<Word>(~Word{0});
    for (std::size_t index = len; index < Capacity; ++index) slots[index] = greatest;
    Keys keys(slots, len);

    Word probes[8];
    std::size_t probe_count = 0;
    probes[probe_count++] = 0;
    probes[probe_count++] = greatest;
    probes[probe_count++] = static_cast<Word>(greatest >> 1);
    probes[probe_count++] = static_cast<Word>((greatest >> 1) + 1);
    if (len > 0) probes[probe_count++] = slots[rng.below(len)];
    if (len > 1) probes[probe_count++] = slots[len / 2];
    probes[probe_count++] = static_cast<Word>(rng.next());
    probes[probe_count++] = static_cast<Word>(rng.next());

    for (std::size_t probe = 0; probe < probe_count; ++probe) {
      const Word target = probes[probe];
      std::size_t below = 0, equal = 0;
      for (std::size_t index = 0; index < len; ++index) {
        if (slots[index] < target) ++below;
        else if (slots[index] == target) ++equal;
      }
      constexpr unsigned shift = 64 - 8 * static_cast<unsigned>(Width);
      const auto found = keys.equal_range(static_cast<InlineWord>(target) << shift);
      CHECK_EQ(found.start, below);
      CHECK_EQ(found.end, below + equal);
    }
  }
}

template <std::size_t Capacity>
void check_hash_tags(shoal_test::Rng& rng) {
  std::uint8_t tags[Capacity];
  for (std::uint8_t& tag : tags) tag = static_cast<std::uint8_t>(rng.below(256));
  for (int round = 0; round < 20; ++round) {
    shoal::detail::IndexRange candidates{rng.below(Capacity + 1), 0};
    candidates.end = candidates.start + rng.below(Capacity - candidates.start + 1);
    const std::uint8_t target = static_cast<std::uint8_t>(rng.below(256));
    const auto got = shoal::detail::U8EqualitySearch::matching<Capacity>(tags, candidates, target);
#if defined(SHOAL_X86_64)
    CHECK(got.has_value());
#else
    if (!got) return;
#endif
    std::size_t hits[3];
    std::size_t count = 0;
    for (std::size_t index = candidates.start; index < candidates.end; ++index) {
      if (tags[index] != target) continue;
      if (count < 3) hits[count] = index;
      ++count;
    }
    using Kind = shoal::detail::SlotMatches::Kind;
    if (count == 0) CHECK(got->kind == Kind::None);
    else if (count == 1) CHECK(got->kind == Kind::One && got->lower == hits[0]);
    else if (count == 2) CHECK(got->kind == Kind::Two && got->lower == hits[0] && got->upper == hits[1]);
    else CHECK(got->kind == Kind::Many);
  }
}

template <std::size_t Capacity>
void check_u64_mask(shoal_test::Rng& rng) {
  std::uint64_t slots[Capacity];
  std::uint64_t previous = 0;
  for (std::size_t index = 0; index < Capacity; ++index) {
    previous += rng.below(4);
    slots[index] = previous;
  }
  for (int round = 0; round < 15; ++round) {
    const std::size_t len = rng.below(Capacity + 1);
    const std::uint64_t target = round % 2 == 0 && len > 0 ? slots[rng.below(len)] : rng.next();
    const auto mask = shoal::detail::U64EqualityMask::matching<Capacity>(slots, target);
#if defined(SHOAL_AVX512F)
    CHECK(mask.has_value());
#else
    if (!mask) return;
#endif
    std::size_t below = 0, equal = 0;
    for (std::size_t index = 0; index < len; ++index) {
      if (slots[index] < target) ++below;
      else if (slots[index] == target) ++equal;
    }
    const auto found = mask->equal_range(len);
    if (equal == 0) {
      CHECK(!found.has_value());
    } else {
      CHECK(found.has_value());
      CHECK_EQ(found->start, below);
      CHECK_EQ(found->end, below + equal);
    }
  }
}

void check_windows_of(const std::uint8_t* bytes, std::size_t size, const std::uint8_t* other, std::size_t other_size) {
  const ByteWindows windows(ByteView{bytes, size});
  const ByteWindows other_windows(ByteView{other, other_size});
  for (std::size_t offset = 0; offset <= 16; ++offset) {
    CHECK_EQ(windows.inline_key_at(offset), reference_window(bytes, size, offset));
  }
  const Ordering full = windows.cmp_from(ByteView{other, other_size}, 0);
  const int reference = reference_order(bytes, size, other, other_size);
  CHECK_EQ(full == Ordering::Equal, reference == 0);
  CHECK_EQ(full == Ordering::Less, reference < 0);
  CHECK(windows.equals(ByteView{other, other_size}) == (reference == 0));

  std::size_t shared = 0;
  const std::size_t limit = size < other_size ? size : other_size;
  while (shared < limit && bytes[shared] == other[shared]) ++shared;
  for (std::size_t equal = 0; equal <= shared; ++equal) {
    const Ordering from = windows.cmp_from(ByteView{other, other_size}, equal);
    CHECK(from == full);
  }
  if (other_size > 0) {
    // `cmp_prefix` zero-pads a query that ends inside the prefix, then
    // compares that many bytes. It does not order by length.
    const Ordering against = windows.cmp_prefix(other, other_size);
    std::vector<std::uint8_t> padded(other_size, 0);
    if (size < other_size) {
      if (size != 0) std::memcpy(padded.data(), bytes, size);
    } else if (other_size != 0) {
      std::memcpy(padded.data(), bytes, other_size);
    }
    const int prefix = reference_order(padded.data(), other_size, other, other_size);
    CHECK_EQ(against == Ordering::Less, prefix < 0);
    CHECK_EQ(against == Ordering::Greater, prefix > 0);
    CHECK_EQ(against == Ordering::Equal, prefix == 0);
  }
}

std::string adversarial_key(std::uint64_t n) {
  switch (n % 14) {
    case 0: return {};
    case 1: return std::string("\0", 1);
    case 2: return std::string("\0\0", 2);
    case 3: return std::string("\0a", 2);
    case 4: return std::string("a\0", 2);
    case 5: return std::string("a\0b", 3);
    case 6: return std::string(static_cast<std::size_t>(n % 80), 'a');
    case 7: return std::string(static_cast<std::size_t>(n % 70), 'b') + "c";
    case 8: {
      std::string key(64, 'm');
      key.push_back(static_cast<char>(1 + n % 23));
      return key;
    }
    case 9: {
      std::string key(63, 'm');
      key.push_back(static_cast<char>(1 + n % 23));
      key += std::to_string(n / 14);
      return key;
    }
    case 10: {
      std::string key(65, 'm');
      key.push_back(static_cast<char>(1 + n % 23));
      return key;
    }
    case 11: {
      std::string key(255, 'z');
      key += std::to_string(n);
      return key;
    }
    case 12: {
      std::string key(300, 'z');
      key[299] = static_cast<char>(1 + n % 200);
      key += std::to_string(n / 14);
      return key;
    }
    default: {
      std::string key = "pref";
      key.append(n % 9, '\0');
      key += "tail";
      key += std::to_string(n);
      return key;
    }
  }
}

template <class Config = shoal::DefaultNodeConfig>
void sweep_integers(std::uint64_t seed, std::size_t limit) {
  using Map = TreeMap<std::uint64_t, std::uint64_t, Config>;
  shoal_test::Rng rng(seed);
  Map map;
  std::map<std::uint64_t, std::uint64_t> model;
  auto touch = [&](std::uint64_t key) {
    const auto* found = map.get(key);
    auto at = model.find(key);
    CHECK_EQ(found != nullptr, at != model.end());
    if (found != nullptr) CHECK_EQ(*found, at->second);
    CHECK_EQ(map.size(), model.size());
  };
  for (int order = 0; order < 3; ++order) {
    map.clear();
    model.clear();
    for (std::size_t index = 0; index < limit; ++index) {
      std::uint64_t key = index;
      if (order == 1) key = limit - 1 - index;
      if (order == 2) key = (index % 2 == 0) ? index / 2 : limit - 1 - index / 2;
      const auto old = map.insert(key, index);
      CHECK(!old.has_value());
      model.emplace(key, index);
      touch(key);
      if (index % 40 == 0 || index + 1 == limit) {
        map.check_invariants();
        if (index % 120 == 0 || index + 1 == limit) check_contents(map, model);
      }
    }
    check_neighbor_bounds(map, model);
    // Overwrite, then remove in the opposite direction, across every merge.
    for (std::size_t index = 0; index < limit; index += 3) {
      const std::uint64_t key = index;
      if (!model.count(key)) continue;
      const auto old = map.insert(key, 1000000 + index);
      CHECK(old.has_value());
      CHECK_EQ(*old, model[key]);
      model[key] = 1000000 + index;
    }
    std::vector<std::uint64_t> keys;
    keys.reserve(model.size());
    for (const auto& entry : model) keys.push_back(entry.first);
    for (std::size_t index = 0; index < keys.size(); ++index) {
      const std::uint64_t key = order == 0 ? keys[keys.size() - 1 - index] : keys[index];
      const auto removed = map.remove(key);
      CHECK(removed.has_value());
      CHECK_EQ(*removed, model[key]);
      model.erase(key);
      if (index % 40 == 0) map.check_invariants();
    }
    CHECK(map.empty());
    map.check_invariants();
  }
  (void)rng;
}

}  // namespace

template <>
struct shoal::is_trivially_relocatable<Bomb> : std::true_type {};

namespace {

TEST(byte_windows_match_a_direct_encoding) {
  const std::uint8_t empty[] = {0};
  check_windows_of(empty, 0, empty, 0);
  for (std::size_t size = 0; size <= 40; ++size) {
    auto bytes = pattern_bytes(1000 + size, size);
    auto other = pattern_bytes(2000 + size, size == 0 ? 0 : size - (size > 8 ? 3 : 0));
    if (size > 0) {
      // A proper prefix, and a difference on each byte.
      other = bytes;
      if (other.size() > 0) other.pop_back();
      check_windows_of(bytes.data(), bytes.size(), other.data(), other.size());
      other = bytes;
      other.push_back(0);
      check_windows_of(bytes.data(), bytes.size(), other.data(), other.size());
      other = bytes;
      other.push_back(1);
      check_windows_of(bytes.data(), bytes.size(), other.data(), other.size());
      for (std::size_t at = 0; at < bytes.size(); ++at) {
        other = bytes;
        other[at] ^= 0x80;
        check_windows_of(bytes.data(), bytes.size(), other.data(), other.size());
      }
    } else {
      check_windows_of(bytes.data(), 0, other.data(), other.size());
    }
  }
  for (std::size_t size : {63u, 64u, 65u, 254u, 255u, 256u, 300u}) {
    auto bytes = pattern_bytes(size, size);
    auto other = bytes;
    other.back() ^= 0xff;
    check_windows_of(bytes.data(), bytes.size(), other.data(), other.size());
    std::vector<std::uint8_t> prefixed(size, 0x11);
    check_windows_of(bytes.data(), bytes.size(), prefixed.data(), prefixed.size());
  }
  // Embedded NULs against the same bytes without them.
  const std::uint8_t with_nul[] = {'a', 0, 'b', 0, 'c'};
  const std::uint8_t without[] = {'a', 'b', 'c'};
  check_windows_of(with_nul, 5, without, 3);
  check_windows_of(with_nul, 5, with_nul, 4);
}

TEST(integer_encodings_sort_as_the_integers) {
  auto check64 = [](std::int64_t key) {
    CHECK_EQ(shoal::InlineKey<std::int64_t>::inline_key_at(key, 0),
             static_cast<InlineWord>(static_cast<std::uint64_t>(key) ^ (std::uint64_t{1} << 63)));
    for (std::size_t offset = 0; offset <= 8; ++offset) {
      const InlineWord shifted = offset == 8 ? 0 : shoal::InlineKey<std::int64_t>::inline_key_at(key, 0) << (8 * offset);
      CHECK_EQ(shoal::InlineKey<std::int64_t>::inline_key_at(key, offset), shifted);
    }
  };
  const std::int64_t samples[] = {0, 1, -1, 2, -2, 255, 256, -255, -256, INT64_C(0x0102030405060708),
                                  INT64_MAX, INT64_MAX - 1, INT64_MIN, INT64_MIN + 1, INT64_C(-1000000000000)};
  for (std::int64_t key : samples) check64(key);
  for (std::size_t i = 0; i < sizeof(samples) / sizeof(samples[0]); ++i) {
    for (std::size_t j = i + 1; j < sizeof(samples) / sizeof(samples[0]); ++j) {
      const auto left = shoal::InlineKey<std::int64_t>::inline_key_at(samples[i], 0);
      const auto right = shoal::InlineKey<std::int64_t>::inline_key_at(samples[j], 0);
      CHECK_EQ(left < right, samples[i] < samples[j]);
      CHECK_EQ(left == right, samples[i] == samples[j]);
    }
  }

#if defined(__SIZEOF_INT128__)
  auto encoding = [](__int128 key, std::uint8_t out[16]) {
    const unsigned __int128 flipped =
        static_cast<unsigned __int128>(key) ^ (static_cast<unsigned __int128>(1) << 127);
    for (int index = 0; index < 16; ++index) {
      out[index] = static_cast<std::uint8_t>(flipped >> (8 * (15 - index)));
    }
  };
  const unsigned __int128 one = 1;
  const __int128 wide[] = {0, 1, -1, 2, -2, __int128{INT64_MAX}, __int128{INT64_MIN},
                           static_cast<__int128>(one << 64), static_cast<__int128>((one << 64) - 1),
                           -static_cast<__int128>(one << 64), -static_cast<__int128>(one << 64) - 1,
                           static_cast<__int128>((one << 127) - 1), static_cast<__int128>(one << 127)};
  for (std::size_t i = 0; i < sizeof(wide) / sizeof(wide[0]); ++i) {
    std::uint8_t bytes[16];
    encoding(wide[i], bytes);
    for (std::size_t offset = 0; offset <= 16; ++offset) {
      CHECK_EQ(shoal::InlineKey<__int128>::inline_key_at(wide[i], offset), reference_window(bytes, 16, offset));
    }
    for (std::size_t j = i + 1; j < sizeof(wide) / sizeof(wide[0]); ++j) {
      std::uint8_t other[16];
      encoding(wide[j], other);
      const int order = reference_order(bytes, 16, other, 16);
      CHECK_EQ(order < 0, wide[i] < wide[j]);
      CHECK_EQ(order == 0, wide[i] == wide[j]);
    }
  }
#endif
}

TEST(simd_slot_search_matches_a_scalar_count) {
  shoal_test::Rng rng(11);
  check_inline_search<2, 12>(rng);
  check_inline_search<2, 13>(rng);
  check_inline_search<2, 16>(rng);
  check_inline_search<2, 17>(rng);
  check_inline_search<2, 24>(rng);
  check_inline_search<2, 31>(rng);
  check_inline_search<2, 32>(rng);
  check_inline_search<2, 33>(rng);
  check_inline_search<2, 40>(rng);
  check_inline_search<2, 48>(rng);
  check_inline_search<2, 64>(rng);
  check_inline_search<4, 12>(rng);
  check_inline_search<4, 15>(rng);
  check_inline_search<4, 16>(rng);
  check_inline_search<4, 28>(rng);
  check_inline_search<4, 31>(rng);
  check_inline_search<4, 32>(rng);
  check_inline_search<8, 12>(rng);
  check_inline_search<8, 16>(rng);
  check_inline_search<8, 28>(rng);
  check_inline_search<8, 30>(rng);
  check_inline_search<8, 31>(rng);
  check_inline_search<8, 32>(rng);
  check_hash_tags<16>(rng);
  check_hash_tags<17>(rng);
  check_hash_tags<24>(rng);
  check_hash_tags<32>(rng);
  check_hash_tags<33>(rng);
  check_hash_tags<48>(rng);
  check_hash_tags<64>(rng);
  check_u64_mask<8>(rng);
  check_u64_mask<28>(rng);
  check_u64_mask<31>(rng);
  check_u64_mask<32>(rng);
  check_u64_mask<64>(rng);
}

TEST(integer_maps_cross_every_split_and_merge) {
  using Shape = shoal::detail::NodeShape<std::uint64_t, std::uint64_t, shoal::detail::Strategy<>>;
  using Narrow = shoal::detail::NodeShape<std::uint64_t, std::uint64_t, shoal::detail::Strategy<NarrowNodes>>;
  std::printf("(leaf %zu/%zu, narrow leaf %zu internal %zu) ", Shape::leaf_capacity, Shape::min_len(Shape::leaf_capacity),
              Narrow::leaf_capacity, Narrow::internal_capacity);
  sweep_integers<>(1, Shape::leaf_capacity * 3 + Shape::internal_capacity);
  sweep_integers<NarrowNodes>(2, Narrow::leaf_capacity * 4 + 50);

  TreeMap<std::int64_t, int> map;
  std::map<std::int64_t, int> model;
  const std::int64_t extremes[] = {INT64_MIN, INT64_MIN + 1, -1000, -1, 0, 1, 1000, INT64_MAX - 1, INT64_MAX};
  for (int round = 0; round < 3; ++round) {
    for (std::size_t index = 0; index < 9; ++index) {
      const std::size_t at = round == 1 ? 8 - index : index;
      map.insert(extremes[at], static_cast<int>(at));
      model[extremes[at]] = static_cast<int>(at);
    }
    for (std::int64_t key = -200; key <= 200; ++key) {
      map.insert(key, static_cast<int>(key));
      model[key] = static_cast<int>(key);
    }
    check_contents(map, model);
    check_neighbor_bounds(map, model);
    // Heterogeneous arithmetic queries convert to the key type.
    CHECK_EQ(*map.get(0), 0);
    CHECK_EQ(*map.get(1), 1);
    CHECK(map.get(2) != nullptr);
    map.clear();
    model.clear();
  }

  TreeMap<std::uint64_t, std::uint64_t> ends;
  ends.insert(0, 1);
  ends.insert(UINT64_MAX, 2);
  ends.insert(1, 3);
  CHECK_EQ(*ends.get(0), 1u);
  CHECK_EQ(*ends.get(UINT64_MAX), 2u);
  CHECK_EQ(ends.size(), 3u);
  CHECK(ends.pop_first()->first == 0);
  CHECK(ends.pop_last()->first == UINT64_MAX);
  CHECK_EQ(ends.size(), 1u);

#if defined(__SIZEOF_INT128__)
  const unsigned __int128 one = 1;
  TreeMap<__int128, int> wide;
  std::map<__int128, int> wide_model;
  const __int128 points[] = {0,
                             1,
                             -1,
                             __int128{INT64_MAX},
                             __int128{INT64_MIN},
                             static_cast<__int128>(one << 64),
                             static_cast<__int128>((one << 64) - 1),
                             -static_cast<__int128>(one << 64),
                             static_cast<__int128>((one << 127) - 1),
                             static_cast<__int128>(one << 127)};
  for (int order = 0; order < 2; ++order) {
    for (int index = 0; index < 10; ++index) {
      const int at = order == 0 ? index : 9 - index;
      wide.insert(points[at], at);
      wide_model[points[at]] = at;
    }
    for (int index = 0; index < 300; ++index) {
      const __int128 key = (static_cast<__int128>(index) << 64) + (index * 17);
      wide.insert(key, index);
      wide_model[key] = index;
    }
    wide.check_invariants();
    CHECK_EQ(wide.size(), wide_model.size());
    auto at = wide_model.begin();
    for (auto [key, value] : wide) {
      CHECK(key == at->first);
      CHECK_EQ(value, at->second);
      ++at;
    }
    wide.clear();
    wide_model.clear();
  }
#endif
}

TEST(prefix_strings_survive_shrink_merge_and_bounds) {
  std::vector<std::string> keys;
  keys.emplace_back();
  for (int length = 1; length <= 80; ++length) keys.emplace_back(static_cast<std::size_t>(length), 'a');
  for (int length = 0; length <= 70; ++length) keys.emplace_back(std::string(static_cast<std::size_t>(length), 'b') + "c");
  keys.emplace_back(std::string("\0", 1));
  keys.emplace_back(std::string("\0\0", 2));
  keys.emplace_back(std::string("\0a", 2));
  keys.emplace_back(std::string("a\0", 2));
  keys.emplace_back(std::string("a\0b", 3));
  keys.emplace_back(std::string("ab"));
  for (int index = 0; index < 25; ++index) {
    keys.emplace_back(std::string(64, 'm') + static_cast<char>(index + 1));
    keys.emplace_back(std::string(63, 'm') + static_cast<char>(index + 1));
    keys.emplace_back(std::string(65, 'm') + static_cast<char>(index + 1));
    keys.emplace_back(std::string(255, 'z') + std::to_string(index));
    std::string long_key(300, 'z');
    long_key += std::to_string(index);
    keys.push_back(std::move(long_key));
  }

  auto install = [&](TreeMap<std::string, std::uint64_t>& map, std::map<std::string, std::uint64_t>& model, int order) {
    map.clear();
    model.clear();
    std::vector<std::size_t> positions(keys.size());
    for (std::size_t index = 0; index < keys.size(); ++index) positions[index] = index;
    if (order == 1) std::reverse(positions.begin(), positions.end());
    if (order == 2) {
      // Outside-in, so separators land on the extreme keys.
      std::vector<std::size_t> folded;
      for (std::size_t index = 0; index < keys.size(); ++index) {
        folded.push_back(index % 2 == 0 ? index / 2 : keys.size() - 1 - index / 2);
      }
      positions.swap(folded);
    }
    for (std::size_t index = 0; index < positions.size(); ++index) {
      const auto& key = keys[positions[index]];
      map.insert(key, positions[index]);
      model[key] = positions[index];
      if (index % 50 == 0) map.check_invariants();
    }
  };

  for (int order = 0; order < 3; ++order) {
    TreeMap<std::string, std::uint64_t> map;
    std::map<std::string, std::uint64_t> model;
    install(map, model, order);
    check_contents(map, model);
    check_neighbor_bounds(map, model);
    for (const auto& entry : model) {
      const std::string_view view(entry.first.data(), entry.first.size());
      const auto* found = map.get(view);
      CHECK(found != nullptr && *found == entry.second);
    }
    // A key the tree does not hold, on either side of a stored prefix.
    CHECK(map.get(std::string(90, 'a')) == nullptr);
    CHECK(map.get(std::string("a\0c", 3)) == nullptr);
    CHECK(map.get(std::string_view("\0b", 2)) == nullptr);

    // Break a long shared prefix, then restore it.
    const auto displaced = map.insert(std::string("q"), 7);
    CHECK(!displaced.has_value());
    model["q"] = 7;
    check_contents(map, model);
    CHECK(map.remove("q").has_value());
    model.erase("q");
    check_contents(map, model);

    // Batch a prefix-breaking key between keys that share the old prefix.
    {
      TreeMap<std::string, std::uint64_t> single, batched;
      std::vector<std::string> batch_keys;
      std::vector<std::uint64_t> batch_values;
      for (int index = 0; index < 12; ++index) {
        batch_keys.push_back(std::string(48, 'p') + std::to_string(index));
        batch_values.push_back(static_cast<std::uint64_t>(index));
      }
      batch_keys.insert(batch_keys.begin() + 6, "break");
      batch_values.insert(batch_values.begin() + 6, 99);
      for (int index = 0; index < 8; ++index) {
        batch_keys.push_back(std::string(48, 'p') + "x" + std::to_string(index));
        batch_values.push_back(100 + static_cast<std::uint64_t>(index));
      }
      auto copies = batch_keys;
      auto value_copies = batch_values;
      std::vector<std::optional<std::uint64_t>> previous(batch_keys.size());
      batched.insert_many(batch_keys.data(), batch_values.data(), batch_keys.size(), previous.data());
      for (std::size_t index = 0; index < copies.size(); ++index) {
        const auto old = single.insert(std::move(copies[index]), value_copies[index]);
        CHECK_EQ(previous[index].has_value(), old.has_value());
      }
      single.check_invariants();
      batched.check_invariants();
      batched.check_same_nodes(single);
    }

    std::vector<std::string> stored;
    stored.reserve(model.size());
    for (const auto& entry : model) stored.push_back(entry.first);
    for (std::size_t index = 0; index < stored.size(); index += order == 2 ? 1 : 2) {
      const std::string& key = stored[order == 1 ? stored.size() - 1 - index : index];
      if (!model.count(key)) continue;
      CHECK(map.remove(key).has_value());
      model.erase(key);
      if (index % 30 == 0) map.check_invariants();
    }
    check_contents(map, model);
  }

  // Built full, then a key that shares nothing shrinks every border.
  std::vector<std::pair<std::string, std::uint64_t>> entries;
  for (std::uint64_t index = 0; index < keys.size(); ++index) entries.push_back({keys[index], index});
  std::sort(entries.begin(), entries.end(), [](const auto& a, const auto& b) { return a.first < b.first; });
  entries.erase(std::unique(entries.begin(), entries.end(),
                            [](const auto& a, const auto& b) { return !(a.first < b.first) && !(b.first < a.first); }),
                entries.end());
  auto built = TreeMap<std::string, std::uint64_t>::from_sorted(entries.begin(), entries.end());
  built.check_invariants();
  built.insert("", 123);
  CHECK_EQ(*built.get(std::string_view()), 123u);
  built.check_invariants();
  for (const auto& entry : entries) {
    if (entry.first.empty()) continue;
    CHECK(built.get(std::string_view(entry.first)) != nullptr);
  }
}

TEST(byte_keys_vectors_and_arrays_agree_with_std_map) {
  {
    TreeMap<std::vector<std::uint8_t>, std::uint64_t> map;
    std::map<std::vector<std::uint8_t>, std::uint64_t> model;
    for (std::uint64_t index = 0; index < 400; ++index) {
      std::vector<std::uint8_t> key;
      if (index % 11 == 0) {
        // The empty key, overwritten as later multiples of 11 arrive.
      } else if (index % 11 == 1) {
        key.push_back(0);
        key.push_back(static_cast<std::uint8_t>(index));
      } else {
        const std::size_t size = static_cast<std::size_t>(index % 40);
        key.assign(size, static_cast<std::uint8_t>(index));
        if (size > 2) key[size / 2] = 0;
        key.push_back(static_cast<std::uint8_t>(index));
      }
      map.insert(key, index);
      model[key] = index;
    }
    check_contents(map, model);
    check_neighbor_bounds(map, model);
    std::vector<std::vector<std::uint8_t>> keys;
    for (const auto& entry : model) keys.push_back(entry.first);
    for (std::size_t index = 0; index < keys.size(); index += 3) {
      map.remove(keys[index]);
      model.erase(keys[index]);
    }
    check_contents(map, model);
  }
  {
    using Key = std::array<std::uint8_t, 20>;
    TreeMap<Key, std::uint64_t> map;
    std::map<Key, std::uint64_t> model;
    for (std::uint64_t index = 0; index < 500; ++index) {
      Key key{};
      for (int byte = 0; byte < 8; ++byte) key[19 - byte] = static_cast<std::uint8_t>(index >> (8 * byte));
      key[0] = static_cast<std::uint8_t>(index % 5);
      map.insert(key, index);
      model[key] = index;
    }
    check_contents(map, model);
    // Whole-key equality on a lone candidate: one key away from each stored one.
    for (const auto& entry : model) {
      Key missing = entry.first;
      missing[10] ^= 0xff;
      CHECK(map.get(missing) == nullptr);
      CHECK_EQ(*map.get(entry.first), entry.second);
    }
  }
  {
    TreeMap<OddKey, std::uint64_t, NarrowNodes> map;
    std::map<OddKey, std::uint64_t> model;
    for (std::uint64_t index = 0; index < 800; ++index) {
      OddKey key{};
      for (int byte = 0; byte < 7; ++byte) key.bytes[6 - byte] = static_cast<std::uint8_t>(index >> (8 * (byte % 3)));
      key.bytes[0] = static_cast<std::uint8_t>(index);
      map.insert(key, index);
      model[key] = index;
      if (index % 100 == 0) map.check_invariants();
    }
    check_contents(map, model);
    check_neighbor_bounds(map, model);
  }
}

TEST(relocatable_values_are_destroyed_once) {
  // The deleter counts a destruction. Trivial relocation must run it once.
  struct CountDelete {
    int* live = nullptr;
    void operator()(int* pointer) const noexcept {
      delete pointer;
      if (live != nullptr) --*live;
    }
  };
  using Pointer = std::unique_ptr<int, CountDelete>;
  int live = 0;
  {
    TreeMap<std::string, Pointer> map;
    for (int index = 0; index < 400; ++index) {
      // The suffix keeps every key, so a split relocates each pointer once.
      std::string key = adversarial_key(static_cast<std::uint64_t>(index));
      key.push_back('\0');
      key += std::to_string(index);
      map.insert(std::move(key), Pointer(new int(index), CountDelete{&live}));
      ++live;
    }
    CHECK_EQ(map.size(), 400u);
    CHECK_EQ(live, 400);
    map.check_invariants();
    int seen = 0;
    for (auto [key, value] : map) {
      (void)key;
      CHECK(value != nullptr);
      ++seen;
    }
    CHECK_EQ(seen, 400);
    std::vector<std::string> keys;
    for (auto [key, value] : map) keys.push_back(key);
    for (std::size_t index = 0; index < keys.size(); index += 2) {
      auto removed = map.remove(keys[index]);
      CHECK(removed.has_value() && *removed != nullptr);
    }
    map.check_invariants();
    TreeMap<std::string, Pointer> moved(std::move(map));
    CHECK(map.empty());
    moved.clear();
    CHECK_EQ(live, 0);
  }
  CHECK_EQ(live, 0);

  TreeMap<std::uint64_t, std::vector<int>> vectors;
  std::map<std::uint64_t, std::vector<int>> model;
  for (std::uint64_t key = 0; key < 300; ++key) {
    std::vector<int> value(static_cast<std::size_t>(key % 17), static_cast<int>(key));
    vectors.insert(key, value);
    model.emplace(key, std::move(value));
  }
  check_contents(vectors, model);
  for (std::uint64_t key = 0; key < 300; key += 4) {
    vectors.remove(key);
    model.erase(key);
  }
  check_contents(vectors, model);
}

TEST(lifecycle_batches_and_set_algebra) {
  int blocks = 0;
  {
    TreeMap<std::uint64_t, std::uint64_t, shoal::DefaultNodeConfig, CountingAlloc> map(CountingAlloc{&blocks});
    for (std::uint64_t key = 0; key < 500; ++key) map.insert(key, key);
    auto copy = map;
    CHECK(blocks > 0);
    copy.insert(1000, 1);
    CHECK(map.get(1000) == nullptr);
    CHECK_EQ(*copy.get(1000), 1u);
    TreeMap<std::uint64_t, std::uint64_t, shoal::DefaultNodeConfig, CountingAlloc> other(CountingAlloc{&blocks});
    for (std::uint64_t key = 400; key < 700; ++key) other.insert(key, key + 5);
    map.append(other);
    CHECK(other.empty());
    CHECK_EQ(*map.get(400), 405u);
    CHECK_EQ(*map.get(0), 0u);
    map.append(map);
    CHECK_EQ(map.size(), 700u);
    CHECK_EQ(*map.get(400), 405u);
    CHECK_EQ(*map.get(699), 704u);
    map.check_invariants();
    auto upper = map.split_off(std::uint64_t{350});
    CHECK(upper.first_key_value()->first == 350);
    CHECK(map.last_key_value()->first == 349);
    CHECK_EQ(map.size() + upper.size(), 700u);
    map.check_invariants();
    upper.check_invariants();
    map.append(upper);
    CHECK_EQ(map.size(), 700u);
    map.check_invariants();
    std::swap(map, copy);
    map = copy;
    map = std::move(copy);
    CHECK(copy.empty());
    map.clear();
    map.insert(1, 1);
    TreeMap<std::uint64_t, std::uint64_t, shoal::DefaultNodeConfig, CountingAlloc>* self = &map;
    map = std::move(*self);
    CHECK_EQ(*map.get(1), 1u);
  }
  CHECK_EQ(blocks, 0);

  // Values handed out by a mutable iteration stay the map's values.
  {
    TreeMap<std::uint64_t, std::uint64_t> map;
    for (std::uint64_t key = 0; key < 200; ++key) map.insert(key * 2, key);
    for (auto [key, value] : map) value = key + 1;
    for (std::uint64_t key = 0; key < 200; ++key) CHECK_EQ(*map.get(key * 2), key * 2 + 1);
    auto value = map.get_mut(std::uint64_t{10});
    CHECK(value != nullptr);
    *value = 42;
    CHECK_EQ(*map.get(std::uint64_t{10}), 42u);
    map.check_invariants();
  }

  // extract_if is lazy: entries it never reaches stay.
  {
    TreeMap<std::uint64_t, std::uint64_t> map;
    for (std::uint64_t key = 0; key < 100; ++key) map.insert(key, key);
    {
      auto extraction = map.extract_if([](const std::uint64_t& key, std::uint64_t&) { return key % 2 == 0; });
      CHECK(extraction.next()->first == 0);
      CHECK(extraction.next()->first == 2);
      CHECK(extraction.next()->first == 4);
    }
    CHECK(map.get(std::uint64_t{0}) == nullptr);
    CHECK(map.get(std::uint64_t{6}) != nullptr);
    CHECK_EQ(*map.get(std::uint64_t{7}), 7u);
    map.check_invariants();
    map.retain([](const std::uint64_t&, std::uint64_t&) { return false; });
    CHECK(map.empty());
    map.retain([](const std::uint64_t&, std::uint64_t&) { return true; });
    CHECK(map.empty());
  }

  // Set algebra on either side of the lookup cutoff, and on byte strings.
  auto fill = [](TreeSet<std::uint64_t>& set, std::uint64_t first, std::uint64_t last, std::uint64_t step) {
    for (std::uint64_t key = first; key < last; key += step) set.insert(key);
  };
  TreeSet<std::uint64_t> small, large, almost;
  fill(small, 0, 160, 16);     // 10 elements
  fill(large, 0, 3200, 2);     // 1600 elements, 10 <= 1600/16
  fill(almost, 0, 300, 2);     // 150 elements, 10 > 150/16
  small.insert(0);
  large.insert(0);
  almost.insert(0);
  CHECK(small.is_subset(large));
  CHECK(!large.is_subset(small));
  CHECK(small.is_disjoint(TreeSet<std::uint64_t>{}));
  auto both = small & large;
  CHECK_EQ(both.size(), small.size());
  auto either = small | almost;
  auto only = large - small;
  auto sym = small ^ almost;
  CHECK(either.size() >= small.size());
  CHECK(only.size() + small.size() >= large.size());
  CHECK_EQ((small & almost).size() + (small ^ almost).size(), (small | almost).size());
  (void)sym;
  CHECK(large.is_superset(small));

  TreeSet<std::string> words, more;
  for (int index = 0; index < 200; ++index) {
    words.insert(adversarial_key(static_cast<std::uint64_t>(index)));
    more.insert(adversarial_key(static_cast<std::uint64_t>(index * 2)));
  }
  auto united = words | more;
  united.check_invariants();
  for (const std::string& word : words) CHECK(united.contains(std::string_view(word)));
  for (const std::string& word : more) CHECK(united.contains(std::string_view(word)));
  auto diff = words - more;
  for (const std::string& word : diff) CHECK(words.contains(word) && !more.contains(word));
  CHECK(words.is_subset(united));
  words.clear();
  CHECK(words.is_subset(more));
  CHECK(more.is_superset(words));
}

TEST(copy_failure_drops_the_partial_tree) {
  Bomb::alive = 0;
  Bomb::budget = -1;
  {
    TreeMap<Bomb, int> map;
    for (int id = 0; id < 80; ++id) map.insert(Bomb(id), id);
    CHECK_EQ(Bomb::alive, 80);
    map.check_invariants();
    for (int budget : {0, 1, 3, 7, 20, 40, 79}) {
      Bomb::budget = budget;
      bool threw = false;
      try {
        TreeMap<Bomb, int> copy(map);
        (void)copy;
      } catch (const std::runtime_error&) {
        threw = true;
      }
      CHECK(threw);
      CHECK_EQ(Bomb::alive, 80);
      map.check_invariants();
      CHECK_EQ(*map.get(Bomb(0)), 0);
      CHECK_EQ(*map.get(Bomb(79)), 79);
    }
    Bomb::budget = -1;
    TreeMap<Bomb, int> copy(map);
    CHECK_EQ(Bomb::alive, 160);
    copy.insert(Bomb(1000), 1);
    CHECK(map.get(Bomb(1000)) == nullptr);
    CHECK_EQ(copy.size(), 81u);
  }
  CHECK_EQ(Bomb::alive, 0);
}

TEST(random_ops_track_std_map) {
  {
    shoal_test::Rng rng(17);
    TreeMap<std::int32_t, std::int32_t, NarrowNodes> map;
    std::map<std::int32_t, std::int32_t> model;
    for (int step = 0; step < 6000; ++step) {
      std::int32_t key = static_cast<std::int32_t>(rng.below(3000)) - 1500;
      if (rng.below(30) == 0) key = INT32_MIN;
      else if (rng.below(30) == 0) key = INT32_MAX;
      const std::uint64_t choice = rng.below(100);
      if (choice < 55) {
        const auto old = map.insert(key, static_cast<std::int32_t>(step));
        auto at = model.find(key);
        CHECK_EQ(old.has_value(), at != model.end());
        if (at != model.end()) {
          CHECK_EQ(*old, at->second);
          at->second = static_cast<std::int32_t>(step);
        } else {
          model.emplace(key, static_cast<std::int32_t>(step));
        }
      } else if (choice < 85) {
        const auto removed = map.remove(key);
        auto at = model.find(key);
        CHECK_EQ(removed.has_value(), at != model.end());
        if (at != model.end()) {
          CHECK_EQ(*removed, at->second);
          model.erase(at);
        }
      } else if (choice < 90 && !model.empty()) {
        const auto popped = map.pop_first();
        CHECK(popped.has_value());
        CHECK_EQ(popped->first, model.begin()->first);
        CHECK_EQ(popped->second, model.begin()->second);
        model.erase(model.begin());
      } else if (choice < 94 && !model.empty()) {
        const auto popped = map.pop_last();
        CHECK(popped.has_value());
        const auto last = std::prev(model.end());
        CHECK_EQ(popped->first, last->first);
        CHECK_EQ(popped->second, last->second);
        model.erase(last);
      } else {
        const auto* found = map.get(key);
        auto at = model.find(key);
        CHECK_EQ(found != nullptr, at != model.end());
        if (found != nullptr) CHECK_EQ(*found, at->second);
      }
      if (step % 400 == 0) map.check_invariants();
    }
    check_contents(map, model);
    check_neighbor_bounds(map, model);
    auto copy = map;
    check_contents(copy, model);
    const std::int32_t pivot = model.empty() ? 0 : model.begin()->first;
    auto upper = map.split_off(pivot);
    map.check_invariants();
    upper.check_invariants();
    map.append(upper);
    check_contents(map, model);
  }

  {
    shoal_test::Rng rng(19);
    TreeMap<std::string, std::uint64_t> map;
    std::map<std::string, std::uint64_t> model;
    for (int step = 0; step < 2500; ++step) {
      std::string key = adversarial_key(rng.below(500));
      if (rng.below(3) == 0) key += std::to_string(rng.below(40));
      const std::uint64_t choice = rng.below(100);
      if (choice < 60) {
        const auto old = map.insert(key, static_cast<std::uint64_t>(step));
        auto at = model.find(key);
        CHECK_EQ(old.has_value(), at != model.end());
        if (at != model.end()) at->second = static_cast<std::uint64_t>(step);
        else model.emplace(key, static_cast<std::uint64_t>(step));
      } else if (choice < 90) {
        const auto removed = map.remove(key);
        CHECK_EQ(removed.has_value(), model.erase(key) == 1);
      } else {
        const auto* found = map.get(std::string_view(key));
        auto at = model.find(key);
        CHECK_EQ(found != nullptr, at != model.end());
        if (found != nullptr) CHECK_EQ(*found, at->second);
      }
      if (step % 250 == 0) map.check_invariants();
    }
    check_contents(map, model);
    // Three grow-shrink cycles across the merge threshold.
    for (int round = 0; round < 3; ++round) {
      std::vector<std::string> keys;
      for (const auto& entry : model) keys.push_back(entry.first);
      for (std::size_t index = 0; index < keys.size(); index += 2) {
        CHECK(map.remove(keys[index]).has_value());
        model.erase(keys[index]);
      }
      map.check_invariants();
      for (std::size_t index = 0; index < keys.size(); index += 2) {
        map.insert(keys[index], static_cast<std::uint64_t>(round));
        model[keys[index]] = static_cast<std::uint64_t>(round);
      }
      map.check_invariants();
    }
    check_contents(map, model);
    check_neighbor_bounds(map, model);
  }
}

// Watched from a forked child, which only POSIX targets have.
#if defined(SHOAL_TEST_FORK)
TEST(unsync_allocation_failure_aborts) {
  // The unsync map cannot undo a split, so a failed page allocation aborts
  // (D10). InsertError is the shared map's refusal, not this one's.
  for (int pages : {0, 1, 2}) {
    const pid_t child = fork();
    CHECK(child >= 0);
    if (child == 0) {
      int left = pages;
      TreeMap<std::uint64_t, std::uint64_t, shoal::DefaultNodeConfig, FailAfter> map(FailAfter{&left});
      for (std::uint64_t key = 0; key < 100000; ++key) map.insert(key, key);
      _exit(2);
    }
    int status = 0;
    CHECK(waitpid(child, &status, 0) == child);
    CHECK(WIFSIGNALED(status) && WTERMSIG(status) == SIGABRT);
  }
}
#endif

}  // namespace

TEST_MAIN()
