// What the target machine provides: its architecture and instruction sets,
// as macros the node code picks its search with (D41), and the few
// instructions the tree needs that C++17 has no word for: bit counts, byte
// swaps, a spin wait and racing loads. GCC and clang (clang-cl too) spell
// them as builtins, MSVC as intrinsics; everything else in the headers is
// standard C++17.
#pragma once

#include <cstddef>
#include <cstdint>
#include <cstring>

#if defined(_MSC_VER) && !defined(__clang__)
#define BBTREE_MSVC 1
#include <intrin.h>
#include <stdlib.h>
#endif

// Architectures.
#if defined(__x86_64__) || (defined(_M_X64) && !defined(_M_ARM64EC))
#define BBTREE_X86_64 1
#elif defined(__aarch64__) || defined(_M_ARM64)
#define BBTREE_ARM64 1
#endif

// x86 extensions beyond x86-64's SSE2. GCC and clang define a macro for each;
// MSVC defines `__AVX__`, `__AVX2__`, `__AVX512F__` and `__AVX512BW__` for
// its `/arch` levels, where `/arch:AVX` brings POPCNT and `/arch:AVX2` BMI2.
#if defined(BBTREE_X86_64)
#if defined(__BMI2__) || (defined(BBTREE_MSVC) && defined(__AVX2__))
#define BBTREE_BMI2 1
#endif
#if defined(__POPCNT__) || (defined(BBTREE_MSVC) && defined(__AVX__))
#define BBTREE_POPCNT 1
#endif
#if defined(__AVX512F__)
#define BBTREE_AVX512F 1
#endif
#if defined(__AVX512BW__)
#define BBTREE_AVX512BW 1
#endif
#endif

#if defined(BBTREE_X86_64)
#include <immintrin.h>
#endif

namespace bbtree::detail {

/// Bit counts of 64-bit words.
struct Bits {
  /// `bits` with all but its `count` lowest cleared, `count <= 64`: one `bzhi`
  /// where BMI2 is there.
  static inline std::uint64_t keep_low(std::uint64_t bits, unsigned count) noexcept {
#if defined(BBTREE_BMI2)
    return _bzhi_u64(bits, count);
#else
    return count >= 64 ? bits : bits & ((std::uint64_t{1} << count) - 1);
#endif
  }

  static inline unsigned count_ones(std::uint64_t bits) noexcept {
#if !defined(BBTREE_MSVC)
    return static_cast<unsigned>(__builtin_popcountll(bits));
#elif defined(BBTREE_X86_64) && defined(BBTREE_POPCNT)
    return static_cast<unsigned>(__popcnt64(bits));
#elif defined(BBTREE_ARM64)
    return static_cast<unsigned>(_CountOneBits64(bits));
#else
    bits = bits - ((bits >> 1) & 0x5555555555555555ull);
    bits = (bits & 0x3333333333333333ull) + ((bits >> 2) & 0x3333333333333333ull);
    bits = (bits + (bits >> 4)) & 0x0F0F0F0F0F0F0F0Full;
    return static_cast<unsigned>((bits * 0x0101010101010101ull) >> 56);
#endif
  }

  /// 64 for no bits set.
  static inline unsigned trailing_zeros(std::uint64_t bits) noexcept {
    if (bits == 0) return 64;
#if !defined(BBTREE_MSVC)
    return static_cast<unsigned>(__builtin_ctzll(bits));
#else
    unsigned long index;
    _BitScanForward64(&index, bits);
    return static_cast<unsigned>(index);
#endif
  }

  static inline unsigned trailing_ones(std::uint64_t bits) noexcept { return trailing_zeros(~bits); }

  /// 64 for no bits set.
  static inline unsigned leading_zeros(std::uint64_t bits) noexcept {
    if (bits == 0) return 64;
#if !defined(BBTREE_MSVC)
    return static_cast<unsigned>(__builtin_clzll(bits));
#else
    unsigned long index;
    _BitScanReverse64(&index, bits);
    return 63 - static_cast<unsigned>(index);
#endif
  }
};

/// Byte order: every target the headers build for is little-endian but
/// big-endian GCC targets, which swap nothing.
struct ByteOrder {
  static inline std::uint64_t big_endian64(std::uint64_t word) noexcept {
#if defined(__BYTE_ORDER__) && __BYTE_ORDER__ == __ORDER_BIG_ENDIAN__
    return word;
#elif defined(BBTREE_MSVC)
    return _byteswap_uint64(word);
#else
    return __builtin_bswap64(word);
#endif
  }

  static inline std::uint32_t big_endian32(std::uint32_t word) noexcept {
#if defined(__BYTE_ORDER__) && __BYTE_ORDER__ == __ORDER_BIG_ENDIAN__
    return word;
#elif defined(BBTREE_MSVC)
    return _byteswap_ulong(word);
#else
    return __builtin_bswap32(word);
#endif
  }
};

/// One wait of a spinning thread, as Rust's `core::hint::spin_loop`: `pause`
/// on x86, `isb` on Arm, where `yield` waits for nothing on most cores.
struct SpinWait {
  static inline void relax() noexcept {
#if defined(BBTREE_X86_64)
    _mm_pause();
#elif defined(BBTREE_ARM64) && defined(BBTREE_MSVC)
    __isb(_ARM64_BARRIER_SY);
#elif defined(BBTREE_ARM64)
    __asm__ __volatile__("isb sy" ::: "memory");
#endif
  }
};

/// A load of a word that writers may be storing meanwhile, as a relaxed
/// atomic load: what an optimistic reader reads before its version check
/// (PORTING.md, P7). `T` is an integer or pointer of 1, 2, 4 or 8 bytes.
struct RacingLoad {
  template <class T>
  static inline T relaxed(const T* word) noexcept {
#if !defined(BBTREE_MSVC)
    return __atomic_load_n(word, __ATOMIC_RELAXED);
#else
    static_assert(sizeof(T) == 1 || sizeof(T) == 2 || sizeof(T) == 4 || sizeof(T) == 8);
    T value;
    if constexpr (sizeof(T) == 1) {
      const auto bits = __iso_volatile_load8(reinterpret_cast<const volatile __int8*>(word));
      std::memcpy(&value, &bits, sizeof(T));
    } else if constexpr (sizeof(T) == 2) {
      const auto bits = __iso_volatile_load16(reinterpret_cast<const volatile __int16*>(word));
      std::memcpy(&value, &bits, sizeof(T));
    } else if constexpr (sizeof(T) == 4) {
      const auto bits = __iso_volatile_load32(reinterpret_cast<const volatile __int32*>(word));
      std::memcpy(&value, &bits, sizeof(T));
    } else {
      const auto bits = __iso_volatile_load64(reinterpret_cast<const volatile __int64*>(word));
      std::memcpy(&value, &bits, sizeof(T));
    }
    return value;
#endif
  }
};

}  // namespace bbtree::detail
