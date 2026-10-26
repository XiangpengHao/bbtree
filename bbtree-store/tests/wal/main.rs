//! The WAL on storage in memory: what a writer appends, an open replays,
//! across files, lost outcomes and a second writer; corruption is an error.

mod memory;

use bbtree_store::wal::{Record, RecordAddress, WalError, WalStorage, WalWriter};
use pollster::block_on;

use memory::{AppendFault, MemoryStorage};

/// A record an open replayed, owning its bytes.
#[derive(Debug, PartialEq, Eq)]
enum Replayed {
    Insert(Vec<u8>, Vec<u8>),
    Remove(Vec<u8>),
}

impl Replayed {
    fn insert(key: &[u8], value: &[u8]) -> Self {
        Self::Insert(key.to_vec(), value.to_vec())
    }
}

impl From<Record<'_>> for Replayed {
    fn from(record: Record<'_>) -> Self {
        match record {
            Record::Insert { key, value } => Self::insert(key, value),
            Record::Remove { key } => Self::Remove(key.to_vec()),
        }
    }
}

/// An opened WAL: its writer, and what the open replayed.
struct Opened<S> {
    writer: WalWriter<S>,
    replayed: Vec<Replayed>,
    addresses: Vec<RecordAddress>,
}

impl<S: WalStorage> Opened<S> {
    fn open(storage: S) -> Result<Self, WalError> {
        let (mut replayed, mut addresses) = (Vec::new(), Vec::new());
        let writer = block_on(WalWriter::open(storage, |address, record| {
            replayed.push(Replayed::from(record));
            addresses.push(address);
        }))?;
        Ok(Self {
            writer,
            replayed,
            addresses,
        })
    }

    /// Appends an insert of `key`, alone.
    fn insert(&mut self, key: &[u8], value: &[u8]) -> Result<RecordAddress, WalError> {
        self.append(Record::Insert { key, value })
    }

    /// Appends a removal of `key`, alone.
    fn remove(&mut self, key: &[u8]) -> Result<RecordAddress, WalError> {
        self.append(Record::Remove { key })
    }

    fn append(&mut self, record: Record<'_>) -> Result<RecordAddress, WalError> {
        block_on(self.writer.append(&[record])).map(|addresses| addresses[0])
    }
}

#[test]
fn an_open_replays_what_was_appended() {
    let storage = MemoryStorage::<10_000>::default();
    let mut wal = Opened::open(storage.clone()).unwrap();
    assert!(wal.replayed.is_empty());
    let records = [
        Record::Insert {
            key: b"a",
            value: b"1",
        },
        Record::Insert {
            key: b"b",
            value: b"",
        },
    ];
    let mut addresses = block_on(wal.writer.append(&records)).unwrap();
    addresses.push(wal.remove(b"a").unwrap());
    assert!(block_on(wal.writer.append(&[])).unwrap().is_empty());

    let reopened = Opened::open(storage.clone()).unwrap();
    let expected = [
        Replayed::insert(b"a", b"1"),
        Replayed::insert(b"b", b""),
        Replayed::Remove(b"a".to_vec()),
    ];
    assert_eq!(reopened.replayed, expected);
    assert_eq!(reopened.addresses, addresses);

    assert_eq!(block_on(addresses[0].read_value(&storage)).unwrap(), b"1");
    assert_eq!(block_on(addresses[1].read_value(&storage)).unwrap(), b"");
    // A removal has no value.
    let read = block_on(addresses[2].read_value(&storage));
    assert!(matches!(read, Err(WalError::Corrupted { .. })), "{read:?}");
}

#[test]
fn a_full_file_starts_the_next() {
    // Two appends a file: the open's claim and one insert fill file 0.
    let storage = MemoryStorage::<2>::default();
    let mut wal = Opened::open(storage.clone()).unwrap();
    let mut expected = Vec::new();
    for i in 0..5u8 {
        wal.insert(&[i], &[i, i]).unwrap();
        expected.push(Replayed::insert(&[i], &[i, i]));
    }
    assert!(block_on(storage.len(2)).unwrap().is_some());
    assert!(block_on(storage.len(3)).unwrap().is_none());

    // File 2 is full: this open's claim starts file 3.
    let mut wal = Opened::open(storage.clone()).unwrap();
    assert_eq!(wal.replayed, expected);
    assert!(block_on(storage.len(3)).unwrap().is_some());
    let address = wal.remove(&[0]).unwrap();
    expected.push(Replayed::Remove(vec![0]));

    let reopened = Opened::open(storage).unwrap();
    assert_eq!(reopened.replayed, expected);
    assert_eq!(reopened.addresses.last(), Some(&address));
}

#[test]
fn a_lost_outcome_is_written_once() {
    for fault in [AppendFault::RequestLost, AppendFault::ResponseLost] {
        let storage = MemoryStorage::<10_000>::default();
        let mut wal = Opened::open(storage.clone()).unwrap();
        storage.fail_append(fault);
        wal.insert(b"a", b"1").unwrap();
        wal.insert(b"b", b"2").unwrap();

        let expected = [Replayed::insert(b"a", b"1"), Replayed::insert(b"b", b"2")];
        assert_eq!(
            Opened::open(storage).unwrap().replayed,
            expected,
            "{fault:?}"
        );
    }
}

#[test]
fn unknown_outcomes_fail_the_append() {
    let storage = MemoryStorage::<10_000>::default();
    let mut wal = Opened::open(storage.clone()).unwrap();
    for _ in 0..3 {
        storage.fail_append(AppendFault::RequestLost);
    }
    let failed = wal.insert(b"a", b"1");
    assert!(matches!(failed, Err(WalError::Storage(_))), "{failed:?}");
    // It wasn't written: the next append goes where it would have.
    wal.insert(b"b", b"2").unwrap();

    // A response lost three times: the first attempt was written, and the
    // next append's offset mismatches.
    for _ in 0..3 {
        storage.fail_append(AppendFault::ResponseLost);
    }
    let failed = wal.insert(b"c", b"3");
    assert!(matches!(failed, Err(WalError::Storage(_))), "{failed:?}");
    let moved = wal.insert(b"d", b"4");
    assert!(matches!(moved, Err(WalError::TailMoved)), "{moved:?}");

    let expected = [Replayed::insert(b"b", b"2"), Replayed::insert(b"c", b"3")];
    assert_eq!(Opened::open(storage).unwrap().replayed, expected);
}

#[test]
fn a_new_writer_fences_the_one_before() {
    let storage = MemoryStorage::<10_000>::default();
    let mut old = Opened::open(storage.clone()).unwrap();
    old.insert(b"a", b"1").unwrap();

    let mut new = Opened::open(storage.clone()).unwrap();
    assert_eq!(new.replayed, [Replayed::insert(b"a", b"1")]);
    let moved = old.insert(b"b", b"2");
    assert!(matches!(moved, Err(WalError::TailMoved)), "{moved:?}");
    new.insert(b"c", b"3").unwrap();

    let expected = [Replayed::insert(b"a", b"1"), Replayed::insert(b"c", b"3")];
    assert_eq!(Opened::open(storage).unwrap().replayed, expected);
}

#[test]
fn corruption_fails_the_open() {
    let storage = MemoryStorage::<10_000>::default();
    let mut wal = Opened::open(storage.clone()).unwrap();
    wal.insert(b"a", b"1").unwrap();
    let end = block_on(storage.len(0)).unwrap().unwrap();
    block_on(storage.append(0, end, b"not a record")).unwrap();
    let corrupted = Opened::open(storage).err();
    assert!(
        matches!(corrupted, Some(WalError::Corrupted { file: 0, offset }) if offset == end),
        "{corrupted:?}"
    );

    // A file of another format version.
    let storage = MemoryStorage::<10_000>::default();
    block_on(storage.append(0, 0, b"bbtreeWL\x02\0\0\0")).unwrap();
    let corrupted = Opened::open(storage).err();
    assert!(
        matches!(corrupted, Some(WalError::Corrupted { file: 0, offset: 0 })),
        "{corrupted:?}"
    );
}
