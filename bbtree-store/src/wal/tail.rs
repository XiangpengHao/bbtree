//! The WAL's tail ([`WalTail`]): where the WAL ends, as the writer knows it.
//! A scan moves it over the last file's bytes ([`WalTail::scan_bytes`]); an
//! append goes after it, in the bytes [`EncodedAppend::new`] encodes.
//!
//! Verus proves that a scan reads exactly what appends write: bytes past a
//! tail scan to records exactly when they hold them ([`WalTail::holds`]),
//! and an append's bytes hold its records; the scan and the append give
//! the same addresses for them, and the same tail after them; and a scan
//! picks up where it stopped (`WalTail::lemma_scan_in_two`, in
//! `lemmas.rs`). The specs are what to review; the proofs are in
//! `lemmas.rs`, which only Verus reads.
use std::vec::Vec;

use vstd::prelude::*;
use vstd::slice::slice_subrange;

use super::record::{Kind, RecordAddress, StoredRecord};

verus! {

/// A WAL file's header's length.
pub(super) const FILE_HEADER_LEN: usize = 12;

/// A WAL file's header: the magic, then the format's version, 1.
pub(super) const FILE_HEADER: [u8; FILE_HEADER_LEN] = [
    b'b',
    b'b',
    b't',
    b'r',
    b'e',
    b'e',
    b'W',
    b'L',
    1,
    0,
    0,
    0,
];

/// Where the WAL ends, as the writer knows it.
#[derive(Clone, Copy)]
pub(super) struct WalTail {
    /// The last WAL file's number.
    pub(super) file: u64,
    /// Its length: 0 while it doesn't exist.
    pub(super) file_len: u64,
    /// The appends it holds.
    pub(super) file_appends: u64,
    /// The LSN of the WAL's next record.
    pub(super) next_lsn: u64,
}

impl WalTail {
    /// The tail of a WAL without files.
    pub(super) fn empty() -> Self {
        WalTail { file: 0, file_len: 0, file_appends: 0, next_lsn: 0 }
    }

    /// The tail at the start of the next file.
    pub(super) fn next_file(&self) -> Self
        requires
            self.file < u64::MAX,
    {
        WalTail { file: self.file + 1, file_len: 0, file_appends: 0, next_lsn: self.next_lsn }
    }

    /// Where the next append goes: here, or at the start of the next file
    /// once this one holds `max_appends`.
    pub(super) fn append_position(&self, max_appends: u32) -> Self
        requires
            self.file < u64::MAX,
    {
        if self.file_appends == max_appends as u64 {
            self.next_file()
        } else {
            *self
        }
    }

    /// The bytes before the first record past this tail: a file header,
    /// where the file starts.
    pub(super) open spec fn header_bytes(self) -> Seq<u8> {
        if self.file_len == 0 {
            FILE_HEADER@
        } else {
            Seq::empty()
        }
    }

    /// Whether `bytes`, the last WAL file's past this tail, hold `records`:
    /// the header where the file starts, then the records' bytes, their
    /// LSNs one after another from the tail's, the last ending an append.
    pub(super) open spec fn holds<'a>(
        self,
        bytes: Seq<u8>,
        records: Seq<StoredRecord<'a>>,
    ) -> bool {
        &&& forall|i: int|
            0 <= i < records.len() ==> #[trigger] records[i].fits() && records[i].lsn
                == self.next_lsn + i
        &&& records.len() > 0 ==> records.last().last_in_append
        &&& bytes == self.header_bytes() + StoredRecord::concat_bytes(records)
    }

    /// The address of the `i`th of `records` past this tail.
    pub(super) open spec fn address_at<'a>(
        self,
        records: Seq<StoredRecord<'a>>,
        i: int,
    ) -> RecordAddress {
        RecordAddress {
            file: self.file,
            offset: (self.file_len + self.header_bytes().len() + StoredRecord::concat_bytes(
                records.take(i),
            ).len()) as u64,
            len: records[i].bytes().len() as u64,
        }
    }

    /// The addresses of `records` past this tail.
    pub(super) open spec fn addresses<'a>(self, records: Seq<StoredRecord<'a>>) -> Seq<
        RecordAddress,
    > {
        Seq::new(records.len(), |i: int| self.address_at(records, i))
    }

    /// How many of `records` end an append.
    pub(super) open spec fn append_ends<'a>(records: Seq<StoredRecord<'a>>) -> nat
        decreases records.len(),
    {
        if records.len() == 0 {
            0
        } else {
            Self::append_ends(records.drop_last()) + if records.last().last_in_append {
                1nat
            } else {
                0nat
            }
        }
    }

    /// The tail after `records`.
    pub(super) open spec fn after<'a>(self, records: Seq<StoredRecord<'a>>) -> WalTail {
        WalTail {
            file: self.file,
            file_len: (self.file_len + self.header_bytes().len() + StoredRecord::concat_bytes(
                records,
            ).len()) as u64,
            file_appends: (self.file_appends + Self::append_ends(records)) as u64,
            next_lsn: (self.next_lsn + records.len()) as u64,
        }
    }

    /// Scans `bytes`, the last WAL file's past this tail: the records they
    /// hold, with their addresses and the tail after them; else where they
    /// stop holding records. It scans records only if the bytes hold them,
    /// and all records they hold.
    pub(super) fn scan_bytes<'a>(&self, bytes: &'a [u8]) -> (scanned: Result<
        ScannedBytes<'a>,
        usize,
    >)
        requires
            self.file_len + bytes.len() <= u64::MAX,
            self.next_lsn + bytes.len() <= u64::MAX,
            self.file_appends + bytes.len() <= u64::MAX,
        ensures
            scanned matches Ok(scanned) ==> self.holds(bytes@, scanned.records@)
                && scanned.addresses@ == self.addresses(scanned.records@) && scanned.tail
                == self.after(scanned.records@),
            forall|records: Seq<StoredRecord<'a>>| #[trigger]
                self.holds(bytes@, records) ==> (scanned matches Ok(scanned) && scanned.records@
                    == records),
    {
        let mut pos = 0;
        if self.file_len == 0 {
            let mut i = 0;
            while i < FILE_HEADER_LEN
                invariant
                    self.file_len == 0,
                    i <= FILE_HEADER_LEN,
                    i <= bytes.len(),
                    bytes@.subrange(0, i as int) =~= FILE_HEADER@.subrange(0, i as int),
                decreases FILE_HEADER_LEN - i,
            {
                if i == bytes.len() || bytes[i] != FILE_HEADER[i] {
                    proof {
                        self.lemma_no_header(bytes@, i as int);
                    }
                    return Err(0);
                }
                i += 1;
            }
            pos = FILE_HEADER_LEN;
        }
        let mut records: Vec<StoredRecord<'a>> = Vec::new();
        let mut addresses: Vec<RecordAddress> = Vec::new();
        // Where the last append scanned whole ends.
        let mut last_append_end = pos;
        let mut next_lsn = self.next_lsn;
        let mut file_appends = self.file_appends;
        proof {
            self.lemma_header_alone(bytes@.subrange(0, pos as int), records@);
        }
        while pos < bytes.len()
            invariant
                self.file_len + bytes.len() <= u64::MAX,
                self.next_lsn + bytes.len() <= u64::MAX,
                self.file_appends + bytes.len() <= u64::MAX,
                pos <= bytes.len(),
                bytes@.subrange(0, pos as int) == self.header_bytes() + StoredRecord::concat_bytes(
                    records@,
                ),
                forall|i: int|
                    0 <= i < records@.len() ==> #[trigger] records@[i].fits() && records@[i].lsn
                        == self.next_lsn + i,
                next_lsn == self.next_lsn + records@.len(),
                file_appends == self.file_appends + Self::append_ends(records@),
                records@.len() <= pos,
                Self::append_ends(records@) <= records@.len(),
                last_append_end <= pos,
                last_append_end == pos <==> (records@.len() == 0 || records@.last().last_in_append),
                addresses@ == self.addresses(records@),
            decreases bytes.len() - pos,
        {
            proof {
                self.lemma_next_held(bytes@, records@, pos as int);
            }
            let rest = slice_subrange(bytes, pos, bytes.len());
            let record = match StoredRecord::decode(rest) {
                Some(record) => record,
                None => return Err(pos),
            };
            if record.lsn != next_lsn {
                return Err(pos);
            }
            proof {
                self.lemma_scanned_one_more(bytes@, records@, record, pos as int);
            }
            let len = record.len();
            addresses.push(
                RecordAddress {
                    file: self.file,
                    offset: self.file_len + pos as u64,
                    len: len as u64,
                },
            );
            pos += len;
            next_lsn += 1;
            if record.last_in_append {
                file_appends += 1;
                last_append_end = pos;
            }
            records.push(record);
        }
        proof {
            self.lemma_scanned_all(bytes@, records@);
        }
        if last_append_end != pos {
            return Err(last_append_end);
        }
        let tail = WalTail {
            file: self.file,
            file_len: self.file_len + bytes.len() as u64,
            file_appends,
            next_lsn,
        };
        Ok(ScannedBytes { records, addresses, tail })
    }
}

/// What a scan read: records, their addresses, and the tail after them.
pub(super) struct ScannedBytes<'a> {
    pub(super) records: Vec<StoredRecord<'a>>,
    pub(super) addresses: Vec<RecordAddress>,
    pub(super) tail: WalTail,
}

/// One append's bytes, the addresses of its records, and the tail after
/// it.
pub(super) struct EncodedAppend {
    pub(super) file: u64,
    pub(super) offset: u64,
    pub(super) bytes: Vec<u8>,
    pub(super) addresses: Vec<RecordAddress>,
    pub(super) tail: WalTail,
}

impl EncodedAppend {
    /// The records an append of `parts`, each a kind, a key and a value, at
    /// `position` holds.
    pub(super) open spec fn records<'a>(
        position: WalTail,
        parts: Seq<(Kind, &'a [u8], &'a [u8])>,
    ) -> Seq<StoredRecord<'a>> {
        Seq::new(
            parts.len(),
            |i: int|
                StoredRecord {
                    lsn: (position.next_lsn + i) as u64,
                    kind: parts[i].0,
                    last_in_append: i == parts.len() - 1,
                    key: parts[i].1,
                    value: parts[i].2,
                },
        )
    }

    /// Encodes `parts`, each a record's kind, key and value, as an append
    /// at `position`: bytes that hold its records there, their addresses,
    /// and the tail after them.
    pub(super) fn new<'a>(position: &WalTail, parts: &[(Kind, &'a [u8], &'a [u8])]) -> (append:
        Self)
        requires
            parts.len() > 0,
            forall|i: int|
                0 <= i < parts.len() ==> (#[trigger] parts@[i]).1@.len() <= u32::MAX
                    && parts@[i].2@.len() <= u32::MAX,
            position.next_lsn + parts.len() <= u64::MAX,
            position.file_appends < u64::MAX,
            position.file_len + position.header_bytes().len() + StoredRecord::concat_bytes(
                Self::records(*position, parts@),
            ).len() <= u64::MAX,
        ensures
            append.file == position.file,
            append.offset == position.file_len,
            position.holds(append.bytes@, Self::records(*position, parts@)),
            append.addresses@ == position.addresses(Self::records(*position, parts@)),
            append.tail == position.after(Self::records(*position, parts@)),
    {
        let ghost records = Self::records(*position, parts@);
        let mut bytes = Vec::new();
        if position.file_len == 0 {
            bytes.extend_from_slice(FILE_HEADER.as_slice());
        }
        proof {
            position.lemma_header_alone(bytes@, records.take(0));
        }
        let mut addresses: Vec<RecordAddress> = Vec::new();
        let mut i = 0;
        while i < parts.len()
            invariant
                records == Self::records(*position, parts@),
                forall|j: int|
                    0 <= j < parts.len() ==> (#[trigger] parts@[j]).1@.len() <= u32::MAX
                        && parts@[j].2@.len() <= u32::MAX,
                position.next_lsn + parts.len() <= u64::MAX,
                position.file_len + position.header_bytes().len() + StoredRecord::concat_bytes(
                    records,
                ).len() <= u64::MAX,
                i <= parts.len(),
                bytes@ == position.header_bytes() + StoredRecord::concat_bytes(
                    records.take(i as int),
                ),
                addresses@.len() == i,
                forall|j: int|
                    0 <= j < i ==> #[trigger] addresses@[j] == position.address_at(records, j),
            decreases parts.len() - i,
        {
            let (kind, key, value) = parts[i];
            let pos = bytes.len();
            let record = StoredRecord {
                lsn: position.next_lsn + i as u64,
                kind,
                last_in_append: i + 1 == parts.len(),
                key,
                value,
            };
            proof {
                position.lemma_encoded_one_more(records, i as int, record);
            }
            record.encode(&mut bytes);
            addresses.push(
                RecordAddress {
                    file: position.file,
                    offset: position.file_len + pos as u64,
                    len: (bytes.len() - pos) as u64,
                },
            );
            i += 1;
        }
        proof {
            position.lemma_encoded_all(records, addresses@);
        }
        let tail = WalTail {
            file: position.file,
            file_len: position.file_len + bytes.len() as u64,
            file_appends: position.file_appends + 1,
            next_lsn: position.next_lsn + parts.len() as u64,
        };
        EncodedAppend { file: position.file, offset: position.file_len, bytes, addresses, tail }
    }
}

} // verus!
