//! The suite runner: measures every selected cell, every repetition, in a
//! fresh child process, then writes the CSV and the markdown summary.
//!
//! Repetitions are the outer loop, so a slow drift (temperature, another
//! process) spreads over all cells instead of piling onto a few.
//!
//! Cells whose keys are read from a dataset that is missing don't run; the
//! report shows them as missing data, and the rest of the suite runs.

use std::fs;
use std::path::Path;
use std::process::Command;

use crate::cell_runner::CellMeasurement;
use crate::command_line::{CellRequest, Named, SuiteRequest};
use crate::datasets::{DataDirectory, DataFile};
use crate::map_adapters::MapKind;
use crate::memory::RunMode;
use crate::perf_counters::{PerfCounts, PerfStat};
use crate::report::{RunHeader, RunLog, RunOutcome, RunRecord, Summary};
use crate::suites::SuiteRow;
use crate::workload::Cell;

pub(crate) struct SuiteRunner {
    request: SuiteRequest,
    mode: RunMode,
}

impl SuiteRunner {
    pub(crate) fn new(request: SuiteRequest, mode: RunMode) -> Self {
        Self { request, mode }
    }

    pub(crate) fn run(self) -> Result<(), String> {
        if self.request.perf() && matches!(self.mode, RunMode::Memory(_)) {
            return Err("--perf needs the timing binary".to_owned());
        }
        let rows = self.selected_rows();
        let maps: Vec<MapKind> = (MapKind::ALL.iter().copied())
            .filter(|&map| self.request.filter().admits_map(map))
            .filter(|map| rows.iter().any(|row| row.suite().maps().contains(map)))
            .collect();
        if rows.is_empty() || maps.is_empty() {
            return Err("no cell matches the filters".to_owned());
        }
        // Every row on each of its suite's maps, split into the cells that
        // run, the cells that are `n/a` and the cells whose data is missing.
        let (applicable, not_applicable): (Vec<_>, Vec<_>) = (rows.iter())
            .flat_map(|&row| {
                (maps.iter())
                    .filter(move |map| row.suite().maps().contains(map))
                    .map(move |&map| (row, Cell::new(map, row.workload())))
            })
            .partition(|&(_, cell)| cell.is_applicable());
        let missing_files = Self::missing_data_files(&applicable);
        let (runnable, missing_data): (Vec<_>, Vec<_>) =
            (applicable.into_iter()).partition(|&(_, cell)| {
                let needed = cell.workload().key().data_files();
                !needed.iter().any(|file| missing_files.contains(file))
            });
        if self.request.list_only() {
            self.print_cells(&runnable, &not_applicable, &missing_data);
            return Ok(());
        }

        let mut header = RunHeader::collect(&self.request, self.mode);
        let directory = self.request.output_directory();
        fs::create_dir_all(directory)
            .map_err(|error| format!("creating {}: {error}", directory.display()))?;
        let file_stem = format!("{}-{}", header.file_stamp(), self.mode.name());
        let mut log = RunLog::create(directory.join(format!("{file_stem}.csv")))?;
        let perf_stat = (self.request.perf())
            .then(|| PerfStat::prepare(directory.join(format!(".perf-{file_stem}"))))
            .transpose()?;
        let executable =
            std::env::current_exe().map_err(|error| format!("finding this executable: {error}"))?;

        let mut records: Vec<RunRecord> =
            (not_applicable.iter())
                .map(|&(row, cell)| RunRecord::not_run(row, cell.map(), RunOutcome::NotApplicable))
                .chain(missing_data.iter().map(|&(row, cell)| {
                    RunRecord::not_run(row, cell.map(), RunOutcome::MissingData)
                }))
                .collect();
        for record in &records {
            log.append(record)?;
        }
        let run_count = runnable.len() * self.request.repetitions();
        for repetition in 0..self.request.repetitions() {
            for (index, &(row, cell)) in runnable.iter().enumerate() {
                let outcome = match self.measure_in_child(&executable, cell, perf_stat.as_ref()) {
                    Ok((measurement, perf_counts)) => RunOutcome::Measured {
                        measurement,
                        perf_counts,
                    },
                    Err(reason) => RunOutcome::Failed(reason),
                };
                let run_number = repetition * runnable.len() + index + 1;
                eprintln!(
                    "[{run_number}/{run_count}] {}: {}",
                    Self::cell_label(row, cell),
                    outcome.brief()
                );
                let record = RunRecord::run(row, cell.map(), repetition, outcome);
                log.append(&record)?;
                records.push(record);
            }
        }

        header.record_end();
        let summary = Summary::new(&header, &rows, &maps, &records, self.mode).markdown();
        let summary_path = directory.join(format!("{file_stem}.md"));
        fs::write(&summary_path, &summary)
            .map_err(|error| format!("writing {}: {error}", summary_path.display()))?;
        println!("{summary}");
        eprintln!(
            "wrote {} and {}",
            log.path().display(),
            summary_path.display()
        );
        Ok(())
    }

    /// The rows the request selects. Memory mode keeps the builds only.
    fn selected_rows(&self) -> Vec<SuiteRow> {
        let mut rows = SuiteRow::select(self.request.suites(), self.request.filter());
        if matches!(self.mode, RunMode::Memory(_)) {
            rows.retain(|row| row.workload().operation().builds_maps());
        }
        rows
    }

    /// The data files that `cells` read which aren't there with their
    /// pinned size, each reported on stderr.
    fn missing_data_files(cells: &[(SuiteRow, Cell)]) -> Vec<DataFile> {
        let needed: Vec<DataFile> = (DataFile::ALL.into_iter())
            .filter(|file| {
                (cells.iter()).any(|(_, cell)| cell.workload().key().data_files().contains(file))
            })
            .collect();
        if needed.is_empty() {
            return needed;
        }
        let missing = match DataDirectory::from_environment() {
            Ok(directory) => (needed.into_iter())
                .filter(|&file| match directory.check_present(file) {
                    Ok(_) => false,
                    Err(problem) => {
                        eprintln!("missing data: {problem}");
                        true
                    }
                })
                .collect(),
            Err(problem) => {
                eprintln!("missing data: {problem}");
                needed
            }
        };
        if !missing.is_empty() {
            eprintln!(
                "the cells that read them don't run; `bbtree-bench verify-data` checks the \
                 datasets, and `bbtree-bench fetch` downloads them"
            );
        }
        missing
    }

    fn print_cells(
        &self,
        runnable: &[(SuiteRow, Cell)],
        not_applicable: &[(SuiteRow, Cell)],
        missing_data: &[(SuiteRow, Cell)],
    ) {
        for &(row, cell) in runnable {
            println!("{}", Self::cell_label(row, cell));
        }
        for &(row, cell) in not_applicable {
            println!("{} (n/a)", Self::cell_label(row, cell));
        }
        for &(row, cell) in missing_data {
            println!("{} (missing data)", Self::cell_label(row, cell));
        }
        println!(
            "{} cells × {} repetition(s) = {} processes; {} cells n/a; {} cells missing data",
            runnable.len(),
            self.request.repetitions(),
            runnable.len() * self.request.repetitions(),
            not_applicable.len(),
            missing_data.len()
        );
    }

    fn cell_label(row: SuiteRow, cell: Cell) -> String {
        let workload = cell.workload();
        format!(
            "{} {} {}→{} {} {}",
            row.suite().name(),
            cell.map().name(),
            workload.key().name(),
            workload.value().name(),
            workload.size().name(),
            workload.operation().name()
        )
    }

    /// Runs one cell in a child process of this executable, under `perf
    /// stat` if asked. The child's stderr is passed on, indented, except for
    /// perf's notices.
    fn measure_in_child(
        &self,
        executable: &Path,
        cell: Cell,
        perf_stat: Option<&PerfStat>,
    ) -> Result<(CellMeasurement, Option<PerfCounts>), String> {
        let perf_control = perf_stat.map(PerfStat::control_argument);
        let arguments = CellRequest::arguments(cell, self.request.scale(), perf_control);
        let mut command = match perf_stat {
            Some(perf_stat) => perf_stat.command(executable, &arguments),
            None => {
                let mut command = Command::new(executable);
                command.args(&arguments);
                command
            }
        };
        let output = (command.output()).map_err(|error| format!("starting the child: {error}"))?;
        for line in String::from_utf8_lossy(&output.stderr).lines() {
            if !PerfStat::is_switch_notice(line) {
                eprintln!("    {line}");
            }
        }
        if !output.status.success() {
            return Err(format!("the child exited with {}", output.status));
        }
        let measurement = CellMeasurement::parse(String::from_utf8_lossy(&output.stdout).trim())?;
        let perf_counts = perf_stat.map(PerfStat::read_counts).transpose()?;
        Ok((measurement, perf_counts))
    }
}
