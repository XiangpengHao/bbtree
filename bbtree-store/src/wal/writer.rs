//! The writer ([`WalWriter`]): it opens the WAL, scanning it and claiming
//! it, then appends. This is the WAL's async shell, unproven: it reads and
//! appends through the storage, and leaves what the bytes are to the tail
//! (`tail.rs`), which Verus proves.
//!
//! - **A scan** reads each WAL file from the tail on, has the tail scan the
//!   bytes, and replays the inserts and removals they hold; then it moves
//!   to the next file, if there is one.
//! - **An append** goes where the tail says, in the bytes it encodes. An
//!   attempt whose outcome is unknown is made again, same bytes at the same
//!   offset; if the offset then mismatches, the writer reads the bytes back
//!   to learn whether an earlier attempt wrote them.
//! - **Opening** scans, then appends a claim, scanning on and claiming
//!   again for as long as another writer appended first.

use std::hash::{BuildHasher, RandomState};
use std::io;
use std::vec::Vec;

use super::record::Kind;
use super::tail::{EncodedAppend, WalTail};
use super::{AppendError, Record, RecordAddress, WalError, WalStorage};

/// Attempts at an append, while its outcome is unknown, before it fails.
const APPEND_ATTEMPTS: u32 = 3;

/// The WAL's one writer.
pub struct WalWriter<S> {
    storage: S,
    tail: WalTail,
}

impl<S: WalStorage> WalWriter<S> {
    /// Scans the WAL in `storage`, giving `replay` each record but claims,
    /// with its address, in LSN order; then claims the WAL.
    pub async fn open(
        storage: S,
        mut replay: impl FnMut(RecordAddress, Record<'_>),
    ) -> Result<Self, WalError> {
        // Random: std seeds every `RandomState` from the system's randomness.
        let writer_id = RandomState::new().hash_one(()).to_le_bytes();
        let claim = [(Kind::Claim, &writer_id[..], &[][..])];
        let mut tail = WalTail::empty();
        loop {
            tail.scan(&storage, &mut replay).await?;
            let append = EncodedAppend::new(&tail.append_position(S::MAX_APPENDS), &claim);
            match append.write(&storage).await {
                Ok(()) => {
                    return Ok(Self {
                        storage,
                        tail: append.tail,
                    });
                }
                // Another writer appended since the scan: scan on.
                Err(WalError::TailMoved) => continue,
                Err(error) => return Err(error),
            }
        }
    }

    /// Appends `records` as one append; gives their addresses. Appends
    /// nothing for no records.
    ///
    /// # Panics
    ///
    /// If a key or a value is longer than `u32::MAX` bytes.
    pub async fn append(&mut self, records: &[Record<'_>]) -> Result<Vec<RecordAddress>, WalError> {
        if records.is_empty() {
            return Ok(Vec::new());
        }
        let parts: Vec<_> = records.iter().map(Record::kind_key_value).collect();
        // `EncodedAppend::new`'s precondition: a record's lengths are `u32`s.
        let fits = |bytes: &[u8]| u32::try_from(bytes.len()).is_ok();
        assert!(
            parts
                .iter()
                .all(|&(_, key, value)| fits(key) && fits(value)),
            "at most u32::MAX bytes"
        );
        let append = EncodedAppend::new(&self.tail.append_position(S::MAX_APPENDS), &parts);
        append.write(&self.storage).await?;
        self.tail = append.tail;
        Ok(append.addresses)
    }
}

impl WalTail {
    /// Reads the WAL from the tail to its end, giving `replay` each record
    /// but claims, and moves the tail there.
    async fn scan<S: WalStorage>(
        &mut self,
        storage: &S,
        replay: &mut impl FnMut(RecordAddress, Record<'_>),
    ) -> Result<(), WalError> {
        let mut file_len = storage.len(self.file).await?;
        while let Some(len) = file_len {
            self.scan_file(storage, len, replay).await?;
            file_len = storage.len(self.file + 1).await?;
            if file_len.is_some() {
                *self = self.next_file();
            }
        }
        Ok(())
    }

    /// Reads the last file from the tail to its end, `file_len`, and moves
    /// the tail there. Reads it whole.
    async fn scan_file<S: WalStorage>(
        &mut self,
        storage: &S,
        file_len: u64,
        replay: &mut impl FnMut(RecordAddress, Record<'_>),
    ) -> Result<(), WalError> {
        let (file, start) = (self.file, self.file_len);
        let corrupted = |pos: usize| WalError::Corrupted {
            file,
            offset: start + pos as u64,
        };
        let Some(unread) = file_len.checked_sub(start) else {
            return Err(corrupted(0));
        };
        let mut bytes = vec![0; usize::try_from(unread).expect("a WAL file fits in memory")];
        storage.read(file, start, &mut bytes).await?;
        let scanned = self.scan_bytes(&bytes).map_err(corrupted)?;
        for (&address, record) in scanned.addresses.iter().zip(&scanned.records) {
            match record.kind {
                Kind::Insert => replay(
                    address,
                    Record::Insert {
                        key: record.key,
                        value: record.value,
                    },
                ),
                Kind::Remove => replay(address, Record::Remove { key: record.key }),
                Kind::Claim => {}
            }
        }
        *self = scanned.tail;
        Ok(())
    }
}

impl EncodedAppend {
    /// Writes the bytes to the storage, attempting again while the outcome
    /// is unknown. Fails with [`WalError::TailMoved`] if the offset isn't
    /// the file's length, unless an earlier attempt wrote these bytes there.
    async fn write<S: WalStorage>(&self, storage: &S) -> Result<(), WalError> {
        let mut attempt = 1;
        loop {
            match storage.append(self.file, self.offset, &self.bytes).await {
                Ok(()) => return Ok(()),
                Err(AppendError::OffsetMismatch) => {
                    if attempt > 1 && self.is_stored(storage).await? {
                        return Ok(());
                    }
                    return Err(WalError::TailMoved);
                }
                Err(AppendError::OutcomeUnknown(error)) if attempt == APPEND_ATTEMPTS => {
                    return Err(WalError::Storage(error));
                }
                Err(AppendError::OutcomeUnknown(_)) => attempt += 1,
            }
        }
    }

    /// Whether the storage holds these bytes at this offset.
    async fn is_stored<S: WalStorage>(&self, storage: &S) -> Result<bool, WalError> {
        let mut stored = vec![0; self.bytes.len()];
        match storage.read(self.file, self.offset, &mut stored).await {
            Ok(()) => Ok(stored == self.bytes),
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
            Err(error) => Err(WalError::Storage(error)),
        }
    }
}
