# bbtree-disk

bbtree on disk: a map compacted into a file map (`bbtree::file`), written
to a file, and used where it lies, mapped into memory. Opening it reads
the header; lookups run bbtree's own code on the file's pages, at a map's
speed. `design/15-index-file.md` has the design and the measurements.

- `IndexFile<K, V>` is a map from keys to values of any length in a file,
  written from a map's entries and read back as the caller's types,
  through `ToBytes` and `FromBytes`: `&str` views of `String` values, with
  no copy.
- `MappedFile` writes a file map to a file, and maps one back, whole or
  from a range of a larger file, in small or huge OS pages, or reads it
  into memory.

```rust
use std::collections::BTreeMap;

use bbtree_disk::{IndexFile, OsPages};

let names = BTreeMap::from([(7_u64, String::from("ferris")), (42, String::from("corro"))]);
IndexFile::create("names.bbtree", &names)?;

// SAFETY: nothing changes the file while it is mapped.
let names = unsafe { IndexFile::<u64, String>::mmap("names.bbtree", OsPages::Small)? };
assert_eq!(names.get(&42), Some("corro"));
```

Examples: `lookups` times a map against its file map, read and mapped;
`var_bytes_lookups` times var-length keys and values against plain ones
and against maps.

bbtree-datafusion-parquet (`../datafusion-parquet`) uses it as an index of
Parquet files.
