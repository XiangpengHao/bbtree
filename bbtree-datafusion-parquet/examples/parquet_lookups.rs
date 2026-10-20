//! An index of a Parquet file's columns, and queries that look their rows
//! up in it, against the same queries scanning the file.
//!
//! `cargo run --release --example parquet_lookups -- <file.parquet>
//! <column>[,<column>...] <predicate>...`: builds the index of the columns
//! in the file, unless it has one, then for each predicate times
//! `SELECT * FROM t WHERE <predicate>` once in a session that scans the
//! file, then once in a session that looks rows up in its indexes.

use std::error::Error;
use std::io::ErrorKind;
use std::time::Instant;

use bbtree_datafusion_parquet::{ParquetIndexBuilder, ParquetIndexSessionBuilderExt};
use datafusion::execution::SessionStateBuilder;
use datafusion::prelude::{ParquetReadOptions, SessionContext};

#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// A session that registers the file at `path` as table `t`, looking rows
/// up in its indexes if `indexed`.
async fn session(path: &str, indexed: bool) -> datafusion::common::Result<SessionContext> {
    let state = SessionStateBuilder::new().with_default_features();
    let state = if indexed {
        state.with_parquet_index()
    } else {
        state
    };
    let ctx = SessionContext::new_with_state(state.build());
    ctx.register_parquet("t", path, ParquetReadOptions::default())
        .await?;
    Ok(ctx)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [path, columns, predicates @ ..] = &args[..] else {
        return Err(
            "usage: parquet_lookups <file.parquet> <column>[,<column>...] <predicate>...".into(),
        );
    };
    let columns: Vec<&str> = columns.split(',').collect();
    match ParquetIndexBuilder::source(path).index(&columns).build() {
        Ok(built) => {
            let index = &built[0];
            println!(
                "indexed {:?} in {:.2?}: {} rows, {} keys, {:.1} MB",
                index.columns(),
                index.build_time(),
                index.rows(),
                index.keys(),
                index.bytes() as f64 / 1e6
            );
        }
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let sessions = [
        ("scan", session(path, false).await?),
        ("index", session(path, true).await?),
    ];
    for predicate in predicates {
        let sql = format!("SELECT * FROM t WHERE {predicate}");
        for (name, ctx) in &sessions {
            let start = Instant::now();
            let batches = ctx.sql(&sql).await?.collect().await?;
            let rows: usize = batches.iter().map(|batch| batch.num_rows()).sum();
            println!(
                "{predicate}: {name}, {rows} rows in {:.2?}",
                start.elapsed()
            );
        }
    }
    Ok(())
}
