//! `compare`: a markdown report of how bbtree changed between two suite runs,
//! such as a pull request's base and head, and how it stands against std's
//! `BTreeMap`, from the runs' CSVs. Each run may come from its own build of
//! the suite: a column that one run lacks shows as missing.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use crate::command_line::{ComparisonRequest, Named};
use crate::map_adapters::MapKind;
use crate::perf_counters::PerfEvent;
use crate::report::{ReportNumber, RunLog, Statistic};
use crate::workload::MapSize;

/// A number the report compares, per operation: time, or a perf count.
#[derive(Clone, Copy)]
enum Metric {
    Time,
    Count(PerfEvent),
}

impl Metric {
    fn all() -> impl Iterator<Item = Self> {
        std::iter::once(Self::Time).chain(PerfEvent::ALL.map(Self::Count))
    }

    fn column(self) -> &'static str {
        match self {
            Self::Time => RunLog::TIME_COLUMN,
            Self::Count(event) => event.column(),
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Time => "ns",
            Self::Count(event) => event.title(),
        }
    }
}

/// What a run recorded of one metric of one cell.
#[derive(Clone, Copy, Debug, PartialEq)]
enum RecordedValue {
    /// The median over the cell's repetitions.
    Measured(f64),
    Failed,
    /// Not run (n/a, missing data), not in the run, or not counted.
    Absent,
}

impl RecordedValue {
    fn text(self) -> String {
        match self {
            Self::Measured(value) => ReportNumber::new(value).to_string(),
            Self::Failed => RunLog::FAILED.to_owned(),
            Self::Absent => "–".to_owned(),
        }
    }

    /// The change from `base` to `self`, in percent; from none, the new
    /// value.
    fn change_from(self, base: Self) -> String {
        match (base, self) {
            (Self::Failed, _) | (_, Self::Failed) => RunLog::FAILED.to_owned(),
            (Self::Measured(base), Self::Measured(head)) if base > 0.0 => {
                format!("{:+.1}%", (head / base - 1.0) * 100.0)
            }
            (Self::Measured(_), Self::Measured(0.0)) => "0".to_owned(),
            (Self::Measured(_), Self::Measured(head)) => format!("0 → {}", ReportNumber::new(head)),
            _ => "–".to_owned(),
        }
    }

    /// `self` as a multiple of `baseline`.
    fn ratio_to(self, baseline: Self) -> String {
        match (self, baseline) {
            (Self::Measured(value), Self::Measured(baseline)) if baseline > 0.0 => {
                format!("{:.2}×", value / baseline)
            }
            (Self::Failed, _) | (_, Self::Failed) => RunLog::FAILED.to_owned(),
            _ => "–".to_owned(),
        }
    }
}

/// A suite run, read back from its CSV.
struct RecordedRun {
    columns: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl RecordedRun {
    fn read(path: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(path)
            .map_err(|error| format!("reading {}: {error}", path.display()))?;
        Self::parse(&text).map_err(|problem| format!("{}: {problem}", path.display()))
    }

    fn parse(text: &str) -> Result<Self, String> {
        let mut lines = text
            .lines()
            .map(|line| line.split(',').map(str::to_owned).collect::<Vec<_>>());
        let run = Self {
            columns: lines.next().ok_or("no header")?,
            rows: lines.collect(),
        };
        for column in [
            RunLog::BENCHMARK_COLUMN,
            RunLog::MAP_COLUMN,
            RunLog::STATUS_COLUMN,
        ] {
            run.column_index(column)
                .ok_or_else(|| format!("no `{column}` column"))?;
        }
        Ok(run)
    }

    fn column_index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|column| column == name)
    }

    fn field<'run>(&self, row: &'run [String], column: &str) -> Option<&'run str> {
        let index = self.column_index(column)?;
        row.get(index).map(String::as_str)
    }

    /// The benchmarks: those at 10M first, the size the report leads with,
    /// then the rest, each in the order the run lists them.
    fn benchmarks(&self) -> Vec<&str> {
        let mut benchmarks: Vec<(&str, &str)> = Vec::new();
        for row in &self.rows {
            let benchmark = self
                .field(row, RunLog::BENCHMARK_COLUMN)
                .unwrap_or_default();
            if !benchmarks.iter().any(|&(listed, _)| listed == benchmark) {
                let size = self.field(row, RunLog::SIZE_COLUMN).unwrap_or_default();
                benchmarks.push((benchmark, size));
            }
        }
        benchmarks.sort_by_key(|&(_, size)| size != MapSize::TenMillion.name());
        benchmarks
            .into_iter()
            .map(|(benchmark, _)| benchmark)
            .collect()
    }

    fn value(&self, benchmark: &str, map: MapKind, metric: Metric) -> RecordedValue {
        let rows: Vec<&Vec<String>> = (self.rows.iter())
            .filter(|row| self.field(row, RunLog::BENCHMARK_COLUMN) == Some(benchmark))
            .filter(|row| self.field(row, RunLog::MAP_COLUMN) == Some(map.name()))
            .collect();
        let has_status =
            |row: &[String], status| self.field(row, RunLog::STATUS_COLUMN) == Some(status);
        if rows.iter().any(|row| has_status(row, RunLog::FAILED)) {
            return RecordedValue::Failed;
        }
        let values = (rows.iter())
            .filter(|row| has_status(row, RunLog::MEASURED))
            .filter_map(|row| self.field(row, metric.column())?.parse().ok())
            .collect();
        Statistic::Median
            .of(values)
            .map_or(RecordedValue::Absent, RecordedValue::Measured)
    }
}

/// The report: bbtree's change from the base run to the head run, bbtree
/// against std's `BTreeMap` in the head run, and every number. Without a
/// base run, the latter two.
pub(crate) struct RunComparison {
    head: RecordedRun,
    base: Option<RecordedRun>,
}

impl RunComparison {
    pub(crate) fn load(request: &ComparisonRequest) -> Result<Self, String> {
        Ok(Self {
            head: RecordedRun::read(request.head())?,
            base: request.base().map(RecordedRun::read).transpose()?,
        })
    }

    pub(crate) fn markdown(&self) -> String {
        let mut text = String::new();
        if let Some(base) = &self.base {
            text.push_str("#### bbtree, change from base to head\n\n");
            text.push_str(&self.metric_table(|benchmark, metric| {
                let value = |run: &RecordedRun| run.value(benchmark, MapKind::BBTree, metric);
                value(&self.head).change_from(value(base))
            }));
        }
        text.push_str("#### bbtree ÷ std BTreeMap, at head (lower is better)\n\n");
        text.push_str(&self.metric_table(|benchmark, metric| {
            let value = |map| self.head.value(benchmark, map, metric);
            value(MapKind::BBTree).ratio_to(value(MapKind::StdBTree))
        }));
        text.push_str("<details><summary>Every number</summary>\n\n");
        for metric in Metric::all() {
            let _ = write!(
                text,
                "#### {}\n\n{}",
                metric.title(),
                self.values_table(metric)
            );
        }
        text.push_str("</details>\n\n");
        text.push_str(
            "Per operation, medians over repetitions. ns is wall time; the counts are \
             perf's, in user space, of the timed regions on the benchmark's main thread. \
             Lines from memory are L1d fills from DRAM on AMD, and L3 misses on Intel.\n",
        );
        text
    }

    /// A row per benchmark of the head run and a column per metric, each
    /// cell `cell(benchmark, metric)`.
    fn metric_table(&self, cell: impl Fn(&str, Metric) -> String) -> String {
        let titles: Vec<&str> = Metric::all().map(Metric::title).collect();
        let mut text = format!(
            "| benchmark | {} |\n|---|{}\n",
            titles.join(" | "),
            "--:|".repeat(titles.len())
        );
        for benchmark in self.head.benchmarks() {
            let cells: Vec<String> = Metric::all()
                .map(|metric| cell(benchmark, metric))
                .collect();
            let _ = writeln!(text, "| {benchmark} | {} |", cells.join(" | "));
        }
        text.push('\n');
        text
    }

    /// `metric` of both maps, in each run.
    fn values_table(&self, metric: Metric) -> String {
        let runs: Vec<(&str, &RecordedRun)> = (self.base.iter())
            .map(|base| ("base", base))
            .chain([("head", &self.head)])
            .collect();
        let maps = [MapKind::BBTree, MapKind::StdBTree];
        let titles: Vec<String> = (maps.iter())
            .flat_map(|map| runs.iter().map(|(run, _)| format!("{} {run}", map.name())))
            .collect();
        let mut text = format!(
            "| benchmark | {} |\n|---|{}\n",
            titles.join(" | "),
            "--:|".repeat(titles.len())
        );
        for benchmark in self.head.benchmarks() {
            let cells: Vec<String> = (maps.iter())
                .flat_map(|&map| {
                    (runs.iter()).map(move |(_, run)| run.value(benchmark, map, metric).text())
                })
                .collect();
            let _ = writeln!(text, "| {benchmark} | {} |", cells.join(" | "));
        }
        text.push('\n');
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "benchmark,map,key,value,size,operation,repetition,status,operations,\
        elapsed_ns,ns_per_op,entries,map_bytes,bytes_per_entry,rss_growth_bytes,peak_rss_bytes,\
        cycles_per_op,instructions_per_op,branches_per_op,branch_misses_per_op,\
        l1d_load_misses_per_op,lines_from_memory_per_op";

    fn run(rows: &[&str]) -> RecordedRun {
        RecordedRun::parse(
            &[HEADER]
                .iter()
                .chain(rows)
                .copied()
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .expect("a valid run")
    }

    fn row(map: &str, repetition: u32, status: &str, ns: &str, instructions: &str) -> String {
        format!(
            "u64_u64_get_hit_1k,{map},u64,u64,1k,get_hit,{repetition},{status},1000000,1,{ns},\
             ,,,,1,1,{instructions},1,1,1,0"
        )
    }

    #[test]
    fn a_value_is_the_median_of_the_measured_repetitions() {
        let rows = [
            row("bbtree", 0, "ok", "12.0", "100"),
            row("bbtree", 1, "ok", "10.0", "100"),
            row("bbtree", 2, "ok", "11.0", "100"),
        ];
        let run = run(&rows.each_ref().map(String::as_str));
        assert_eq!(
            run.value("u64_u64_get_hit_1k", MapKind::BBTree, Metric::Time),
            RecordedValue::Measured(11.0)
        );
        assert_eq!(
            run.value("u64_u64_get_hit_1k", MapKind::StdBTree, Metric::Time),
            RecordedValue::Absent
        );
    }

    #[test]
    fn a_failed_repetition_fails_the_cell() {
        let rows = [
            row("bbtree", 0, "ok", "12.0", "100"),
            format!(
                "u64_u64_get_hit_1k,bbtree,u64,u64,1k,get_hit,1,failed{}",
                ",".repeat(14)
            ),
        ];
        let run = run(&rows.each_ref().map(String::as_str));
        assert_eq!(
            run.value("u64_u64_get_hit_1k", MapKind::BBTree, Metric::Time),
            RecordedValue::Failed
        );
    }

    #[test]
    fn a_column_the_run_lacks_is_absent() {
        let run =
            RecordedRun::parse("benchmark,map,status,ns_per_op\nu64_u64_get_hit_1k,bbtree,ok,12.5")
                .expect("a valid run");
        assert_eq!(
            run.value("u64_u64_get_hit_1k", MapKind::BBTree, Metric::Time),
            RecordedValue::Measured(12.5)
        );
        assert_eq!(
            run.value(
                "u64_u64_get_hit_1k",
                MapKind::BBTree,
                Metric::Count(PerfEvent::Branches)
            ),
            RecordedValue::Absent
        );
    }

    #[test]
    fn benchmarks_at_10m_come_first_and_the_rest_keep_the_run_order() {
        let run = RecordedRun::parse(
            "benchmark,map,size,status\n\
             u64_u64_get_hit_1k,bbtree,1k,ok\n\
             u64_u64_get_hit_10M,bbtree,10M,ok\n\
             u64_u64_get_hit_1M,bbtree,1M,ok\n\
             u64_u64_get_hit_10M,std-btree,10M,ok\n\
             u64_u64_get_miss_10M,bbtree,10M,ok",
        )
        .expect("a valid run");
        assert_eq!(
            run.benchmarks(),
            [
                "u64_u64_get_hit_10M",
                "u64_u64_get_miss_10M",
                "u64_u64_get_hit_1k",
                "u64_u64_get_hit_1M",
            ]
        );
    }

    #[test]
    fn changes_and_ratios() {
        let measured = RecordedValue::Measured;
        assert_eq!(measured(105.0).change_from(measured(100.0)), "+5.0%");
        assert_eq!(measured(99.0).change_from(measured(100.0)), "-1.0%");
        assert_eq!(measured(0.0).change_from(measured(0.0)), "0");
        assert_eq!(measured(0.0123).change_from(measured(0.0)), "0 → 0.0123");
        assert_eq!(measured(1.0).change_from(RecordedValue::Absent), "–");
        assert_eq!(RecordedValue::Failed.change_from(measured(1.0)), "failed");
        assert_eq!(measured(50.0).ratio_to(measured(100.0)), "0.50×");
        assert_eq!(measured(50.0).ratio_to(measured(0.0)), "–");
    }

    #[test]
    fn the_report_compares_bbtree_with_its_base_and_with_std() {
        let base = [
            row("bbtree", 0, "ok", "10.0", "200"),
            row("std-btree", 0, "ok", "20.0", "400"),
        ];
        let head = [
            row("bbtree", 0, "ok", "9.0", "200"),
            row("std-btree", 0, "ok", "20.0", "400"),
        ];
        let comparison = RunComparison {
            head: run(&head.each_ref().map(String::as_str)),
            base: Some(run(&base.each_ref().map(String::as_str))),
        };
        let markdown = comparison.markdown();
        let lines: Vec<&str> = markdown.lines().collect();
        let row_after = |title: &str| {
            let at = lines.iter().position(|line| *line == title).expect(title);
            lines[at + 4]
        };
        assert_eq!(
            row_after("#### bbtree, change from base to head"),
            "| u64_u64_get_hit_1k | -10.0% | +0.0% | +0.0% | +0.0% | +0.0% | +0.0% | 0 |"
        );
        assert_eq!(
            row_after("#### bbtree ÷ std BTreeMap, at head (lower is better)"),
            "| u64_u64_get_hit_1k | 0.45× | 1.00× | 0.50× | 1.00× | 1.00× | 1.00× | – |"
        );
        assert_eq!(
            row_after("#### ns"),
            "| u64_u64_get_hit_1k | 10.0 | 9.00 | 20.0 | 20.0 |"
        );
    }
}
