# 09 — Concurrency: BCTree, closures instead of handles

Status: built (2026-10-15): `BCTreeMap` with `read`, `update`,
`get_cloned`, `contains_key`, `insert`, `remove` and `remove_entry`: bbtree's
tree, shared through optimistic lock coupling, its tree operations
bbtree's own. No `entry` or `scan` yet. `BBTreeMap` stays single-threaded
(D4).

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

## One tree, shared

BCTree is bbtree's tree: the same nodes, `Leaf`, `Internal` and `RootPage`,
the same shared prefix (D50) and prefixes fit to their fences, the same
4-byte windows of heap strings in internal nodes (D43), the same root that
grows and shrinks in its page, and an append to the last leaf (D45). Its
nodes have the strategy `Latched<S>` (`tree/node/latch.rs`), a
crate-private wrapper of the map's: the same inline keys, plus a latch word
in every node's header. The layouts put the inline keys 16 bytes in
instead of 8 (`LatchedNodeHeader`); u64→u64 nodes keep 55 entries per leaf
and go from 49 to 48 children per internal node. The keys need inline keys:
an internal node read optimistically must keep no keys (D49).

Its tree operations are bbtree's own too. A split, a removal that
rebalances, and a shrinking of the shared prefix first take every node
bbtree's operation will change or read a separator of, without waiting,
then run it along a node stack from the topmost node they hold
(`NodeStackMut::along`, "Changes to more than a leaf" below). BCTree's own
code is what sharing needs and nothing else (`tree/latched/`):

- the optimistic descent (`descent.rs`);
- which nodes each change takes (`insert.rs`, `remove.rs`,
  `shared_prefix.rs`), and taking them (`held.rs`);
- type-stable pages (`pages.rs`);
- the pointer to the last leaf that appends go to (`append.rs`).

A first version rewrote the tree layer, and left out the shared prefix,
fitting prefixes after removals, lowering the tree to a leaf, 4-byte
windows and appends. Each cut put a special case into shared code, a flag
in the node plan and in the invariant check, a root shape bbtree never
has, or made another path hot: with an empty shared prefix, dense integers
tied at the root and along the borders, and once removal latched ties,
`u64-seq` hits took 205 ns.

bbtree changed in three places for it, none on its lookup paths: the
inline-key searches read their slots through a raw `Slots` view, so that a
racing reader forms no reference into a node a writer is changing; a node
stack can start along a path a descent kept; and a removal's fixing decides
whether to go up from the node it is at, reading no node above the last it
changes, and lowers the root only if it emptied it.

## Latching

One word per node, our own (`NodeLatch`): from the low bit, an exclusive
bit, a count of shared holders (20 bits), and a version (43 bits). One word
serves both modes, so the root page keeps it when it turns from a leaf into
an internal node and back.

- **Inner nodes: optimistic.** A reader reads the version (waiting while
  the exclusive bit is set), reads the node, and checks the version after
  an acquire fence, ignoring the shared count. A writer takes the node by
  compare-and-swap from the version its descent read, never waiting:
  failure restarts the operation. Letting go bumps the version. bf-tree
  does the same (`src/utils/inner_lock.rs`: `try_read`, `check_version`,
  `upgrade`).
- **Leaves: pessimistic.** Shared for `read`, exclusive for `update` and
  `insert`, spinning with exponential backoff. No scheduler hint: the crate
  is `no_std` (D3). Releasing exclusively bumps the version too. Writers
  can starve under a steady stream of readers.
- **Descent** (`descent.rs`): a key that lacks what every key shares is in
  no node, which the descent says once the root's version vouches for the
  shared prefix it read, or with the root leaf latched. Optimistic through
  the inner nodes: at each, route the key, load the child pointer, check
  the node's version, read the child's version, then check the node's
  again; at the leaf, latch it, then check its parent's version, so the
  leaf still covers the key. A root leaf checks the tree's root pointer
  instead, once latched, and an internal root checks it once its version
  is read: a lowering to a leaf that finished between the pointer's read
  and the version's leaves a version that checks out on a leaf, but
  retagged the pointer first. Any failed check restarts from the root.

Deadlock: an operation holds at most one leaf latch while it waits, and
takes everything else without waiting. Pages it allocates it latches
waiting, for a thread that reached a free page through a stale pointer may
hold it a moment, waiting on nothing.

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
  give an answer in bounds (D14). The shared prefix is copied out of the
  root page, as raw bytes.
- **Edges** are followed only once the node's version vouches for them. A
  pointer read from a node is then to a node page of the kind its tag says,
  live or free (type-stable nodes, below). The check before following is
  for the root page, the one page that changes kind: an edge read from it
  may be a leaf's bytes.
- **Ties** compare the separator's key, the head of the leftmost leaf right
  of it (D49), with that leaf latched shared: the walk reads the node's
  edge, checks the node's version, walks down first edges, which only a
  thread holding the node changes, latches the leaf, and checks the node's
  version again before comparing. The latch keeps the head in place: a
  removal takes out a head with its leaf latched exclusively, and its key's
  memory may be freed at once. Keys ordered by their own bytes alone, the
  std integers and byte arrays `Auto` recognizes, skip the latch: the walk
  copies the head's key, compares the copy, and checks the node's version
  after, as the head changes only while the node is held. `u64` keeps
  2-byte windows in internal nodes, so about one descent in nine ties
  somewhere; with the latch, each tie wrote two atomics to a leaf every
  other thread's ties write too. A walk that meets a null edge, a leaf
  without a head, or a changed version restarts.
- **A separator match** is checked at the leaf: the descent checks only
  the leaf's parent, and a separator matched higher up may have been
  removed since, its successor the leaf's head now. The latched leaf's head
  must be the key, or the descent starts over.

## Changes to more than a leaf

A change that a leaf can't take alone descends again, keeping each internal
node it passes with the version it read, the edge it took and the length
(D37's second search, `TreePath`). Holding its leaf exclusively, it takes,
without waiting (`held.rs`), internal nodes on the path by compare-and-swap
at the versions it read, so that each is still the parent of the next, and
nodes beside the path at the versions they are at, reached through nodes
it holds. Any node held by another thread, or changed, lets go of all and
starts over. Then it runs bbtree's operation along a node stack from the
topmost node it holds, as if that were the root, on the tree as it is
(`LatchedTree::change_held`), and lets go once all is done: a reader that
read a node meanwhile fails its check. Pages it allocates come latched and
stay so until then (`pages.rs`), and a change of the root's kind retags the
tree's root pointer before the root is let go.

What each takes, so that bbtree's operation changes nothing and reads no
separator it doesn't hold:

- **A split**: the nodes from the leaf's parent up to the lowest with room,
  which takes the separator, or the root if none has room.
- **A removal that rebalances**: for each level it leaves underfull, from
  the leaf up, the parent and the sibling the child merges with or takes an
  entry from, as `Internal::fix_underfull_child` decides. For a separator,
  also the node holding it, the nodes on the path from there down, whose
  lower fence its successor becomes, and the nodes along the right border
  of the subtree left of it, whose upper fence it becomes. A stress run
  found that the path below the separator is needed: a descent that went
  right of the separator for a key below its successor checks only the
  leaf's parent.
- **Fences** of the topmost node either changes, which the changed nodes'
  prefixes are fit to (`tree/prefix.rs`): the nodes up to the nearest
  above it with a separator on each side of the path
  (`TreePath::fences_top`). A side without one is the tree's border, which
  a node stays on whatever others change, and whose fence is what every
  key shares. Fence values move but never change under a split, merge or
  move between siblings elsewhere: separators go up and come down whole.
  So prefixes stay exact, as bbtree's.
- **Shrinking the shared prefix**, for a key that lacks it: the root and
  every node along both borders (`shared_prefix.rs`). An operation holding
  a border node holds the shared prefix still.

Holding a node holds its separators' keys, though they are heads of leaves
below (D49): a head changes only when its separator is removed, replaced
by its successor, or rotated through the node by a merge or a move between
siblings, each holding the node. So bbtree's operations read every key
they need where it is: a separator's, a fence's, or a window's other bytes
when a node hands out a 4-byte window of a heap string (D43).

## Appends

The tree points at its last leaf, which only a thread holding that leaf
exclusively changes (`append.rs`): a split of it points it at the leaf last
after the split, found down the last edges of the node that took the
separator; a merge that frees it, or a lowering that moves it into the
root page, at the leaf that took its entries. An insert latches the leaf
the pointer names, and if the pointer still names it, the leaf is last and
stays so while held: a key greater than its last key, that shares what
every key shares, goes in if the leaf has room, as bbtree's append (D45).
A new tree's root leaf is latched until the pointer names it.

## Performance

Ryzen 9 9900X, first die only (`taskset -c 0-5,12-17`), mimalloc, 10M
entries, one run each, on a quiet machine (2026-10-15).

One thread, ns per operation (`bbtree-bench --map bbtree,bctree`):

| cell | bbtree | bctree |
|---|--:|--:|
| `u64` get hit / miss | 129 / 94.6 | 132 / 103 |
| `u64` random / sequential insert | 146 / 5.33 | 153 / 10.6 |
| `u64` random remove | 250 | 172 |
| `u64-seq` get hit / miss | 132 / 94.0 | 138 / 102 |
| `hash32` get hit / miss | 158 / 108 | 159 / 120 |
| `str-json` get hit / miss | 217 / 153 | 248 / 191 |

The version with a tree layer of its own ran `u64-seq` hits at 205 ns,
tying at the root for want of a shared prefix, `str-json` hits at 293 with
8-byte internal windows, and sequential inserts at 38.5 without appends.
A random insert pays about 1.5 ns for the append's look at the last leaf
and 1–2 ns for the split path's setup; the rest of the gap is latching:
the leaf's compare-and-swap, and the versions read and checked on the way
down. Removal runs bbtree's own code, yet is faster than bbtree's: most
likely because bbtree keeps a node stack for every removal, where BCTree
removes an entry whose leaf keeps its minimum with the leaf alone. Not
measured yet; if so, bbtree could do the same.

Threads (`bbtree-scaling`, `u64 → u64`), on the whole machine: workers
pinned one per core, 1–6 on the first die and 7–12 on the second, then the
SMT siblings; millions of operations per second, `RwLock<BBTreeMap>` for
contrast:

| threads | get | 90/10 get/update | insert | `RwLock` get | `RwLock` insert |
|--:|--:|--:|--:|--:|--:|
| 1 | 6.81 | 7.39 | 6.85 | 7.52 | 7.14 |
| 6 | 41.2 (6.0×) | 40.3 (5.4×) | 30.1 (4.4×) | 18.8 | 3.89 |
| 12 | 75.7 (11.1×) | 73.7 (10.0×) | 43.8 (6.4×) | 21.7 | 2.36 |
| 24 | 85.8 (12.6×) | 84.4 (11.4×) | 48.2 (7.0×) | 27.4 | 1.98 |

Lookups scale near linearly across both dies' cores; the SMT siblings add
about an eighth more. Inserts scale less: each dirties several leaf lines,
and a profile at 6 threads showed no time spinning on latches and under 1%
in splits.

Against other crates' concurrent ordered maps, the same runs (`--maps`),
millions of operations per second at 1 / 12 / 24 threads:

| map | get | 90/10 get/update | insert |
|---|--:|--:|--:|
| BCTree | 6.83 / 75.5 / 86.1 | 7.33 / 71.2 / 84.3 | 6.82 / 45.2 / 48.5 |
| Congee 0.4.1 (ART, 8-byte keys) | 6.65 / 70.0 / 85.4 | 6.43 / 67.7 / 86.7 | 8.55 / 56.6 / 58.8 |
| scc 3.8.8 `TreeIndex` | 2.48 / 28.6 / 51.1 | 2.44 / 27.9 / 48.4 | 2.50 / 25.7 / 32.3 |
| concurrent-map 5.0.37 | 2.48 / 27.2 / 47.0 | 2.26 / 22.9 / 36.9 | 2.42 / 12.9 / 14.6 |
| crossbeam-skiplist 0.1.3 | 0.99 / 12.3 / 22.1 | 1.04 / 12.2 / 22.1 | 1.30 / 10.2 / 15.7 |

Each as its crate ships it: Congee pins an epoch on every call; `SkipMap`
and `TreeIndex` hand out values shared only, so theirs are atomics an
update adds to; concurrent-map gives each worker a clone and returns values
by copy. Congee, which keeps only 8-byte keys, inserts about a fifth faster
than BCTree, and looks up as fast.

## Type-stable nodes

An optimistic reader follows pointers it read without a latch and checks
them only after, so a node a merge frees may still be read. Its page can't
go back to the allocator, which could hand it to anything. Instead
(`tree/latched/pages.rs`):

- **Free lists per kind.** A freed leaf or internal page goes on the
  tree's free list for its kind, linked through its first inline-key slots,
  and only a new node of that kind takes it. A reader that reaches it reads
  a node of the kind its pointer says, with edges that are old pointers to
  node pages of their kind, or null.
- **One latch word for life.** The page keeps its latch word, whose version
  only grows: freeing bumps it, and a node moved into a page (a raise, a
  lowered root) keeps the destination's word, so a reader's check can't
  find an old version again. Pages are put on a list only after the
  operation that freed them lets go of its nodes.
- **Back to the allocator** when the map is dropped, with the tree: a map
  keeps the pages of its largest size. No epochs, no guards, nothing per
  operation.

The root page alone changes kind, in place, when the tree grows from a
leaf or lowers to one; readers check a node's version before following an
edge they read from it.

Fresh internal and root pages come zeroed, so that every edge a reader may
read before its check is initialized; a fresh leaf gets its latch word
alone, as a leaf is read past what its node writes only under its latch.

## What is left out

- **No `entry`, no `scan`.**
- **No `len`** (above).

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
- **Returning free pages** before the map drops, under `&mut self`.

## Testing

Differential tests against std one thread at a time, inserts and
removals, with bbtree's invariant check, exact prefixes included, and a
check that appends go to the last leaf; a test of the shared prefix
growing, shrinking for keys that lack it, and starting anew in an emptied
tree; a test that removed nodes' pages serve later splits; and threaded
tests: writers whose keys readers check as they appear, threads inserting
and updating the same keys, threads removing while others insert, threads
racing to remove the same keys, threads checking their own interleaved
keys against models of their own, with many keys and with about a leaf of
them, and threads appending while another shrinks the shared prefix
(`bctree_map/tests.rs`).

Races that need a narrow schedule escape them, and a 12-thread release
stress run outside the crate found two: a removal that left the path below
a replaced separator unchanged, and a descent that read the root's version
after a lowering to a leaf, without checking the root pointer again. The
model tests passed with both; the stress run's models crashed on the
second only with about a leaf of keys, the root splitting and lowering
over and over (`threads_raise_and_lower_the_root_around_one_leaf` keeps
that shape). Those races need controlled schedules (Shuttle).
