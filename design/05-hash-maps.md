# 05 — Hash maps

Status: built (2026-10-25, 2026-10-26). This repository's own maps, with
no Rust reference: `shoal::unsync::HashMap`, single-threaded, with stable
references; `shoal::HashMap`, sharded and read without locks; and
`shoal::unsync::HashIndex`, an index over rows stored elsewhere. They came
from the `codex/hashmap` branch (`f8ef72f`), whose DuckDB benchmark and
integration stay there, with their reports and data.

```cpp
#include "shoal/unsync/hash_map.hpp"
#include "shoal/hash_map.hpp"

shoal::unsync::HashMap<std::string, int> owned;
owned.insert("one", 1);                       // optional previous value
const int* value = owned.get("one");          // borrowed query, no string allocation
owned.reserve(10000);                        // value's address stays valid

shoal::HashMap<std::string, int> shared;
auto inserted = shared.insert("one", 1);      // InsertOutcome, as the shared tree
auto copied = shared.get_copied("one");
shared.update("one", [](const auto&, int& value) { ++value; });
```

## Two tables (D25)

The owned map hands out references that must stay valid through growth,
so its entries never move: a paged table. The shared map hands out none
that outlives a callback, so its entries may move, and live where the
probe finds them: an inline-slot table. Both are header-only, and all
their memory comes from the supplied allocator's
`allocate_zeroed`/`deallocate`.

### The owned map's paged table

After DuckDB's [two-part aggregate hash table](https://duckdb.org/2022/03/07/aggregate-hashtable):
a compact probe index filters candidates before following references into
payload storage, and cached full hashes let the index grow without hashing
keys again. Descriptors are a 16-bit fingerprint and a 48-bit entry
identifier, no pointers. Empty and deleted identifiers are distinct;
equality always checks the real key. Linear probing stops only at an
empty descriptor.

A slot also has a tag (D27), a byte beside the descriptors in the same
block: empty, deleted, or occupied with the hash's top seven bits. Lookups
walk the tags (a ninth of the index's bytes) and read a descriptor only
where the tag matches, so a miss reads no descriptor. Descriptors stay the
truth; every write of one writes its tag
(`HashProbeIndex::write_descriptor`).

Entries hold the full mixed hash, `K` and `V`. Pages hold at most 64
entries and aim for 4 KiB when entry size permits; oversized entries get
one slot per page, over-alignment kept. Page bitmaps serve iteration, and
deleted slots form a free list.

The index grows at 75% occupied-plus-deleted. At that threshold, with live
occupancy below 50%, it rebuilds at the same capacity instead, in place.
A rebuild is linear in allocated entry slots plus index capacity, and
moves no keys or values and calls neither hash nor equality. Payload pages
and holes stay allocated until `clear` or destruction.

With index occupancy `alpha`, payload occupancy `beta` and aligned entry
size `E`, memory is about `9 / alpha + E / beta` bytes a live entry, plus
page headers, the directory and alignment; right after doubling, index
occupancy is about 37.5%. For `u64 -> u64`, `E = 24`: 36 bytes an entry at
75% and full pages. A hit depends on descriptor, page pointer, entry and
any heap-owned key bytes; bulk reads overlap independent queries but don't
remove those dependencies.

### The shared map's inline-slot table

A shard (`detail/hash/inline_table.hpp`) is open addressing with linear
probing, the entries (cached hash, `K`, `V`) in the slots it probes, a tag
a slot in front of them (empty, or the hash's top seven bits), all in one
block: a header line with the capacity, the tags (the first seven cloned
past the last, so that eight never wrap), the slots aligned to cache
lines. A lookup reads eight tags as a word and finds the first match and
the first empty one with a zero-byte test (D27), then reads the one slot
whose tag matches: a miss reads no slot, a hit its own.

It grows at 75% occupancy, moving entries by their cached hashes, no user
code run. A removal shifts the entries after the hole back into it
(Knuth's Algorithm R, D29): no tombstones, no rebuild but growth. Memory
is `(1 + E) / alpha` bytes a live entry for an aligned entry of `E` bytes:
54 for UUID keys and `u64` values at 61% occupancy, against about 47 in
the paged table.

## Lock-free reads (D26)

A shard is the tree's `NodeLatch`: writers hold it exclusively and bump
its version. A reader of trivially copyable keys and values reads the
version, probes the block and copies the entry it finds without the latch
(`InlineHashTable::racing_find`), checks the version, and only then runs
user code, on its copy; after four changed versions it holds the shard
shared. Keys or values that aren't trivially copyable are always read with
the shard held shared: their copies can't be made from bytes a writer is
changing.

Whatever a writer does meanwhile, a racing reader stays in memory it may
read: it learns a block's bounds from the block's header, never from a
separate capacity it might read from another state; a block that growth
replaces stays with the shard (`RetainedBlocks`) until the map is
destroyed; `clear` keeps the block. A release fence orders a new block's
contents before the pointer that publishes it.

## Bulk reads (D28)

`get_many_copied` reads 64 keys a call in steps, each step for every key
before the next, so that the keys' waits for memory overlap, without
prefetching: every shard's version by a relaxed load, then one fence; the
eight tags at each key's home; which keys go on, those whose tags hold a
match before an empty one, or neither; for those, the first match's
cached hash; the entry copied and keys compared; one fence, then every
version checked, a version read while a writer held the shard turning the
key away. A key whose shard changed is read again on its own.

No step branches on whether a key is there. The keys that go on are
listed by writing every key at the list's end and moving the end by
whether it goes on, so a key its tags show absent stops after one line,
as a Boost miss does. A listed key with no candidate reads its hash from,
and copies, a zeroed entry (`NoEntry`) in its place; a hit is `candidate &
hash equal & keys equal`; a miss's value is written and dropped. The compiler must agree: the slot's address is computed before
the select between it and `NoEntry`, which clang otherwise makes a branch.
Only a key whose first match was another's, or whose eight tags held
neither its own nor an empty one, takes a branch, rarely.

## How the shared map got there

Measured with `bench/race/race_bench.cpp` (10M random UUID keys, half the
lookups missing, M4 Pro), millions of lookups per second. Rows ran on
different occasions; each ran next to the one before it.

| | bulk, 1 thread | bulk, 14 threads | one key, 14 threads |
|---|--:|--:|--:|
| paged, shard behind `std::shared_mutex` | 8.6 | 13.5 | |
| paged, optimistic, versions by acquire loads (D26) | 8.5 | 113.2 | |
| paged, versions relaxed, one fence (D28) | 10.3 | | |
| paged, in steps across the bulk read (D28) | 28.5 | 247.9 | 120.0 |
| paged, tags before descriptors (D27) | 27.4 | 270.8 | 134.3 |
| inline slots (D25, D29) | 37.5 | 373.0 | 260.4 |
| inline, eight tags a word, no branch on the outcome (D27, D28) | 44.6 | 469.7 | 306.1 |
| the slot's address before the select (D28) | 45.5 | 474.4 | |
| only the keys that may be there go on (D28) | 54.5 | 501.9 | |
| Boost `concurrent_flat_map` (bulk `cvisit`) | 40.2 | 427.1 | |

Each step answered what the one before it measured:

- **Locks (D26).** Every read took its shard's `std::shared_mutex`, which
  writes the lock's cache line; 14 cores reading bounced those lines.
  Locked `absl::flat_hash_map` shows the same at its worst, slower with
  more threads.
- **One fence (D28).** An acquire load of each key's version held each
  key up behind the previous key's wait for memory.
- **Steps (D28).** Each key's walk was dependent loads; in steps, 64 keys'
  loads are in flight at once.
- **Tags (D27).** The first guess was that Boost's byte of metadata a slot
  stays in cache where 8-byte descriptors don't. Tags took that away and
  helped only modestly: the cached counts (100K keys, one thread) showed
  the gap is work, 160 instructions and 96 cycles a key against Boost's 85
  and 41, a hit walking shard, version, index header, tag, descriptor,
  directory, page, entry and version again.
- **Inline slots (D25).** The descriptor, directory and page hops exist
  for stable references, which the shared map doesn't give: entries in
  the slots removed them.
- **Only the keys that may be there (D28).** At 10% hits Boost led by
  1.5× (870 against 585 M/s, 14 threads): a Boost miss stops at its
  group's metadata, where a shoal miss ran every step a hit did. Counted
  on one thread, 133 instructions and 74 cycles a lookup against Boost's
  56 and 48. Listing the keys that go on: 112 and 55 at 10% hits, 811
  against Boost's 830 M/s at 14 threads; 127 and 76 at 50%, from 131 and
  97, 502 against 399. Why cycles fell five times as much as
  instructions at 50% hits is open. With nearly every key going on (90%
  hits) the list costs 14 instructions a lookup and saves 7 cycles; in
  cache it costs 11% there.
- **No branch on the outcome (D28).** With every key present the same code
  ran at Boost's speed (23 cycles a key cached, 176 against 174 M/s); with
  half absent, 78 cycles against Boost's 40. The outcome branches
  mispredicted half the time, and each misprediction threw away the
  following keys' loads too. Branch-free, 46. Merging steps had saved
  instructions and cost time (27.4 to 19.8 M/s) for the same reason.
- **The select kept (D28).** With every key in cache (100K keys) on one
  thread, Boost still led, 99 against 80 M/s, and shoal's bulk read at
  50% hits ran half as fast as at 90%: the outcome branch was back. clang had
  compiled `candidate ? slot : NoEntry` to a branch, the slot's address
  being work in one arm. Computed before the select, the address leaves a
  conditional move: 80 to 120 M/s cached at 50% hits (Boost 110), 150 to
  164 at 90% (Boost 158); uncached, 43.8 to 45.5 on one thread and 457 to
  474 at 14, back to back with the binary before.

A policy that declares `is_avalanching` is used unmixed (D30): the race's
UUID hash declares it, and both shoal's and Boost's maps skip their mix
for it.

## API and ownership

Template parameters are `K, V, Hasher, Equal, Allocator`; the shared map
also takes a power-of-two shard count (default 64, at most 65536). Keys
and values need non-throwing move constructors and destructors; entries
are constructed in place, so byte relocation (D7) is unnecessary and
`std::string` works on every standard library. Replacement keeps the
original key and returns the previous value.

Hashing defaults to `shoal::Hash<K>` (on `std::hash`); owning byte strings
hash their borrowed view. Both maps mix the policy's result before
taking placement and tag bits (D30). This is no keyed hash and no defense
against collision attacks: supply a suitable hasher for hostile input.
Equivalent keys and heterogeneous queries must hash alike; hashing and
equality must stay consistent while entries are stored, and must not
re-enter the map.

**The owned map** supports lookup, mutable lookup, key/value lookup,
insertion, removal, reservation, copying and moving, iteration, and bulk
operations. `clear_retain_capacity` destroys entries and keeps the index
and pages for another build. Fresh slots take a bump cursor, erased ones
the free list. Insert and reserve keep existing references; erase
invalidates the removed entry; clear and assignment invalidate all. The
iteration order is unspecified; structural changes invalidate iteration.
Empty maps allocate nothing; `reserve` reserves index capacity, pages
still allocated lazily. Allocation failure throws `std::bad_alloc` and
keeps the entries, though capacity may have changed; so do hash and
equality exceptions. Copy assignment builds the replacement before
changing the target.

**The shared map** offers copied and cloned reads, scoped `read` and
`update`, insertion, removal and conditional removal, traversal, clear,
and bulk operations. Hash and equality policies must allow simultaneous
const calls; allocator copies, calls from different shards. Read
callbacks run on a copy of the entry for trivially copyable keys and
values, with the shard held shared otherwise; updates hold it
exclusively. Callbacks must neither re-enter the map nor keep entry
references; callbacks returning pointers or references are rejected. A
callback's exception releases the shard; what an update changed before
throwing stays. Shared maps are neither copyable nor movable.

Shared insertion returns an `InsertOutcome`, the input entry kept on the
allocator's refusal (D10). Exceptions from user policies propagate,
`std::bad_alloc` thrown by a policy included. A shard is chosen by mixed
bits 32–47, leaving the tag and the low 32 placement bits alone. Growth
pauses its shard while it moves the entries to a block twice the size.
Counts, traversal, clear and invariant checks hold one shard at a time,
and are not map-wide snapshots; exclude concurrent writers if `clear` must
leave the whole map empty. `clear` keeps a shard's memory for later
entries; the map gives it back when destroyed.

**Bulk operations.** Owned `get_many` advances bounded tiles of 128
queries through descriptor discovery and key comparison, writing pointers
in input order; completed queries leave further rounds.
`get_or_insert_many` hashes each borrowed query once, creates missing owned
keys with an initializer, and returns stable mutable value pointers;
duplicate keys share a pointer and run the initializer once. A cached
empty slot is published directly when there are no tombstones, the slot
is still empty and enough capacity was reserved for the tile; otherwise
it searches again. Earlier inserts stay committed if initialization or
allocation throws. `update_or_insert_many` shares that discovery and
publication and fuses value updates with resolution:
`initialize(row, query)` constructs a missing value, `update(row, query,
value)` changes an existing one, in input order, duplicates included,
without re-entering the map. `for_each_hashed` walks dense page runs and
exports the cached mixed hash without hashing again; that hash is local to
the map's policy, not a persistent identifier. Shared bulk reads go 64
keys at a time (D28). Bulk inserts go in input order, duplicates included,
and move from input arrays; shared bulk inserts put refused entries back
in their input slots and report `failed[i]`. Earlier operations stay
committed on a later exception. Bulk inputs and outputs must be valid
disjoint arrays; a zero count permits null pointers. No hash map path
prefetches (AGENTS.md).

## `HashIndex`

`shoal::unsync::HashIndex<Handle, Allocator>`
([`unsync/hash_index.hpp`](../src/shoal/unsync/hash_index.hpp)) indexes rows
owned elsewhere: it takes cached hashes unchanged, stores row handles
directly, and does key matching and row creation in bulk, through
callbacks. It shares the owned map's descriptor storage.

## Verification

`hash_map_test` checks mixed operations against `std::unordered_map`,
including forced collisions and tombstones; tests also cover borrowed
strings, stable references, cached hashes, move-only and non-assignable
values, over-aligned oversized entries, object lifetimes, copy failures
and allocator refusals. `shared_hash_map_test` checks racing writers and
readers, atomic updates, scoped access, bulk semantics, allocator failure
and retry, exception unwinding; long, wrapping probe runs against
`std::unordered_map`, which catch a wrong shift rule (D29); optimistic
readers against writers that grow, erase and clear, and against writers
that rewrite eight-word values (without its version check a reader sees
words from two writes, and the test fails); and that what shards retain
goes back to the allocator. `hash_index_test` covers `HashIndex`. All run
in CTest and the sanitizer builds.
