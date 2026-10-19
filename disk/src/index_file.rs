//! Maps of keys and values of any length in a file: a file map of byte keys
//! and values (`VarBytes`), read and written as the caller's types through
//! [`ToBytes`] and [`FromBytes`].

use std::borrow::{Borrow, Cow};
use std::io;
use std::marker::PhantomData;
use std::ops::{Bound, RangeBounds};
use std::path::Path;

use bbtree::WithInlineKey;
use bbtree::file::{FileWriter, VarBytes, VarBytesBuf};

use crate::{FromBytes, MappedFile, OsPages, ToBytes};

/// The file map an index file holds: keys and values as bytes, in the
/// leaves' tails, searched by their inline keys.
type VarBytesFile = MappedFile<VarBytes, VarBytes, WithInlineKey>;

/// A map from `K` to `V` in a file, keys and values of any length: written
/// once from a map's entries, then mapped into memory, or read into it, and
/// looked up where it lies. Keys and values go in as their bytes
/// ([`ToBytes`]) and come out as views of the file's ([`FromBytes`]).
///
/// # Examples
///
/// ```
/// use std::collections::BTreeMap;
///
/// use bbtree_disk::IndexFile;
///
/// let path = std::env::temp_dir().join(format!("bbtree-disk-doc-{}", std::process::id()));
/// let names = BTreeMap::from([(7_u64, String::from("ferris")), (42, String::from("corro"))]);
/// IndexFile::create(&path, &names)?;
///
/// let file = IndexFile::<u64, String>::read(&path)?;
/// assert_eq!(file.get(&42), Some("corro"));
/// assert_eq!(file.iter().next(), Some((7, "ferris")));
/// # std::fs::remove_file(&path)?;
/// # Ok::<(), std::io::Error>(())
/// ```
pub struct IndexFile<K, V> {
    file: VarBytesFile,
    _types: PhantomData<fn() -> (K, V)>,
}

impl<K: ToBytes + FromBytes, V: ToBytes + FromBytes> IndexFile<K, V> {
    /// Writes the map of `entries` to the file at `path`, creating it or
    /// truncating it. Their keys must ascend, as a map's entries do; an
    /// error of kind `InvalidInput` if not.
    pub fn create<'e>(
        path: impl AsRef<Path>,
        entries: impl IntoIterator<Item = (&'e K, &'e V)>,
    ) -> io::Result<()>
    where
        K: 'e,
        V: 'e,
    {
        let owned = |bytes: &dyn AsRef<[u8]>| Cow::Owned(bytes.as_ref().to_vec());
        let entries = (entries.into_iter())
            .map(|(key, value)| (owned(&key.to_bytes()), owned(&value.to_bytes())));
        let writer = FileWriter::<VarBytes, VarBytes, WithInlineKey>::from_ascending(entries)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        VarBytesFile::create(path, &writer)
    }

    /// The map in the file at `path`, mapped into memory in `pages`; see
    /// [`MappedFile::map`].
    ///
    /// # Safety
    ///
    /// The file holds what [`Self::create`] wrote, and nothing changes or
    /// truncates it while it is mapped.
    pub unsafe fn mmap(path: impl AsRef<Path>, pages: OsPages) -> io::Result<Self> {
        Ok(Self {
            // SAFETY: the caller's.
            file: unsafe { VarBytesFile::mmap_path(path, pages)? },
            _types: PhantomData,
        })
    }

    /// The map in the file at `path`, read into memory and checked whole;
    /// see [`MappedFile::read`].
    pub fn read(path: impl AsRef<Path>) -> io::Result<Self> {
        Ok(Self {
            file: VarBytesFile::read_path(path)?,
            _types: PhantomData,
        })
    }

    /// The number of entries.
    pub fn len(&self) -> usize {
        self.file.map().len()
    }

    /// Whether there are no entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The value of the entry whose key is `key`, if any.
    pub fn get<Q: ToBytes + ?Sized>(&self, key: &Q) -> Option<V::View<'_>>
    where
        K: Borrow<Q>,
    {
        // Written in place, not moved: see `VarBytesBuf::new`.
        let mut query = VarBytesBuf::empty();
        query.set(key.to_bytes().as_ref());
        let value = self.file.map().get(&*query)?;
        Some(V::from_bytes(value.bytes()))
    }

    /// The entry whose key is `key`, if any.
    pub fn get_key_value<Q: ToBytes + ?Sized>(&self, key: &Q) -> Option<(K::View<'_>, V::View<'_>)>
    where
        K: Borrow<Q>,
    {
        // Written in place, not moved: see `VarBytesBuf::new`.
        let mut query = VarBytesBuf::empty();
        query.set(key.to_bytes().as_ref());
        let (key, value) = self.file.map().get_key_value(&*query)?;
        Some((K::from_bytes(key.bytes()), V::from_bytes(value.bytes())))
    }

    /// The entries, in key order.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (K::View<'_>, V::View<'_>)> {
        (self.file.map().iter())
            .map(|(key, value)| (K::from_bytes(key.bytes()), V::from_bytes(value.bytes())))
    }

    /// The entries whose keys lie in `range`, in key order.
    ///
    /// # Panics
    ///
    /// If the range's start is past its end, as a map's ranges do.
    pub fn range<Q, R>(
        &self,
        range: R,
    ) -> impl DoubleEndedIterator<Item = (K::View<'_>, V::View<'_>)>
    where
        K: Borrow<Q>,
        Q: ToBytes + ?Sized,
        R: RangeBounds<Q>,
    {
        let query = |bound: Bound<&Q>| bound.map(|key| VarBytesBuf::new(key.to_bytes().as_ref()));
        let (start, end) = (query(range.start_bound()), query(range.end_bound()));
        let bounds = (
            start.as_ref().map(|key| &**key),
            end.as_ref().map(|key| &**key),
        );
        (self.file.map().range::<VarBytes, _>(bounds))
            .map(|(key, value)| (K::from_bytes(key.bytes()), V::from_bytes(value.bytes())))
    }
}
