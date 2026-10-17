# bbtree

[![test](https://github.com/XiangpengHao/bbtree/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/XiangpengHao/bbtree/actions/workflows/ci.yml?query=branch%3Amain)
[![miri](https://github.com/XiangpengHao/bbtree/actions/workflows/miri.yml/badge.svg?branch=main)](https://github.com/XiangpengHao/bbtree/actions/workflows/miri.yml?query=branch%3Amain)
[![sanitizer](https://github.com/XiangpengHao/bbtree/actions/workflows/sanitizer.yml/badge.svg?branch=main)](https://github.com/XiangpengHao/bbtree/actions/workflows/sanitizer.yml?query=branch%3Amain)
[![fuzzing](https://github.com/XiangpengHao/bbtree/actions/workflows/fuzz.yml/badge.svg?branch=main)](https://github.com/XiangpengHao/bbtree/actions/workflows/fuzz.yml?query=branch%3Amain)

Blessed B-tree:
1. An ordered map that threads share, `BBTreeMap`, and one for a single
   thread, `unsync::BBTreeMap`, a drop-in replacement for std's
   `BTreeMap`/`BTreeSet`, on the same nodes.
2. Zero dependencies, `no_std` compatible.
3. x% lower memory usage, y% faster search, z% lower tail latency.
4. Production-ready by formal verification, model checking, fuzzing, and lots of testing.
5. Brought to you by decades of database research and careful curation from [Xiangpeng Hao](https://xiangpeng.systems).

See it find a key at [bbtree.xiangpeng.systems](https://bbtree.xiangpeng.systems).

## usage

```bash
cargo add bbtree
```

Threads share a `BBTreeMap` through `&self`. A lookup copies a value out,
or runs a closure on the entry; nothing it hands out outlives the call:

```rust
use bbtree::BBTreeMap;

let map = BBTreeMap::new();
std::thread::scope(|scope| {
    scope.spawn(|| map.insert(1_u64, 10_u64));
    scope.spawn(|| map.insert(2, 20));
});
assert_eq!(map.get_copied(&1), Some(10));
assert_eq!(map.read(&2, |_, value| value + 1), Some(21));
assert_eq!(map.get_many_copied([&1, &2, &3]), [Some(10), Some(20), None]);
```

One thread at a time, `unsync::BBTreeMap` and `unsync::BBTreeSet` have
std's API:

```rust
use bbtree::unsync::BBTreeMap;

let mut map = BBTreeMap::new();
map.insert(1, "one");
map.insert(2, "two");
assert_eq!(map.get(&1), Some(&"one"));
```

## license

Licensed under either of Apache License, Version 2.0 or MIT license, at
your option; [LICENSE](LICENSE) holds both texts.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in bbtree by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
