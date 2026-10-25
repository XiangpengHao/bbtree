# 01 — WAL: the WAL holds the data

Status: design (2026-10-24), nothing built. 00-init's WAL, laid out: WAL
files and records, their headers and checks, how a writer appends, and
how an open recovers.

From 00-init:

- The WAL holds the data; the bbtree, in memory, points into it. A crash
  rebuilds the tree by scanning the WAL.
- S3 Express One Zone by default, appended every 1 ms (group commit); an
  append waits for the one before it to finish.
- A new WAL file every 10,000 appends.
- One linear history, in LSN order.

Proposed here (mine, for review): each record holds its LSN, one more
than the record before; a CRC32C on each record; a writer claims the WAL
with a record, which fences the writer before it; WAL files are found by
number, never by listing; a storage trait of three methods, with S3's
rules as its contract.

## Terms

- **WAL:** the store's WAL files, in order: every write of the store.
- **WAL file:** the `n`th file of the WAL, named by its backend: a
  header, then records. A store has many.
- **Record:** one key's insert or removal.
- **LSN:** a record's sequence number, in its header.
- **Append:** a few records at the end of the last WAL file, or starting
  the next one, in one call: written whole or not at all.
- **Writer:** the process that appends to the WAL, one at a time. Each
  open of a store is a new writer, with a random id.
- **Claim:** a writer's first append, one record of its id ("Opening").

## What S3 Express gives

From AWS's documentation of `PutObject` and of appends to directory
buckets:

- **Appends at the end only.** `PutObject` with `x-amz-write-offset-bytes`
  equal to the object's size appends; any other offset fails
  (`InvalidWriteOffset`). Offset 0 creates the object, and fails if it
  exists.
- **Whole or nothing.** An append is written entire or not at all: S3
  never adds part of a body.
- **10,000 appends** an object; past them, appends fail (`TooManyParts`).
- **5 GB** an append at most, no minimum.
- **Checksums.** An append may carry its body's CRC32C, which S3 checks
  on arrival.
- **No ordered listing.** A directory bucket lists keys out of
  lexicographic order.
- **Billed as a PUT:** $0.00113 per 1,000 in us-east-1 (April 2025
  prices), plus $0.0032 per GB.

The write offset makes an append a compare-and-append: two writers never
both write at one offset. The WAL leans on it for its one writer.

## Interface

What the store calls: a writer that appends, and records' addresses,
which read their records back while it does.

```rust
impl<S: WalStorage> WalWriter<S> {
    /// Scans the WAL, giving `replay` each record but claims, with its
    /// address; then claims the WAL.
    async fn open(storage: S, replay: impl FnMut(RecordAddress, Record<'_>)) -> Result<Self, WalError>;

    /// Appends `records` as one append; gives their addresses.
    async fn append(&mut self, records: &[Record<'_>]) -> Result<Vec<RecordAddress>, WalError>;
}

impl RecordAddress {
    /// Reads the insert here from `storage`, and gives its value.
    async fn read_value(self, storage: &impl WalStorage) -> Result<Vec<u8>, WalError>;
}

enum Record<'a> {
    Insert { key: &'a [u8], value: &'a [u8] },
    Remove { key: &'a [u8] },
}

enum WalError {
    /// The storage failed: an append's records may or may not have been
    /// written.
    Storage(io::Error),
    /// The WAL's tail moved under the writer: another writer claimed the
    /// WAL, or an append that failed was written after all. Open the WAL
    /// again.
    TailMoved,
    /// The bytes at `offset` of WAL file `file` fail their checks.
    Corrupted { file: u64, offset: u64 },
}
```

- **`WalWriter` is the writer:** `open` is "Opening", `append` one
  append, and `&mut self` keeps one in flight. The store times its
  appends (every 1 ms) and puts its commits in them; the WAL times
  nothing.
- **A `RecordAddress`** is a record's WAL file, offset and length: what
  the index keeps. A lookup reads its record with one `WalStorage::read`
  of that range, while the writer appends.
- **An append whose outcome is unknown is attempted 3 times,** then
  fails with `Storage`. The writer's tail stays where it was: its next
  append goes where the failed one would have, or, if that one was
  written after all, fails with `TailMoved`.

## Storage

The WAL reaches its storage through one trait, whose contract is S3
Express's rules, so that they hold on every backend:

```rust
trait WalStorage: Send + Sync {
    /// Appends a file takes at most (S3 Express: 10,000).
    const MAX_APPENDS: u32;

    /// Appends `bytes` to file `file` if its length is `offset`; offset 0
    /// creates it. Whole or nothing, durable once it returns `Ok`.
    async fn append(&self, file: u64, offset: u64, bytes: &[u8]) -> Result<(), AppendError>;

    /// File `file`'s length; `None` if it doesn't exist.
    async fn len(&self, file: u64) -> io::Result<Option<u64>>;

    /// Reads `buf.len()` bytes of file `file` from `offset`.
    async fn read(&self, file: u64, offset: u64, buf: &mut [u8]) -> io::Result<()>;
}

enum AppendError {
    /// The offset isn't the file's length, or, for offset 0, the file
    /// exists. Nothing was written.
    OffsetMismatch,
    /// The bytes may or may not have been written.
    OutcomeUnknown(io::Error),
}
```

- **`append` is the contract:** a compare-and-append, written only at
  the file's length; whole or nothing; durable once it returns. Creating a
  file is an append at offset 0, under the same rule, which a claim's new
  file relies on. No flush: an append that returned is durable.
- **Any error but `OffsetMismatch` leaves the outcome unknown.** The WAL
  retries and reads back ("Writing"), the same way on every backend.
- **`len`** finds the last file, and where each ends ("Opening").
- **`MAX_APPENDS`** keeps S3's limit out of the WAL's code; a local
  file's is `u32::MAX`.
- **Names are the backend's.** The WAL passes a file's number; the S3
  backend names it `{prefix}/{n:016x}`, a local one a path.
- **Async:** S3 is a network service, and a lookup's read takes
  milliseconds that shouldn't hold a thread. The trait declares each
  method `fn … -> impl Future<Output = …> + Send`, so that the writer's
  futures are `Send`; backends write `async fn`.
- **Not in it:** listing (files are named by number), deleting (the WAL
  is kept forever), a create or a flush of their own.
- **Backends:**
  - **S3 Express:** `append` is a `PutObject` with
    `x-amz-write-offset-bytes` the offset and `x-amz-checksum-crc32c` the
    body's CRC; `len` a `HeadObject`; `read` a ranged `GetObject`.
  - **In memory,** in the tests (`bbtree-store/tests/wal/memory.rs`). It
    injects the two failures the WAL must survive: an append that is
    written but reports `OutcomeUnknown`, and one that isn't written and
    reports it.
  - **A local file,** for development. Its append isn't whole or nothing
    across a crash ("On a local SSD").

## Layout

```text
WAL        WAL file 0 | WAL file 1 | …
WAL file   | header | record | record | … |
record     | crc | LSN | key length | value length | kind | key | value |
```

Integers are little-endian; nothing is aligned or padded.

### Header

```text
offset  size  field
 0      8     magic     b"bbtreeWL"
 8      4     version   the format's, 1
```

Written once, by the append that creates the file, with its first
records. A file of another magic or format version is refused, never
misread.

### Record

```text
offset  size  field
 0      4     crc            CRC32C of bytes 4..
 4      8     LSN            the record before's, plus 1; the WAL's first is 0
12      4     key length
16      4     value length   0 for a removal or a claim
20      1     kind           1 insert, 2 removal, 3 claim; bit 7: the append's last
21            key, then value
```

- **21 bytes, then the key and the value**, lengths `u32`s as the
  in-memory store's records (../16-bbtree-store.md). An append is at most
  5 GB, S3's limit; longer values are out of scope.
- **The LSN is the record's, wherever it lies.** A record is found by its
  WAL file, offset and length, which the index keeps: one range request
  reads it. A record copied elsewhere, by a WAL compaction, keeps its
  LSN. 64 bits last 5,800 years at 100M records a second.
- **An append's last record has bit 7 set.** A commit of many keys is
  atomic if it goes in one append; the WAL holds a whole number of
  appends.
- **A removal keeps its key,** for the scan to remove it from the index.
- **A claim's key is its writer's id**, 8 bytes, alone in its append.
- **Read alone.** A lookup reads one record by its address, checks its
  CRC, its length and that it is an insert, and returns its value. A
  buffer manager over the WAL, caching what lookups read, is a later doc.

### CRC

- **CRC32C everywhere.** x86 (SSE 4.2) and ARMv8.1 compute it with an
  instruction, 8 bytes each: 100 KB checks in about 10 µs. It is also
  S3's: each append carries its body's `x-amz-checksum-crc32c`, and S3
  refuses a body corrupted on the way.
- **One a record,** because records are read alone: a CRC over an append
  can't check a record read by range without reading the whole append.
  The price is 4 bytes a record: 11% of a record of an 8-byte key and an
  8-byte value (37 bytes).
- **32 bits,** not 64: random corruption passes once in 4 billion checks, and
  every record pays the width.

## Writing

- **One append in flight.** An append's write offset is its file's
  length, known once the append before it finished. A store appends
  every 1 ms (00-init), or once the append before finished if that takes
  longer. A
  commit is acknowledged when its append succeeds.
- **A WAL file takes `MAX_APPENDS` appends;** the next append creates
  the next file, at offset 0, its header and records together. Not `CopyObject`
  to reset the count: each copy rewrites the whole file, the copies add
  up to bytes quadratic in its length, and appends wait for them.
- **An append whose outcome is unknown** (a timeout, a dropped connection)
  is attempted again, same bytes at the same offset. If that fails with
  `OffsetMismatch`, the writer reads back the bytes at that offset: its
  own mean an earlier attempt was written; others mean another writer
  claimed the WAL. It stops, failing
  the commits it hasn't acknowledged.
- **Cost.** Each append is a PUT: 1,000 appends a second cost $98 a day.
  An idle store appends nothing, and a store appends at most once per
  append latency.

## Opening

1. **Find the WAL files**, from a snapshot's file (a later doc) or file 0:
   ask `len` of each next number until one is missing. Names, not
   listings, which directory buckets don't sort.
2. **Scan each file**, in order. Check its header, then each record: its
   CRC, its lengths within the file, its LSN the record before's plus 1.
   Apply the records: an insert points its key to the record's file,
   offset and length; a removal removes the key.
3. **A failed check is corruption, an error,** never the WAL's end: an
   append is written whole, so the WAL ends where its last file ends,
   after an append's last record. The error names the file and the offset.
4. **Claim the WAL:** append a claim at the end of the last file, or
   create the next file with it if the last is full, or file 0 for a new
   store. It is written only if no writer appended since the scan. A
   writer before this one, still alive (failed over, not dead), finds its
   next append's offset mismatched, reads back the claim and stops. If the claim
   fails, another writer appended: scan on from where the scan ended, and
   claim again.
5. **Serve once the claim is written.** Before it, the writer before could
   still commit, and reads would miss its commits.

Files are read and checked in parallel; their records apply in LSN
order.

## On a local SSD

00-init's alternative, a replicated SSD, keeps this layout. Its backend
appends with an `fdatasync` before it returns, and needs two things the
trait doesn't have:

- **One writer** by an exclusive lock on the WAL's directory, so that
  `append`'s offset check holds across processes. The claim is still
  written, but the lock fences.
- **A torn last append.** A crash during an append leaves part of it,
  records failing their checks at the end of the last file. They were
  never acknowledged: the scan ends after the last append's last record,
  and the backend truncates the rest. A failed check anywhere else is
  still corruption. Stale bytes, of a preallocated or recycled file, fail
  their CRCs or the LSN check.

Until SSD is wanted, the local backend is for development, its appends
assumed whole.

## Open

- **4 bytes a record for its CRC,** 11% of the smallest records. The
  alternative, one CRC an append, leaves a lookup's read checked only by
  HTTPS on the way.
- **SSD pages.** An append that starts inside a 4 KiB page rewrites
  acknowledged records with it; a drive without power-loss protection can
  tear them. Padding appends to pages would need the scan to skip the
  padding.
- **Retention.** The WAL is kept forever (00-init), at $0.11 per
  GB-month.
- **Later docs:** snapshots, the buffer manager over the WAL.

### Verification

With Verus, as bbtree's proofs (`design/08-fuzzing-verification.md`):
proofs on pure functions over bytes, the async calls to the storage left
thin and unproven. `cargo verus focus -p bbtree-store` checks them, in CI
too (`verus.yml`).

- **Records round-trip** (proven, `wal/record.rs`). `encode` appends a
  record's bytes; `decode` returns a record only if the bytes start with
  its bytes, and returns every record they start with. Its key and value
  must be at most `u32::MAX` bytes, which the writer checks.

Planned:

- **The scan accepts exactly what the writer writes.** The bytes of any
  appends, from any tail, claims and new files included, scan to the
  appends' inserts and removals in order, at the addresses the appends
  gave, and to the writer's tail. A scan that succeeds read only such
  bytes: it never makes up a record.
- **A scan picks up where it stopped.** Scanning two runs of bytes one
  after the other gives what scanning them at once does, as an open does
  after its claim fails.
- **A lookup reads what was written.** `read_value` at an address the
  writer or the scan gave returns that insert's value.
- **No overflow or panic** in any of it.
- **An append's result is true,** against storage that only appends at
  a file's length, loses responses and has other writers: `Ok` means its
  bytes are at its offset, `TailMoved` that they aren't and won't be;
  they are never written twice.
- **A claim fences.** Once a writer's claim is written, every later
  append of a writer before it fails.

Trusted (`wal/trusted.rs`): CRC32C, as a function of its bytes;
little-endian words, as vstd describes them; that each backend keeps the
storage contract ("Storage"). Not proven: that a CRC catches corruption,
which only a probability bounds.
