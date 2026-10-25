//! Records: the caller's ([`Record`]), and as the WAL stores them
//! ([`StoredRecord`]), which the writer encodes, and the scan and lookups
//! ([`RecordAddress::read_value`]) decode and check:
//!
//! ```text
//! | crc: u32 | LSN: u64 | key length: u32 | value length: u32 | kind: u8 | key … | value … |
//! ```
//!
//! little-endian and unaligned, the CRC32C of every byte after it. The kind
//! is an insert, a removal or a claim, plus 0x80 on an append's last record.
//!
//! Verus proves that encoding round-trips ([`StoredRecord::decode`]):
//! a record decodes from its bytes, whatever follows them, and nothing
//! else does. The specs are what to review; the one lemma they need,
//! [`RecordView::lemma_starts_with`], Verus checks, and `trusted.rs` holds
//! what the proofs trust.
use std::vec::Vec;

use vstd::bytes::{u32_from_le_bytes, u64_from_le_bytes};
use vstd::prelude::*;
use vstd::slice::slice_subrange;

use super::trusted::{Crc32c, LittleEndian};
use super::{WalError, WalStorage};

#[cfg(verus_only)]
use vstd::bytes::{
    lemma_auto_spec_u32_to_from_le_bytes, lemma_auto_spec_u64_to_from_le_bytes,
    spec_u32_from_le_bytes, spec_u32_to_le_bytes, spec_u64_from_le_bytes, spec_u64_to_le_bytes,
};

/// One key's insert or removal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Record<'a> {
    Insert { key: &'a [u8], value: &'a [u8] },
    Remove { key: &'a [u8] },
}

impl<'a> Record<'a> {
    pub(super) fn kind_key_value(&self) -> (Kind, &'a [u8], &'a [u8]) {
        match *self {
            Self::Insert { key, value } => (Kind::Insert, key, value),
            Self::Remove { key } => (Kind::Remove, key, &[]),
        }
    }
}

/// Where a record lies: its WAL file, its offset in it, and its length.
/// The index keeps it, to read the record back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordAddress {
    pub(super) file: u64,
    pub(super) offset: u64,
    pub(super) len: u64,
}

impl RecordAddress {
    /// Reads the insert here from `storage`, and gives its value.
    pub async fn read_value(self, storage: &impl WalStorage) -> Result<Vec<u8>, WalError> {
        let Self { file, offset, len } = self;
        let mut bytes = vec![0; usize::try_from(len).expect("a record fits in memory")];
        storage.read(file, offset, &mut bytes).await?;
        let value_len = StoredRecord::decode(&bytes)
            .filter(|record| record.len() == bytes.len() && record.kind == Kind::Insert)
            .ok_or(WalError::Corrupted { file, offset })?
            .value
            .len();
        bytes.drain(..bytes.len() - value_len);
        Ok(bytes)
    }
}

verus! {

/// A record's bytes before its key: CRC, LSN, both lengths and kind.
const RECORD_HEADER_LEN: usize = 21;

/// A record's kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Insert,
    Remove,
    /// A writer's claim on the WAL, its key the writer's id.
    Claim,
}

impl Kind {
    /// The kind byte of a record of this kind: 1, 2 or 3, plus 0x80 on an
    /// append's last record.
    pub(super) open spec fn spec_byte(self, last_in_append: bool) -> u8 {
        let byte: u8 = match self {
            Kind::Insert => 1,
            Kind::Remove => 2,
            Kind::Claim => 3,
        };
        if last_in_append {
            (byte + 0x80) as u8
        } else {
            byte
        }
    }

    fn byte(self, last_in_append: bool) -> (byte: u8)
        ensures
            byte == self.spec_byte(last_in_append),
    {
        let byte: u8 = match self {
            Kind::Insert => 1,
            Kind::Remove => 2,
            Kind::Claim => 3,
        };
        if last_in_append {
            byte + 0x80
        } else {
            byte
        }
    }

    /// The kind of a kind byte, and whether its record is its append's last.
    fn of_byte(byte: u8) -> (kind: Option<(Kind, bool)>)
        ensures
            forall|k: Kind, last: bool| #[trigger]
                k.spec_byte(last) == byte <==> kind == Some((k, last)),
    {
        match byte {
            1 => Some((Kind::Insert, false)),
            2 => Some((Kind::Remove, false)),
            3 => Some((Kind::Claim, false)),
            0x81 => Some((Kind::Insert, true)),
            0x82 => Some((Kind::Remove, true)),
            0x83 => Some((Kind::Claim, true)),
            _ => None,
        }
    }
}

/// What a record's bytes hold.
#[allow(dead_code, reason = "only specs read it, and a normal build erases them")]
pub(super) struct RecordView {
    pub(super) lsn: u64,
    pub(super) kind: Kind,
    pub(super) last_in_append: bool,
    pub(super) key: Seq<u8>,
    pub(super) value: Seq<u8>,
}

impl RecordView {
    /// Whether a record can hold it: its key's and value's lengths fit
    /// `u32`s.
    pub(super) open spec fn fits(self) -> bool {
        self.key.len() <= u32::MAX && self.value.len() <= u32::MAX
    }

    /// The record's bytes after its CRC, which the CRC covers.
    pub(super) open spec fn checked_bytes(self) -> Seq<u8> {
        spec_u64_to_le_bytes(self.lsn) + spec_u32_to_le_bytes(self.key.len() as u32)
            + spec_u32_to_le_bytes(self.value.len() as u32) + seq![
            self.kind.spec_byte(self.last_in_append),
        ] + self.key + self.value
    }

    /// The record's bytes.
    pub(super) open spec fn bytes(self) -> Seq<u8> {
        spec_u32_to_le_bytes(Crc32c::spec_of(self.checked_bytes())) + self.checked_bytes()
    }

    /// Whether decoding `bytes` reads this record's fields: its lengths,
    /// LSN, kind byte, key and value where a record keeps them, and a CRC
    /// that matches the bytes after it.
    spec fn is_read_from(self, bytes: Seq<u8>) -> bool {
        let key_end = 21 + self.key.len() as int;
        let len = key_end + self.value.len();
        &&& len <= bytes.len()
        &&& spec_u32_from_le_bytes(bytes.subrange(0, 4)) == Crc32c::spec_of(bytes.subrange(4, len))
        &&& spec_u64_from_le_bytes(bytes.subrange(4, 12)) == self.lsn
        &&& spec_u32_from_le_bytes(bytes.subrange(12, 16)) as int == self.key.len()
        &&& spec_u32_from_le_bytes(bytes.subrange(16, 20)) as int == self.value.len()
        &&& bytes[20] == self.kind.spec_byte(self.last_in_append)
        &&& bytes.subrange(21, key_end) == self.key
        &&& bytes.subrange(key_end, len) == self.value
    }

    /// `bytes` start with this record's bytes exactly when decoding them
    /// reads its fields.
    proof fn lemma_starts_with(self, bytes: Seq<u8>)
        requires
            self.fits(),
        ensures
            self.bytes().is_prefix_of(bytes) <==> self.is_read_from(bytes),
    {
        lemma_auto_spec_u32_to_from_le_bytes();
        lemma_auto_spec_u64_to_from_le_bytes();
        let (encoded, checked) = (self.bytes(), self.checked_bytes());
        let len = encoded.len() as int;
        let key_end = 21 + self.key.len() as int;
        // Where the fields lie in `encoded`.
        assert(encoded.subrange(0, 4) =~= spec_u32_to_le_bytes(Crc32c::spec_of(checked)));
        assert(encoded.subrange(4, len) =~= checked);
        assert(encoded.subrange(4, 12) =~= spec_u64_to_le_bytes(self.lsn));
        assert(encoded.subrange(12, 16) =~= spec_u32_to_le_bytes(self.key.len() as u32));
        assert(encoded.subrange(16, 20) =~= spec_u32_to_le_bytes(self.value.len() as u32));
        assert(encoded.subrange(21, key_end) =~= self.key);
        assert(encoded.subrange(key_end, len) =~= self.value);
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
            assert(self.key.len() as u32 == spec_u32_from_le_bytes(bytes.subrange(12, 16)));
            assert(spec_u32_to_le_bytes(self.key.len() as u32) == bytes.subrange(12, 16));
            assert(self.value.len() as u32 == spec_u32_from_le_bytes(bytes.subrange(16, 20)));
            assert(spec_u32_to_le_bytes(self.value.len() as u32) == bytes.subrange(16, 20));
            assert(checked =~= bytes.subrange(4, len));
            assert(spec_u32_to_le_bytes(Crc32c::spec_of(checked)) == bytes.subrange(0, 4));
            assert(encoded =~= bytes.subrange(0, len));
        }
    }
}

/// A record as the WAL stores it.
pub(super) struct StoredRecord<'a> {
    pub(super) lsn: u64,
    pub(super) kind: Kind,
    pub(super) last_in_append: bool,
    pub(super) key: &'a [u8],
    pub(super) value: &'a [u8],
}

impl View for StoredRecord<'_> {
    type V = RecordView;

    closed spec fn view(&self) -> RecordView {
        RecordView {
            lsn: self.lsn,
            kind: self.kind,
            last_in_append: self.last_in_append,
            key: self.key@,
            value: self.value@,
        }
    }
}

impl<'a> StoredRecord<'a> {
    /// Writes the record's bytes to the end of `out`.
    pub(super) fn encode(&self, out: &mut Vec<u8>)
        requires
            self@.fits(),
        ensures
            final(out)@ == old(out)@ + self@.bytes(),
    {
        let start = out.len();
        LittleEndian::push_u32(out, 0);
        let checked_start = out.len();
        LittleEndian::push_u64(out, self.lsn);
        LittleEndian::push_u32(out, self.key.len() as u32);
        LittleEndian::push_u32(out, self.value.len() as u32);
        out.push(self.kind.byte(self.last_in_append));
        out.extend_from_slice(self.key);
        out.extend_from_slice(self.value);
        proof {
            lemma_auto_spec_u32_to_from_le_bytes();
            lemma_auto_spec_u64_to_from_le_bytes();
            assert(out@ =~= old(out)@ + spec_u32_to_le_bytes(0) + self@.checked_bytes());
            assert(out@.subrange(checked_start as int, out@.len() as int)
                =~= self@.checked_bytes());
        }
        let crc = Crc32c::of(slice_subrange(out.as_slice(), checked_start, out.len()));
        let ghost before_crc = out@;
        LittleEndian::write_u32(out.as_mut_slice(), start, crc);
        proof {
            assert(before_crc.subrange(0, start as int) =~= old(out)@);
            assert(before_crc.subrange(checked_start as int, before_crc.len() as int)
                =~= self@.checked_bytes());
            assert(out@ =~= old(out)@ + self@.bytes());
        }
    }

    /// The record at the start of `bytes`, if one is.
    pub(super) fn decode(bytes: &'a [u8]) -> (decoded: Option<Self>)
        ensures
    // What it decodes, `bytes` start with.

            decoded matches Some(record) ==> record@.fits() && record@.bytes().is_prefix_of(bytes@),
            // Whatever record `bytes` start with, it decodes.
            forall|record: RecordView|
                record.fits() && #[trigger] record.bytes().is_prefix_of(bytes@) ==> (
                decoded matches Some(decoded) && decoded@ == record),
    {
        proof {
            assert forall|record: RecordView| record.fits() implies (
            #[trigger] record.bytes().is_prefix_of(bytes@) <==> record.is_read_from(bytes@)) by {
                record.lemma_starts_with(bytes@);
            }
        }
        if bytes.len() < RECORD_HEADER_LEN {
            return None;
        }
        let crc = u32_from_le_bytes(slice_subrange(bytes, 0, 4));
        let lsn = u64_from_le_bytes(slice_subrange(bytes, 4, 12));
        let key_len = u32_from_le_bytes(slice_subrange(bytes, 12, 16)) as usize;
        let value_len = u32_from_le_bytes(slice_subrange(bytes, 16, 20)) as usize;
        let (kind, last_in_append) = Kind::of_byte(bytes[20])?;
        if key_len > bytes.len() - RECORD_HEADER_LEN {
            return None;
        }
        let key_end = RECORD_HEADER_LEN + key_len;
        if value_len > bytes.len() - key_end {
            return None;
        }
        let len = key_end + value_len;
        if Crc32c::of(slice_subrange(bytes, 4, len)) != crc {
            return None;
        }
        let key = slice_subrange(bytes, RECORD_HEADER_LEN, key_end);
        let value = slice_subrange(bytes, key_end, len);
        Some(Self { lsn, kind, last_in_append, key, value })
    }

    /// Its length in bytes.
    pub(super) fn len(&self) -> (len: usize)
        requires
            self@.bytes().len() <= usize::MAX,
        ensures
            len == self@.bytes().len(),
    {
        proof {
            lemma_auto_spec_u32_to_from_le_bytes();
            lemma_auto_spec_u64_to_from_le_bytes();
        }
        RECORD_HEADER_LEN + self.key.len() + self.value.len()
    }
}

} // verus!
#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(record: StoredRecord<'_>) -> Vec<u8> {
        let mut bytes = Vec::new();
        record.encode(&mut bytes);
        bytes
    }

    #[test]
    fn decodes_what_it_encodes() {
        let (key, value) = (&b"key"[..], &b"value"[..]);
        let bytes = encoded(StoredRecord {
            lsn: 7,
            kind: Kind::Insert,
            last_in_append: true,
            key,
            value,
        });
        let record = StoredRecord::decode(&bytes).unwrap();
        assert_eq!(
            (record.lsn, record.kind, record.last_in_append),
            (7, Kind::Insert, true)
        );
        assert_eq!((record.key, record.value), (key, value));
        assert_eq!(record.len(), bytes.len());
    }

    #[test]
    fn any_flipped_bit_fails() {
        let record = StoredRecord {
            lsn: u64::MAX,
            kind: Kind::Remove,
            last_in_append: false,
            key: b"key",
            value: &[],
        };
        let bytes = encoded(record);
        for bit in 0..bytes.len() * 8 {
            let mut flipped = bytes.clone();
            flipped[bit / 8] ^= 1 << (bit % 8);
            assert!(StoredRecord::decode(&flipped).is_none(), "bit {bit}");
        }
    }

    #[test]
    fn a_cut_record_fails() {
        let record = StoredRecord {
            lsn: 0,
            kind: Kind::Insert,
            last_in_append: true,
            key: b"key",
            value: b"value",
        };
        let bytes = encoded(record);
        for len in 0..bytes.len() {
            assert!(StoredRecord::decode(&bytes[..len]).is_none(), "{len} bytes");
        }
    }
}
