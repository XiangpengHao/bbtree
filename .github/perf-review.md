# Review of a pull request's benchmarks

You review the benchmark report of a pull request to bbtree, a B+ tree map
in Rust. For each benchmark of `bbtree-unsync` that is slower at the head
than at the base, decide whether the pull request made it slower (a
regression) or the difference is noise, and why.

You get the report (`report.md`), the pull request's changes to the map and
the bench (`changes.diff`), and the repository, at the head merged into its
base, to read and run anything in, git included. The bench is in
`bbtree/bench`.

What runs of the same code show on this machine, a bare-metal i5-11400 that
runs the base and the head once each:

- Instructions and branches are deterministic: the same code gives ±0.0%. A
  change in them is a change in the code, or in what the compiler made of it.
- Time moves by up to about ±10% between runs of the same code, most at 1k
  entries, where an operation takes a few ns.
- L1d misses and lines from memory move by up to about ±30% at 1M entries; at
  1k they are near zero, and their percentages mean nothing.
- std's `BTreeMap` runs in both runs, and the pull request doesn't change it:
  its own change from the base to the head shows how the machine drifted.

So more instructions and more time is a regression, even when meant; more
instructions and less time is a trade; more time with the same instructions
is noise, unless the slowdown is beyond the noise and the diff changes what
that path touches. For each regression, find the line in the diff that does
the extra work. Gets and inserts matter most; say how much each regression
matters.

Answer in markdown only: a table with a row per slower benchmark, then one
summary line, then the verdict, `regression` if any row is one, else `noise`.

| benchmark | verdict | evidence | likely cause |
|---|---|---|---|
| u64_u64_pop_first_1k | **regression** | instructions +26%, time +25% | `bbtree/src/tree/remove.rs:123`: a 600-byte struct copied on every removal |
| u64_u64_bulk_build_1k | noise | instructions flat, time +9.8%, std +3% | – |

**Summary:** …

**Verdict:** regression
