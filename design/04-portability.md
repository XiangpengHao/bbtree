# 04 — Portability: compilers, standard libraries, SIMD paths

Status: built (2026-10-25). DuckDB builds with GCC, clang and MSVC, on
x86_64 and Arm, on Linux, macOS and Windows; the port was first built with
Apple clang alone. What it takes to build everywhere DuckDB does, and
what CI runs.

## One header for the target (D23)

Everything outside `src/shoal/detail/target.hpp` is standard C++17. That
header says what the machine provides:

- **Architectures and instruction sets,** as macros the node search picks
  its code with (OG-D41): `SHOAL_X86_64`, `SHOAL_ARM64`, and the x86
  extensions beyond SSE2. GCC and clang define a macro for each
  extension; MSVC defines `__AVX__`, `__AVX2__` and `__AVX512*__` for its
  `/arch` levels, where `/arch:AVX` brings POPCNT and `/arch:AVX2` BMI2.
- **Instructions C++17 has no word for:** bit counts, byte swaps, the
  spin wait (D19), and the relaxed loads optimistic readers use (D12), as
  GCC/clang builtins or MSVC intrinsics. clang-cl takes the GCC spelling.

Code-generation hints (`SHOAL_ALWAYS_INLINE`, `SHOAL_NOINLINE`,
`SHOAL_COLD`, `SHOAL_LIKELY`) are in `detail/config.hpp`, with MSVC's
`__forceinline` and `__declspec(noinline)`. The one prefetch is in
`Prefetcher` (D11), with MSVC's intrinsics beside the builtins.

## Standard libraries (D7)

The node code moves keys and values as bytes, so what a map may store
depends on the standard library:

| | libc++ | MSVC STL | libstdc++ |
|---|---|---|---|
| `std::string` | relocatable, 24 bytes | relocatable, 32 bytes | not relocatable (points into itself) |
| `std::vector`, `std::unique_ptr`, `std::shared_ptr` | relocatable | relocatable | relocatable |
| under checked iterators | — | refused (`_ITERATOR_DEBUG_LEVEL` > 0) | refused (`_GLIBCXX_DEBUG`) |

A refused type fails a `static_assert`: a DuckDB Debug build with checked
iterators can't store `std::string` or `std::vector` in a tree map, rather
than corrupt memory. The hash maps construct entries in place and take any
type with non-throwing moves (D25).

The layout tests check `std::string` itself (D7): Rust's numbers, worked
out by hand for its 24-byte `String`, where `std::string` is 24 bytes;
elsewhere the choices that hold at either size (a 32-byte string key
leaves room for hash tags in a set, which a 24-byte one doesn't), and the
layout code's own compile-time invariants for every shape.

## MSVC

Built with `/W4 /WX /permissive- /Zc:__cplusplus /utf-8`. What its first
runs turned up:

- **Warnings:** C4245, a 16-bit `~Word{0}` promoted to `int`, now cast
  back; C4324 (`CachePadded` pads on purpose) and C4702 ("unreachable
  code" after an inlined throw, which the hash maps' tests do) are off.
- **Over-aligned values can't go by value:** the owned hash map's
  `insert` takes references.
- **NEON types are one type** under MSVC; the search never overloads on
  them, so nothing collides.

## CI (D24)

`.github/workflows/cpp.yml`:

| job | compiler, library | paths run |
|---|---|---|
| Linux x86_64 | clang, libc++ | SSE2 (`-march=x86-64`), AVX2+BMI2+POPCNT (`x86-64-v3`), AVX-512 (`x86-64-v4`, runs where the runner has it) |
| Linux aarch64 | clang, libc++ | NEON |
| macOS arm64, x86_64 | Apple clang, libc++ | NEON; macOS's x86_64 baseline (SSE2) |
| Windows x64 | MSVC | SSE2, `/arch:AVX2`; `/arch:AVX512` built only |
| Windows arm64 | MSVC | NEON |
| sanitizers, Linux x86_64 and aarch64 | clang, libc++ | ASan+UBSan, TSan |

Not in CI: GCC with libstdc++. The library compiles with GCC 13 and
`-Werror` (checked in a container); the tests store `std::string`, which
libstdc++ can't relocate.

Sanitizer notes: TSan reads `tests/tsan.supp`, which names the racing
readers (D12, D26) and nothing else, so races between writers stay
reported. Linux ASan runs with `alloc_dealloc_mismatch=0`: Ubuntu's libc++
allocates an exception's message with `new` and its libc++abi frees it
with `free`, inside the standard library, whenever a test throws.

## Not measured

MSVC and x86 performance: the measurements are on an M4 Pro (02, 06).
AVX-512 runs only in CI.
