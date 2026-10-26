// Operations on two `shoal::unsync::TreeSet<uint64_t>`, folded into a
// count, as the Rust reference's `set` suite timed the Rust set: two sets
// that share half their elements, walked side by side; and a set with a
// 37th of one of them, looked up in it.
//
// usage: set_bench [elements]; `SET_ROUNDS` repeats each operation.
#include <cstdint>
#include <cstdlib>
#include <vector>

#include "shoal/unsync/set.hpp"
#include "workload.hpp"

using shoal::unsync::TreeSet;
using shoal_bench::Keys;
using shoal_bench::Stopwatch;

namespace {

TreeSet<std::uint64_t> set_of(std::uint64_t first, std::uint64_t count, std::uint64_t step) {
  std::vector<std::uint64_t> elements;
  for (std::uint64_t index = 0; index < count; ++index) elements.push_back(Keys::mix(first + index * step));
  return TreeSet<std::uint64_t>::from_elements(std::move(elements));
}

template <class Operation>
void time(const char* name, std::size_t elements, std::size_t rounds, Operation operation) {
  Stopwatch watch;
  std::size_t count = 0;
  for (std::size_t round = 0; round < rounds; ++round) count += operation();
  shoal_bench::keep(count);
  shoal_bench::report(name, elements, watch.ns_per(elements * rounds));
}

}  // namespace

int main(int argc, char** argv) {
  const std::size_t elements = argc > 1 ? std::strtoull(argv[1], nullptr, 10) : 1000000;
  const char* rounds_variable = std::getenv("SET_ROUNDS");
  const std::size_t rounds = rounds_variable ? std::strtoull(rounds_variable, nullptr, 10) : 1;
  const TreeSet<std::uint64_t> a = set_of(0, elements, 1);
  const TreeSet<std::uint64_t> b = set_of(elements / 2, elements, 1);
  const TreeSet<std::uint64_t> small = set_of(0, elements / 37, 37);
  auto count = [](auto operation) {
    std::size_t counted = 0;
    operation.for_each([&](const std::uint64_t&) { ++counted; });
    return counted;
  };
  time("set difference", elements, rounds, [&] { return count(a.difference(b)); });
  time("set intersection", elements, rounds, [&] { return count(a.intersection(b)); });
  time("set union", elements, rounds, [&] { return count(a.union_with(b)); });
  time("set intersection small", elements / 37, rounds, [&] { return count(small.intersection(a)); });
  time("set is_subset small", elements / 37, rounds, [&] { return std::size_t(small.is_subset(a)); });
  return 0;
}
