//! The writer ([`WalWriter`]): it opens the WAL, scanning it and claiming
//! it, then appends.
//!
//! - **The tail** ([`WalTail`]) is where the WAL ends, as the writer knows
//!   it: the last WAL file's number, length and appends, and the next LSN.
//!   A scan moves it over the files that follow; an append moves it past
//!   the append.
//! - **An append** goes at the end of the last file, or, if that holds
//!   [`WalStorage::MAX_APPENDS`], at the start of the next, after a file
//!   header. An attempt whose outcome is unknown is made again, same bytes
//!   at the same offset; if the offset then mismatches, the writer reads
//!   the bytes back to learn whether an earlier attempt wrote them.
//! - **Opening** scans, then appends a claim, scanning on and claiming
//!   again for as long as another writer appended first.

use core::iter;
use std::hash::{BuildHasher, RandomState};
use std::io;
use std::vec::Vec;

use super::record::{Kind, StoredRecord};
use super::{AppendError, Record, RecordAddress, WalError, WalStorage};

/// A WAL file's header: the magic, then the format's version, 1.
const FILE_HEADER: [u8; 12] = *b"bbtreeWL\x01\0\0\0";

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
        let claim = || iter::once((Kind::Claim, &writer_id[..], &[][..]));
        let mut tail = WalTail::default();
        loop {
            tail.scan(&storage, &mut replay).await?;
            match tail.append(&storage, claim()).await {
                Ok(_) => return Ok(Self { storage, tail }),
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
        let records = records.iter().map(Record::kind_key_value);
        self.tail.append(&self.storage, records).await
    }
}

/// Where the WAL ends, as the writer knows it.
#[derive(Default)]
struct WalTail {
    /// The last WAL file's number.
    file: u64,
    /// Its length: 0 while it doesn't exist.
    file_len: u64,
    /// The appends it holds.
    file_appends: u32,
    /// The LSN of the WAL's next record.
    next_lsn: u64,
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
                self.file += 1;
                self.file_len = 0;
                self.file_appends = 0;
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
        let mut pos = 0;
        if start == 0 {
            if !bytes.starts_with(&FILE_HEADER) {
                return Err(corrupted(0));
            }
            pos = FILE_HEADER.len();
        }
        // Where the last append read whole ends.
        let mut last_append_end = pos;
        let (mut next_lsn, mut file_appends) = (self.next_lsn, self.file_appends);
        while pos < bytes.len() {
            let record = StoredRecord::decode(&bytes[pos..])
                .filter(|record| record.lsn == next_lsn)
                .ok_or_else(|| corrupted(pos))?;
            let address = RecordAddress {
                file,
                offset: start + pos as u64,
                len: record.len() as u64,
            };
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
            next_lsn += 1;
            pos += record.len();
            if record.last_in_append {
                file_appends += 1;
                last_append_end = pos;
            }
        }
        // An append is written whole: a file ends with an append's last record.
        if last_append_end != bytes.len() {
            return Err(corrupted(last_append_end));
        }
        *self = Self {
            file,
            file_len,
            file_appends,
            next_lsn,
        };
        Ok(())
    }

    /// Appends `records`, each a kind, a key and a value, after the tail as
    /// one append; moves the tail past it, and gives the records' addresses.
    /// Leaves the tail where it was on any error.
    async fn append<'a, S: WalStorage>(
        &mut self,
        storage: &S,
        records: impl ExactSizeIterator<Item = (Kind, &'a [u8], &'a [u8])>,
    ) -> Result<Vec<RecordAddress>, WalError> {
        let starts_file = self.file_appends == S::MAX_APPENDS;
        let (file, offset, file_appends) = if starts_file {
            (self.file + 1, 0, 0)
        } else {
            (self.file, self.file_len, self.file_appends)
        };
        let append = EncodedAppend::new(file, offset, self.next_lsn, records);
        append.write(storage).await?;
        *self = Self {
            file,
            file_len: offset + append.bytes.len() as u64,
            file_appends: file_appends + 1,
            next_lsn: self.next_lsn + append.addresses.len() as u64,
        };
        Ok(append.addresses)
    }
}

/// One append's bytes, and the addresses of its records.
struct EncodedAppend {
    file: u64,
    offset: u64,
    bytes: Vec<u8>,
    addresses: Vec<RecordAddress>,
}

impl EncodedAppend {
    /// Encodes `records` as an append at `offset` of WAL file `file`, the
    /// first with LSN `first_lsn`; at offset 0, after a file header.
    fn new<'a>(
        file: u64,
        offset: u64,
        first_lsn: u64,
        records: impl ExactSizeIterator<Item = (Kind, &'a [u8], &'a [u8])>,
    ) -> Self {
        let mut bytes = Vec::new();
        if offset == 0 {
            bytes.extend_from_slice(&FILE_HEADER);
        }
        let count = records.len();
        let mut addresses = Vec::with_capacity(count);
        for (i, (kind, key, value)) in records.enumerate() {
            let pos = bytes.len();
            let last_in_append = i + 1 == count;
            let lsn = first_lsn + i as u64;
            // `encode`'s precondition: a record's lengths are `u32`s.
            let fits = |bytes: &[u8]| u32::try_from(bytes.len()).is_ok();
            assert!(fits(key) && fits(value), "at most u32::MAX bytes");
            StoredRecord {
                lsn,
                kind,
                last_in_append,
                key,
                value,
            }
            .encode(&mut bytes);
            let len = (bytes.len() - pos) as u64;
            addresses.push(RecordAddress {
                file,
                offset: offset + pos as u64,
                len,
            });
        }
        Self {
            file,
            offset,
            bytes,
            addresses,
        }
    }

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
