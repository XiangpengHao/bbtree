// Node configs (D59): what a map may tune about its nodes, the sizes of
// their pages, as constants of one type. A config derives from
// `DefaultNodeConfig` and redefines the constants it changes:
//
//     struct LargeLeaves : bbtree::DefaultNodeConfig {
//       static constexpr std::size_t leaf_page_size = 4096;
//     };
//
// A page size is a power of two. A node is the smallest multiple of its
// kind's page size that holds 12 keys, aligned to that page size, or more if
// its keys or values need it. A config that breaks a bound fails to compile.
#pragma once

#include <cstddef>

namespace bbtree {

struct DefaultNodeConfig {
  /// The page size of leaves: at least 1024. Larger leaves are fewer, but an
  /// insert or a remove shifts more of a leaf's bytes.
  static constexpr std::size_t leaf_page_size = 1024;
  /// The page size of internal nodes of keys with inline keys, which keep
  /// only the inline keys and their children: at least 64.
  static constexpr std::size_t internal_page_size = 512;
  /// The page size of internal nodes of keys without inline keys, which keep
  /// their keys: at least 64.
  static constexpr std::size_t plain_internal_page_size = 1024;
};

}  // namespace bbtree
