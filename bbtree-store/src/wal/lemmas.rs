//! The WAL's proofs: lemmas that show the code in `record.rs` and `tail.rs`
//! keeps its specs. Only Verus reads them; a reviewer reads the specs
//! (`design/bbtree-store/01-wal.md`, "Verification"). One lemma states a
//! property of its own: a scan picks up where it stopped
//! (`WalTail::lemma_scan_in_two`).
use vstd::prelude::*;

use super::record::StoredRecord;
use super::tail::WalTail;

#[cfg(verus_only)]
use vstd::bytes::{
    lemma_auto_spec_u32_to_from_le_bytes, lemma_auto_spec_u64_to_from_le_bytes,
    spec_u32_from_le_bytes, spec_u32_to_le_bytes, spec_u64_from_le_bytes, spec_u64_to_le_bytes,
};

#[cfg(verus_only)]
use super::record::RecordAddress;
#[cfg(verus_only)]
use super::tail::{FILE_HEADER, FILE_HEADER_LEN};
#[cfg(verus_only)]
use super::trusted::Crc32c;

verus! {

impl<'a> StoredRecord<'a> {
    /// Whether decoding `bytes` reads this record's fields: its lengths,
    /// LSN, kind byte, key and value where a record keeps them, and a CRC
    /// that matches the bytes after it.
    pub(super) open spec fn is_read_from(self, bytes: Seq<u8>) -> bool {
        let key_end = 21 + self.key@.len() as int;
        let len = key_end + self.value@.len();
        &&& len <= bytes.len()
        &&& spec_u32_from_le_bytes(bytes.subrange(0, 4)) == Crc32c::spec_of(bytes.subrange(4, len))
        &&& spec_u64_from_le_bytes(bytes.subrange(4, 12)) == self.lsn
        &&& spec_u32_from_le_bytes(bytes.subrange(12, 16)) as int == self.key@.len()
        &&& spec_u32_from_le_bytes(bytes.subrange(16, 20)) as int == self.value@.len()
        &&& bytes[20] == self.kind.spec_byte(self.last_in_append)
        &&& bytes.subrange(21, key_end) == self.key@
        &&& bytes.subrange(key_end, len) == self.value@
    }

    /// `bytes` start with a record's bytes exactly when decoding them reads
    /// its fields.
    pub(super) proof fn lemma_read_from(bytes: Seq<u8>)
        ensures
            forall|record: Self|
                record.fits() ==> (#[trigger] record.bytes().is_prefix_of(bytes)
                    <==> record.is_read_from(bytes)),
    {
        assert forall|record: Self| record.fits() implies (#[trigger] record.bytes().is_prefix_of(
            bytes,
        ) <==> record.is_read_from(bytes)) by {
            record.lemma_starts_with(bytes);
        }
    }

    /// This record is the only one whose bytes start `bytes`.
    pub(super) proof fn lemma_only_record(self, bytes: Seq<u8>)
        requires
            self.fits(),
            self.bytes().is_prefix_of(bytes),
        ensures
            forall|record: Self|
                record.fits() && #[trigger] record.bytes().is_prefix_of(bytes) ==> record == self,
    {
        assert forall|record: Self|
            record.fits() && #[trigger] record.bytes().is_prefix_of(bytes) implies record
            == self by {
            self.lemma_same_record(record, bytes);
        }
    }

    /// What `encode` pushes after `before`: a zero CRC, then the bytes the
    /// CRC covers.
    pub(super) proof fn lemma_pushed(self, before: Seq<u8>, pushed: Seq<u8>)
        requires
            pushed =~= (before + spec_u32_to_le_bytes(0) + spec_u64_to_le_bytes(self.lsn)
                + spec_u32_to_le_bytes(self.key@.len() as u32) + spec_u32_to_le_bytes(
                self.value@.len() as u32,
            )).push(self.kind.spec_byte(self.last_in_append)) + self.key@ + self.value@,
        ensures
            pushed.len() == before.len() + 4 + self.checked_bytes().len(),
            pushed.subrange(0, before.len() as int) == before,
            pushed.subrange((before.len() + 4) as int, pushed.len() as int) == self.checked_bytes(),
    {
        lemma_auto_spec_u32_to_from_le_bytes();
        lemma_auto_spec_u64_to_from_le_bytes();
        assert(pushed =~= before + spec_u32_to_le_bytes(0) + self.checked_bytes());
        assert(pushed.subrange(0, before.len() as int) =~= before);
        assert(pushed.subrange((before.len() + 4) as int, pushed.len() as int)
            =~= self.checked_bytes());
    }

    /// With the CRC written over the zero, `encode` has pushed the record's
    /// bytes.
    pub(super) proof fn lemma_crc_written(self, before: Seq<u8>, encoded: Seq<u8>)
        requires
            encoded == before + spec_u32_to_le_bytes(Crc32c::spec_of(self.checked_bytes()))
                + self.checked_bytes(),
        ensures
            encoded == before + self.bytes(),
    {
        reveal(StoredRecord::bytes);
        assert(encoded =~= before + self.bytes());
    }

    /// A record's length: its header's 21 bytes, then its key and value.
    pub(super) proof fn lemma_len(self)
        ensures
            self.bytes().len() == 21 + self.key@.len() + self.value@.len(),
    {
        reveal(StoredRecord::bytes);
        lemma_auto_spec_u32_to_from_le_bytes();
        lemma_auto_spec_u64_to_from_le_bytes();
    }

    /// `bytes` start with this record's bytes exactly when decoding them
    /// reads its fields.
    proof fn lemma_starts_with(self, bytes: Seq<u8>)
        requires
            self.fits(),
        ensures
            self.bytes().is_prefix_of(bytes) <==> self.is_read_from(bytes),
    {
        reveal(StoredRecord::bytes);
        lemma_auto_spec_u32_to_from_le_bytes();
        lemma_auto_spec_u64_to_from_le_bytes();
        let (encoded, checked) = (self.bytes(), self.checked_bytes());
        let len = encoded.len() as int;
        let key_end = 21 + self.key@.len() as int;
        // Where the fields lie in `encoded`.
        assert(encoded.subrange(0, 4) =~= spec_u32_to_le_bytes(Crc32c::spec_of(checked)));
        assert(encoded.subrange(4, len) =~= checked);
        assert(encoded.subrange(4, 12) =~= spec_u64_to_le_bytes(self.lsn));
        assert(encoded.subrange(12, 16) =~= spec_u32_to_le_bytes(self.key@.len() as u32));
        assert(encoded.subrange(16, 20) =~= spec_u32_to_le_bytes(self.value@.len() as u32));
        assert(encoded.subrange(21, key_end) =~= self.key@);
        assert(encoded.subrange(key_end, len) =~= self.value@);
        if encoded.is_prefix_of(bytes) {
            // Each field of `bytes` is the one at the same place in `encoded`.
            assert(bytes.subrange(0, len) == encoded);
            assert(bytes.subrange(0, 4) =~= encoded.subrange(0, 4));
            assert(bytes.subrange(4, len) =~= encoded.subrange(4, len));
            assert(bytes.subrange(4, 12) =~= encoded.subrange(4, 12));
            assert(bytes.subrange(12, 16) =~= encoded.subrange(12, 16));
            assert(bytes.subrange(16, 20) =~= encoded.subrange(16, 20));
            assert(bytes[20] == encoded[20]);
            assert(bytes.subrange(21, key_end) =~= encoded.subrange(21, key_end));
            assert(bytes.subrange(key_end, len) =~= encoded.subrange(key_end, len));
        }
        if self.is_read_from(bytes) {
            // Each field of `encoded` is the one at the same place in `bytes`.
            assert(spec_u64_to_le_bytes(self.lsn) == bytes.subrange(4, 12));
            assert(self.key@.len() as u32 == spec_u32_from_le_bytes(bytes.subrange(12, 16)));
            assert(spec_u32_to_le_bytes(self.key@.len() as u32) == bytes.subrange(12, 16));
            assert(self.value@.len() as u32 == spec_u32_from_le_bytes(bytes.subrange(16, 20)));
            assert(spec_u32_to_le_bytes(self.value@.len() as u32) == bytes.subrange(16, 20));
            assert(checked =~= bytes.subrange(4, len));
            assert(spec_u32_to_le_bytes(Crc32c::spec_of(checked)) == bytes.subrange(0, 4));
            assert(encoded =~= bytes.subrange(0, len));
        }
    }

    /// Two records whose bytes both start `bytes` are one.
    proof fn lemma_same_record(self, other: Self, bytes: Seq<u8>)
        requires
            self.fits(),
            other.fits(),
            self.bytes().is_prefix_of(bytes),
            other.bytes().is_prefix_of(bytes),
        ensures
            self == other,
    {
        self.lemma_starts_with(bytes);
        other.lemma_starts_with(bytes);
        assert(self.key =~= other.key);
        assert(self.value =~= other.value);
    }

    /// The bytes of `records`, then `record`, are those of `records`, then
    /// `record`'s.
    proof fn lemma_concat_push(records: Seq<Self>, record: Self)
        ensures
            Self::concat_bytes(records.push(record)) == Self::concat_bytes(records)
                + record.bytes(),
        decreases records.len(),
    {
        let pushed = records.push(record);
        if records.len() == 0 {
            assert(pushed.skip(1) =~= records);
        } else {
            assert(pushed.skip(1) =~= records.skip(1).push(record));
            Self::lemma_concat_push(records.skip(1), record);
        }
        assert(Self::concat_bytes(pushed) =~= Self::concat_bytes(records) + record.bytes());
    }

    /// The bytes of the first `n` of `records` start the bytes of all.
    proof fn lemma_concat_take(records: Seq<Self>, n: int)
        requires
            0 <= n <= records.len(),
        ensures
            Self::concat_bytes(records.take(n)).is_prefix_of(Self::concat_bytes(records)),
        decreases n,
    {
        if n > 0 {
            let rest = records.skip(1);
            assert(records.take(n).skip(1) =~= rest.take(n - 1));
            Self::lemma_concat_take(rest, n - 1);
            let (first, taken) = (records[0].bytes(), Self::concat_bytes(rest.take(n - 1)));
            assert(Self::concat_bytes(records.take(n)) =~= Self::concat_bytes(records).subrange(
                0,
                (first.len() + taken.len()) as int,
            ));
        }
    }

    /// The bytes of `records`, then `others`, are those of `records`, then
    /// those of `others`.
    proof fn lemma_concat_append(records: Seq<Self>, others: Seq<Self>)
        ensures
            Self::concat_bytes(records + others) == Self::concat_bytes(records)
                + Self::concat_bytes(others),
        decreases records.len(),
    {
        if records.len() == 0 {
            assert(records + others =~= others);
        } else {
            assert((records + others).skip(1) =~= records.skip(1) + others);
            Self::lemma_concat_append(records.skip(1), others);
        }
        assert(Self::concat_bytes(records + others) =~= Self::concat_bytes(records)
            + Self::concat_bytes(others));
    }

    /// Records whose bytes start the bytes of `others` are the first of
    /// them: the bytes of records split into records one way only.
    proof fn lemma_concat_unique(records: Seq<Self>, others: Seq<Self>)
        requires
            forall|i: int| 0 <= i < records.len() ==> #[trigger] records[i].fits(),
            forall|i: int| 0 <= i < others.len() ==> #[trigger] others[i].fits(),
            Self::concat_bytes(records).is_prefix_of(Self::concat_bytes(others)),
        ensures
            records.len() <= others.len(),
            records == others.take(records.len() as int),
        decreases records.len(),
    {
        if records.len() == 0 {
            assert(records =~= others.take(0));
        } else {
            let (first, rest) = (records[0], records.skip(1));
            let all = Self::concat_bytes(others);
            first.lemma_len();
            assert(others.len() > 0);
            let (other_first, other_rest) = (others[0], others.skip(1));
            let first_len = first.bytes().len() as int;
            assert(first.bytes() =~= all.subrange(0, first_len));
            assert(other_first.bytes() =~= all.subrange(0, other_first.bytes().len() as int));
            first.lemma_same_record(other_first, all);
            // Past the first record, the rest of `records`' bytes start the
            // rest of `others`'.
            let (mine, rest_bytes) = (Self::concat_bytes(records), Self::concat_bytes(rest));
            let other_rest_bytes = Self::concat_bytes(other_rest);
            assert(mine == first.bytes() + rest_bytes);
            assert(all == other_first.bytes() + other_rest_bytes);
            assert forall|i: int| 0 <= i < rest_bytes.len() implies rest_bytes[i]
                == other_rest_bytes[i] by {
                assert(rest_bytes[i] == mine[first_len + i]);
                assert(mine[first_len + i] == all[first_len + i]);
            }
            assert(rest_bytes =~= other_rest_bytes.subrange(0, rest_bytes.len() as int));
            Self::lemma_concat_unique(rest, other_rest);
            assert forall|i: int| 0 <= i < records.len() implies records[i] == others[i] by {
                if i > 0 {
                    assert(records[i] == rest[i - 1]);
                }
            }
            assert(records =~= others.take(records.len() as int));
        }
    }
}

impl WalTail {
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

    /// Bytes that end, or differ from the file header, at `i` hold no
    /// records past a tail where the file starts.
    pub(super) proof fn lemma_no_header<'a>(self, bytes: Seq<u8>, i: int)
        requires
            self.file_len == 0,
            0 <= i < FILE_HEADER_LEN,
            i <= bytes.len(),
            i == bytes.len() || bytes[i] != FILE_HEADER@[i],
        ensures
            forall|held: Seq<StoredRecord<'a>>|
                #![trigger self.holds(bytes, held)]
                !self.holds(bytes, held),
    {
        assert forall|held: Seq<StoredRecord<'a>>| #[trigger]
            self.holds(bytes, held) implies false by {
            let all = FILE_HEADER@ + StoredRecord::concat_bytes(held);
            assert(bytes[i] == all[i]);
        }
    }

    /// The bytes of the header past this tail and no records are the
    /// header's.
    pub(super) proof fn lemma_header_alone<'a>(self, bytes: Seq<u8>, records: Seq<StoredRecord<'a>>)
        requires
            records.len() == 0,
            bytes =~= self.header_bytes(),
        ensures
            bytes == self.header_bytes() + StoredRecord::concat_bytes(records),
    {
        assert(bytes =~= self.header_bytes() + StoredRecord::concat_bytes(records));
    }

    /// Past the records a scan read, `scanned`, to `pos`, bytes that hold
    /// records hold the next one: it fits, has the next LSN, and its bytes
    /// start those at `pos`.
    pub(super) proof fn lemma_next_held<'a>(
        self,
        bytes: Seq<u8>,
        scanned: Seq<StoredRecord<'a>>,
        pos: int,
    )
        requires
            forall|i: int| 0 <= i < scanned.len() ==> #[trigger] scanned[i].fits(),
            0 <= pos < bytes.len(),
            bytes.subrange(0, pos) == self.header_bytes() + StoredRecord::concat_bytes(scanned),
        ensures
            forall|held: Seq<StoredRecord<'a>>| #[trigger]
                self.holds(bytes, held) ==> {
                    &&& scanned.len() < held.len()
                    &&& held[scanned.len() as int].fits()
                    &&& held[scanned.len() as int].lsn == self.next_lsn + scanned.len()
                    &&& held[scanned.len() as int].bytes().is_prefix_of(
                        bytes.subrange(pos, bytes.len() as int),
                    )
                },
    {
        let (header, header_len) = (self.header_bytes(), self.header_bytes().len() as int);
        let (read, k) = (StoredRecord::concat_bytes(scanned), scanned.len() as int);
        assert forall|held: Seq<StoredRecord<'a>>| #[trigger] self.holds(bytes, held) implies {
            &&& k < held.len()
            &&& held[k].fits()
            &&& held[k].lsn == self.next_lsn + k
            &&& held[k].bytes().is_prefix_of(bytes.subrange(pos, bytes.len() as int))
        } by {
            let all = StoredRecord::concat_bytes(held);
            // Past the header, the bytes scanned start the records' bytes:
            // the records scanned are the first held.
            assert(bytes.subrange(0, pos).len() == pos);
            assert(read.len() + header_len == pos);
            assert forall|i: int| 0 <= i < read.len() implies read[i] == all[i] by {
                assert(read[i] == (header + read)[header_len + i]);
                assert(bytes[header_len + i] == (header + all)[header_len + i]);
            }
            assert(read =~= all.subrange(0, read.len() as int));
            StoredRecord::lemma_concat_unique(scanned, held);
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
            assert(next.is_prefix_of(bytes.subrange(pos, bytes.len() as int)));
        }
    }

    /// The records a scan read, `scanned`, to `pos`, then `record`, which
    /// starts the bytes there: their bytes, addresses and appends.
    pub(super) proof fn lemma_scanned_one_more<'a>(
        self,
        bytes: Seq<u8>,
        scanned: Seq<StoredRecord<'a>>,
        record: StoredRecord<'a>,
        pos: int,
    )
        requires
            0 <= pos <= bytes.len(),
            bytes.subrange(0, pos) == self.header_bytes() + StoredRecord::concat_bytes(scanned),
            record.bytes().is_prefix_of(bytes.subrange(pos, bytes.len() as int)),
        ensures
            record.bytes().len() > 0,
            bytes.subrange(0, pos + record.bytes().len()) == self.header_bytes()
                + StoredRecord::concat_bytes(scanned.push(record)),
            self.addresses(scanned.push(record)) == self.addresses(scanned).push(
                RecordAddress {
                    file: self.file,
                    offset: (self.file_len + pos) as u64,
                    len: record.bytes().len() as u64,
                },
            ),
            Self::append_ends(scanned.push(record)) == Self::append_ends(scanned)
                + if record.last_in_append {
                1nat
            } else {
                0nat
            },
    {
        let pushed = scanned.push(record);
        let len = record.bytes().len() as int;
        record.lemma_len();
        assert(record.bytes() =~= bytes.subrange(pos, pos + len));
        StoredRecord::lemma_concat_push(scanned, record);
        assert(bytes.subrange(0, pos + len) =~= bytes.subrange(0, pos) + record.bytes());
        assert(pushed.drop_last() =~= scanned);
        assert(pushed.take(scanned.len() as int) =~= scanned);
        assert forall|j: int| 0 <= j < scanned.len() implies #[trigger] self.address_at(pushed, j)
            == self.address_at(scanned, j) by {
            assert(pushed.take(j) =~= scanned.take(j));
        }
        assert(self.addresses(pushed) =~= self.addresses(scanned).push(
            RecordAddress {
                file: self.file,
                offset: (self.file_len + pos) as u64,
                len: record.bytes().len() as u64,
            },
        ));
    }

    /// Bytes that are those of the records a scan read, `scanned`, past the
    /// header, are all that hold records, and only those.
    pub(super) proof fn lemma_scanned_all<'a>(self, bytes: Seq<u8>, scanned: Seq<StoredRecord<'a>>)
        requires
            forall|i: int| 0 <= i < scanned.len() ==> #[trigger] scanned[i].fits(),
            bytes.subrange(0, bytes.len() as int) == self.header_bytes()
                + StoredRecord::concat_bytes(scanned),
        ensures
            bytes == self.header_bytes() + StoredRecord::concat_bytes(scanned),
            forall|held: Seq<StoredRecord<'a>>| #[trigger]
                self.holds(bytes, held) ==> held == scanned,
    {
        let (header, header_len) = (self.header_bytes(), self.header_bytes().len() as int);
        let read = StoredRecord::concat_bytes(scanned);
        assert(bytes.subrange(0, bytes.len() as int) =~= bytes);
        assert forall|held: Seq<StoredRecord<'a>>| #[trigger] self.holds(bytes, held) implies held
            == scanned by {
            let all = StoredRecord::concat_bytes(held);
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
    }

    /// The bytes of the first `i + 1` of `records`, the `i`th being
    /// `record`, are those of the first `i`, then `record`'s.
    pub(super) proof fn lemma_encoded_one_more<'a>(
        self,
        records: Seq<StoredRecord<'a>>,
        i: int,
        record: StoredRecord<'a>,
    )
        requires
            0 <= i < records.len(),
            record == records[i],
        ensures
            self.header_bytes() + StoredRecord::concat_bytes(records.take(i + 1))
                == self.header_bytes() + StoredRecord::concat_bytes(records.take(i))
                + record.bytes(),
            StoredRecord::concat_bytes(records.take(i + 1)).len() <= StoredRecord::concat_bytes(
                records,
            ).len(),
    {
        StoredRecord::lemma_concat_take(records, i + 1);
        assert(records.take(i + 1) =~= records.take(i).push(record));
        StoredRecord::lemma_concat_push(records.take(i), record);
        assert(self.header_bytes() + StoredRecord::concat_bytes(records.take(i + 1))
            =~= self.header_bytes() + StoredRecord::concat_bytes(records.take(i)) + record.bytes());
    }

    /// An append's records, `records`, the last alone ending it, all
    /// encoded at `addresses`.
    pub(super) proof fn lemma_encoded_all<'a>(
        self,
        records: Seq<StoredRecord<'a>>,
        addresses: Seq<RecordAddress>,
    )
        requires
            records.len() > 0,
            forall|j: int|
                0 <= j < records.len() ==> (#[trigger] records[j]).last_in_append == (j
                    == records.len() - 1),
            addresses.len() == records.len(),
            forall|j: int|
                0 <= j < records.len() ==> #[trigger] addresses[j] == self.address_at(records, j),
        ensures
            records.take(records.len() as int) == records,
            addresses == self.addresses(records),
            Self::append_ends(records) == 1,
    {
        assert(records.take(records.len() as int) =~= records);
        assert(addresses =~= self.addresses(records));
        assert(records.drop_last().len() + 1 == records.len());
        Self::lemma_no_append_ends(records.drop_last());
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

} // verus!
