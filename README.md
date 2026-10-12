# bbtree

[![CI](https://github.com/XiangpengHao/bbtree/actions/workflows/ci.yml/badge.svg)](https://github.com/XiangpengHao/bbtree/actions/workflows/ci.yml)

Blessed B-tree:
1. Drop-in replacement for std's `BTreeMap`/`BTreeSet`.
2. Zero dependencies.
3. x% lower memory usage, y% faster search, z% lower tail latency.
4. Production-ready by formal verification, model checking, fuzzing, and lots of testing.
5. Brought to you by decades of database research and careful curation from [Xiangpeng Hao](https://xiangpeng.systems).

## usage

```bash
cargo add bbtree
```

```rust
use bbtree::{BBTreeMap, BBTreeSet};

let mut map = BBTreeMap::new();
map.insert(1, "one");
map.insert(2, "two");
map.insert(3, "three");

println!("{:?}", map);
```

## license

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in bbtree by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
