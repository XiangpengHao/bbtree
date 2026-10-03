//! The command line: suite runs with filters; `verify-data` and `fetch`,
//! which manage the datasets; and `measure`, the internal command that the
//! suite runner starts each child process with.

use std::path::PathBuf;

use crate::datasets::DataDirectory;
use crate::keys::KeyKind;
use crate::map_adapters::MapKind;
use crate::operations::Operation;
use crate::suites::Suite;
use crate::values::ValueKind;
use crate::workload::{Cell, MapSize, Scale, Workload};

/// An enum whose variants have names on the command line, which reports
/// use too.
pub(crate) trait Named: Copy + PartialEq + 'static {
    const ALL: &'static [Self];

    fn name(self) -> &'static str;

    fn from_name(name: &str) -> Result<Self, String> {
        (Self::ALL.iter().copied())
            .find(|variant| variant.name() == name)
            .ok_or_else(|| {
                format!(
                    "unknown name `{name}`; expected one of: {}",
                    Self::all_names()
                )
            })
    }

    /// Parses a comma-separated list of names.
    fn from_name_list(names: &str) -> Result<Vec<Self>, String> {
        names.split(',').map(Self::from_name).collect()
    }

    fn all_names() -> String {
        let names: Vec<&str> = Self::ALL.iter().map(|variant| variant.name()).collect();
        names.join(",")
    }
}

pub(crate) enum Invocation {
    Help,
    /// `verify-data`: hashes the datasets against the manifest.
    VerifyData,
    /// `fetch`: installs the datasets and verifies them.
    FetchData,
    RunSuites(SuiteRequest),
    MeasureCell(CellRequest),
}

impl Invocation {
    pub(crate) fn parse(arguments: &[String]) -> Result<Self, String> {
        match arguments.first().map(String::as_str) {
            None | Some("-h" | "--help" | "help") => Ok(Self::Help),
            Some("verify-data") => Self::without_arguments(arguments, Self::VerifyData),
            Some("fetch") => Self::without_arguments(arguments, Self::FetchData),
            Some(CellRequest::COMMAND) => {
                CellRequest::parse(&arguments[1..]).map(Self::MeasureCell)
            }
            Some(_) => SuiteRequest::parse(arguments).map(Self::RunSuites),
        }
    }

    /// `invocation`, for a command that takes no arguments.
    fn without_arguments(arguments: &[String], invocation: Self) -> Result<Self, String> {
        match arguments {
            [_command] => Ok(invocation),
            [command, ..] => Err(format!("{command} takes no arguments")),
            [] => unreachable!("the command is the first argument"),
        }
    }

    pub(crate) fn usage() -> String {
        format!(
            "\
bbtree's benchmark suite. Every cell (map × key × value × size × operation)
runs in a fresh process, once per repetition.

usage: bbtree-bench <suite>... [filters] [options]
       bbtree-bench-memory <suite>... [filters] [options]
       bbtree-bench verify-data
       bbtree-bench fetch

The memory binary reports the memory held by the maps that build operations
make, instead of time.

suites: {suites}, or all

Real-world key kinds read their datasets from ${data_variable}, or else
$XDG_CACHE_HOME/bbtree-bench, or else ~/.cache/bbtree-bench. Their cells
check only that each file has its pinned size, and are reported as missing
data when one doesn't.
  verify-data  hashes every dataset file against the manifest
  fetch        downloads the datasets from their GitHub release with gh and
               zstd, derives Common Crawl's URLs from the crawl's index with
               python3, then verifies them

filters, comma-separated (default: everything in the suites):
  --map    {maps}
  --key    {keys}
  --value  {values}
  --size   {sizes}
  --op     {operations}

options:
  --reps N   repetitions per cell (default 1; more for cells shown to be
             noisy)
  --quick    10× smaller sizes and operation counts, for CI and development
  --perf     count cycles, instructions, branch misses, L1d misses and DRAM
             fills of the timed regions with `perf stat`
  --list     print the selected cells and exit
  --out DIR  where to write the CSV and the summary (default: bench/results)",
            suites = Suite::all_names(),
            data_variable = DataDirectory::VARIABLE,
            maps = MapKind::all_names(),
            keys = KeyKind::all_names(),
            values = ValueKind::all_names(),
            sizes = MapSize::all_names(),
            operations = Operation::all_names(),
        )
    }
}

/// Walks the arguments, handing out flag values.
struct ArgumentCursor<'a> {
    arguments: std::slice::Iter<'a, String>,
}

impl<'a> ArgumentCursor<'a> {
    fn new(arguments: &'a [String]) -> Self {
        Self {
            arguments: arguments.iter(),
        }
    }

    fn next_argument(&mut self) -> Option<&'a str> {
        self.arguments.next().map(String::as_str)
    }

    fn value_of(&mut self, flag: &str) -> Result<&'a str, String> {
        self.next_argument()
            .ok_or_else(|| format!("{flag} needs a value"))
    }
}

/// Which workloads and maps to run. An empty list admits everything.
#[derive(Default)]
pub(crate) struct CellFilter {
    maps: Vec<MapKind>,
    keys: Vec<KeyKind>,
    values: Vec<ValueKind>,
    sizes: Vec<MapSize>,
    operations: Vec<Operation>,
}

impl CellFilter {
    /// An empty list admits every map but the opt-in ones.
    pub(crate) fn admits_map(&self, map: MapKind) -> bool {
        if self.maps.is_empty() {
            !map.is_opt_in()
        } else {
            self.maps.contains(&map)
        }
    }

    pub(crate) fn admits(&self, workload: Workload) -> bool {
        Self::list_admits(&self.keys, workload.key())
            && Self::list_admits(&self.values, workload.value())
            && Self::list_admits(&self.sizes, workload.size())
            && Self::list_admits(&self.operations, workload.operation())
    }

    fn list_admits<T: Named>(list: &[T], variant: T) -> bool {
        list.is_empty() || list.contains(&variant)
    }

    /// Adds the names after a filter flag.
    fn parse_flag(&mut self, flag: &str, cursor: &mut ArgumentCursor) -> Result<(), String> {
        let names = cursor.value_of(flag)?;
        match flag {
            "--map" => self.maps.extend(MapKind::from_name_list(names)?),
            "--key" => self.keys.extend(KeyKind::from_name_list(names)?),
            "--value" => self.values.extend(ValueKind::from_name_list(names)?),
            "--size" => self.sizes.extend(MapSize::from_name_list(names)?),
            "--op" => self.operations.extend(Operation::from_name_list(names)?),
            _ => return Err(format!("unknown option {flag}")),
        }
        Ok(())
    }

    /// The one cell a filter names, when each of its lists has one entry.
    fn only_cell(&self) -> Result<Cell, String> {
        let (key, value, size, operation) = (
            Self::only(&self.keys, "--key")?,
            Self::only(&self.values, "--value")?,
            Self::only(&self.sizes, "--size")?,
            Self::only(&self.operations, "--op")?,
        );
        if !key.sizes().contains(&size) {
            return Err(format!(
                "{} keys don't come in size {}",
                key.name(),
                size.name()
            ));
        }
        if !operation.applies_to(size) {
            return Err(format!(
                "{} does not apply to size {}",
                operation.name(),
                size.name()
            ));
        }
        let workload = Workload::new(key, value, size, operation);
        Ok(Cell::new(Self::only(&self.maps, "--map")?, workload))
    }

    fn only<T: Named>(list: &[T], flag: &str) -> Result<T, String> {
        match list {
            [variant] => Ok(*variant),
            _ => Err(format!("{flag} takes exactly one name here")),
        }
    }
}

/// `bbtree-bench <suite>... [filters] [options]`.
pub(crate) struct SuiteRequest {
    suites: Vec<Suite>,
    filter: CellFilter,
    repetitions: usize,
    scale: Scale,
    perf: bool,
    list_only: bool,
    output_directory: PathBuf,
    command_line: String,
}

impl SuiteRequest {
    fn parse(arguments: &[String]) -> Result<Self, String> {
        let mut request = Self {
            suites: Vec::new(),
            filter: CellFilter::default(),
            repetitions: 0,
            scale: Scale::Full,
            perf: false,
            list_only: false,
            output_directory: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("results"),
            command_line: arguments.join(" "),
        };
        let mut repetitions = None;
        let mut cursor = ArgumentCursor::new(arguments);
        while let Some(argument) = cursor.next_argument() {
            match argument {
                "--quick" => request.scale = Scale::Quick,
                "--perf" => request.perf = true,
                "--list" => request.list_only = true,
                "--reps" => {
                    let count = cursor.value_of(argument)?;
                    let count = count
                        .parse()
                        .map_err(|_| format!("--reps takes a count, not `{count}`"))?;
                    repetitions = Some(count);
                }
                "--out" => request.output_directory = PathBuf::from(cursor.value_of(argument)?),
                "all" => request.suites.extend(Suite::ALL),
                flag if flag.starts_with("--") => request.filter.parse_flag(flag, &mut cursor)?,
                suite => request.suites.push(Suite::from_name(suite)?),
            }
        }
        if request.suites.is_empty() {
            return Err("name at least one suite, or `all`".to_owned());
        }
        request.repetitions = match repetitions {
            Some(0) => return Err("--reps must be at least 1".to_owned()),
            Some(count) => count,
            None => 1,
        };
        Ok(request)
    }

    pub(crate) fn suites(&self) -> &[Suite] {
        &self.suites
    }

    pub(crate) fn filter(&self) -> &CellFilter {
        &self.filter
    }

    pub(crate) fn repetitions(&self) -> usize {
        self.repetitions
    }

    pub(crate) fn scale(&self) -> Scale {
        self.scale
    }

    pub(crate) fn perf(&self) -> bool {
        self.perf
    }

    pub(crate) fn list_only(&self) -> bool {
        self.list_only
    }

    pub(crate) fn output_directory(&self) -> &PathBuf {
        &self.output_directory
    }

    pub(crate) fn command_line(&self) -> &str {
        &self.command_line
    }
}

/// `bbtree-bench measure <cell> [--quick] [--perf-control CONTROL,ACK]`:
/// measures one cell in this process, for the suite runner.
pub(crate) struct CellRequest {
    cell: Cell,
    scale: Scale,
    perf_control: Option<String>,
}

impl CellRequest {
    const COMMAND: &str = "measure";

    /// The arguments that make a child process measure `cell`.
    pub(crate) fn arguments(cell: Cell, scale: Scale, perf_control: Option<String>) -> Vec<String> {
        let workload = cell.workload();
        let mut arguments: Vec<String> = [
            Self::COMMAND,
            "--map",
            cell.map().name(),
            "--key",
            workload.key().name(),
            "--value",
            workload.value().name(),
            "--size",
            workload.size().name(),
            "--op",
            workload.operation().name(),
        ]
        .map(str::to_owned)
        .into();
        if scale == Scale::Quick {
            arguments.push("--quick".to_owned());
        }
        if let Some(perf_control) = perf_control {
            arguments.extend(["--perf-control".to_owned(), perf_control]);
        }
        arguments
    }

    fn parse(arguments: &[String]) -> Result<Self, String> {
        let mut filter = CellFilter::default();
        let mut scale = Scale::Full;
        let mut perf_control = None;
        let mut cursor = ArgumentCursor::new(arguments);
        while let Some(argument) = cursor.next_argument() {
            match argument {
                "--quick" => scale = Scale::Quick,
                "--perf-control" => perf_control = Some(cursor.value_of(argument)?.to_owned()),
                flag => filter.parse_flag(flag, &mut cursor)?,
            }
        }
        Ok(Self {
            cell: filter.only_cell()?,
            scale,
            perf_control,
        })
    }

    pub(crate) fn cell(&self) -> Cell {
        self.cell
    }

    pub(crate) fn scale(&self) -> Scale {
        self.scale
    }

    pub(crate) fn perf_control(&self) -> Option<&str> {
        self.perf_control.as_deref()
    }
}
