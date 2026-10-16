The blog post is along with the visualization.

Title: Fastest ordered map.

1. we introduct bbtree, and bctree (its concurrent version), the fastest ordered map in the world.
Then we say btree is the most important data structure in the world "qoute jim gray".
it is zero-dependency, no_std compatible, and it is a drop-in replacement for std::collections::BTreeMap.

2. then we immediately show a performance comparsion.
This comparsion has a few tabs.
First tab shows the single thread bbtree compare with std Btree and skip list (different series).
the x-axis is differnt workloads, u64, uuid, wiki titles, json-str.
y axis is the latency, show p50, p99, and pmax.

Second tab shows the bctree.
Compare with concurrent-map, crossbeam skip list, std btree with rw lock, and bctree.
x-axis is the thread count.
within each, there's a way to switch the workloads.
We should probably show memory consumption as well.

Thrid tab shows the performance on real aws servers, x86 instance, and arm instance.

Fourth tab shows the integrations. SlateDB, Turso, SpacetimeDB(?), fjall, and maybe others. Each tab show a before and after performance comparsion.

the tabs are like cards, it will automatically switch if not clicked.

3. then we say how it is made.

There's two parts: the inline key tricks, and the batched interleaved search tricks.

And they are in two different sections, guided by the visualization we already have.

