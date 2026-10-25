//! The storage under the WAL ([`WalStorage`]): numbered files that take
//! appends at their end, under S3 Express's rules on every backend.

use core::future::Future;
use std::io;

/// WAL files, numbered from 0, that take appends at their end.
pub trait WalStorage: Send + Sync {
    /// Appends a file takes at most (S3 Express: 10,000).
    const MAX_APPENDS: u32;

    /// Appends `bytes` to file `file` if its length is `offset`; offset 0
    /// creates the file, which must not exist. Whole or nothing, and
    /// durable once it returns `Ok`.
    fn append(
        &self,
        file: u64,
        offset: u64,
        bytes: &[u8],
    ) -> impl Future<Output = Result<(), AppendError>> + Send;

    /// File `file`'s length; `None` if it doesn't exist.
    fn len(&self, file: u64) -> impl Future<Output = io::Result<Option<u64>>> + Send;

    /// Reads `buf.len()` bytes of file `file` from `offset`; fails with
    /// [`io::ErrorKind::UnexpectedEof`] if the file holds fewer.
    fn read(
        &self,
        file: u64,
        offset: u64,
        buf: &mut [u8],
    ) -> impl Future<Output = io::Result<()>> + Send;
}

/// Why [`WalStorage::append`] failed.
#[derive(Debug)]
pub enum AppendError {
    /// The offset isn't the file's length, or, for offset 0, the file
    /// exists. Nothing was written.
    OffsetMismatch,
    /// The bytes may or may not have been written.
    OutcomeUnknown(io::Error),
}
