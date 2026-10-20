//! `--perf`: hardware counters for the timed regions only.
//!
//! The runner starts each child under `perf stat --delay=-1 --control
//! fifo:CONTROL,ACK`, which starts with the counters off, and the child's
//! stopwatch switches them on and off around each timed region through the
//! control FIFO. The events count user space only (`:u`), which unprivileged
//! perf allows and which keeps the FIFO round trips out of the counts; kernel
//! time, such as page faults, shows in ns/op but not in the counters.
//!
//! The counters follow the child's main thread only (`--no-inherit`), which
//! runs every timed region: the rayon threads of the untimed setup would
//! otherwise add whatever they still run while a region is timed. And the
//! events form one group, which the CPU counts all at once or not at all, so
//! that each count is exact rather than scaled up from a share of the time; a
//! cell whose counts aren't exact fails.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Copy)]
pub(crate) enum PerfEvent {
    Cycles,
    Instructions,
    Branches,
    BranchMisses,
    L1dLoadMisses,
    /// Cache lines brought in from beyond the last-level cache, from DRAM
    /// mostly. Each CPU vendor counts them its own way (`perf_names`), so
    /// counts compare only between runs on one kind of CPU.
    LinesFromMemory,
}

impl PerfEvent {
    pub(crate) const ALL: [Self; 6] = [
        Self::Cycles,
        Self::Instructions,
        Self::Branches,
        Self::BranchMisses,
        Self::L1dLoadMisses,
        Self::LinesFromMemory,
    ];

    /// perf's names for the event, in the order to try them: the first that
    /// perf counts on this CPU is the one used.
    fn perf_names(self) -> &'static [&'static str] {
        match self {
            Self::Cycles => &["cycles:u"],
            Self::Instructions => &["instructions:u"],
            Self::Branches => &["branches:u"],
            Self::BranchMisses => &["branch-misses:u"],
            Self::L1dLoadMisses => &["L1-dcache-load-misses:u"],
            Self::LinesFromMemory => &[
                // AMD Zen 4 and later: L1d fills from DRAM or I/O, demand
                // and prefetch.
                "ls_any_fills_from_sys.dram_io_all:u",
                // Intel: the core's requests that miss L3: loads, stores
                // and prefetches.
                "longest_lat_cache.miss:u",
            ],
        }
    }

    /// The CSV column of the count per operation.
    pub(crate) fn column(self) -> &'static str {
        match self {
            Self::Cycles => "cycles_per_op",
            Self::Instructions => "instructions_per_op",
            Self::Branches => "branches_per_op",
            Self::BranchMisses => "branch_misses_per_op",
            Self::L1dLoadMisses => "l1d_load_misses_per_op",
            Self::LinesFromMemory => "lines_from_memory_per_op",
        }
    }

    /// The event's name in reports.
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Cycles => "cycles",
            Self::Instructions => "instructions",
            Self::Branches => "branches",
            Self::BranchMisses => "branch misses",
            Self::L1dLoadMisses => "L1d misses",
            Self::LinesFromMemory => "lines from memory",
        }
    }
}

/// The runner's side: the events as perf names them on this CPU, and the
/// control FIFOs and the counts file, in a scratch directory that lives as
/// long as this does.
pub(crate) struct PerfStat {
    directory: PathBuf,
    event_names: EventNames,
}

impl PerfStat {
    /// Checks that perf counts every event exactly on this CPU, then creates
    /// the scratch directory and its FIFOs. Its path must not contain a
    /// comma, which separates the FIFOs in perf's arguments.
    pub(crate) fn prepare(directory: PathBuf) -> Result<Self, String> {
        let event_names = EventNames::for_this_cpu()?;
        fs::create_dir_all(&directory)
            .map_err(|error| format!("creating {}: {error}", directory.display()))?;
        let perf_stat = Self {
            directory,
            event_names,
        };
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
        let mut command = self.event_names.perf_stat();
        command
            .arg("--delay=-1")
            .arg("-o")
            .arg(self.counts_path())
            .arg(format!("--control=fifo:{}", self.control_argument()))
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

    /// The counts of the last run; an error if any isn't exact.
    pub(crate) fn read_counts(&self) -> Result<PerfCounts, String> {
        let path = self.counts_path();
        let text = fs::read_to_string(&path)
            .map_err(|error| format!("reading {}: {error}", path.display()))?;
        PerfCounts::parse(&text, &self.event_names)
    }
}

impl Drop for PerfStat {
    fn drop(&mut self) {
        // Best effort: a leftover scratch directory is harmless.
        let _ = fs::remove_dir_all(&self.directory);
    }
}

/// perf's name for each of `PerfEvent::ALL` on this CPU.
struct EventNames {
    names: [&'static str; PerfEvent::ALL.len()],
}

impl EventNames {
    /// The first name of each event that perf counts here, checked to count
    /// as one group.
    fn for_this_cpu() -> Result<Self, String> {
        let mut names = [""; PerfEvent::ALL.len()];
        for event in PerfEvent::ALL {
            let mut failures = Vec::new();
            names[event as usize] = (event.perf_names().iter().copied())
                .find(
                    |&candidate| match Self::only(event, candidate).count_trial() {
                        Ok(()) => true,
                        Err(reason) => {
                            failures.push(reason);
                            false
                        }
                    },
                )
                .ok_or_else(|| {
                    format!(
                        "perf can't count {} here: {}",
                        event.title(),
                        failures.join("; ")
                    )
                })?;
        }
        let event_names = Self { names };
        event_names.count_trial().map_err(|reason| {
            format!(
                "{reason}. The group needs a hardware counter per event at once: \
                 another perf session may hold some, or, on AMD, the NMI watchdog \
                 (`sysctl kernel.nmi_watchdog=0` frees it)"
            )
        })?;
        Ok(event_names)
    }

    /// `event` alone, as `name`.
    fn only(event: PerfEvent, name: &'static str) -> Self {
        let mut names = [""; PerfEvent::ALL.len()];
        names[event as usize] = name;
        Self { names }
    }

    /// Counts the events over `true`; an error unless every count is exact.
    fn count_trial(&self) -> Result<(), String> {
        let output = (self.perf_stat().args(["--", "true"]).output())
            .map_err(|error| format!("starting perf: {error}"))?;
        // `perf stat -x` reports on stderr.
        let report = String::from_utf8_lossy(&output.stderr);
        if !output.status.success() {
            return Err(format!("perf stat failed: {}", report.trim()));
        }
        PerfCounts::parse(&report, self).map(drop)
    }

    /// `perf stat` counting the events as one group, on the main thread
    /// only, in CSV.
    fn perf_stat(&self) -> Command {
        let group: Vec<&str> = self.present().map(|(_, name)| name).collect();
        let mut command = Command::new("perf");
        command
            .args(["stat", "-x", ",", "--no-inherit", "-e"])
            .arg(format!("{{{}}}", group.join(",")));
        command
    }

    /// The events that have a name here, by their index in
    /// `PerfEvent::ALL`, with their names.
    fn present(&self) -> impl Iterator<Item = (usize, &'static str)> {
        (self.names.iter().copied().enumerate()).filter(|(_, name)| !name.is_empty())
    }
}

/// The counts of one run, by event.
pub(crate) struct PerfCounts {
    counts: [f64; PerfEvent::ALL.len()],
}

impl PerfCounts {
    /// Parses `perf stat -x ,` output: `value,unit,event,run time,percent
    /// running,…` lines, after `#` comments. Every event of `event_names`
    /// must be there, counted all the time.
    fn parse(text: &str, event_names: &EventNames) -> Result<Self, String> {
        let mut counts = [0.0; PerfEvent::ALL.len()];
        for (index, name) in event_names.present() {
            let fields = (text.lines())
                .map(|line| line.split(',').collect::<Vec<_>>())
                .find(|fields| fields.get(2) == Some(&name))
                .ok_or_else(|| format!("perf reported no count of {name}"))?;
            let (Some(value), Some(running)) = (fields.first(), fields.get(4)) else {
                return Err(format!("perf reported `{}` for {name}", fields.join(",")));
            };
            let value: f64 =
                (value.parse()).map_err(|_| format!("perf couldn't count {name}: {value}"))?;
            let all_the_time = (running.parse::<f64>()).is_ok_and(|percent| percent >= 100.0);
            if !all_the_time {
                return Err(format!(
                    "perf counted {name} for {running}% of the time only"
                ));
            }
            counts[index] = value;
        }
        Ok(Self { counts })
    }

    pub(crate) fn per_operation(&self, event: PerfEvent, operations: u64) -> f64 {
        self.counts[event as usize] / operations as f64
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

#[cfg(test)]
mod tests {
    use super::*;

    fn names(names: [&'static str; PerfEvent::ALL.len()]) -> EventNames {
        EventNames { names }
    }

    #[test]
    fn parses_exact_counts_by_event() {
        let event_names = names([
            "cycles:u",
            "instructions:u",
            "branches:u",
            "branch-misses:u",
            "L1-dcache-load-misses:u",
            "longest_lat_cache.miss:u",
        ]);
        let report = "# started on Mon Oct 12 10:54:28 2026\n\n\
            87055130223,,cycles:u,12092926674,100.00,,\n\
            273361525211,,instructions:u,12094247971,100.00,,\n\
            32039811985,,branches:u,12095836269,100.00,,\n\
            49610007,,branch-misses:u,12095747969,100.00,,\n\
            40365269,,L1-dcache-load-misses:u,12093700987,100.00,,\n\
            2875015,,longest_lat_cache.miss:u,12095501715,100.00,,\n";
        let counts = PerfCounts::parse(report, &event_names).expect("exact counts");
        assert_eq!(
            counts.per_operation(PerfEvent::Instructions, 1),
            273_361_525_211.0
        );
        assert_eq!(
            counts.per_operation(PerfEvent::LinesFromMemory, 5),
            575_003.0
        );
    }

    #[test]
    fn rejects_counts_scaled_from_part_of_the_time() {
        let event_names = names(["cycles:u", "", "", "", "", ""]);
        let report = "80122262908,,cycles:u,12092926674,83.00,,\n";
        let error = PerfCounts::parse(report, &event_names).err();
        assert_eq!(
            error.as_deref(),
            Some("perf counted cycles:u for 83.00% of the time only")
        );
    }

    #[test]
    fn rejects_a_group_that_was_not_counted() {
        let event_names = names(["cycles:u", "instructions:u", "", "", "", ""]);
        let report = "<not counted>,,cycles:u,0,0.00,,\n<not counted>,,instructions:u,0,0.00,,\n";
        let error = PerfCounts::parse(report, &event_names).err();
        assert_eq!(
            error.as_deref(),
            Some("perf couldn't count cycles:u: <not counted>")
        );
    }
}
