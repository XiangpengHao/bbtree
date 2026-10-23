- [-] I think we should still use 4KB nodes, because it allows much easier memory management, we need no allocator at all, and allows us to easily extend to disk snapshot etc.
In 4KB nodes, the search performance is not great, because each of the binary search only split the search space by half. So each cache miss load 64 bytes, for a key of 8 byte, rest of 56 bytes are wasted. 
But we can engineer a better layout to improve the search performance of a node.
1. We always have inline keys, which is roughly 8 bytes.
2. instead of laying out the inline keys the same order as they keys, we can group them into 16 keys per group, a node of 4KB can have up to 256 inline keys, aka 16 groups. And we put the lowest key of each group at the begining of the node.
This way, each search pay one cache line miss and linear search the group head, then it navigates to the corresponding group, and do another linear search within the group to locate the key.
3. once key (might be multiple keys depends on the effectiveness of inline key) is located, we can directly jump to the key and value (they should be colocated) with just one more cache line miss.
4. This means that we only pay 3 cache line misses for a 4KB node search, no binary search is needed.
5. The insert performance will drop, esp at leaf node, because we need to shift more keys. We can mitigate this by having a indirection slot for the keys, each slot is 2 bytes, and it points to the key. So when inserting a key, we can always append the key to the end of the node, and only shift the indirection slot to the right. We can limit the slot to 1byte, if we want to limit the fanout to 256.

- [ ] To improve memory usage even more, I think when we want to split, we probably want to first look at neighboring nodes, and see if we can rebalance a bit.

- [ ] Performance infra: we want to continously monitor the performance, so something that doesn't rely on the CI machine is needed. I wonder if we can just collect the instructions, and just analyze the instruction count and their memory access pattern to get a accurate performance expectation? Basically, we still run the benchmark, but we collect every instruction get executed, measure the memory latency, and build a model to e.g., interleave memory access, model prefetching, parameterize the load/store latency, etc.

- [x] the 8-byte inline-key count compiled to the slow bit-by-bit form at 28–31 slots. Now counted in explicit AVX-512
  vectors there (05-fence-key.md): memtable lookups 1,102 → 393 instructions. String sets no longer have 31-slot leaves.


- [-] I think leaf node should not use inline key, because leaf node is to find that key, not to find the gap that contains the key. so we should do this: instead of storing the 8/4/2 bytes inline key which contains the raw values, we should store the hashed inline key, and only store 2 bytes (even 1 byte?). 
Since the hash is over the inline key, the K itself doesn't require Hash trait. If the K already implements Hash, we can hash the K itself this can gives us even better performance.  This might remove the hash tag from the leaf node.

- [-] When we insert to sorted array, we need to shift the keys, which is a memmov, is it? and I think it has to first copy the data to a temporary buffer, and then copy back, because the move is interleaving.
BUT if we move data in the reverse order, we can avoid this buffer. 

- [x] We should specialize the root node. It's a special type in addition to internal and leaf nodes.
Reason is that for small tree, root node should be smaller than 1KB. So for root node, we start with 128 bytes, and each time double the size until 1KB, then we do the normal split.
This should make our memory usage more efficient for small tree.
Root node has no parents, so its prefix lenght is determined by the min and max keys of the node.
In fact, since we never truncate the prefix in storage, I think we can always make the fence key to be the min/max keys of the current node, which means the inline keys are maintained once a new key is inserted.


- [x] In node layout, should we put children/values first, then keys? looking at the vis, I realized that most of the keys are actually not used in the lookup, because the inline keys are very effective, but they still take up spaces.
This probably means we should push the keys down to the leaf, and inner node should just hold a point to the leaf's key.
Worked out in 11-separator-key.md.


- [x] I think, if we want to specialize the inner, leaf, and root. then the abstraction of "Subtree" doesn't makes sense. because the tree is no longer a
  recursive thing, it has well defined structure. the way we currently organize the code makes it confusing and unnecessarily complicated. wdyt?
Phase 1 built (D51, D52).

- [ ] we might want to benchmark the insert performance of roughly sorted data, compare with hashmap.

- [ ] entry should also go through the right most leaf fast path.

- [ ] We are better than radix tree: 1. support prefix search, 2. better memory usage, 3. faster scan.

- [x] We need to support a file mode. allowing bbtree to be written to a file, and read from a file. This requires key to implement FromBytes and ToBytes.
  Built as file maps (15-index-file.md, D58): the tree's pages, compacted, edges as offsets, mapped and used in place; keys and values `PlainBytes` in their slots or `VarBytes`, of any length, in each leaf's tail. bbtree-mmap has typed maps in files (`BBTreeMapFile`, `DiskType`).

- [-] We need to add a user case of DataFusion index provider for Parquet. and show it in ClickBench.
Zac's experiement of using bbtree as index vs just sorted array shows that this is not a good position.
Sorted array is much faster and much more memory efficient. while bbtree's benefits are mostly mutable index.
We probably want to remove that use case, and replace with something else.

So we should make bbtree-store to be disk based. automatically write larger than memory data to disk.
And the api should be async.
And we need a disk trait. And a disk space allocator. On linux, it should be io_uring based.
We should be gracefully shutting down, and write everything to disk. During recovery, we rebuilt the inner and leaf nodes from disk, through a scan.
Inner and leaf nodes are still pinned to memory, so we can still do the normal operations; only heap pages are written to disk.

We will have a buffer management. which swaps the cold pages to disk, and bring hot pages to memory.


