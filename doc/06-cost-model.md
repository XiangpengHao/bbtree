I think it's good to define a cost model before we start to benchmark or profiling.

Workload:
- U64 and String keys, u64 values
- 1M, 10M, 100M entries in the tree

Parameters:
- Node size: inner node, leaf node -> tree levels

Cost:
- Memory usage.
- Expected number of cacheline loads 
- Expected of chained cacheline loads vs parallel cacheline loads
- Expected number of comparisons, key loads
- Expected instructions
- Expected branches

Mindset:
- I think above metrics can be calculated, not just measured.
- We should use the cost model to guide the design of data structure.

