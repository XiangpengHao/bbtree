//! A bbtree map in a file: written once from a map's entries, then mapped
//! into memory, or read into it, and looked up where it lies. Each key and
//! value type keeps its own storage ([`DiskType`]).

use std::borrow::Borrow;
use std::fs::File;
use std::io;
use std::marker::PhantomData;
use std::ops::{Range, RangeBounds};
use std::path::{Path, PathBuf};

use bbtree::file::{FileWriter, InWriter};

use crate::disk_type::DiskStored;
use crate::disk_type::storage::DiskStorage;
use crate::heap_pages::HeapPages;
use crate::mapped_file::MappedFile;
use crate::var_bytes::ByteArena;
use crate::{DiskInput, DiskQuery, DiskType, OsPages};

/// A map from `K` to `V` in a file: written once from a map's entries, then
/// mapped into memory, or read into it, and looked up where it lies, with
/// the map's own code. Integers and other plain types stay as they are, in
/// their slots; text and byte strings keep their bytes in heap pages after
/// their leaves ([`DiskType`]). Lookups hand back views of the file: `&u64`,
/// `&str`, `&[u8]`.
///
/// It is read-only: a changed map is a new file.
///
/// # Examples
///
/// ```
/// use bbtree::unsync::BBTreeMap;
/// use bbtree_mmap::BBTreeMapFile;
///
/// let path = std::env::temp_dir().join(format!("bbtree-mmap-doc-{}", std::process::id()));
/// let names = BBTreeMap::from([(7_u64, String::from("ferris")), (42, String::from("corro"))]);
/// BBTreeMapFile::<u64, String>::create(&path, &names)?;
///
/// let file = BBTreeMapFile::<u64, String>::read(&path)?;
/// assert_eq!(file.get(&42), Some("corro"));
/// assert_eq!(file.iter().next(), Some((&7, "ferris")));
/// # std::fs::remove_file(&path)?;
/// # Ok::<(), std::io::Error>(())
/// ```
pub struct BBTreeMapFile<K: DiskType, V: DiskType> {
    file: MappedFile<K::Stored, V::Stored, K::Strategy>,
    _types: PhantomData<fn() -> (K, V)>,
}

impl<K, V> BBTreeMapFile<K, V>
where
    K: DiskType<Stored: Ord>,
    V: DiskType,
    InWriter<K::Stored>: Ord,
{
    /// Writes the map of `entries` to the file at `path`, creating it or
    /// truncating it. Their keys must ascend, as a map's entries do; an
    /// error of kind `InvalidInput` if not, before the file is touched.
    ///
    /// An entry's key and value are owned or borrowed ([`DiskInput`]):
    /// `&map` of an [`unsync::BBTreeMap`](bbtree::unsync::BBTreeMap) or
    /// std's map; `&mut map` of a [`BBTreeMap`](bbtree::BBTreeMap), by its
    /// owner, exactly its entries; or `map.range_cloned(..)` of one that
    /// threads still change, which is not a snapshot.
    pub fn create(
        path: impl AsRef<Path>,
        entries: impl IntoIterator<Item = (impl DiskInput<K::Stored>, impl DiskInput<V::Stored>)>,
    ) -> io::Result<()> {
        MappedFile::create(path, &Self::writer(entries)?)
    }

    /// Writes the map of `entries` into `file` at `offset`, rounded up to
    /// its alignment, in place of what lay there, and ends the file after
    /// it; returns where it lies, for [`MmapOptions::range`] and
    /// [`Self::read_range`]: a map embedded in a file of other data, as
    /// bbtree-datafusion-parquet embeds one in a Parquet file. Entries as
    /// for [`Self::create`]; `file` is untouched if they don't ascend.
    pub fn write_at(
        file: &mut File,
        offset: u64,
        entries: impl IntoIterator<Item = (impl DiskInput<K::Stored>, impl DiskInput<V::Stored>)>,
    ) -> io::Result<Range<u64>> {
        MappedFile::write_at(file, offset, &Self::writer(entries)?)
    }

    /// The compacted map of `entries`, in memory, ready to write.
    fn writer(
        entries: impl IntoIterator<Item = (impl DiskInput<K::Stored>, impl DiskInput<V::Stored>)>,
    ) -> io::Result<DiskWriter<K::Stored, V::Stored, K::Strategy>> {
        let mut arena = ByteArena::default();
        let inputs = (entries.into_iter()).map(|(key, value)| {
            (
                K::Stored::in_writer(key.into_input(), &mut arena),
                V::Stored::in_writer(value.into_input(), &mut arena),
            )
        });
        let writer = FileWriter::from_ascending(inputs)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        Ok(DiskWriter {
            writer,
            _arena: arena,
        })
    }

    /// How to map the file at `path` into memory: the whole file, in small
    /// OS pages, unless set otherwise; [`MmapOptions::open`] maps it.
    pub fn mmap(path: impl AsRef<Path>) -> MmapOptions<K, V> {
        MmapOptions {
            path: path.as_ref().to_path_buf(),
            range: None,
            pages: OsPages::Small,
            _types: PhantomData,
        }
    }

    /// The map in the file at `path`, read into memory and checked whole.
    pub fn read(path: impl AsRef<Path>) -> io::Result<Self> {
        Ok(Self::holding(MappedFile::read_path(path)?))
    }

    /// The map in `range` of `file`, as [`Self::write_at`] returned it, read
    /// into memory and checked whole.
    pub fn read_range(file: &mut File, range: Range<u64>) -> io::Result<Self> {
        Ok(Self::holding(MappedFile::read(file, range)?))
    }

    fn holding(file: MappedFile<K::Stored, V::Stored, K::Strategy>) -> Self {
        Self {
            file,
            _types: PhantomData,
        }
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
    pub fn get<Q>(&self, key: &Q) -> Option<V::View<'_>>
    where
        K: Borrow<Q>,
        Q: DiskQuery<Stored = K::Stored> + ?Sized,
    {
        let value = self.file.map().get::<K::Stored>(&key.query())?;
        Some(V::view(value))
    }

    /// The entry whose key is `key`, if any.
    pub fn get_key_value<Q>(&self, key: &Q) -> Option<(K::View<'_>, V::View<'_>)>
    where
        K: Borrow<Q>,
        Q: DiskQuery<Stored = K::Stored> + ?Sized,
    {
        let (key, value) = self.file.map().get_key_value::<K::Stored>(&key.query())?;
        Some((K::view(key), V::view(value)))
    }

    /// Each key's value, in input order, as separate calls to [`Self::get`]
    /// would, the keys' waits for memory overlapped; see
    /// [`FileMap::get_many`](bbtree::file::FileMap::get_many).
    pub fn get_many<Q, const N: usize>(&self, keys: [&Q; N]) -> [Option<V::View<'_>>; N]
    where
        K: Borrow<Q>,
        Q: DiskQuery<Stored = K::Stored> + ?Sized,
    {
        let queries = keys.map(|key| key.query());
        let keys = queries.each_ref().map(|query| &**query);
        (self.file.map().get_many::<K::Stored, N>(keys)).map(|value| value.map(V::view))
    }

    /// The entries, in key order.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = (K::View<'_>, V::View<'_>)> {
        (self.file.map().iter()).map(|(key, value)| (K::view(key), V::view(value)))
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
        Q: DiskQuery<Stored = K::Stored> + ?Sized,
        R: RangeBounds<Q>,
    {
        let (start, end) = (
            range.start_bound().map(|key| key.query()),
            range.end_bound().map(|key| key.query()),
        );
        // The range finds both ends as it is made, and keeps no borrow of
        // the queries.
        let bounds = (
            start.as_ref().map(|query| &**query),
            end.as_ref().map(|query| &**query),
        );
        (self.file.map().range::<K::Stored, _>(bounds))
            .map(|(key, value)| (K::view(key), V::view(value)))
    }
}

/// The compacted map of a `BBTreeMapFile`, ready to write: a file map's
/// writer, its leaves followed by heap pages, and the arena its tree's byte
/// strings point into, which outlives it.
pub(crate) struct DiskWriter<
    K: DiskStored,
    V: DiskStored,
    S: bbtree::InlineKeyStrategy<K::InWriter>,
> {
    /// Dropped first, before the bytes its tree points into.
    pub(crate) writer: FileWriter<K, V, S, HeapPages>,
    _arena: ByteArena,
}

/// How [`BBTreeMapFile::mmap`] maps a file into memory: the whole file or
/// the range of it a map lies in, in small or huge OS pages. The whole
/// file, in small pages, unless set.
///
/// # Examples
///
/// ```
/// use bbtree::unsync::BBTreeMap;
/// use bbtree_mmap::{BBTreeMapFile, OsPages};
///
/// let path = std::env::temp_dir().join(format!("bbtree-mmap-options-{}", std::process::id()));
/// let names = BBTreeMap::from([(7_u64, String::from("ferris"))]);
/// BBTreeMapFile::<u64, String>::create(&path, &names)?;
///
/// // SAFETY: nothing changes the file while it is mapped.
/// let names = unsafe { BBTreeMapFile::<u64, String>::mmap(&path).pages(OsPages::Huge).open()? };
/// assert_eq!(names.get(&7), Some("ferris"));
/// # drop(names);
/// # std::fs::remove_file(&path)?;
/// # Ok::<(), std::io::Error>(())
/// ```
#[must_use = "nothing is mapped until `open`"]
pub struct MmapOptions<K, V> {
    path: PathBuf,
    range: Option<Range<u64>>,
    pages: OsPages,
    _types: PhantomData<fn() -> (K, V)>,
}

impl<K, V> MmapOptions<K, V>
where
    K: DiskType<Stored: Ord>,
    V: DiskType,
    InWriter<K::Stored>: Ord,
{
    /// The OS pages to map the file in: [`OsPages::Small`] unless set.
    pub fn pages(mut self, pages: OsPages) -> Self {
        self.pages = pages;
        self
    }

    /// The range of the file the map lies in, as
    /// [`BBTreeMapFile::write_at`] returned it: the whole file unless set.
    pub fn range(mut self, range: Range<u64>) -> Self {
        self.range = Some(range);
        self
    }

    /// The map, mapped into memory: opening it reads its first page;
    /// lookups read the rest as they reach it.
    ///
    /// # Safety
    ///
    /// The file, or its range, holds what [`BBTreeMapFile::create`] or
    /// [`BBTreeMapFile::write_at`] wrote, and nothing changes or truncates
    /// the file while it is mapped.
    pub unsafe fn open(self) -> io::Result<BBTreeMapFile<K, V>> {
        let file = File::open(&self.path)?;
        let range = match self.range {
            Some(range) => range,
            None => 0..file.metadata()?.len(),
        };
        // SAFETY: the caller's.
        let mapped = unsafe { MappedFile::mmap(&file, range, self.pages)? };
        Ok(BBTreeMapFile::holding(mapped))
    }
}
