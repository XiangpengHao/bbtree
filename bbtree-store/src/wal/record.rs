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
//! else does. The specs are what to review; the proofs are in `lemmas.rs`,
//! which only Verus reads, and `trusted.rs` holds what they trust.
use std::vec::Vec;

use vstd::bytes::{u32_from_le_bytes, u64_from_le_bytes};
use vstd::prelude::*;
use vstd::slice::slice_subrange;

use super::trusted::{Crc32c, LittleEndian};
use super::{WalError, WalStorage};

#[cfg(verus_only)]
use vstd::bytes::{spec_u32_to_le_bytes, spec_u64_to_le_bytes};

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

/// Where a record lies: its WAL file, its offset in it, and its length.
/// The index keeps it, to read the record back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecordAddress {
    pub(super) file: u64,
    pub(super) offset: u64,
    pub(super) len: u64,
}

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

/// A record as the WAL stores it.
pub(super) struct StoredRecord<'a> {
    pub(super) lsn: u64,
    pub(super) kind: Kind,
    pub(super) last_in_append: bool,
    pub(super) key: &'a [u8],
    pub(super) value: &'a [u8],
}

impl<'a> StoredRecord<'a> {
    /// Whether its key's and value's lengths fit the `u32`s its bytes keep
    /// them in.
    pub(super) open spec fn fits(self) -> bool {
        self.key@.len() <= u32::MAX && self.value@.len() <= u32::MAX
    }

    /// The record's bytes after its CRC, which the CRC covers.
    pub(super) open spec fn checked_bytes(self) -> Seq<u8> {
        spec_u64_to_le_bytes(self.lsn) + spec_u32_to_le_bytes(self.key@.len() as u32)
            + spec_u32_to_le_bytes(self.value@.len() as u32) + seq![
            self.kind.spec_byte(self.last_in_append),
        ] + self.key@ + self.value@
    }

    /// The record's bytes. Opaque: proofs that read its layout reveal it,
    /// others see only its length (`lemma_len`).
    #[verifier::opaque]
    pub(super) open spec fn bytes(self) -> Seq<u8> {
        spec_u32_to_le_bytes(Crc32c::spec_of(self.checked_bytes())) + self.checked_bytes()
    }

    /// The bytes of `records`, one after another.
    pub(super) open spec fn concat_bytes(records: Seq<Self>) -> Seq<u8>
        decreases records.len(),
    {
        if records.len() == 0 {
            Seq::empty()
        } else {
            records[0].bytes() + Self::concat_bytes(records.skip(1))
        }
    }
}

impl<'a> StoredRecord<'a> {
    /// Writes the record's bytes to the end of `out`.
    pub(super) fn encode(&self, out: &mut Vec<u8>)
        requires
            self.fits(),
        ensures
            final(out)@ == old(out)@ + self.bytes(),
    {
        let start = out.len();
        // A zero CRC, written over once the CRC of the bytes after it is known.
        LittleEndian::push_u32(out, 0);
        LittleEndian::push_u64(out, self.lsn);
        LittleEndian::push_u32(out, self.key.len() as u32);
        LittleEndian::push_u32(out, self.value.len() as u32);
        out.push(self.kind.byte(self.last_in_append));
        out.extend_from_slice(self.key);
        out.extend_from_slice(self.value);
        proof {
            self.lemma_pushed(old(out)@, out@);
        }
        let end = out.len();
        let crc = Crc32c::of(slice_subrange(out.as_slice(), start + 4, end));
        LittleEndian::write_u32(out.as_mut_slice(), start, crc);
        proof {
            self.lemma_crc_written(old(out)@, out@);
        }
    }

    /// The record at the start of `bytes`, if one is: it decodes a record
    /// only if `bytes` start with its bytes, and every record they start
    /// with.
    pub(super) fn decode(bytes: &'a [u8]) -> (decoded: Option<Self>)
        ensures
            decoded matches Some(record) ==> record.fits() && record.bytes().is_prefix_of(bytes@),
            forall|record: StoredRecord<'a>|
                record.fits() && #[trigger] record.bytes().is_prefix_of(bytes@) ==> (
                decoded matches Some(decoded) && decoded == record),
    {
        proof {
            Self::lemma_read_from(bytes@);
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
        let decoded = Self { lsn, kind, last_in_append, key, value };
        proof {
            decoded.lemma_only_record(bytes@);
        }
        Some(decoded)
    }

    /// Its length in bytes.
    pub(super) fn len(&self) -> (len: usize)
        requires
            self.bytes().len() <= usize::MAX,
        ensures
            len == self.bytes().len(),
    {
        proof {
            self.lemma_len();
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
}
