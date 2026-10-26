bbtree-store extend bbtree to storage systems. Maybe we should give it a name "OctalDB".
1. crash consistent with WAL.
2. bbtree (leaf and inner) remains in memory.

Data is stored in WAL. bbtree just point the offset and location to the wal file.
A crash will need to scan the WAL to rebuild the index.

bbtree-store is more like RocksDB or SlateDB.

bbtree-store is single node by design. unless someone can convince me otherwise.

A local in-memory/disk buffer manager is possible. It runs a standard 4kb/1mb page replacement over S3 WAL.

### WAL

WAL is stored in replicated storage (raid SSD) or appendable object store like S3-express-one-zone (this is the default).
WAL is flushed every 1ms, so bbtree-store commits every 1ms.
A s3 write might be slow, so a next commit needs to wait before the previous one commits.
S3 express one zone can only append 10,000 times. So every 10,000 flush, we need to create a new file (or copy it).
Each group commit may write multiple commits/WAL, and they togeter count as one PUT/APPEND.
Alternative to check: S2. But we probably don't persist on SSD, unless we want to build a replicated thing ourselves.

WAL compaction is possible, but probably not necessary.
WAL compaction is to bring relevant records together. the relevant here mostly mean "sortness". Which brings us very similar to 

### Branching

bbtree-store is time-travelling by default. So WAL keeps increasing. LSN is the central thing that controls version.
bbtree-store must have a linear history, as serialized by LSN.

time-travelling, or branching. is mostly intended for agents to use. like they just branching out at a point, and once decided, commit to the main thing.
when branching out, it mostly mean that you'll need to create a new bbtree index, by setupping bbtree from previous snapshot, and applying wal log to that lsn.

Forking bbtree means rewind to a LSN, then create a new WAL trace.

The key point is bbtree snapshotting, so rewinding/forking is low cost.
Snapshotting the bbtree means writing the _leaf nodes_ to a snapshot file (maybe just append to WAL?).
Rebuilding bbtree from snapshotting meaning scanning and copying the leaf nodes (maybe mmap), the inner nodes still need to be rebuilt, but should be very very fast.

**Overlay bbtree**
overlay bbtree takes a base bbtree, and applies write operations on a new bbtree.
when querying, this means first query the top bbtree, then the base bbtree. and a commit means merge the top bbtree into the base?

I think this might be a good idea. Each group commit is a bbtree, it consists of many commits ready to by persisted. and that overlay can generate a wal.
once the wal is acked, that overlay can be merged to base.
before the wal is acked, a new layer can be created.

### Time travelling and MVCC

As-of queries can be supported. each key is a pair of (key, version). so the tree keeps growing and new versions are inserted.

But note that as-of query doesn't mean time travelling or branching.

But this is low piority, unless I'm convinced as of is important.

### Audit and isolation

This is another design goal. Each table, ideally, each row, should have a permission model. A request that doesn't have access to those values are denied.

