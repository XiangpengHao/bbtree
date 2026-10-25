//! The WAL (`design/bbtree-store/01-wal.md`): records, each with its LSN
//! and a CRC, appended to numbered WAL files in a [`WalStorage`].
//!
//! - **The writer** ([`WalWriter`]) opens the WAL: it scans every record,
//!   giving each to the caller, then claims the WAL with a record of its
//!   own, which fences the writer before it. Then it appends, one append
//!   at a time, each written whole or not at all.
//! - **Lookups** read a record back by its address
//!   ([`RecordAddress::read_value`]), while the writer appends.

use core::fmt;
use std::error::Error;
use std::io;

mod record;
mod storage;
mod writer;

pub use record::{Record, RecordAddress};
pub use storage::{AppendError, WalStorage};
pub use writer::WalWriter;

/// Why a WAL operation failed.
#[derive(Debug)]
pub enum WalError {
    /// The storage failed: an append's records may or may not have been
    /// written.
    Storage(io::Error),
    /// The WAL's tail moved under the writer: another writer claimed the
    /// WAL, or an append that failed was written after all. Open the WAL
    /// again.
    TailMoved,
    /// The bytes at `offset` of WAL file `file` fail their checks.
    Corrupted { file: u64, offset: u64 },
}

impl fmt::Display for WalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(error) => write!(f, "the WAL's storage failed: {error}"),
            Self::TailMoved => f.write_str("the WAL's tail moved under its writer"),
            Self::Corrupted { file, offset } => {
                write!(f, "WAL file {file} is corrupted at offset {offset}")
            }
        }
    }
}

impl Error for WalError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Storage(error) => Some(error),
            Self::TailMoved | Self::Corrupted { .. } => None,
        }
    }
}

impl From<io::Error> for WalError {
    fn from(error: io::Error) -> Self {
        Self::Storage(error)
    }
}
