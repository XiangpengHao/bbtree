// A minimal test harness: `TEST(name) { ... }` registers a test, `CHECK`
// and `CHECK_EQ` fail it with the expression and the values, and `main` runs
// every test in registration order. Zero dependencies (D1, OG-D2).
#pragma once

#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <functional>
#include <sstream>
#include <string>
#include <vector>

namespace shoal_test {

class Registry {
 public:
  struct Case {
    const char* name;
    std::function<void()> body;
  };

  static Registry& instance() {
    static Registry registry;
    return registry;
  }

  void add(const char* name, std::function<void()> body) { cases_.push_back({name, std::move(body)}); }

  int run_all(int argc, char** argv) {
    const char* filter = argc > 1 ? argv[1] : nullptr;
    int run = 0;
    for (const Case& test : cases_) {
      if (filter != nullptr && std::string(test.name).find(filter) == std::string::npos) continue;
      std::printf("test %s ... ", test.name);
      std::fflush(stdout);
      test.body();
      std::printf("ok\n");
      ++run;
    }
    std::printf("%d tests passed\n", run);
    return 0;
  }

 private:
  std::vector<Case> cases_;
};

struct Registration {
  Registration(const char* name, std::function<void()> body) {
    Registry::instance().add(name, std::move(body));
  }
};

template <class T>
std::string show(const T& value) {
  if constexpr (std::is_same_v<T, bool>) {
    return value ? "true" : "false";
  } else if constexpr (std::is_enum_v<T>) {
    return std::to_string(static_cast<long long>(value));
  } else if constexpr (std::is_arithmetic_v<T>) {
    return std::to_string(value);
  } else if constexpr (std::is_convertible_v<T, std::string>) {
    return "\"" + std::string(value) + "\"";
  } else {
    return "<value>";
  }
}

[[noreturn]] inline void fail(const char* file, int line, const std::string& message) {
  std::printf("FAILED\n  %s:%d: %s\n", file, line, message.c_str());
  std::exit(1);
}

/// A deterministic random number generator, xorshift64*, as the Rust tests'
/// `TestRng`.
class Rng {
 public:
  explicit Rng(std::uint64_t seed) : state_(seed * 0x9E3779B97F4A7C15ull + 1) {}

  std::uint64_t next() {
    state_ ^= state_ >> 12;
    state_ ^= state_ << 25;
    state_ ^= state_ >> 27;
    return state_ * 0x2545F4914F6CDD1Dull;
  }

  /// A number in `0..bound`.
  std::uint64_t below(std::uint64_t bound) { return next() % bound; }

 private:
  std::uint64_t state_;
};

}  // namespace shoal_test

#define SHOAL_CONCAT_INNER(a, b) a##b
#define SHOAL_CONCAT(a, b) SHOAL_CONCAT_INNER(a, b)

#define TEST(name)                                                              \
  static void name();                                                           \
  static ::shoal_test::Registration SHOAL_CONCAT(name, _registration)(#name, name); \
  static void name()

#define CHECK(condition)                                                     \
  do {                                                                       \
    if (!(condition)) ::shoal_test::fail(__FILE__, __LINE__, #condition);   \
  } while (0)

#define CHECK_EQ(actual, expected)                                                   \
  do {                                                                               \
    const auto& shoal_actual = (actual);                                            \
    const auto& shoal_expected = (expected);                                        \
    if (!(shoal_actual == shoal_expected)) {                                       \
      ::shoal_test::fail(__FILE__, __LINE__,                                        \
                          std::string(#actual " == " #expected ": ") +               \
                              ::shoal_test::show(shoal_actual) + " vs " +          \
                              ::shoal_test::show(shoal_expected));                 \
    }                                                                                \
  } while (0)

#define TEST_MAIN() \
  int main(int argc, char** argv) { return ::shoal_test::Registry::instance().run_all(argc, argv); }
