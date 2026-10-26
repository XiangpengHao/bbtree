//! The WAL's tail ([`WalTail`]): where the WAL ends, as the writer knows it.
//! A scan moves it over the last file's bytes ([`WalTail::scan_bytes`]); an
//! append goes after it, in the bytes [`EncodedAppend::new`] encodes.
//!
//! Verus proves that a scan reads exactly what appends write: bytes past a
//! tail scan to records exactly when they hold them ([`WalTail::holds`]),
//! and an append's bytes hold its records; the scan and the append give
//! the same addresses for them, and the same tail after them. The specs
//! are what to review; the lemmas, at the end and in `record.rs`, Verus
//! checks.
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
                        assert forall|records: Seq<StoredRecord<'a>>| #[trigger]
                            self.holds(bytes@, records) implies false by {
                            let held = FILE_HEADER@ + StoredRecord::concat_bytes(records);
                            assert(bytes@[i as int] == held[i as int]);
                        }
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
            assert(bytes@.subrange(0, pos as int) =~= self.header_bytes()
                + StoredRecord::concat_bytes(records@));
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
            let rest = slice_subrange(bytes, pos, bytes.len());
            let record = match StoredRecord::decode(rest) {
                Some(record) => record,
                None => {
                    proof {
                        assert forall|held: Seq<StoredRecord<'a>>| #[trigger]
                            self.holds(bytes@, held) implies false by {
                            self.lemma_held_records_go_on(bytes@, records@, held, pos as int);
                        }
                    }
                    return Err(pos);
                },
            };
            if record.lsn != next_lsn {
                proof {
                    assert forall|held: Seq<StoredRecord<'a>>| #[trigger]
                        self.holds(bytes@, held) implies false by {
                        self.lemma_held_records_go_on(bytes@, records@, held, pos as int);
                    }
                }
                return Err(pos);
            }
            proof {
                record.lemma_len();
            }
            let len = record.len();
            let ghost scanned = records@.push(record);
            proof {
                assert(record.bytes() =~= bytes@.subrange(pos as int, pos + len));
                StoredRecord::lemma_concat_push(records@, record);
                assert(bytes@.subrange(0, pos + len) =~= bytes@.subrange(0, pos as int)
                    + record.bytes());
                assert(scanned.drop_last() =~= records@);
                assert(scanned.take(records@.len() as int) =~= records@);
                assert forall|j: int| 0 <= j < records@.len() implies #[trigger] self.address_at(
                    scanned,
                    j,
                ) == self.address_at(records@, j) by {
                    assert(scanned.take(j) =~= records@.take(j));
                }
            }
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
            proof {
                assert(addresses@ =~= self.addresses(records@));
            }
        }
        if last_append_end != pos {
            proof {
                assert(bytes@.subrange(0, pos as int) =~= bytes@);
                assert forall|held: Seq<StoredRecord<'a>>| #[trigger]
                    self.holds(bytes@, held) implies false by {
                    self.lemma_held_records_are(bytes@, records@, held);
                }
            }
            return Err(last_append_end);
        }
        let tail = WalTail {
            file: self.file,
            file_len: self.file_len + bytes.len() as u64,
            file_appends,
            next_lsn,
        };
        let scanned = ScannedBytes { records, addresses, tail };
        proof {
            assert(bytes@.subrange(0, pos as int) =~= bytes@);
            assert forall|held: Seq<StoredRecord<'a>>| #[trigger]
                self.holds(bytes@, held) implies scanned.records@ == held by {
                self.lemma_held_records_are(bytes@, records@, held);
            }
        }
        Ok(scanned)
    }

    /// Records the scan read, at the start of bytes that hold records, are
    /// the first of those, and more follow while bytes do.
    proof fn lemma_held_records_go_on<'a>(
        self,
        bytes: Seq<u8>,
        scanned: Seq<StoredRecord<'a>>,
        held: Seq<StoredRecord<'a>>,
        pos: int,
    )
        requires
            self.holds(bytes, held),
            forall|i: int| 0 <= i < scanned.len() ==> #[trigger] scanned[i].fits(),
            0 <= pos < bytes.len(),
            bytes.subrange(0, pos) == self.header_bytes() + StoredRecord::concat_bytes(scanned),
        ensures
            scanned.len() < held.len(),
            scanned == held.take(scanned.len() as int),
            held[scanned.len() as int].bytes().is_prefix_of(
                bytes.subrange(pos, bytes.len() as int),
            ),
    {
        let (header, header_len) = (self.header_bytes(), self.header_bytes().len() as int);
        let (read, all) = (StoredRecord::concat_bytes(scanned), StoredRecord::concat_bytes(held));
        // Past the header, the bytes scanned start the records' bytes.
        assert(bytes.subrange(0, pos).len() == pos);
        assert(read.len() + header_len == pos);
        assert forall|i: int| 0 <= i < read.len() implies read[i] == all[i] by {
            assert(read[i] == (header + read)[header_len + i]);
            assert(bytes[header_len + i] == (header + all)[header_len + i]);
        }
        assert(read =~= all.subrange(0, read.len() as int));
        StoredRecord::lemma_concat_unique(scanned, held);
        let k = scanned.len() as int;
        if k == held.len() {
            assert(held.take(k) =~= held);
        }
        // The next record's bytes are those from `pos` on.

        StoredRecord::lemma_concat_take(held, k + 1);
        assert(held.take(k + 1) =~= held.take(k).push(held[k]));
        StoredRecord::lemma_concat_push(held.take(k), held[k]);
        let next = held[k].bytes();
        let through_next = StoredRecord::concat_bytes(held.take(k + 1));
        assert forall|i: int| 0 <= i < next.len() implies next[i] == bytes[pos + i] by {
            assert(next[i] == through_next[read.len() + i]);
            assert(bytes[pos + i] == (header + all)[header_len + read.len() + i]);
        }
        assert(next =~= bytes.subrange(pos, bytes.len() as int).subrange(0, next.len() as int));
    }

    /// Records the scan read, where they fill bytes that hold records, are
    /// those.
    proof fn lemma_held_records_are<'a>(
        self,
        bytes: Seq<u8>,
        scanned: Seq<StoredRecord<'a>>,
        held: Seq<StoredRecord<'a>>,
    )
        requires
            self.holds(bytes, held),
            forall|i: int| 0 <= i < scanned.len() ==> #[trigger] scanned[i].fits(),
            bytes == self.header_bytes() + StoredRecord::concat_bytes(scanned),
        ensures
            scanned == held,
    {
        let (header, header_len) = (self.header_bytes(), self.header_bytes().len() as int);
        let (read, all) = (StoredRecord::concat_bytes(scanned), StoredRecord::concat_bytes(held));
        assert((header + read).len() == (header + all).len());
        assert forall|i: int| 0 <= i < read.len() implies read[i] == all[i] by {
            assert(read[i] == (header + read)[header_len + i]);
            assert(all[i] == (header + all)[header_len + i]);
        }
        assert(read =~= all);
        assert(read =~= all.subrange(0, read.len() as int));
        assert(all =~= read.subrange(0, all.len() as int));
        StoredRecord::lemma_concat_unique(scanned, held);
        StoredRecord::lemma_concat_unique(held, scanned);
        assert(scanned =~= held);
    }

    /// Bytes that hold records past this tail, then bytes that hold more
    /// past the tail after them, hold all of them: a scan of both at once
    /// reads what a scan of each, one after the other, does, at the same
    /// addresses, to the same tail.
    pub(super) proof fn lemma_scan_in_two<'a>(
        self,
        bytes: Seq<u8>,
        records: Seq<StoredRecord<'a>>,
        more_bytes: Seq<u8>,
        more: Seq<StoredRecord<'a>>,
    )
        requires
            self.holds(bytes, records),
            self.after(records).holds(more_bytes, more),
            self.file_len + bytes.len() + more_bytes.len() <= u64::MAX,
            self.next_lsn + records.len() + more.len() <= u64::MAX,
            self.file_appends + records.len() + more.len() <= u64::MAX,
        ensures
            self.holds(bytes + more_bytes, records + more),
            self.addresses(records + more) == self.addresses(records) + self.after(
                records,
            ).addresses(more),
            self.after(records + more) == self.after(records).after(more),
    {
        let (between, all) = (self.after(records), records + more);
        StoredRecord::lemma_concat_append(records, more);
        Self::lemma_append_ends_append(records, more);
        Self::lemma_append_ends_at_most(records);
        Self::lemma_append_ends_at_most(more);
        // Past the first records' bytes, the file has started: no header.
        if self.file_len == 0 {
            assert(self.header_bytes().len() == FILE_HEADER_LEN);
        }
        assert(between.file_len > 0);
        assert(between.header_bytes() =~= Seq::<u8>::empty());
        assert(bytes + more_bytes =~= self.header_bytes() + StoredRecord::concat_bytes(all));
        assert forall|i: int| 0 <= i < all.len() implies #[trigger] all[i].fits() && all[i].lsn
            == self.next_lsn + i by {
            if i >= records.len() {
                assert(all[i] == more[i - records.len()]);
            }
        }
        assert forall|i: int| 0 <= i < all.len() implies #[trigger] self.address_at(all, i) == (
        self.addresses(records) + between.addresses(more))[i] by {
            if i < records.len() {
                assert(all.take(i) =~= records.take(i));
            } else {
                let j = i - records.len();
                assert(all.take(i) =~= records + more.take(j));
                StoredRecord::lemma_concat_append(records, more.take(j));
            }
        }
        assert(self.addresses(all) =~= self.addresses(records) + between.addresses(more));
    }

    /// How many of two runs of records end an append, together.
    proof fn lemma_append_ends_append<'a>(
        records: Seq<StoredRecord<'a>>,
        more: Seq<StoredRecord<'a>>,
    )
        ensures
            Self::append_ends(records + more) == Self::append_ends(records) + Self::append_ends(
                more,
            ),
        decreases more.len(),
    {
        if more.len() == 0 {
            assert(records + more =~= records);
        } else {
            assert((records + more).drop_last() =~= records + more.drop_last());
            Self::lemma_append_ends_append(records, more.drop_last());
        }
    }

    /// At most all of `records` end an append.
    proof fn lemma_append_ends_at_most<'a>(records: Seq<StoredRecord<'a>>)
        ensures
            Self::append_ends(records) <= records.len(),
        decreases records.len(),
    {
        if records.len() > 0 {
            Self::lemma_append_ends_at_most(records.drop_last());
        }
    }

    /// Records none of which ends an append end none.
    proof fn lemma_no_append_ends<'a>(records: Seq<StoredRecord<'a>>)
        requires
            forall|i: int| 0 <= i < records.len() ==> !(#[trigger] records[i]).last_in_append,
        ensures
            Self::append_ends(records) == 0,
        decreases records.len(),
    {
        if records.len() > 0 {
            Self::lemma_no_append_ends(records.drop_last());
        }
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
            assert(bytes@ =~= position.header_bytes());
            assert(records.take(0) =~= Seq::<StoredRecord<'a>>::empty());
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
                assert(record == records[i as int]);
                StoredRecord::lemma_concat_take(records, i as int);
                StoredRecord::lemma_concat_take(records, i + 1);
                assert(records.take(i + 1) =~= records.take(i as int).push(records[i as int]));
                StoredRecord::lemma_concat_push(records.take(i as int), records[i as int]);
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
            assert(records.take(i as int) =~= records);
            assert(addresses@ =~= position.addresses(records));
            assert(records.drop_last().len() + 1 == records.len());
            WalTail::lemma_no_append_ends(records.drop_last());
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
