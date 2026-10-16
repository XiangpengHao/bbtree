# 09 — Concurrency: BCTree, closures instead of handles

Status: phase 1 built (2026-10-15): `BCTreeMap` with `read`, `update`,
`get_cloned`, `contains_key` and `insert`; a tree that only grows. No
removal, `entry` or `scan` yet. `BBTreeMap` stays single-threaded (D4).

## Scope

A concurrent B-tree differs enough in design and cost that `BBTreeMap`
stays single-threaded, and the concurrent tree is a separate type, BCTree
(working name, `bctree_map/`). The two share the node code.

BCTree is about single-entry concurrency: latches that keep the tree
consistent while threads read and write it, one entry per operation. No
transactions, no atomicity across entries.

## Requirements

1. Users never take a latch by hand.
2. Users never hold an epoch, guard or handle. BCTree's API stays close to
   `BBTreeMap`'s.
3. A single thread never deadlocks with itself. Two threads may deadlock
   only where the semantics ask for it.

## API: closures, not handles

An operation that exposes a key or value runs the caller's closure while
the entry's leaf is latched, unlatches, and returns what the closure
returned. No reference, guard or handle outlives the call, so no latch
does either.

```rust
impl<K: Ord, V> BCTreeMap<K, V> {
    /// Runs `f` on the entry while its leaf is latched shared.
    fn read<Q, R>(&self, key: &Q, f: impl FnOnce(&K, &V) -> R) -> Option<R>;
    /// Runs `f` on the entry while its leaf is latched exclusive.
    fn update<Q, R>(&self, key: &Q, f: impl FnOnce(&K, &mut V) -> R) -> Option<R>;
    /// std's `Entry` API within one exclusive latch: insert or modify.
    fn entry<R>(&self, key: K, f: impl FnOnce(Entry<'_, K, V>) -> R) -> R;
    /// Visits the range one leaf at a time until `f` breaks.
    fn scan<Q, R>(&self, range: impl RangeBounds<Q>,
                  f: impl FnMut(&K, &V) -> ControlFlow<R>) -> Option<R>;

    fn get_cloned<Q>(&self, key: &Q) -> Option<V> where V: Clone {
        self.read(key, |_, v| v.clone())
    }
}
```

The types keep references from escaping: `impl FnOnce(&K, &V) -> R` is
`for<'a> FnOnce(&'a K, &'a V) -> R`, and `R` is fixed outside `'a`, so
`map.read(&k, |_, v| v)` doesn't compile. `std::thread::scope` and
`LocalKey::with` rely on the same rule. scc's `HashMap` (`read_sync`,
`update_sync`, `iter_sync`) and DashMap (`view`, `alter`) offer the same
shape.

One entry at a time: an entry can't be held across calls, there is no
`get_mut` handle, and an operation on two entries isn't atomic.

There is no `len`: a shared counter would cost every insert an atomic on
one line all threads write.

Rejected:

- **A handle that holds the leaf latch**, like DashMap's `Ref`. Its docs
  warn on `insert`, `remove`, `get_mut` and others: "May deadlock if called
  when holding any sort of reference into the map". That breaks
  requirement 3, and a held latch also blocks the leaf's splits.
- **A per-entry lock under an intention lock on the leaf.** A split needs
  the leaf exclusive, which conflicts with every intention lock, so a
  thread that holds an entry and then inserts into the same leaf can wait
  on itself; and a split moves entries to another leaf, so their locks
  can't live in it.

## Sharing the node code

BCTree's nodes are `Leaf`, `Internal` and `RootPage` with the strategy
`Latched<S>` (`tree/node/latch.rs`), a crate-private wrapper of the map's
strategy: the same inline keys, plus a latch word in every node's header.
The layouts take a `latched` flag, which puts the inline keys 16 bytes in
instead of 8 (`LatchedNodeHeader`); u64→u64 nodes keep 55 entries per leaf
and go from 49 to 48 children per internal node. The searches, splits and
prefix fits are bbtree's own, unchanged. The keys need inline keys: an
internal node read optimistically must keep no keys (D49).

What is BCTree's alone stays out of the shared paths, so that bbtree
compiles as before: a racing reader's single read of the header is its
own function (`NodePtr::load_racing_header`), and zeroed pages come from an
allocator wrapper (`ZeroedPages`, `tree/latched`), not from `FreshPage`.
The one shared change: the inline-key searches read their slots through a
raw `Slots` view instead of `&[W]`, so that a racing reader forms no
reference into a node a writer is changing.

## Latching

One word per node, our own (`NodeLatch`): from the low bit, an exclusive
bit, a count of shared holders (20 bits), and a version (43 bits). One word
serves both modes, so the root page keeps it when it turns from a leaf into
an internal node.

- **Inner nodes: optimistic.** A reader reads the version (waiting while
  the exclusive bit is set), reads the node, and checks the version after
  an acquire fence, ignoring the shared count. A writer takes the node by
  compare-and-swap from the version its descent read, never waiting:
  failure restarts the operation. Letting go bumps the version, or restores
  it for a node held but unchanged. bf-tree does the same
  (`src/utils/inner_lock.rs`: `try_read`, `check_version`, `upgrade`).
- **Leaves: pessimistic.** Shared for `read`, exclusive for `update` and
  `insert`, spinning with exponential backoff. No scheduler hint: the crate
  is `no_std` (D3). Releasing exclusively bumps the version too. Writers
  can starve under a steady stream of readers.
- **Descent** (`LatchedTree::descend`): optimistic through the inner
  nodes; at each, load the child pointer, read the child's version, then
  check the node's; at the leaf, latch it, then check its parent's version,
  so the leaf still covers the key. A root leaf checks the tree's root
  pointer instead. Any failed check restarts from the root.

Deadlock: a thread holds at most one leaf latch, and takes internal nodes
only by a compare-and-swap that never waits.

## Optimistic lock coupling

Readers read internal nodes in place, through raw pointers, while writers
holding a node change it in place with the node code as it is; the version
check throws away whatever a reader read meanwhile (`node/optimistic.rs`),
as in C++ OLC B-trees (Leis et al., "The ART of Practical Synchronization").
No copies on either side. A racing read forms no reference into the node,
so the aliasing model is not involved; what it reads before its check must
not crash it or run user code on garbage:

- **Routing** reads `len` and `prefix_len` once each (relaxed atomic loads,
  so the compiler loads them once), clamps the length to the capacity, and
  runs the shared inline-key search on the page's slots: any inline keys
  give an answer in bounds (D14).
- **Edges** are read as raw pointers; pages start zeroed, so an edge no
  writer stored is null, which restarts. A pointer read from a node is to a
  live page, of the kind its tag says: nothing is freed while the map
  lives, and no page but the root's changes kind.
- **Ties** read the separator's key, the head of the leftmost leaf right of
  it (D49), without a latch: the walk reads the node's edge, checks the
  node's version, and only then walks down first edges, which a published
  node never changes, to a head key, which never changes while its leaf is
  in a tree that only grows (a split leaves the left half's head in place,
  and gives the right half a new one before linking it). A walk that meets
  a null edge, a leaf without a head, or a changed version restarts.
  Removal will break this: a removed separator's successor replaces the
  head key in place (D22).

## Splits

An insert into a full leaf (`LatchedTree::split_leaf`) searches again,
keeping the path's nodes, versions, edges and lengths (D37's second
search). Holding the leaf exclusively, it takes, by compare-and-swap at
the versions it read, the nodes from the leaf's parent up to the first
with room, or the root if none has room; if the top node that splits is
its parent's first or last child, also the nearest node above holding that
fence, unchanged. Any failure lets go of everything and starts over.

It then splits in place, as bbtree does (`Leaf::split_insert`,
`Internal::insert`), from the leaf up; a root that splits raises the tree
in place (`RootPage::raise`), or for a root leaf, whose root page no
optimistic reader reads while the root pointer says it is a leaf, starting
its internal node on zeroed bytes (`RootPage::raise_latched_leaf`). Right
after each split, while the nodes above are still as the descent read them,
the halves' prefixes are fit to their fences (D26), so prefixes stay exact.
Then it retags the root pointer if the root was a leaf, and lets go. A
reader that read a node it changed fails its check: every node that changes
stays held until all are done, and new pages are reached only through
them.

## Performance

Ryzen 9 9900X, first die only (`taskset -c 0-5,12-17`), mimalloc, 10M
`u64 → u64`, one run each (2026-10-15).

One thread, ns per operation (`bbtree-bench --map bbtree,bctree`):

| cell | bbtree | bctree |
|---|--:|--:|
| `u64` get hit / miss | 128 / 93.6 | 130 / 98.8 |
| `u64` random / sequential insert | 147 / 5.34 | 138 / 35.7 |
| `u64-seq` get hit / miss | 132 / 93.7 | 169 / 125 |
| `hash32` get hit / miss | 149 / 107 | 156 / 119 |
| `str-json` get hit / miss | 210 / 157 | 235 / 171 |

The first version, which copied inner nodes in and out with atomics, ran
`u64` hits at 147 ns. `u64-seq` pays for the empty shared prefix, sequential
inserts for the missing append shortcut. bbtree's `hash32` random insert
(285 ns, BCTree 175) is slow in bbtree's own insert path, at HEAD too.

Threads (`bbtree-scaling`), millions of operations per second, workers
pinned one per core, then SMT siblings; `RwLock<BBTreeMap>` for contrast:

| threads | get | 90/10 get/update | insert | `RwLock` get | `RwLock` insert |
|--:|--:|--:|--:|--:|--:|
| 1 | 7.07 | 7.51 | 7.27 | 6.96 | 7.23 |
| 6 | 42.0 (5.9×) | 41.1 (5.5×) | 32.2 (4.4×) | 24.4 | 3.81 |
| 12 | 49.9 (7.1×) | 48.8 (6.5×) | 39.6 (5.4×) | 19.6 | 3.22 |

Lookups scale near linearly on the cores. Inserts scale less: each dirties
several leaf lines, and a profile at 6 threads shows no time spinning on
latches and under 1% in splits.

## What phase 1 leaves out

- **The shared prefix (D50) stays empty.** No insert shrinks it, so no
  insert takes the borders' nodes. Keys that share leading bytes, dense
  integers or common string prefixes, then tie at the root and along the
  borders, which bbtree's root skips.
- **No append shortcut (D45).** Ascending inserts descend each time.
- **Nothing is freed while the map lives**, so no reclamation scheme is
  needed yet.

## Scans

`scan` latches one leaf at a time. Between leaves it unlatches and resumes
by searching for the first key after the last one it returned. A scan is
not a snapshot: a change behind the cursor is missed and one ahead of it
is seen. A key present for the whole scan is visited exactly once.

## Panics

Whatever is simplest. The latch guard unlatches on unwind; there is no
poisoning; a value that a panicking `update` closure left half-changed
stays as it is.

## Open

- **Re-entry:** a closure that calls into the same map. Deferred. Known
  deadlocks: a shared latch taken again on the same leaf while a writer
  waits between the two; any call on the leaf from inside `update`; two
  threads in closures on leaves A and B that each call into the other's
  leaf.
- **Removal and merges:** a removed separator's successor replaces a head
  key in place, which racing tie reads then see half written; and merges
  free nodes that optimistic readers may be reading.
- **Memory reclamation:** what keeps a node an optimistic reader is reading
  from being freed (epochs, or type-stable nodes).
- **The shared prefix:** shrinking it takes every border node.

## Testing

Phase 1: differential tests against std one thread at a time, with the
tree's invariant check (exact prefixes included), and threaded tests:
writers whose keys readers check as they appear, and threads inserting and
updating the same keys (`bctree_map/tests.rs`). A 16-thread stress run
outside the repository (3.2M random and 1.6M dense integers, 800k strings,
with readers and updaters) passed with debug assertions on.

Next: Shuttle, for randomized schedules of the concurrent paths. Miri's
race detector flags OLC's racing reads by design, so the threaded tests
need `cfg_attr(miri, ignore)` in CI.
