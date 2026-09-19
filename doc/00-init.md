Blessed B-tree is designed to be a drop-in replacement of Rust's `std::collections::BTreeMap`, `BBTreeMap` and `BBTreeSet`.

So the hard constraints are that we want to keep almost the same API as Rust's, and we should promose almost the same behavior. Unless there's a strong reason to deviate, in that case, we explicitly document in the README.md.

What makes bbtree different is that we are more performant and more memory efficient.

## First principles

- Design BBTree with the mindset that "it might be merged into Rust's stdlib someday", anything breaking this should be flagged.

- High performance is our differentiator; correctness is the long term value proposition.


## Design

- BBTree is zero-dependency, with optional features like `serde` and `rand`.

- BBTree supports `no_std` (might need alloc?).

- Every allocation in BBTree is counted, known before hand, no runtime surprises. We support system allocator, and custom allocator. Basically we have our own allocator trait.

- BBTree design is very much like the high-performance b-tree in databases: nodes are 4KB in size. We might need to use Rust's unsafe for some of the memory packing etc, but it's fine. I think this is one of the primary gain of performance. But we need to consider where the generic K/V is larger than 4KB.

- For now BBTree is single threaded, just like Rust's.

- Author of BBTree, Xiangpeng Hao, is author of [bf-tree](https://github.com/microsoft/bf-tree/), so be professional.

## Correctness

- We are obessed with correctness, as BBTree is designed to "offload" accountability. Dealing with low level datastructure is hard, and that's what we are good at, and that's the spine of our value proposition.

- Fuzzing is of course a must. We do differential fuzzing with Rust's btree.

- Formal verification with verus is optional, but welcomed.

## Marketing

- We're going to compare with Rust's btree, as well as other b-tree implementations.

- We're going to compare with hash tables and adaptive radix trees (even though it's not a fair comparison).

## Rust's btree

- My understanding of Rust's btree is that each node is like a pair of (Vec<K>, Vec<V>), which can be extremely wasteful in terms of memory layout and cache efficiency.
