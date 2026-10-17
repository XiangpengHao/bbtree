# 09 — Concurrency: BCTree, closures instead of handles

Status: built (2026-10-15, 2026-10-16): `BCTreeMap` with `read`,
`update`, `get_cloned`, `contains_key`, `insert`, `get_or_insert`,
`remove`, `remove_entry`, `remove_if`, `scan`, `scan_rev`,
`range_cloned`, and batched lookups, `read_many` and `get_many_cloned`:
bbtree's tree, shared through optimistic lock coupling, its tree
operations bbtree's own. No `entry` yet. `BBTreeMap` stays
single-threaded (D4).

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
    /// Runs `f` on the entry, inserting `key` and `value` first if there
    /// is none, while the leaf is latched exclusive.
    fn get_or_insert<R>(&self, key: K, value: V, f: impl FnOnce(&K, &V) -> R) -> R;
    /// Removes the entry if `condition` holds of it, latched exclusive.
    fn remove_if<Q>(&self, key: &Q, condition: impl FnMut(&K, &V) -> bool)
        -> Option<(K, V)>;
    /// Visits the range one leaf at a time until `f` breaks: in key order,
    /// and in reverse (`scan_rev`).
    fn scan<Q, R>(&self, range: impl RangeBounds<Q>,
                  f: impl FnMut(&K, &V) -> ControlFlow<R>) -> Option<R>;
    /// Clones of the range's entries, from both ends, holding nothing
    /// between calls.
    fn range_cloned<Q, R>(&self, range: R) -> RangeCloned<'_, K, V, ..>
    where K: Clone, V: Clone;

    fn get_cloned<Q>(&self, key: &Q) -> Option<V> where V: Clone {
        self.read(key, |_, v| v.clone())
    }

    /// Runs `f` on each key's entry, as `read`s would, the lookups'
    /// waits for memory overlapped (Batches, below).
    fn read_many<Q, R, const N: usize>(&self, keys: [&Q; N],
                                       f: impl FnMut(&K, &V) -> R) -> [Option<R>; N];
    fn get_many_cloned<Q, const N: usize>(&self, keys: [&Q; N]) -> [Option<V>; N]
    where V: Clone;
}
```

The types keep references from escaping: `impl FnOnce(&K, &V) -> R` is
`for<'a> FnOnce(&'a K, &'a V) -> R`, and `R` is fixed outside `'a`, so
`map.read(&k, |_, v| v)` doesn't compile. `std::thread::scope` and
`LocalKey::with` rely on the same rule. scc's `HashMap` (`read_sync`,
`update_sync`, `iter_sync`) and DashMap (`view`, `alter`) offer the same
shape.

One entry at a time: an entry can't be held across calls, there is no
`get_mut` handle, and an operation on two entries isn't atomic. `entry`,
std's API within one exclusive latch, is not built: `get_or_insert`
(insert unless present, then read) and `remove_if` (remove only the entry
the caller saw) cover what the integrations surveyed need of it.

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

Against other crates' concurrent maps (`bbtree-scaling --maps`), each as
its crate ships it: Congee pins an epoch on every call; `SkipMap` and
`TreeIndex` hand out values shared only, so theirs are atomics an update
adds to; concurrent-map gives each worker a clone and returns values by
copy; DashMap, a hash map in locked shards, keeps no order and scans
nothing. Millions of operations a second at 1 / 12 / 24 threads; a scan
visits 100 entries from a key the map holds:

| map | get | 90/10 get/update | insert | scan |
|---|--:|--:|--:|--:|
| BCTree | 6.85 / 72.0 / 85.5 | 7.37 / 73.8 / 84.8 | 6.89 / 45.6 / 49.2 | 1.68 / 11.4 / 12.0 |
| Congee 0.4.1 (ART, 8-byte keys) | 6.78 / 70.3 / 87.9 | 6.55 / 68.2 / 87.4 | 8.71 / 69.3 / 55.3 | 0.08 / 0.94 / 1.54 |
| scc 3.8.8 `TreeIndex` | 2.48 / 28.9 / 50.8 | 2.45 / 27.9 / 48.4 | 2.54 / 25.5 / 41.4 | 0.55 / 5.96 / 8.73 |
| concurrent-map 5.0.37 | 2.43 / 26.7 / 46.0 | 2.25 / 23.1 / 37.8 | 2.43 / 12.7 / 13.7 | 1.37 / 11.4 / 12.7 |
| crossbeam-skiplist 0.1.3 | 1.07 / 12.4 / 22.2 | 1.05 / 12.3 / 22.1 | 1.29 / 10.2 / 15.7 | 0.10 / 1.01 / 1.68 |
| DashMap 6.2.1 (hash) | 13.4 / 101 / 150 | 14.6 / 106 / 140 | 14.4 / 53.8 / 65.6 | — |
| bbtree in a `RwLock` | 7.58 / 21.6 / 25.9 | 7.45 / 4.90 / 5.19 | 7.15 / 2.48 / 2.11 | 1.43 / 11.3 / 12.9 |

Congee scans by copying a range between two keys out: given an end key
where 150 entries are expected, it took 5.4 µs a scan of 100 out of 200,000
entries in a probe of its own. DashMap reaches one bucket where a tree
descends.

Latency, each operation timed alone with the time-stamp counter (10 ns
resolution here), ns, p50 / p99 / p99.9 at 1 and 24 threads (`--measure
latency`). These were read with a bare `rdtsc` on each side, which an
out-of-order core reads before a lookup's cache misses return: they
understate. Since 2026-10-16 the reads are fenced (`lfence; rdtsc;
lfence` before, `rdtscp; lfence` after; `latency.rs`), which adds 20–25 ns
to every operation and keeps consecutive ones from overlapping; the blog
post's runs (`bench/tools/blog-runs.sh`) use it. At 10M `u64`, one thread,
fenced: bbtree's get 240 / 541 / 711, std's 281 / 581 / 681, artmap's
arena 210 / 561 / 601; back to back the same gets average 109, 277 and
84 ns, since bbtree's branchless search lets independent lookups overlap.

| map | get, 1 thread | get, 24 | insert, 1 thread | insert, 24 |
|---|--:|--:|--:|--:|
| BCTree | 150 / 461 / 591 | 190 / 601 / 842 | 180 / 591 / 882 | 341 / 1823 / 3556 |
| Congee | 190 / 601 / 681 | 230 / 591 / 852 | 90 / 511 / 812 | 210 / 1302 / 5035 |
| scc `TreeIndex` | 391 / 751 / 922 | 441 / 842 / 1102 | 351 / 1031 / 1603 | 471 / 2004 / 3414 |
| concurrent-map | 411 / 852 / 1262 | 441 / 1022 / 1663 | 361 / 1071 / 3884 | 1323 / 4627 / 25299 |
| crossbeam-skiplist | 932 / 2124 / 2783 | 1011 / 2343 / 3144 | 731 / 1924 / 2543 | 1372 / 2933 / 4875 |
| DashMap | 70 / 351 / 451 | 120 / 511 / 681 | 40 / 341 / 431 | 190 / 792 / 4518 |
| bbtree in a `RwLock` | 130 / 481 / 651 | 1312 / 3184 / 4314 | 140 / 531 / 761 | 8919 / 59050 / 96299 |

The slowest operation at 24 threads took 0.9–2.4 ms for every map alike:
the workers fill every CPU, and the operating system preempts them.

Memory, 10M `u64 → u64` entries, each map built in a process of its own
(`bbtree-scaling-memory`): heap bytes per entry the map frees when dropped,
and the resident set's growth per entry while it was built:

| map | heap | resident |
|---|--:|--:|
| BCTree | 26.9 | 27.1 |
| concurrent-map | 25.9 | 33.7 |
| DashMap | 28.5 | 52.1 |
| scc `TreeIndex` | 39.5 | 45.6 |
| crossbeam-skiplist | 40.0 | 43.7 |
| Congee | 90.9 | 94.9 |

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

- **No `entry`.**
- **No `len`** (above).

## Scans

`scan` and `scan_rev` (`tree/latched/scan.rs`) run the caller's closure on
each entry of a range, in key order or in reverse, with its leaf latched
shared, until the closure breaks. One code serves both orders, a type
parameter deciding each mirrored step. A scan starts at the leaf where the
bound it starts from belongs, and moves on to the leaf beside it, right in
key order, left in reverse, through the internal nodes its descent kept:
the nearest with an edge on that side of the path, then the edges nearest
the held leaf down, each read optimistically. It latches the next leaf
before it lets go of the one it has, which can't split meanwhile, and the
next leaf is that one's neighbor if every node kept from there down is at
the version read. Otherwise it searches again for the last key it visited,
still holding that key's leaf, and goes on past it: no key is cloned.

Scans in both directions latch a leaf while holding another, left to right
and right to left, and can't deadlock: a thread waits for a leaf while it
holds one only in a scan, for a shared latch, which waits only while a
thread holds the leaf exclusively (the latch has no writer preference); and
a thread holding leaves exclusively takes every other node without waiting,
but for a page off a free list, which a thread that reached it through a
stale pointer holds a moment, waiting for nothing.

A scan is not a snapshot: an entry inserted or removed while it runs may or
may not be visited. An entry present for the whole scan is visited exactly
once.

`range_cloned` (`bctree_map/range.rs`) is the iterator over a range that
the integrations surveyed need: a cursor held across calls, by a query
plan or an `async` task. It holds no latch between calls. Each end copies a
few entries during a scan, each with its leaf's stamp: the leaf's latch
word version while the scan held it. It hands a copy out while the stamps
of its leaf and of the previous entry's leaf are current, so that nothing
was inserted or removed between the two, and otherwise scans again past the
last key it handed out. So it hands out entries other threads insert ahead
of it, as a skiplist cursor does. Pages stay the map's until it drops
(type-stable nodes, above), so a kept page is always there to latch or
read. An end copies 1 entry, then 4, then 16 at a time, so that a caller
taking a few entries clones a few.

A copy goes on where the last one's scan stopped (`ScanResume`): it latches
that leaf, checks the stamp, and goes on from the place kept, without a
descent. Past that leaf it descends once, from the leaf's last key, unless
the resume keeps the path down to it too: one box, which the cursor asks
for at its third copy, a caller that scans on, and then the copies cross
leaves along it. A cursor that only seeks stays 264 bytes, with no
allocation; keeping a path inline made it 2,088.

Measured (2026-10-16, 100-entry scans, 10M keys, one thread): copies that
each descended from the root, about six a scan, ran 0.66 million scans a
second for `u64` keys; with the resume, 0.88, as many as the OLC B+ tree of
Turso's PR #9267, whose cursor keeps its leaf, at 1, 12 and 24 threads. In
Turso's own map benchmark, 100-entry scans of table rows take 3.0 µs, of
index records 7.2 µs, from 3.6 and 8.1. The closure scan stays the fastest
way through a range: 1.5 million a second.

The visit counts the entries it visits with a plain counter, for the place
a stopped scan keeps: `enumerate` over the chained head-and-keys iterator
cost about 1,450 instructions more a 100-entry scan, 12% of the closure
scan's throughput.

## Batches

`read_many` looks up a fixed array of keys at once, as
`BBTreeMap::get_many` does (doc 12), and `get_many_cloned` is `read_many`
with a clone (`tree/latched/batch.rs`, `bctree_map/batch.rs`). Each key
descends in its own lane, an async block that prefetches the line it
reads next and yields, on bbtree's executor and its `Prefetcher`, the one
place that prefetches (D46). Results come in input order; the closure runs
once per entry found, in the order the lanes reach them.

A suspended lane holds no latch. So the lanes of a batch never wait for
one another, and no yield falls between latching and letting go: a lane
latches its leaf, searches it, runs the closure and lets go within one
poll. On the way down it reads the internal nodes as the descent does,
optimistically, each stage after fetching the line it needs: the chosen
edge, then the child's header, whose version it reads before checking the
parent's again. At the leaf it reads the inline keys, unlatched, at the
version it read:

- **No inline key equals the key's:** absent, if the leaf's version is
  still the same. A miss writes nothing.
- **One candidate:** the lane fetches its entry. For a heap key it then
  latches the leaf a moment to read where the key's bytes are, lets go,
  and fetches them: a pointer into a key is read only under the latch,
  and a prefetch of a stale one is harmless. Then, latched shared, the
  leaf still at the version read, it compares the candidate and runs the
  closure.
- **Ties**, and keys equal to a separator, go to the leaf's own search, or
  to its head, latched, as `read`.

A failed check starts the lane over from the root, alone.

Below about an L2 of leaf pages, the lanes find their lines in cache and
cost more than they save: at 10k `u64` keys, 28.3 ns a batched hit against
17.7 alone. There batches go one key at a time, as bbtree's do. BCTree has
no length to estimate that from, so its height tells: three internal
levels or more, which a tree grown by splits reaches only past 2·m²
leaves, m half an internal node's capacity, about 1,150 pages for `u64`
keys.

Measured (2026-10-16): 10M entries, `u64` values, 64 keys a batch, one
run each. One thread pinned to a core, ns per key (`bbtree-bench batch`):

| keys | | one key a call | batch | speedup | bbtree's batch |
|---|---|--:|--:|--:|--:|
| `u64` | hit | 137 | 98.8 | 1.39× | 74.9 |
| `u64` | miss | 101 | 77.8 | 1.30× | 63.5 |
| JSON | hit | 249 | 111 | 2.24× | 90.0 |
| JSON | miss | 184 | 81.9 | 2.25× | 71.7 |

Threads (`bbtree-scaling`, whole machine), millions of lookups a second at
1 / 12 / 24 threads; bbtree's batch runs under a `RwLock`'s read lock,
taken once a batch:

| keys | | one key a call | batch | bbtree's batch |
|---|---|--:|--:|--:|
| `u64` | hit | 8.03 / 75.4 / 84.1 | 13.4 / 102.9 / 107.9 | 17.3 / 138.7 / 148.3 |
| `u64` | miss | 11.1 / 101.5 / 111.4 | 17.0 / 147.6 / 153.9 | 20.2 / 164.4 / 173.2 |
| JSON | hit | 5.03 / 49.2 / 63.4 | 10.2 / 75.3 / 76.9 | 12.8 / 99.4 / 106.3 |
| JSON | miss | 8.31 / 75.5 / 86.4 | 14.8 / 113.3 / 118.8 | 16.2 / 114.3 / 118.1 |

At 12 and 24 threads, JSON misses batch as well as bbtree's and `u64`
misses within 11%; hits trail by 24–28%. A hit latches its leaf: two locked atomics, each a full barrier
that the lanes' later loads wait for, and a dirtied leaf header line,
which goes back to memory when evicted. At 24 threads, batched `u64` hits
read 22.7 GB/s from memory (3.29 lines a lookup), bbtree's 30.9 GB/s
(3.26), near the 32 GB/s that scans reach; a dirtied line a hit would add
about 7 GB/s of writes, which no counter of one process sees here. At one
thread the lane runs about 230 instructions a key more than bbtree's: the
versions read and checked at each level, and the state kept across
yields.

Batches don't fetch fewer lines (3.8 a 10M `u64` hit on one thread,
batched or not): they keep more in flight, until the reads near what
memory delivers.

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
them, threads appending while another shrinks the shared prefix, and
scans of the steady keys while other threads insert and remove others,
which visit each once, in order, in both directions, and the same for
`range_cloned` iterators (`bctree_map/tests.rs`). Scans of random ranges,
in both directions, and `range_cloned` from both ends in random
interleavings, match std's `range`; a `range_cloned` iterator hands out a
key inserted ahead of it in a leaf it copied, and not one removed;
`get_or_insert` matches std's `entry().or_insert`, and threads racing to
`get_or_insert` the same keys all see the entry the map keeps;
`remove_if` removes only what its condition holds of. Batched lookups,
8 and 65 keys at a time, on every key the differential tests insert and
look up, keys outside the shared prefix included, match separate `read`s
and run the closure once per entry found; and batches of steady and
changing keys, while threads insert and remove, find every steady key,
also while the root grows from a leaf and lowers back (tests run lanes on
every tree with an internal root).

Races that need a narrow schedule escape them, and a 12-thread release
stress run outside the crate found two: a removal that left the path below
a replaced separator unchanged, and a descent that read the root's version
after a lowering to a leaf, without checking the root pointer again. The
model tests passed with both; the stress run's models crashed on the
second only with about a leaf of keys, the root splitting and lowering
over and over (`threads_raise_and_lower_the_root_around_one_leaf` keeps
that shape). Those races need controlled schedules.

### Shuttle

Shuttle (`bctree_map/shuttle_tests.rs`, `.github/workflows/shuttle.yml`) runs a test's
threads one at a time and switches between them where the tree touches an
atomic that threads share or waits for another thread
(`tree/interleaving.rs`): a latch word, the root and last-leaf pointers, a
free list, the header fields a racing read loads. Built with
`--cfg shuttle`, like loom's `cfg(loom)`; other builds compile the points
to nothing. Shuttle's own atomics would do the same, but the latch words
live in node pages and racing reads load page fields in place. One thread
runs at a time, so Shuttle finds bad interleavings, not reorderings of a
weak memory model, nor torn reads of a node a writer is changing.

Each schedule reruns the whole test, so the tests stay small. Values are
64 bytes, so a leaf holds 12, and each test sets up a tree one operation
from a change, its shape found by building it (`Shapes`): a full root leaf,
two leaves a removal merges back into it, a full root of two levels, three
levels a removal lowers to two, or one of these with a sibling filled so
that a removal takes from it instead of merging. Small tests race two or
three threads of one to four operations over one change: an empty tree
started, emptied, raised from a leaf and lowered back, raised from two
levels to three and lowered back, leaves and internal nodes taking from a
full sibling, separators removed (the root's too), the shared prefix
shrunk, the last leaf freed under appends, a freed page reused under a
cloned range, and removals and `get_or_insert`s racing for one key. Three
larger ones run random operations, every kind the map has, against
per-thread models around one leaf, around the last leaves of three levels,
and with string keys whose 4-byte windows tie. Every closure checks its
value is the requested key's and not dropped, after a switch point: a
closure that runs on an entry its thread has not latched sees another
thread drop it. A test whose race may or may not reach its change counts
the schedules that did, and fails if none did. Schedules cost about 60 ns a
switch, a test thousands to hundreds of thousands of schedules.

Checked by breaking the tree on purpose, a check at a time: the tests
catch a scan or a descent that skips the root pointer's second read, a
lookup that skips its leaf's or its parent's version, an append that skips
its check of the last leaf, a removal that takes neither sibling leaf nor
sibling internal node, and a new tree's root leaf let go before appends
point at it. They miss two kinds of break. A descent that skips its check
that the leaf's head is the separator it matched is not one: a removal of
the separator holds every node from it down to the leaf, so a parent's
version check fails first. And a change to a node its thread doesn't hold,
as a removal of a separator that doesn't take the right border left of it,
or a shrink of the shared prefix that doesn't take the right border, whose
prefixes each refits: Shuttle runs a change whole between two switch points,
so no reader sees it half done. A debug check that a latched node's writer
holds it would catch those.

It found two bugs the threaded tests and the stress run had not:

- **A scan from either end** read the root's version without checking the
  root pointer again (`scan.rs`, `descend_to_first_leaf`), the descent's
  old bug: after a lowering to a leaf, it took the leaf's bytes for an edge
  and latched whatever they pointed at.
- **A batched lookup of a separator key** latched the leaf and checked only
  that its head was the key (`batch.rs`, `read_head`), not that the leaf was
  still at the version read under its parent. A merge leaves the freed
  page's bytes as they were, so the lane ran the closure on a stale copy of
  an entry that lives on in the sibling, which another thread could then
  remove and drop.
