//! Records: the caller's ([`Record`]), and as the WAL stores them
//! ([`StoredRecord`]), which the writer encodes, and the scan and lookups
//! ([`RecordAddress::read_value`]) decode and check:
//!
//! ```text
//! | crc: u32 | LSN: u64 | key length: u32 | value length: u32 | kind: u8 | key … | value … |
//! ```
//!
//! little-endian and unaligned, the CRC32C of every byte after it. The kind
//! is an insert, a removal or a claim, its bit 7 set on an append's last
//! record.

use std::vec::Vec;

use super::{WalError, WalStorage};

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

/// A record's kind byte, but for bit 7.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    Insert = 1,
    Remove = 2,
    /// A writer's claim on the WAL, its key the writer's id.
    Claim = 3,
}

impl Kind {
    /// The kind byte's bit set on an append's last record.
    const LAST_IN_APPEND: u8 = 0x80;

    fn from_byte(byte: u8) -> Option<Self> {
        match byte & !Self::LAST_IN_APPEND {
            1 => Some(Self::Insert),
            2 => Some(Self::Remove),
            3 => Some(Self::Claim),
            _ => None,
        }
    }
}

/// A record's bytes before its key: CRC, LSN, both lengths and kind.
const RECORD_HEADER_LEN: usize = 21;

/// A record as the WAL stores it.
pub(super) struct StoredRecord<'a> {
    pub(super) lsn: u64,
    pub(super) kind: Kind,
    pub(super) last_in_append: bool,
    pub(super) key: &'a [u8],
    pub(super) value: &'a [u8],
}

impl<'a> StoredRecord<'a> {
    /// Writes the record's bytes to the end of `out`.
    ///
    /// # Panics
    ///
    /// If the key or the value is longer than `u32::MAX` bytes.
    pub(super) fn encode(&self, out: &mut Vec<u8>) {
        let length = |bytes: &[u8]| u32::try_from(bytes.len()).expect("at most u32::MAX bytes");
        let mut kind = self.kind as u8;
        if self.last_in_append {
            kind |= Kind::LAST_IN_APPEND;
        }
        let start = out.len();
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&self.lsn.to_le_bytes());
        out.extend_from_slice(&length(self.key).to_le_bytes());
        out.extend_from_slice(&length(self.value).to_le_bytes());
        out.push(kind);
        out.extend_from_slice(self.key);
        out.extend_from_slice(self.value);
        let crc = crc32c::crc32c(&out[start + 4..]);
        out[start..start + 4].copy_from_slice(&crc.to_le_bytes());
    }

    /// The record at the start of `bytes`, if its lengths fit in `bytes`,
    /// its CRC checks and its kind is one.
    pub(super) fn decode(bytes: &'a [u8]) -> Option<Self> {
        let header: &[u8; RECORD_HEADER_LEN] = bytes.get(..RECORD_HEADER_LEN)?.try_into().ok()?;
        let word = |pos: usize| {
            u32::from_le_bytes([
                header[pos],
                header[pos + 1],
                header[pos + 2],
                header[pos + 3],
            ])
        };
        let key_len = usize::try_from(word(12)).ok()?;
        let value_len = usize::try_from(word(16)).ok()?;
        let len = RECORD_HEADER_LEN
            .checked_add(key_len)?
            .checked_add(value_len)?;
        let record = bytes.get(..len)?;
        if crc32c::crc32c(&record[4..]) != word(0) {
            return None;
        }
        let (key, value) = record[RECORD_HEADER_LEN..].split_at(key_len);
        Some(Self {
            lsn: u64::from_le_bytes(header[4..12].try_into().ok()?),
            kind: Kind::from_byte(header[20])?,
            last_in_append: header[20] & Kind::LAST_IN_APPEND != 0,
            key,
            value,
        })
    }

    /// Its length in bytes.
    pub(super) fn len(&self) -> usize {
        RECORD_HEADER_LEN + self.key.len() + self.value.len()
    }
}

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
