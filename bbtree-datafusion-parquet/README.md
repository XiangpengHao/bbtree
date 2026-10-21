# bbtree-datafusion-parquet

bbtree-mmap as indexes of Parquet files.

bbtree index is directly appended at the end of the file, so no rewrite is needed.

### Build index

```bash
cargo add bbtree-datafusion-parquet
```

```rust
use bbtree_datafusion_parquet::{ParquetIndexBuilder, ParquetIndexSessionBuilderExt};
use datafusion::execution::SessionStateBuilder;
use datafusion::prelude::{ParquetReadOptions, SessionContext};

let built = ParquetIndexBuilder::source("hits.parquet")
    .index(&["UserID"])
    .index(&["CounterID", "EventDate"])
    .build()?;
for index in &built {
    println!(
        "{:?}: {} keys, {} bytes",
        index.columns(),
        index.keys(),
        index.bytes()
    );
}
```

### Query with index

```rust
let state = SessionStateBuilder::new()
    .with_default_features()
    .with_parquet_index();
let ctx = SessionContext::new_with_state(state.build());

ctx.register_parquet("hits", "hits.parquet", ParquetReadOptions::default())
    .await?;
ctx.sql(r#"SELECT * FROM hits WHERE "CounterID" = 62 AND "EventDate" > 15900"#)
    .await?
    .show()
    .await?;
```
