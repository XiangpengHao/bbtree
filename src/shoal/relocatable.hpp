// Trivial relocation (PORTING.md, P2): a type whose objects may move to
// another address as their bytes, the source then treated as gone, without
// its move constructor or destructor running. The nodes move keys and values
// that way: shifts within a node, runs between siblings, whole pages
// (design/03-decisions.md, D9, D14).
#pragma once

#include <memory>
#include <string>
#include <type_traits>
#include <vector>

namespace shoal {

/// Whether `T` is trivially relocatable. Specialize it for a type of your
/// own that holds no pointer into itself and registers its address nowhere.
template <class T>
struct is_trivially_relocatable : std::is_trivially_copyable<T> {};

// Checked iterators register with their container: MSVC's when
// `_ITERATOR_DEBUG_LEVEL` is above 0, as in its Debug builds, and libstdc++'s
// debug mode. A container that keeps them points into nothing of its own but
// is pointed at, so it doesn't move as its bytes.
#if (defined(_ITERATOR_DEBUG_LEVEL) && _ITERATOR_DEBUG_LEVEL != 0) || defined(_GLIBCXX_DEBUG)
#define SHOAL_CHECKED_ITERATORS 1
#endif

#if !defined(SHOAL_CHECKED_ITERATORS)
template <class T, class Allocator>
struct is_trivially_relocatable<std::vector<T, Allocator>>
    : std::is_trivially_copyable<Allocator> {};
#endif

template <class T, class Deleter>
struct is_trivially_relocatable<std::unique_ptr<T, Deleter>>
    : is_trivially_relocatable<Deleter> {};

template <class T>
struct is_trivially_relocatable<std::shared_ptr<T>> : std::true_type {};

#if defined(_LIBCPP_VERSION) || (defined(_MSVC_STL_VERSION) && !defined(SHOAL_CHECKED_ITERATORS))
// libc++'s and MSVC's short strings keep their bytes in place, without a
// pointer to them: a string moves as its bytes. libstdc++'s point into
// themselves.
template <class Char, class Traits, class Allocator>
struct is_trivially_relocatable<std::basic_string<Char, Traits, Allocator>>
    : std::is_trivially_copyable<Allocator> {};
#endif

template <class T>
inline constexpr bool is_trivially_relocatable_v = is_trivially_relocatable<T>::value;

}  // namespace shoal
