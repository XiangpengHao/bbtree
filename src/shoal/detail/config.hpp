// Assertions and code-generation hints shared by every header.
//
// - `SHOAL_ASSERT` checks a contract on the hot path, in debug builds only
//   (`SHOAL_DEBUG_CHECKS`), as Rust's `debug_assert!`.
// - `SHOAL_CHECK` checks one off the hot path always, as Rust's `assert!`:
//   a broken one ends the program.
// - `SHOAL_ALWAYS_INLINE` is Rust's `#[inline(always)]`: where a plan's
//   constant must fold into its caller (D41, D47); `SHOAL_NOINLINE` and
//   `SHOAL_COLD` are `#[inline(never)]` and `#[cold]`.
#pragma once

#include <cstdio>
#include <cstdlib>

#if defined(__GNUC__) || defined(__clang__)
#define SHOAL_ALWAYS_INLINE inline __attribute__((always_inline))
#define SHOAL_NOINLINE __attribute__((noinline))
#define SHOAL_COLD __attribute__((cold, noinline))
#define SHOAL_LIKELY(condition) __builtin_expect(!!(condition), 1)
#define SHOAL_UNLIKELY(condition) __builtin_expect(!!(condition), 0)
#elif defined(_MSC_VER)
#define SHOAL_ALWAYS_INLINE __forceinline
#define SHOAL_NOINLINE __declspec(noinline)
#define SHOAL_COLD __declspec(noinline)
#define SHOAL_LIKELY(condition) (condition)
#define SHOAL_UNLIKELY(condition) (condition)
#else
#define SHOAL_ALWAYS_INLINE inline
#define SHOAL_NOINLINE
#define SHOAL_COLD
#define SHOAL_LIKELY(condition) (condition)
#define SHOAL_UNLIKELY(condition) (condition)
#endif

#if defined(__has_feature)
#if __has_feature(thread_sanitizer)
#define SHOAL_TSAN 1
#endif
#endif
#if defined(__SANITIZE_THREAD__)
#define SHOAL_TSAN 1
#endif

/// A function whose loads race with writers by design, checked by a version
/// afterwards (optimistic reads, PORTING.md P7), always inlined. Under
/// ThreadSanitizer it is kept out of line and uninstrumented instead: the
/// sanitizer instruments code after inlining, by the function it lands in.
#if defined(SHOAL_TSAN)
#define SHOAL_RACING_READ __attribute__((no_sanitize("thread"), noinline))
#else
#define SHOAL_RACING_READ SHOAL_ALWAYS_INLINE
#endif

namespace shoal::detail {

struct ContractFailure {
  [[noreturn]] SHOAL_COLD static void fail(const char* condition, const char* file, int line) {
    std::fprintf(stderr, "shoal: contract broken: %s at %s:%d\n", condition, file, line);
    std::abort();
  }
};

}  // namespace shoal::detail

#define SHOAL_CHECK(condition)                                                        \
  do {                                                                                 \
    if (SHOAL_UNLIKELY(!(condition)))                                                 \
      ::shoal::detail::ContractFailure::fail(#condition, __FILE__, __LINE__);         \
  } while (0)

#if defined(SHOAL_DEBUG_CHECKS) && SHOAL_DEBUG_CHECKS
#define SHOAL_ASSERT(condition) SHOAL_CHECK(condition)
#else
#define SHOAL_ASSERT(condition) ((void)0)
#endif
