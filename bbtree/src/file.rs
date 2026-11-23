//! File maps (`design/15-index-file.md`, D58): a map compacted into one byte
//! string, its tree's pages one after another, which a reader maps from a
//! file, or reads into memory, and uses where it lies. Opening one builds
//! nothing: it reads the header, and checks each page once, or trusts them.
//! Its lookups run the map's own code, at the map's speed.
//!
//! - [`FileWriter`] compacts a map, or entries in ascending order, into
//!   full nodes, and writes them out.
//! - [`FileMap`] reads a file map: lookups, ranges and iteration, by shared
//!   reference.
//! - [`Storable`] is what keys and values may be: [`PlainBytes`], kept in
//!   the leaves as they are; or a type whose slot points to bytes that lie
//!   right after its leaf ([`LeafBytes`]), which the code that keeps them
//!   lays out and checks: bbtree-mmap's `VarBytes`, bytes of any length in
//!   heap pages.
//! - [`FileBuf`] holds a file map in memory, aligned for a reader.
//!
//! A file map is for one layout: this format of bbtree's, the same key and
//! value types and strategy, pointer width and byte order. A reader of
//! another refuses it ([`FileError::LayoutMismatch`]). Its bytes must start
//! at an address aligned to [`FileMap::alignment`], a few KiB at most, as
//! a file mapped from its start is, or from an offset that is a multiple of
//! it.
//!
//! # Examples
//!
//! ```
//! use bbtree::file::{FileMap, FileWriter};
//! use bbtree::unsync::BBTreeMap;
//!
//! let map: BBTreeMap<u64, u64> = (0..1000).map(|key| (key, key * 2)).collect();
//! let bytes = FileWriter::from(&map).to_buf();
//!
//! let read = FileMap::<u64, u64>::open(&bytes)?;
//! assert_eq!(read.get(&21), Some(&42));
//! assert_eq!(read.range(10..13).count(), 3);
//! # Ok::<(), bbtree::file::FileError>(())
//! ```

use core::alloc::Layout;
use core::borrow::Borrow;
use core::error::Error;
use core::fmt;
use core::iter::FusedIterator;
use core::marker::PhantomData;
use core::ops::{Deref, DerefMut, RangeBounds};
use core::ptr::NonNull;

pub use crate::tree::file::{
    FileError, InWriter, LeafBytes, NoLeafBytes, PlainBytes, Storable, WriterEntry, WriterLeaf,
};

use crate::allocator::Global;
use crate::strategy::{Auto, InlineKeyStrategy};
use crate::tree::build::TreeBuilder;
use crate::tree::file::{FileFormat, FileHeader, FilePages, InFile};
use crate::tree::root::Tree;
use crate::unsync::BBTreeMap;
use crate::unsync::map::{Iter, Range};

/// A map compacted for a file: its entries, in key order, in nodes that
/// are full but along the right border, ready to be written out
/// ([`Self::write`]), each leaf followed by what `L` keeps after it
/// ([`LeafBytes`]).
///
/// Made from a map of plain keys and values ([`From`]), or from entries in
/// ascending key order ([`Self::from_ascending`]), which it copies: what
/// its tree holds for each ([`InWriter`]), the values themselves for plain
/// types. It allocates the compacted map's nodes from the global
/// allocator, and frees them when dropped.
pub struct FileWriter<
    K: Storable,
    V: Storable,
    S: InlineKeyStrategy<K::InWriter> = Auto,
    L: LeafBytes<K, V> = NoLeafBytes,
> {
    tree: Tree<K::InWriter, V::InWriter, S>,
    length: usize,
    _leaf_bytes: PhantomData<fn() -> L>,
}

impl<K, V, S, L> FileWriter<K, V, S, L>
where
    K: Storable<InWriter: Ord>,
    V: Storable,
    S: InlineKeyStrategy<K::InWriter>,
    L: LeafBytes<K, V>,
{
    /// The compacted map of `entries`, whose keys must ascend strictly;
    /// [`NotAscending`] at the first that doesn't.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::file::FileWriter;
    ///
    /// let writer = FileWriter::<u64, u32>::from_ascending((0..100).map(|key| (key, 7)))?;
    /// assert_eq!(writer.len(), 100);
    /// assert!(FileWriter::<u64, u32>::from_ascending([(2, 0), (1, 0)]).is_err());
    /// # Ok::<(), bbtree::file::NotAscending>(())
    /// ```
    pub fn from_ascending(
        entries: impl IntoIterator<Item = (K::InWriter, V::InWriter)>,
    ) -> Result<Self, NotAscending> {
        let mut entries = AscendingEntries {
            entries: entries.into_iter(),
            previous: None,
            count: 0,
            out_of_order: false,
        };
        let writer = Self::build(&mut entries);
        if entries.out_of_order {
            return Err(NotAscending {
                index: entries.count,
            });
        }
        Ok(writer)
    }

    /// The compacted tree of `entries`, which must be in ascending key
    /// order: out of order, the file map's lookups give wrong answers, but
    /// stay memory-safe.
    fn build(entries: impl Iterator<Item = (K::InWriter, V::InWriter)>) -> Self {
        let mut builder = TreeBuilder::<K::InWriter, V::InWriter, S, Global>::new(Global);
        builder.extend(entries);
        let (tree, length) = builder.finish();
        Self {
            tree,
            length,
            _leaf_bytes: PhantomData,
        }
    }
}

impl<K, V, S, L> FileWriter<K, V, S, L>
where
    K: Storable,
    V: Storable,
    S: InlineKeyStrategy<K> + InlineKeyStrategy<K::InWriter>,
    L: LeafBytes<K, V>,
{
    /// The number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.length
    }

    /// Whether there are no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.length == 0
    }

    /// The file map's size in bytes: how many [`Self::write`] hands out.
    #[must_use]
    pub fn byte_len(&self) -> usize {
        FilePages::<K, V, S, L>::of(&self.tree, self.length).byte_len()
    }

    /// Hands the file map to `write`, in order, a header, a page, the bytes
    /// after a leaf, or the zeros between them at a time:
    /// [`Self::byte_len`] bytes in all. Stops at the first error `write`
    /// returns, and returns it.
    ///
    /// # Examples
    ///
    /// ```
    /// use bbtree::file::FileWriter;
    ///
    /// let writer = FileWriter::<u64, u64>::from_ascending((0..100).map(|key| (key, key)))?;
    /// let mut file_map = Vec::new();
    /// writer.write(|bytes| {
    ///     file_map.extend_from_slice(bytes);
    ///     Ok::<(), ()>(())
    /// });
    /// assert_eq!(file_map.len(), writer.byte_len());
    /// # Ok::<(), bbtree::file::NotAscending>(())
    /// ```
    pub fn write<E>(&self, mut write: impl FnMut(&[u8]) -> Result<(), E>) -> Result<(), E> {
        FilePages::<K, V, S, L>::of(&self.tree, self.length).write(&mut write)
    }

    /// The file map, in memory, aligned for [`FileMap::open`].
    #[must_use]
    pub fn to_buf(&self) -> FileBuf {
        let file_bytes = FilePages::<K, V, S, L>::of(&self.tree, self.length);
        let mut buffer = FileBuf::zeroed(file_bytes.byte_len(), FileFormat::<K, V, S>::alignment());
        let mut written = 0;
        file_bytes
            .write(&mut |bytes: &[u8]| {
                buffer[written..written + bytes.len()].copy_from_slice(bytes);
                written += bytes.len();
                Ok::<(), ()>(())
            })
            .expect("writing to memory doesn't fail");
        buffer
    }
}

impl<K: PlainBytes + Ord, V: PlainBytes, S: InlineKeyStrategy<K>> FileWriter<K, V, S> {
    /// The compacted map of a map's entries, in key order: copies of
    /// them in full nodes.
    fn of_map_entries(entries: impl Iterator<Item = (K, V)>) -> Self {
        Self::build(entries)
    }
}

impl<K, V, S, A> From<&BBTreeMap<K, V, S, A>> for FileWriter<K, V, S>
where
    K: PlainBytes + Ord,
    V: PlainBytes,
    S: InlineKeyStrategy<K>,
    A: crate::Allocator + Clone,
{
    /// The map, compacted: a copy of its entries in full nodes.
    fn from(map: &BBTreeMap<K, V, S, A>) -> Self {
        Self::of_map_entries(map.iter().map(|(&key, &value)| (key, value)))
    }
}

impl<K, V, S, A> From<&mut crate::BBTreeMap<K, V, S, A>> for FileWriter<K, V, S>
where
    K: PlainBytes + Ord,
    V: PlainBytes,
    S: InlineKeyStrategy<K>,
    A: crate::Allocator,
{
    /// The shared map, compacted, read by its owner: `&mut` keeps other
    /// threads out, so the file map holds exactly the map's entries. Of a
    /// map threads still change, [`Self::from_ascending`] takes
    /// `map.range_cloned(..)`, which is not a snapshot.
    fn from(map: &mut crate::BBTreeMap<K, V, S, A>) -> Self {
        Self::of_map_entries(map.iter_mut().map(|(&key, &mut value)| (key, value)))
    }
}

impl<K, V, S, L> Drop for FileWriter<K, V, S, L>
where
    K: Storable,
    V: Storable,
    S: InlineKeyStrategy<K::InWriter>,
    L: LeafBytes<K, V>,
{
    /// Takes the tree apart, before whatever its keys and values point into
    /// goes.
    fn drop(&mut self) {
        self.tree.take().destroy(&Global);
    }
}

/// Entries whose keys must ascend strictly: an iterator that ends at the
/// first that doesn't, and says so.
struct AscendingEntries<I, K> {
    entries: I,
    /// The last key handed out.
    previous: Option<K>,
    /// How many entries were handed out.
    count: usize,
    /// Whether the entries ended at one out of order.
    out_of_order: bool,
}

impl<I: Iterator<Item = (K, V)>, K: Ord + Copy, V> Iterator for AscendingEntries<I, K> {
    type Item = (K, V);

    fn next(&mut self) -> Option<(K, V)> {
        if self.out_of_order {
            return None;
        }
        let (key, value) = self.entries.next()?;
        if self.previous.is_some_and(|previous| previous >= key) {
            self.out_of_order = true;
            return None;
        }
        self.previous = Some(key);
        self.count += 1;
        Some((key, value))
    }
}

/// Entries given to [`FileWriter::from_ascending`] whose keys don't
/// ascend strictly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotAscending {
    index: usize,
}

impl NotAscending {
    /// The index of the first entry whose key is not greater than the key
    /// before it.
    #[must_use]
    pub fn index(&self) -> usize {
        self.index
    }
}

impl fmt::Display for NotAscending {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "entry {} has a key not greater than the key before it",
            self.index
        )
    }
}

impl Error for NotAscending {}

/// A map read from a file map: lookups, ranges and iteration, by shared
/// reference, on the file map's bytes where they lie, which it borrows.
///
/// Opening one reads only the header ([`Self::open_unchecked`]), or checks
/// every page too ([`Self::open`]). Its lookups run the code of
/// [`unsync::BBTreeMap`](crate::unsync::BBTreeMap) itself on the file map's
/// pages, adding each edge's offset to its node's address as they go down.
///
/// # Examples
///
/// ```
/// use bbtree::file::{FileMap, FileWriter};
///
/// let writer = FileWriter::<i64, [u8; 4]>::from_ascending((-50..50).map(|key| (key, [1; 4])))?;
/// let bytes = writer.to_buf();
/// let read = FileMap::<i64, [u8; 4]>::open(&bytes).unwrap();
/// assert_eq!(read.len(), 100);
/// assert_eq!(read.first_key_value(), Some((&-50, &[1; 4])));
/// # Ok::<(), bbtree::file::NotAscending>(())
/// ```
pub struct FileMap<'a, K, V, S = Auto> {
    tree: Tree<K, V, InFile<S>>,
    length: usize,
    _bytes: PhantomData<&'a [u8]>,
}

impl<'a, K: Storable, V: Storable, S: InlineKeyStrategy<K>> FileMap<'a, K, V, S> {
    /// What a file map's first byte must be aligned to, for this map's key
    /// and value types and strategy: the largest alignment of its pages,
    /// 1 KiB or more. With the default [`NodeConfig`](crate::NodeConfig), it
    /// is at most 4 KiB for keys and values aligned to no more than that.
    #[must_use]
    pub fn alignment() -> usize {
        FileFormat::<K, V, S>::alignment()
    }

    /// The map in `file_bytes`, after checking that `file_bytes` is one of this
    /// map's layout, as long as its header says and aligned
    /// ([`Self::open_unchecked`]), and that every page its tree reaches
    /// lies in it and is well formed: what the map relies on to stay in
    /// bounds. This reads every page once.
    ///
    /// A damaged file map that passes may still give wrong answers, or panic,
    /// but never undefined behavior.
    pub fn open(file_bytes: &'a [u8]) -> Result<Self, FileError> {
        let header = FileHeader::read::<K, V, S>(file_bytes)?;
        FileFormat::<K, V, S>::check(file_bytes, header)?;
        // SAFETY: the file map's header is of this map's layout, and every page
        // its tree reaches checked (just above); keys and values are plain,
        // and the borrow keeps the bytes unchanged.
        Ok(unsafe { Self::in_file(file_bytes, header) })
    }

    /// The map in `file_bytes`, after checking its header alone: that `file_bytes` is
    /// one of this map's layout, as long as the header says, aligned, and
    /// its root page within it. It reads no other page, so opening a mapped
    /// file reads none of it but its first page.
    ///
    /// # Safety
    ///
    /// `file_bytes` holds what a [`FileWriter`] wrote, unchanged, or what
    /// [`Self::open`] accepted once: a file map whose pages the map can trust
    /// to stay in bounds. For a file mapped into memory, nothing else
    /// changes it while it is mapped.
    pub unsafe fn open_unchecked(file_bytes: &'a [u8]) -> Result<Self, FileError> {
        let header = FileHeader::read::<K, V, S>(file_bytes)?;
        // SAFETY: the caller's.
        Ok(unsafe { Self::in_file(file_bytes, header) })
    }

    /// # Safety
    ///
    /// As for [`Tree::in_file`].
    unsafe fn in_file(file_bytes: &'a [u8], header: FileHeader) -> Self {
        Self {
            // SAFETY: forwarded.
            tree: unsafe { Tree::in_file(file_bytes, header) },
            length: header.length(),
            _bytes: PhantomData,
        }
    }

    /// The number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.length
    }

    /// Whether there are no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.length == 0
    }

    /// The value of the entry whose key is `key`, if there is one.
    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q> + Ord,
        Q: Ord + ?Sized,
    {
        self.get_key_value(key).map(|(_, value)| value)
    }

    /// The entry whose key is `key`, if there is one.
    pub fn get_key_value<Q>(&self, key: &Q) -> Option<(&K, &V)>
    where
        K: Borrow<Q> + Ord,
        Q: Ord + ?Sized,
    {
        self.tree.find(key)
    }

    /// Whether there is an entry whose key is `key`.
    pub fn contains_key<Q>(&self, key: &Q) -> bool
    where
        K: Borrow<Q> + Ord,
        Q: Ord + ?Sized,
    {
        self.get_key_value(key).is_some()
    }

    /// Each key's value, in input order, as separate calls to `get` would.
    /// Keys may repeat, and an empty batch returns an empty array. A batch
    /// overlaps its keys' waits for memory, not for the disk: the first read
    /// of a page of a mapped file faults, as it does for `get`.
    pub fn get_many<Q, const N: usize>(&self, keys: [&Q; N]) -> [Option<&V>; N]
    where
        K: Borrow<Q> + Ord,
        Q: Ord + ?Sized,
    {
        self.tree.find_many(keys, self.length)
    }

    /// The entry of the least key, if any.
    #[must_use]
    pub fn first_key_value(&self) -> Option<(&K, &V)> {
        self.tree.first_key_value()
    }

    /// The entry of the greatest key, if any.
    #[must_use]
    pub fn last_key_value(&self) -> Option<(&K, &V)> {
        self.tree.last_key_value()
    }

    /// The entries, in key order.
    pub fn iter(
        &self,
    ) -> impl DoubleEndedIterator<Item = (&K, &V)> + ExactSizeIterator + FusedIterator + Clone {
        Iter::of_tree(&self.tree, self.length)
    }

    /// The keys, in order.
    pub fn keys(
        &self,
    ) -> impl DoubleEndedIterator<Item = &K> + ExactSizeIterator + FusedIterator + Clone {
        self.iter().map(|(key, _)| key)
    }

    /// The values, in key order.
    pub fn values(
        &self,
    ) -> impl DoubleEndedIterator<Item = &V> + ExactSizeIterator + FusedIterator + Clone {
        self.iter().map(|(_, value)| value)
    }

    /// The entries whose keys lie in `range`, in key order. The iterator
    /// finds both ends as it is made, and keeps no borrow of `range`.
    ///
    /// # Panics
    ///
    /// As [`BBTreeMap::range`] does, if the range's start is past its end.
    pub fn range<T, R>(&self, range: R) -> FileMapRange<'_, K, V, S>
    where
        T: Ord + ?Sized,
        K: Borrow<T> + Ord,
        R: RangeBounds<T>,
    {
        FileMapRange {
            range: Range::of_tree(&self.tree, range),
        }
    }
}

/// An iterator over the entries of a [`FileMap`] whose keys lie in a
/// range, in key order, from [`FileMap::range`].
#[must_use = "iterators are lazy and do nothing unless consumed"]
pub struct FileMapRange<'m, K, V, S = Auto> {
    range: Range<'m, K, V, InFile<S>>,
}

impl<'m, K: 'm, V: 'm, S: InlineKeyStrategy<K>> Iterator for FileMapRange<'m, K, V, S> {
    type Item = (&'m K, &'m V);

    fn next(&mut self) -> Option<(&'m K, &'m V)> {
        self.range.next()
    }

    fn fold<A, F>(self, accumulator: A, fold: F) -> A
    where
        F: FnMut(A, Self::Item) -> A,
    {
        self.range.fold(accumulator, fold)
    }
}

impl<'m, K: 'm, V: 'm, S: InlineKeyStrategy<K>> DoubleEndedIterator for FileMapRange<'m, K, V, S> {
    fn next_back(&mut self) -> Option<(&'m K, &'m V)> {
        self.range.next_back()
    }

    fn rfold<A, F>(self, accumulator: A, fold: F) -> A
    where
        F: FnMut(A, Self::Item) -> A,
    {
        self.range.rfold(accumulator, fold)
    }
}

impl<K, V, S: InlineKeyStrategy<K>> FusedIterator for FileMapRange<'_, K, V, S> {}

impl<K, V, S> Clone for FileMapRange<'_, K, V, S> {
    fn clone(&self) -> Self {
        Self {
            range: self.range.clone(),
        }
    }
}

impl<K, V, S> fmt::Debug for FileMap<'_, K, V, S>
where
    K: Storable + fmt::Debug,
    V: Storable + fmt::Debug,
    S: InlineKeyStrategy<K>,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

/// Bytes in memory for a file map: one allocation, aligned as a file map needs
/// ([`FileMap::alignment`]), zeroed when made. What a file map read into
/// memory, rather than mapped, lives in.
pub struct FileBuf {
    bytes: NonNull<u8>,
    len: usize,
    layout: Layout,
}

// SAFETY: the buffer owns its bytes, as a `Box<[u8]>` does.
unsafe impl Send for FileBuf {}

// SAFETY: as for `Send`; `&FileBuf` gives only shared access.
unsafe impl Sync for FileBuf {}

impl FileBuf {
    /// `len` zeros, the first aligned to `alignment`.
    ///
    /// # Panics
    ///
    /// If `alignment` is not a power of two, or `len` rounded up to it
    /// overflows `isize`. Ends the program, as `Vec` does, if there is no
    /// memory to give.
    #[must_use]
    pub fn zeroed(len: usize, alignment: usize) -> Self {
        let layout = Layout::from_size_align(len.max(1), alignment)
            .expect("a file buffer's alignment is a power of two");
        let bytes = crate::Allocator::allocate_zeroed(&Global, layout)
            .unwrap_or_else(|_| alloc::alloc::handle_alloc_error(layout));
        Self { bytes, len, layout }
    }
}

impl Deref for FileBuf {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        // SAFETY: the allocation holds `len` initialized bytes, owned by the
        // buffer.
        unsafe { core::slice::from_raw_parts(self.bytes.as_ptr(), self.len) }
    }
}

impl DerefMut for FileBuf {
    fn deref_mut(&mut self) -> &mut [u8] {
        // SAFETY: as for `deref`, exclusive as `self` is.
        unsafe { core::slice::from_raw_parts_mut(self.bytes.as_ptr(), self.len) }
    }
}

impl Drop for FileBuf {
    fn drop(&mut self) {
        // SAFETY: the bytes came from the global allocator, for `layout`.
        unsafe { crate::Allocator::deallocate(&Global, self.bytes, self.layout) }
    }
}

impl fmt::Debug for FileBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileBuf")
            .field("len", &self.len)
            .field("alignment", &self.layout.align())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests;
