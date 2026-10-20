//! DataFusion's Parquet scans, looking their rows up in the indexes of the
//! files they read: they keep the rows scans without the indexes keep.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bbtree_datafusion_parquet::{ParquetIndexBuilder, ParquetIndexSessionBuilderExt};
use datafusion::arrow::array::{
    ArrayRef, Date32Array, Float64Array, Int64Array, RecordBatch, StringArray, UInt8Array,
};
use datafusion::arrow::util::pretty::pretty_format_batches;
use datafusion::execution::SessionStateBuilder;
use datafusion::parquet::arrow::ArrowWriter;
use datafusion::parquet::file::properties::WriterProperties;
use datafusion::physical_plan::{ExecutionPlan, collect, displayable};
use datafusion::prelude::{ParquetReadOptions, SessionContext};

/// A directory of its own for a test, removed when dropped.
struct TestDir(PathBuf);

impl TestDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "bbtree-datafusion-parquet-{name}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const ROWS: usize = 10_000;

/// The `id` of each row: repeats, and nulls.
fn ids() -> Vec<Option<i64>> {
    (0..ROWS)
        .map(|row| (row % 7 != 3).then(|| (row as i64 * 7919) % 1000 - 500))
        .collect()
}

/// The `name` of each row: text sharing long prefixes.
fn names() -> Vec<String> {
    (0..ROWS)
        .map(|row| format!("user-name-{:05}", (row * 31) % 2000))
        .collect()
}

/// Writes a Parquet file of `ROWS` rows, in row groups of 3000 and small
/// pages, with page indexes: `id`, with repeats and nulls; `name`, text
/// sharing long prefixes; `day`, a date, the same for runs of rows;
/// `score`, floats with NaNs; and `bucket`, which no index holds.
fn write_file(path: &Path) {
    let ids = Int64Array::from(ids());
    let names = StringArray::from(names());
    let days: Date32Array = (0..ROWS)
        .map(|row| Some(19_000 + (row / 700) as i32))
        .collect();
    let scores: Float64Array = (0..ROWS)
        .map(|row| {
            Some(if row % 101 == 0 {
                f64::NAN
            } else {
                ((row * 13) % 200) as f64 / 8.0 - 12.5
            })
        })
        .collect();
    let buckets: UInt8Array = (0..ROWS).map(|row| Some((row % 5) as u8)).collect();
    let batch = RecordBatch::try_from_iter([
        ("id", Arc::new(ids) as ArrayRef),
        ("name", Arc::new(names) as ArrayRef),
        ("day", Arc::new(days) as ArrayRef),
        ("score", Arc::new(scores) as ArrayRef),
        ("bucket", Arc::new(buckets) as ArrayRef),
    ])
    .unwrap();
    let properties = WriterProperties::builder()
        .set_max_row_group_row_count(Some(3000))
        .set_data_page_row_count_limit(500)
        .build();
    let mut writer = ArrowWriter::try_new(
        File::create(path).unwrap(),
        batch.schema(),
        Some(properties),
    )
    .unwrap();
    writer.write(&batch).unwrap();
    writer.close().unwrap();
}

/// The indexes the tests build.
const INDEXES: [&[&str]; 4] = [&["id"], &["name"], &["day", "id"], &["score"]];

fn build_indexes(path: &Path) {
    let builder = (INDEXES.iter()).fold(ParquetIndexBuilder::source(path), |builder, columns| {
        builder.index(columns)
    });
    builder.build().unwrap();
}

/// A query's rows, one line each, sorted, and the plan that found them.
async fn run(ctx: &SessionContext, sql: &str) -> (Vec<String>, Arc<dyn ExecutionPlan>) {
    let plan = ctx
        .sql(sql)
        .await
        .unwrap()
        .create_physical_plan()
        .await
        .unwrap();
    let batches = collect(Arc::clone(&plan), ctx.task_ctx()).await.unwrap();
    let table = pretty_format_batches(&batches).unwrap().to_string();
    let mut lines: Vec<String> = table.lines().skip(3).map(str::to_owned).collect();
    lines.retain(|line| !line.starts_with('+'));
    lines.sort();
    (lines, plan)
}

/// The sum of the metric `name` over the plan's nodes.
fn metric(plan: &Arc<dyn ExecutionPlan>, name: &str) -> usize {
    let own = (plan.metrics())
        .and_then(|metrics| metrics.sum_by_name(name))
        .map_or(0, |value| value.as_usize());
    own + plan
        .children()
        .into_iter()
        .map(|child| metric(child, name))
        .sum::<usize>()
}

/// A session that registers `path` as table `t`, looking rows up in its
/// indexes if `indexed`.
async fn session(path: &Path, indexed: bool) -> SessionContext {
    let state = SessionStateBuilder::new().with_default_features();
    let state = if indexed {
        state.with_parquet_index()
    } else {
        state
    };
    let ctx = SessionContext::new_with_state(state.build());
    let path = path.to_str().unwrap();
    ctx.register_parquet("t", path, ParquetReadOptions::default())
        .await
        .unwrap();
    ctx
}

/// How a query uses the indexes.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Lookup {
    /// No index applies.
    None,
    /// Indexes find exactly the rows the query keeps.
    Exact,
    /// Indexes find the rows the query keeps, and others.
    Superset,
}

const QUERIES: [(&str, Lookup); 23] = [
    ("id = 17", Lookup::Exact),
    ("id = 12345", Lookup::Exact),
    ("17 = id", Lookup::Exact),
    ("id IN (-500, 3, 499)", Lookup::Exact),
    ("id IN (3, NULL)", Lookup::Exact),
    ("id > 480", Lookup::Exact),
    ("id BETWEEN -10 AND 10", Lookup::Exact),
    ("id < -490 OR id >= 495", Lookup::Exact),
    ("id > 0 AND id < 0", Lookup::Exact),
    ("name = 'user-name-00042'", Lookup::Exact),
    ("name >= 'user-name-01990'", Lookup::Exact),
    ("name IN ('user-name-00001', 'nope')", Lookup::Exact),
    ("day = DATE '2022-01-10'", Lookup::Exact),
    ("day = DATE '2022-01-10' AND id > 400", Lookup::Exact),
    (
        "day IN (DATE '2022-01-09', DATE '2022-01-20') AND id IN (1, 2, 3)",
        Lookup::Exact,
    ),
    ("day > DATE '2022-01-18'", Lookup::Exact),
    ("score > 10.0", Lookup::Exact),
    ("score <= -12.0 OR score = 0.5", Lookup::Exact),
    ("id = 17 AND name = 'user-name-01077'", Lookup::Exact),
    ("id = 17 AND bucket = 2", Lookup::Superset),
    ("id = 5 OR name = 'user-name-00042'", Lookup::None),
    ("id IS NULL", Lookup::None),
    ("id + 1 = 18", Lookup::None),
];

#[tokio::test]
async fn scans_keep_the_rows_scans_without_indexes_keep() {
    let dir = TestDir::new("scans");
    let path = dir.file("t.parquet");
    write_file(&path);
    build_indexes(&path);
    let (plain, indexed) = (session(&path, false).await, session(&path, true).await);
    for (predicate, lookup) in QUERIES {
        let sql = format!("SELECT * FROM t WHERE {predicate}");
        let (expected, _) = run(&plain, &sql).await;
        let (rows, plan) = run(&indexed, &sql).await;
        assert_eq!(rows, expected, "{predicate}");
        let indexed_files = metric(&plan, "bbtree_indexed_files");
        assert_eq!(indexed_files > 0, lookup != Lookup::None, "{predicate}");
        let indexed_rows = metric(&plan, "bbtree_indexed_rows");
        match lookup {
            Lookup::Exact => assert_eq!(indexed_rows, rows.len(), "{predicate}"),
            Lookup::Superset => assert!(indexed_rows > rows.len(), "{predicate}"),
            Lookup::None => assert_eq!(indexed_rows, 0, "{predicate}"),
        }
        if lookup != Lookup::None {
            let shown = displayable(plan.as_ref()).indent(true).to_string();
            assert!(shown.contains("bbtree_index=true"), "{shown}");
        }
    }
}

#[tokio::test]
async fn tables_of_files_with_and_without_indexes() {
    let dir = TestDir::new("tables");
    write_file(&dir.file("a.parquet"));
    write_file(&dir.file("b.parquet"));
    build_indexes(&dir.file("a.parquet"));
    let (plain, indexed) = (session(&dir.0, false).await, session(&dir.0, true).await);
    for predicate in ["id = 17", "name = 'user-name-00042' AND id < 0"] {
        let sql = format!("SELECT * FROM t WHERE {predicate}");
        let (expected, _) = run(&plain, &sql).await;
        let (rows, plan) = run(&indexed, &sql).await;
        assert_eq!(rows, expected, "{predicate}");
        assert_eq!(metric(&plan, "bbtree_indexed_files"), 1, "{predicate}");
    }
}
