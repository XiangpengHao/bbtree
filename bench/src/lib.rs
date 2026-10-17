//! bbtree's benchmark suite: `bbtree::unsync::BBTreeMap` (`bbtree-unsync`),
//! and `bbtree::BBTreeMap` (`bbtree`, the map threads share, where `--map`
//! names it), against std's `BTreeMap`, hashbrown's `HashMap` and std's
//! `HashMap`, on the key types, value types, sizes and operations that
//! `doc/02-field_study.md` §6 derives from how Rust code uses maps.
//!
//! - A *workload* is a key kind, a value kind, a size and an operation; a
//!   *cell* is a workload on one map. Suites are lists of workloads
//!   (`suites.rs`), and each workload runs on every map.
//! - The suite runner (`suite_runner.rs`) measures every cell and repetition
//!   in a fresh child process of the same executable (`cell_runner.rs`), so
//!   that no cell inherits another one's heap or cache state.
//! - The binary fixes the global allocator: `bbtree-bench` times cells on
//!   plain mimalloc, and `bbtree-bench-memory` counts live bytes instead
//!   (`memory.rs`).
//! - `bbtree-scaling` measures throughput as threads are added, on maps
//!   threads share (`scaling.rs`).
//! - Some key kinds are read from real-world datasets (`datasets.rs`), which
//!   live outside the repository: `fetch` downloads them and `verify-data`
//!   checks them.

use std::process::ExitCode;

mod cell_runner;
mod command_line;
mod concurrent_maps;
mod datasets;
mod keys;
mod latency;
mod map_adapters;
mod memory;
mod operations;
mod parallel_setup;
mod perf_counters;
mod random;
mod report;
mod run_comparison;
mod scaling;
#[cfg(feature = "inspect")]
pub mod separator_census;
mod suite_runner;
mod suites;
mod values;
mod workload;

use cell_runner::CellRun;
use command_line::Invocation;
use datasets::{DataDirectory, DatasetFetch};
use memory::RunMode;
use run_comparison::RunComparison;
use suite_runner::SuiteRunner;

pub use memory::{BaseAllocator, CountingAllocator};
pub use scaling::run as run_scaling;

/// Runs either binary's command line. The memory binary passes its global
/// allocator, whose live-byte count its cells report.
pub fn run(counting_allocator: Option<&'static CountingAllocator>) -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let mode = RunMode::new(counting_allocator);
    let outcome = match Invocation::parse(&arguments) {
        Ok(Invocation::Help) => {
            println!("{}", Invocation::usage());
            Ok(())
        }
        Ok(Invocation::VerifyData) => {
            DataDirectory::from_environment().and_then(|directory| directory.verify())
        }
        Ok(Invocation::FetchData) => DataDirectory::from_environment()
            .and_then(|directory| DatasetFetch::fetch_into(&directory)),
        Ok(Invocation::RunSuites(request)) => SuiteRunner::new(request, mode).run(),
        Ok(Invocation::CompareRuns(request)) => {
            RunComparison::load(&request).map(|comparison| print!("{}", comparison.markdown()))
        }
        Ok(Invocation::MeasureCell(request)) => CellRun::new(request, mode).measure_and_report(),
        Err(message) => Err(format!("{message}\n\n{}", Invocation::usage())),
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}
