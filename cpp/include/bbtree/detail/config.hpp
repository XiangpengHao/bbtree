// Assertions and code-generation hints shared by every header.
//
// - `BBTREE_ASSERT` checks a contract on the hot path, in debug builds only
//   (`BBTREE_DEBUG_CHECKS`), as Rust's `debug_assert!`.
// - `BBTREE_CHECK` checks one off the hot path always, as Rust's `assert!`:
//   a broken one ends the program.
// - `BBTREE_ALWAYS_INLINE` is Rust's `#[inline(always)]`: where a plan's
//   constant must fold into its caller (D41, D47); `BBTREE_NOINLINE` and
//   `BBTREE_COLD` are `#[inline(never)]` and `#[cold]`.
#pragma once

#include <cstdio>
#include <cstdlib>

#if defined(__GNUC__) || defined(__clang__)
#define BBTREE_ALWAYS_INLINE inline __attribute__((always_inline))
#define BBTREE_NOINLINE __attribute__((noinline))
#define BBTREE_COLD __attribute__((cold, noinline))
#define BBTREE_LIKELY(condition) __builtin_expect(!!(condition), 1)
#define BBTREE_UNLIKELY(condition) __builtin_expect(!!(condition), 0)
#else
#define BBTREE_ALWAYS_INLINE inline
#define BBTREE_NOINLINE
#define BBTREE_COLD
#define BBTREE_LIKELY(condition) (condition)
#define BBTREE_UNLIKELY(condition) (condition)
#endif

#if defined(__has_feature)
#if __has_feature(thread_sanitizer)
#define BBTREE_TSAN 1
#endif
#endif
#if defined(__SANITIZE_THREAD__)
#define BBTREE_TSAN 1
#endif

/// A function whose loads race with writers by design, checked by a version
/// afterwards (optimistic reads, PORTING.md P7), always inlined. Under
/// ThreadSanitizer it is kept out of line and uninstrumented instead: the
/// sanitizer instruments code after inlining, by the function it lands in.
#if defined(BBTREE_TSAN)
#define BBTREE_RACING_READ __attribute__((no_sanitize("thread"), noinline))
#else
#define BBTREE_RACING_READ BBTREE_ALWAYS_INLINE
#endif

namespace bbtree::detail {

struct ContractFailure {
  [[noreturn]] BBTREE_COLD static void fail(const char* condition, const char* file, int line) {
    std::fprintf(stderr, "bbtree: contract broken: %s at %s:%d\n", condition, file, line);
    std::abort();
  }
};

}  // namespace bbtree::detail

#define BBTREE_CHECK(condition)                                                        \
  do {                                                                                 \
    if (BBTREE_UNLIKELY(!(condition)))                                                 \
      ::bbtree::detail::ContractFailure::fail(#condition, __FILE__, __LINE__);         \
  } while (0)

#if defined(BBTREE_DEBUG_CHECKS) && BBTREE_DEBUG_CHECKS
#define BBTREE_ASSERT(condition) BBTREE_CHECK(condition)
#else
#define BBTREE_ASSERT(condition) ((void)0)
#endif
