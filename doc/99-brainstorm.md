

- [ ] With effecive inline keys (+fence key), we should change the kv layout so that they are side by side?

- [ ] Inline key should not just for string and bytes, but also for integer types like u64, i64, etc, although the byte order depends on the endianness. This also means that we should relax the inline key width, so it's not hardcoded to 8 bytes.

- [ ] I think we should still use 4KB nodes, because it allows much easier memory management, we need no allocator at all.
In 4KB nodes, the search performance is not great, because each of the binary search only split the search space by half. So each cache miss load 64 bytes, for a key of 8 byte, rest of 56 bytes are wasted. 
But we can engineer a better layout to improve the search performance of a node.
1. We always have inline keys, which is roughly 8 bytes.
2. instead of laying out the inline keys the same order as they keys, we can group them into 16 keys per group, a node of 4KB can have up to 256 inline keys, aka 16 groups. And we put the lowest key of each group at the begining of the node.
This way, each search pay one cache line miss and linear search the group head, then it navigates to the corresponding group, and do another linear search within the group to locate the key.
3. once key (might be multiple keys depends on the effectiveness of inline key) is located, we can directly jump to the key and value (they should be colocated) with just one more cache line miss.
4. This means that we only pay 3 cache line misses for a 4KB node search, no binary search is needed.

- [ ] To improve memory usage even more, I think when we want to split, we probably want to first look at neighboring nodes, and see if we can rebalance a bit.
