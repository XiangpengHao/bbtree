# bbtree-disk

bbtree on disk: compact a bbtree map to a file, mmap it back into memory and instantly query it. 

```rust
use bbtree::unsync::BBTreeMap;
use bbtree_disk::BBTreeMapFile;

let names = BBTreeMap::from([(7_u64, String::from("ferris")), (42, String::from("corro"))]);
BBTreeMapFile::<u64, String>::create("names.bbtree", &names)?;

// SAFETY: nothing changes the file while it is mapped.
let names = unsafe { BBTreeMapFile::<u64, String>::mmap("names.bbtree").open()? };
assert_eq!(names.get(&42), Some("corro"));
```
