//! `--perf`: hardware counters for the timed regions only.
//!
//! The runner starts each child under `perf stat --delay=-1 --control
//! fifo:CONTROL,ACK`, which starts with the counters off, and the child's
//! stopwatch switches them on and off around each timed region through the
//! control FIFO. The events count user space only (`:u`), which unprivileged
//! perf allows and which keeps the FIFO round trips out of the counts; kernel
//! time, such as page faults, shows in ns/op but not in the counters.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Copy)]
pub(crate) enum PerfEvent {
    Cycles,
    Instructions,
    BranchMisses,
    L1dLoadMisses,
    /// Cache lines filled from DRAM or I/O (AMD Zen 4 and later).
    DramFills,
}

impl PerfEvent {
    pub(crate) const ALL: [Self; 5] = [
        Self::Cycles,
        Self::Instructions,
        Self::BranchMisses,
        Self::L1dLoadMisses,
        Self::DramFills,
    ];

    fn perf_name(self) -> &'static str {
        match self {
            Self::Cycles => "cycles:u",
            Self::Instructions => "instructions:u",
            Self::BranchMisses => "branch-misses:u",
            Self::L1dLoadMisses => "L1-dcache-load-misses:u",
            Self::DramFills => "ls_any_fills_from_sys.dram_io_all:u",
        }
    }

    /// The CSV column of the count per operation.
    pub(crate) fn column(self) -> &'static str {
        match self {
            Self::Cycles => "cycles_per_op",
            Self::Instructions => "instructions_per_op",
            Self::BranchMisses => "branch_misses_per_op",
            Self::L1dLoadMisses => "l1d_load_misses_per_op",
            Self::DramFills => "dram_fills_per_op",
        }
    }
}

/// The runner's side: the control FIFOs and the counts file, in a scratch
/// directory that lives as long as this does.
pub(crate) struct PerfStat {
    directory: PathBuf,
}

impl PerfStat {
    /// Creates the scratch directory and its FIFOs. Its path must not
    /// contain a comma, which separates the FIFOs in perf's arguments.
    pub(crate) fn prepare(directory: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(&directory)
            .map_err(|error| format!("creating {}: {error}", directory.display()))?;
        let perf_stat = Self { directory };
        for fifo in [perf_stat.control_path(), perf_stat.ack_path()] {
            let created = Command::new("mkfifo").arg(&fifo).status();
            if !created.is_ok_and(|status| status.success()) {
                return Err(format!("mkfifo {} failed", fifo.display()));
            }
        }
        Ok(perf_stat)
    }

    fn control_path(&self) -> PathBuf {
        self.directory.join("control")
    }

    fn ack_path(&self) -> PathBuf {
        self.directory.join("ack")
    }

    fn counts_path(&self) -> PathBuf {
        self.directory.join("counts.csv")
    }

    /// The child's `--perf-control` value: the FIFOs it switches the
    /// counters through, as `CONTROL,ACK`.
    pub(crate) fn control_argument(&self) -> String {
        format!(
            "{},{}",
            self.control_path().display(),
            self.ack_path().display()
        )
    }

    /// `perf stat` running `program` with `arguments`, counters off until
    /// the program switches them on.
    pub(crate) fn command(&self, program: &Path, arguments: &[String]) -> Command {
        let events: Vec<&str> = PerfEvent::ALL
            .iter()
            .map(|event| event.perf_name())
            .collect();
        let mut command = Command::new("perf");
        command
            .args(["stat", "-x", ",", "--delay=-1"])
            .arg("-o")
            .arg(self.counts_path())
            .arg(format!("--control=fifo:{}", self.control_argument()))
            .args(["-e", &events.join(",")])
            .arg("--")
            .arg(program)
            .args(arguments);
        command
    }

    /// The notice perf prints to stderr at every switch, which the runner
    /// drops.
    pub(crate) fn is_switch_notice(line: &str) -> bool {
        matches!(line, "Events enabled" | "Events disabled")
    }

    /// The counts of the last run.
    pub(crate) fn read_counts(&self) -> Result<PerfCounts, String> {
        let path = self.counts_path();
        let text = fs::read_to_string(&path)
            .map_err(|error| format!("reading {}: {error}", path.display()))?;
        Ok(PerfCounts::parse(&text))
    }
}

impl Drop for PerfStat {
    fn drop(&mut self) {
        // Best effort: a leftover scratch directory is harmless.
        let _ = fs::remove_dir_all(&self.directory);
    }
}

/// The counts of one run by event, `None` where perf couldn't count it.
pub(crate) struct PerfCounts {
    counts: [Option<f64>; PerfEvent::ALL.len()],
}

impl PerfCounts {
    /// Parses `perf stat -x ,` output: `value,unit,event,…` lines, after
    /// `#` comments.
    fn parse(text: &str) -> Self {
        let mut counts = [None; PerfEvent::ALL.len()];
        for line in text.lines() {
            let fields: Vec<&str> = line.split(',').collect();
            let [value, _unit, event, ..] = fields.as_slice() else {
                continue;
            };
            if let Some(index) = PerfEvent::ALL
                .iter()
                .position(|known| known.perf_name() == *event)
            {
                counts[index] = value.parse().ok();
            }
        }
        Self { counts }
    }

    pub(crate) fn per_operation(&self, event: PerfEvent, operations: u64) -> Option<f64> {
        self.counts[event as usize].map(|count| count / operations as f64)
    }
}

/// The child's side of the control FIFOs.
pub(crate) struct PerfControl {
    commands: File,
    acknowledgements: File,
}

impl PerfControl {
    /// Opens the FIFOs named by `--perf-control`. perf holds both open
    /// already, so neither open blocks.
    pub(crate) fn open(argument: &str) -> Result<Self, String> {
        let (control, ack) = argument
            .split_once(',')
            .ok_or_else(|| format!("--perf-control takes CONTROL,ACK, not `{argument}`"))?;
        let open_error = |error| format!("opening perf's control FIFOs: {error}");
        Ok(Self {
            commands: OpenOptions::new()
                .write(true)
                .open(control)
                .map_err(open_error)?,
            acknowledgements: File::open(ack).map_err(open_error)?,
        })
    }

    pub(crate) fn start_counting(&mut self) {
        self.send(b"enable\n");
    }

    pub(crate) fn stop_counting(&mut self) {
        self.send(b"disable\n");
    }

    /// Sends a command and waits for perf to acknowledge it, so the
    /// counters have switched by the time this returns.
    fn send(&mut self, command: &[u8]) {
        self.commands
            .write_all(command)
            .expect("perf's control FIFO accepts commands");
        let mut reply = [0; 16];
        let length = self
            .acknowledgements
            .read(&mut reply)
            .expect("perf acknowledges commands");
        assert!(
            reply[..length].starts_with(b"ack"),
            "perf replied {:?}",
            &reply[..length]
        );
    }
}
