//! WAL files in memory ([`MemoryStorage`]), for the tests: they keep S3
//! Express's rules, and fail appends on demand.

use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::vec::Vec;

use bbtree_store::bbtree::unsync::BBTreeMap;
use bbtree_store::wal::{AppendError, WalStorage};

/// WAL files in memory, which take `MAX_APPENDS` appends each. Its clones
/// share its files, as handles to one storage.
#[derive(Clone, Default)]
pub struct MemoryStorage<const MAX_APPENDS: u32 = 10_000> {
    files: Arc<Mutex<MemoryFiles>>,
}

/// How an append fails, set by [`MemoryStorage::fail_append`]. Either way,
/// it reports [`AppendError::OutcomeUnknown`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppendFault {
    /// The request is lost: the append doesn't happen.
    RequestLost,
    /// The response is lost: the append happens, written or not.
    ResponseLost,
}

#[derive(Default)]
struct MemoryFiles {
    files: BBTreeMap<u64, MemoryFile>,
    /// The faults of the next appends, in order.
    faults: VecDeque<AppendFault>,
}

#[derive(Default)]
struct MemoryFile {
    bytes: Vec<u8>,
    appends: u32,
}

impl<const MAX_APPENDS: u32> MemoryStorage<MAX_APPENDS> {
    /// Fails the next append that has no fault set yet with `fault`.
    pub fn fail_append(&self, fault: AppendFault) {
        self.lock().faults.push_back(fault);
    }

    fn lock(&self) -> MutexGuard<'_, MemoryFiles> {
        self.files.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl MemoryFiles {
    fn append(
        &mut self,
        file: u64,
        offset: u64,
        bytes: &[u8],
        max_appends: u32,
    ) -> Result<(), AppendError> {
        let mismatch = match self.files.get(&file) {
            None => offset != 0,
            Some(stored) => offset == 0 || stored.bytes.len() as u64 != offset,
        };
        if mismatch {
            return Err(AppendError::OffsetMismatch);
        }
        let stored = self.files.entry(file).or_default();
        if stored.appends == max_appends {
            return Err(AppendError::OutcomeUnknown(io::Error::other(
                "the file takes no more appends",
            )));
        }
        stored.bytes.extend_from_slice(bytes);
        stored.appends += 1;
        Ok(())
    }
}

impl<const MAX_APPENDS: u32> WalStorage for MemoryStorage<MAX_APPENDS> {
    const MAX_APPENDS: u32 = MAX_APPENDS;

    async fn append(&self, file: u64, offset: u64, bytes: &[u8]) -> Result<(), AppendError> {
        let mut files = self.lock();
        let fault = files.faults.pop_front();
        let lost = || AppendError::OutcomeUnknown(io::Error::new(io::ErrorKind::TimedOut, "lost"));
        match fault {
            Some(AppendFault::RequestLost) => Err(lost()),
            Some(AppendFault::ResponseLost) => {
                let _ = files.append(file, offset, bytes, MAX_APPENDS);
                Err(lost())
            }
            None => files.append(file, offset, bytes, MAX_APPENDS),
        }
    }

    async fn len(&self, file: u64) -> io::Result<Option<u64>> {
        Ok(self
            .lock()
            .files
            .get(&file)
            .map(|stored| stored.bytes.len() as u64))
    }

    async fn read(&self, file: u64, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let files = self.lock();
        let stored = files.files.get(&file).ok_or(io::ErrorKind::NotFound)?;
        let start = usize::try_from(offset).map_err(|_| io::ErrorKind::UnexpectedEof)?;
        let bytes = start
            .checked_add(buf.len())
            .and_then(|end| stored.bytes.get(start..end))
            .ok_or(io::ErrorKind::UnexpectedEof)?;
        buf.copy_from_slice(bytes);
        Ok(())
    }
}
