In BBTree, we are about single value concurrency, i.e., latches.

Since concurrent b-tree can have significant different design, and will have significant performance impact. BBTree should by default stay single thread. And a new Btree type should be created for concurrent b-tree, maybe called BCTree.

So there's a common crete that allows both tress to share the same codebase.

Design of current tree:
1. Users should not require to manually acquire locks.
2. USers should not require to manually hold epoch etc. BCTree should have almost the same API as BBTree.
3. Users should be able to hold multiple references without worrying about dead locks. (unless the semantic is by definition deadlock, e.g., one thread holding A and requesting B, and another thread holding B and requesting A.) But a single thread should never deadlock with itself.

This means that:
1. Each node will has a lock (spin lock with proper backoff, scheudler hint etc.).
2. Each KV will have a lock.
3. Each X access to a KV will hold the X lock of the KV, and S lock of the leaf node.

