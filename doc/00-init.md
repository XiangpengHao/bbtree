Blessed B-tree is designed to be a drop-in replacement of Rust's `std::collections::BTreeMap`, `BBTreeMap` and `BBTreeSet`.

So the hard constraints are that we want to keep almost the same API as Rust's, and we should promose almost the same behavior. Unless there's a strong reason to deviate, in that case, we explicitly document in the README.md.

What makes bbtree different is that we are more performant and more memory efficient.

## First principles

- Design BBTree with the mindset that any exsiting users of std BTreeMap/BTreeSet should be able to switch to BBTree without any code changes. But not the other way around.

- High performance is our differentiator; correctness is the long term value proposition.

- Large data first, small data not ugly. People whose data is small don't care about performance, so they won't bother switching to bbtree; only people with sufficiently large data will. So our goal is to make large data very, very efficient, and meanwhile not make small data too ugly: when the two conflict, large data wins, as long as small data stays reasonably close to std.

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

- We need to compare integration with some AI stuff, like codex-cli open source?

- We want to integrate with DataFusion sortmerge join, the SlateDB, and other LSM-tree or B-Tree based systems. 

- We want to measure memory usage, latency, and latency variance.

- When we say drop-in replacement, we're going to replace every std BTree in top 100 crates on crates.io, run their test cases, and confirm they all compile and pass.

