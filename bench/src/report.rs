//! A suite run's results: a CSV row per run, and a markdown summary under a
//! header that records where and how the numbers were made.

use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufWriter, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use crate::cell_runner::CellMeasurement;
use crate::command_line::{Named, SuiteRequest};
use crate::map_adapters::MapKind;
use crate::memory::RunMode;
use crate::perf_counters::{PerfCounts, PerfEvent};
use crate::suites::SuiteRow;
use crate::workload::Scale;

/// Where and how a run's numbers were made, and how long it took on how busy
/// a machine.
pub(crate) struct RunHeader {
    started_at: String,
    commit: String,
    cpu: String,
    allocator: String,
    vector_extensions: &'static str,
    bbtree_prefetch: &'static str,
    settings: String,
    command_line: String,
    started: Instant,
    load_average_at_start: String,
    /// Set by `record_end`.
    end: Option<RunEnd>,
}

/// When a run ended.
struct RunEnd {
    wall_time: Duration,
    load_average: String,
}

impl RunHeader {
    pub(crate) fn collect(request: &SuiteRequest, mode: RunMode) -> Self {
        let scale = match request.scale() {
            Scale::Full => "full size",
            Scale::Quick => "--quick (10× smaller)",
        };
        let perf = if request.perf() {
            ", perf counters on (user space)"
        } else {
            ""
        };
        Self {
            started_at: Self::command_output("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"])
                .unwrap_or_else(|| "unknown".to_owned()),
            commit: Self::git_commit(),
            cpu: Self::cpu_model(),
            allocator: mode.allocator_description(),
            vector_extensions: Self::vector_extensions(),
            bbtree_prefetch: Self::bbtree_prefetch(),
            settings: format!(
                "{} mode, {scale}, {} repetition(s) per cell{perf}",
                mode.name(),
                request.repetitions()
            ),
            command_line: request.command_line().to_owned(),
            started: Instant::now(),
            load_average_at_start: Self::load_average(),
            end: None,
        }
    }

    /// The widest vector extension the binary was built for, which the
    /// maps' searches may use: `.cargo/config.toml` builds for the host CPU.
    fn vector_extensions() -> &'static str {
        if cfg!(target_feature = "avx512f") {
            "AVX-512"
        } else if cfg!(target_feature = "avx2") {
            "AVX2"
        } else {
            "SSE2 (x86-64 baseline)"
        }
    }

    /// Whether bbtree was built with its software prefetch (the bench's
    /// `prefetch` feature).
    fn bbtree_prefetch() -> &'static str {
        if cfg!(feature = "prefetch") {
            "on (`--features prefetch`)"
        } else {
            "off (default)"
        }
    }

    /// Records the run's wall time and the load average at its end, once
    /// every cell has run.
    pub(crate) fn record_end(&mut self) {
        self.end = Some(RunEnd {
            wall_time: self.started.elapsed(),
            load_average: Self::load_average(),
        });
    }

    /// The start time, safe for file names: `20261007T143810Z`.
    pub(crate) fn file_stamp(&self) -> String {
        (self.started_at.chars())
            .filter(char::is_ascii_alphanumeric)
            .collect()
    }

    fn markdown(&self) -> String {
        let mut text = format!(
            "| | |\n|---|---|\n| date | {} |\n| commit | {} |\n| CPU | {} |\n| allocator | {} |\n\
             | vector extensions | {} |\n| bbtree prefetch | {} |\n| settings | {} |\n\
             | command | `{}` |\n| load average (1, 5, 15 min) at start | {} |\n",
            self.started_at,
            self.commit,
            self.cpu,
            self.allocator,
            self.vector_extensions,
            self.bbtree_prefetch,
            self.settings,
            self.command_line,
            self.load_average_at_start
        );
        if let Some(end) = &self.end {
            let seconds = end.wall_time.as_secs();
            let _ = write!(
                text,
                "| load average at end | {} |\n| wall time | {} min {:02} s |\n",
                end.load_average,
                seconds / 60,
                seconds % 60
            );
        }
        text
    }

    /// The load average over the last 1, 5 and 15 minutes, which shows
    /// whether other work shared the machine.
    fn load_average() -> String {
        let text = std::fs::read_to_string("/proc/loadavg").unwrap_or_default();
        let fields: Vec<&str> = text.split_whitespace().take(3).collect();
        if fields.len() == 3 {
            fields.join(", ")
        } else {
            "unknown".to_owned()
        }
    }

    /// `git rev-parse HEAD` in this repository, marked dirty if anything
    /// tracked or unignored has changed.
    fn git_commit() -> String {
        let repository = concat!(env!("CARGO_MANIFEST_DIR"), "/..");
        let Some(commit) = Self::command_output("git", &["-C", repository, "rev-parse", "HEAD"])
        else {
            return "unknown".to_owned();
        };
        let status = Self::command_output("git", &["-C", repository, "status", "--porcelain"]);
        if status.is_some_and(|changes| !changes.is_empty()) {
            format!("{commit} (dirty)")
        } else {
            commit
        }
    }

    fn cpu_model() -> String {
        let cpu_info = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
        (cpu_info.lines())
            .find_map(|line| line.strip_prefix("model name"))
            .and_then(|line| line.split_once(':'))
            .map_or_else(
                || "unknown".to_owned(),
                |(_, model)| model.trim().to_owned(),
            )
    }

    /// The trimmed stdout of a command that succeeded.
    fn command_output(program: &str, arguments: &[&str]) -> Option<String> {
        let output = Command::new(program).args(arguments).output().ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }
}

/// How one run of a cell ended.
pub(crate) enum RunOutcome {
    /// The map lacks the operation, so the cell doesn't run.
    NotApplicable,
    /// A data file the keys are read from is missing or has the wrong size,
    /// so the cell doesn't run.
    MissingData,
    Failed(String),
    Measured {
        measurement: CellMeasurement,
        perf_counts: Option<PerfCounts>,
    },
}

impl RunOutcome {
    const MISSING_DATA: &str = "missing data (run verify-data)";

    fn summary_value(&self) -> Option<f64> {
        match self {
            Self::Measured { measurement, .. } => Some(measurement.summary_value()),
            Self::NotApplicable | Self::MissingData | Self::Failed(_) => None,
        }
    }

    /// A few words for the progress line.
    pub(crate) fn brief(&self) -> String {
        match self {
            Self::NotApplicable => "n/a".to_owned(),
            Self::MissingData => Self::MISSING_DATA.to_owned(),
            Self::Failed(reason) => format!("FAILED: {reason}"),
            Self::Measured { measurement, .. } => match measurement {
                CellMeasurement::Timing { .. } => {
                    format!("{:.1} ns/op", measurement.summary_value())
                }
                CellMeasurement::Memory { .. } => {
                    format!("{:.1} B/entry", measurement.summary_value())
                }
            },
        }
    }
}

/// One run of one cell, or the single record of a cell that doesn't run:
/// `n/a`, or missing data.
pub(crate) struct RunRecord {
    row: SuiteRow,
    map: MapKind,
    repetition: Option<usize>,
    outcome: RunOutcome,
}

impl RunRecord {
    /// The record of a cell that doesn't run, for `outcome`'s reason.
    pub(crate) fn not_run(row: SuiteRow, map: MapKind, outcome: RunOutcome) -> Self {
        assert!(
            matches!(outcome, RunOutcome::NotApplicable | RunOutcome::MissingData),
            "cells that run have a record per repetition"
        );
        Self {
            row,
            map,
            repetition: None,
            outcome,
        }
    }

    pub(crate) fn run(row: SuiteRow, map: MapKind, repetition: usize, outcome: RunOutcome) -> Self {
        Self {
            row,
            map,
            repetition: Some(repetition),
            outcome,
        }
    }

    fn is_of(&self, row: SuiteRow, map: MapKind) -> bool {
        self.map == map && self.row.workload() == row.workload()
    }

    fn csv_row(&self) -> String {
        let workload = self.row.workload();
        let status = match self.outcome {
            RunOutcome::NotApplicable => "n/a",
            RunOutcome::MissingData => "missing-data",
            RunOutcome::Failed(_) => "failed",
            RunOutcome::Measured { .. } => "ok",
        };
        let identity = [
            workload.name(),
            self.map.name().to_owned(),
            workload.key().name().to_owned(),
            workload.value().name().to_owned(),
            workload.size().name().to_owned(),
            workload.operation().name().to_owned(),
            self.repetition
                .map_or_else(String::new, |repetition| repetition.to_string()),
            status.to_owned(),
        ];
        let (measured, perf) = match &self.outcome {
            RunOutcome::Measured {
                measurement,
                perf_counts,
            } => (
                Self::measured_fields(*measurement),
                Self::perf_fields(*measurement, perf_counts.as_ref()),
            ),
            _ => Default::default(),
        };
        let fields: Vec<String> = identity.into_iter().chain(measured).chain(perf).collect();
        fields.join(",")
    }

    /// The fields of `RunLog::MEASURED_COLUMNS`.
    fn measured_fields(measurement: CellMeasurement) -> [String; 8] {
        let summary_value = format!("{:.3}", measurement.summary_value());
        let none = String::new;
        match measurement {
            CellMeasurement::Timing {
                operations,
                elapsed_ns,
                peak_rss_bytes,
            } => [
                operations.to_string(),
                elapsed_ns.to_string(),
                summary_value,
                none(),
                none(),
                none(),
                none(),
                peak_rss_bytes.to_string(),
            ],
            CellMeasurement::Memory {
                entries,
                map_bytes,
                rss_growth_bytes,
                peak_rss_bytes,
            } => [
                none(),
                none(),
                none(),
                entries.to_string(),
                map_bytes.to_string(),
                summary_value,
                rss_growth_bytes.to_string(),
                peak_rss_bytes.to_string(),
            ],
        }
    }

    /// The perf counts per operation, in `PerfEvent::ALL` order.
    fn perf_fields(measurement: CellMeasurement, perf_counts: Option<&PerfCounts>) -> [String; 5] {
        let CellMeasurement::Timing { operations, .. } = measurement else {
            return Default::default();
        };
        PerfEvent::ALL.map(|event| {
            (perf_counts.and_then(|counts| counts.per_operation(event, operations)))
                .map_or_else(String::new, |count| format!("{count:.3}"))
        })
    }
}

/// The CSV of every run, written as runs finish, so an interrupted suite
/// keeps what it measured.
pub(crate) struct RunLog {
    path: PathBuf,
    writer: BufWriter<File>,
}

impl RunLog {
    const IDENTITY_COLUMNS: [&str; 8] = [
        "benchmark",
        "map",
        "key",
        "value",
        "size",
        "operation",
        "repetition",
        "status",
    ];

    /// Timing runs fill the first three and the last; memory runs the rest.
    const MEASURED_COLUMNS: [&str; 8] = [
        "operations",
        "elapsed_ns",
        "ns_per_op",
        "entries",
        "map_bytes",
        "bytes_per_entry",
        "rss_growth_bytes",
        "peak_rss_bytes",
    ];

    pub(crate) fn create(path: PathBuf) -> Result<Self, String> {
        let file =
            File::create(&path).map_err(|error| format!("creating {}: {error}", path.display()))?;
        let mut log = Self {
            path,
            writer: BufWriter::new(file),
        };
        let columns: Vec<&str> = (Self::IDENTITY_COLUMNS.into_iter())
            .chain(Self::MEASURED_COLUMNS)
            .chain(PerfEvent::ALL.map(PerfEvent::column))
            .collect();
        log.write_line(&columns.join(","))?;
        Ok(log)
    }

    pub(crate) fn append(&mut self, record: &RunRecord) -> Result<(), String> {
        self.write_line(&record.csv_row())
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    fn write_line(&mut self, line: &str) -> Result<(), String> {
        writeln!(self.writer, "{line}")
            .and_then(|()| self.writer.flush())
            .map_err(|error| format!("writing {}: {error}", self.path.display()))
    }
}

/// Which number of a cell's runs a summary table shows.
#[derive(Clone, Copy)]
enum Statistic {
    Median,
    Minimum,
}

impl Statistic {
    fn of(self, mut values: Vec<f64>) -> Option<f64> {
        values.sort_by(f64::total_cmp);
        let middle = values.len() / 2;
        match self {
            _ if values.is_empty() => None,
            Self::Minimum => Some(values[0]),
            Self::Median if values.len() % 2 == 1 => Some(values[middle]),
            Self::Median => Some(f64::midpoint(values[middle - 1], values[middle])),
        }
    }
}

/// The markdown summary: one row per workload, one column per map, and
/// bbtree against std's `BTreeMap`.
pub(crate) struct Summary<'run> {
    header: &'run RunHeader,
    rows: &'run [SuiteRow],
    maps: &'run [MapKind],
    records: &'run [RunRecord],
    mode: RunMode,
}

impl<'run> Summary<'run> {
    pub(crate) fn new(
        header: &'run RunHeader,
        rows: &'run [SuiteRow],
        maps: &'run [MapKind],
        records: &'run [RunRecord],
        mode: RunMode,
    ) -> Self {
        Self {
            header,
            rows,
            maps,
            records,
            mode,
        }
    }

    pub(crate) fn markdown(&self) -> String {
        let mut text = format!(
            "# bbtree benchmark: {}\n\n{}\n",
            self.mode.name(),
            self.header.markdown()
        );
        match self.mode {
            RunMode::Timing => {
                text.push_str(
                    "Time per operation: per entry for `iterate` and `bulk_build`, per scan \
                     for `range64`, per call otherwise.\n\n",
                );
                text.push_str(&self.table("Median ns per operation", Statistic::Median));
                if self.has_repetitions() {
                    text.push_str(&self.table("Fastest run, ns per operation", Statistic::Minimum));
                }
            }
            RunMode::Memory(_) => {
                text.push_str(
                    "Bytes per entry: everything the maps hold on the heap, keys' and values' \
                     own allocations included, plus the maps' inline headers.\n\n",
                );
                text.push_str(&self.table("Bytes per entry", Statistic::Median));
            }
        }
        text
    }

    fn has_repetitions(&self) -> bool {
        self.records
            .iter()
            .any(|record| record.repetition > Some(0))
    }

    fn table(&self, title: &str, statistic: Statistic) -> String {
        let comparison = self.comparison_title();
        let mut columns = vec!["benchmark"];
        columns.extend(self.maps.iter().map(|map| map.title()));
        columns.extend(comparison);
        let mut alignments = vec!["---"];
        alignments.resize(columns.len(), "--:");

        let mut text = format!(
            "## {title}\n\n| {} |\n|{}|\n",
            columns.join(" | "),
            alignments.join("|")
        );
        for &row in self.rows {
            let mut cells = vec![row.workload().name()];
            cells.extend(
                self.maps
                    .iter()
                    .map(|&map| self.cell_text(row, map, statistic)),
            );
            if comparison.is_some() {
                cells.push(self.comparison_text(row, statistic));
            }
            let _ = writeln!(text, "| {} |", cells.join(" | "));
        }
        text.push('\n');
        text
    }

    fn outcomes(&self, row: SuiteRow, map: MapKind) -> impl Iterator<Item = &RunOutcome> {
        (self.records.iter())
            .filter(move |record| record.is_of(row, map))
            .map(|record| &record.outcome)
    }

    fn value(&self, row: SuiteRow, map: MapKind, statistic: Statistic) -> Option<f64> {
        let values = self
            .outcomes(row, map)
            .filter_map(RunOutcome::summary_value)
            .collect();
        statistic.of(values)
    }

    fn cell_text(&self, row: SuiteRow, map: MapKind, statistic: Statistic) -> String {
        if self
            .outcomes(row, map)
            .any(|outcome| matches!(outcome, RunOutcome::NotApplicable))
        {
            return "n/a".to_owned();
        }
        if self
            .outcomes(row, map)
            .any(|outcome| matches!(outcome, RunOutcome::MissingData))
        {
            return RunOutcome::MISSING_DATA.to_owned();
        }
        if self
            .outcomes(row, map)
            .any(|outcome| matches!(outcome, RunOutcome::Failed(_)))
        {
            return "failed".to_owned();
        }
        self.value(row, map, statistic)
            .map_or_else(|| "–".to_owned(), Self::format_number)
    }

    /// The comparison column's title, when both maps it compares are in the
    /// table.
    fn comparison_title(&self) -> Option<&'static str> {
        let compared = [MapKind::BBTree, MapKind::StdBTree];
        if !compared.iter().all(|map| self.maps.contains(map)) {
            return None;
        }
        Some(match self.mode {
            RunMode::Timing => "bbtree speedup vs std BTreeMap",
            RunMode::Memory(_) => "bbtree ÷ std BTreeMap",
        })
    }

    fn comparison_text(&self, row: SuiteRow, statistic: Statistic) -> String {
        let bbtree = self.value(row, MapKind::BBTree, statistic);
        let std_btree = self.value(row, MapKind::StdBTree, statistic);
        let (Some(bbtree), Some(std_btree)) = (bbtree, std_btree) else {
            return "n/a".to_owned();
        };
        let ratio = match self.mode {
            RunMode::Timing => std_btree / bbtree,
            RunMode::Memory(_) => bbtree / std_btree,
        };
        format!("{ratio:.2}×")
    }

    /// Three significant digits or so: `2.31`, `23.1`, `231`.
    fn format_number(value: f64) -> String {
        match value {
            value if value < 10.0 => format!("{value:.2}"),
            value if value < 100.0 => format!("{value:.1}"),
            value => format!("{value:.0}"),
        }
    }
}
