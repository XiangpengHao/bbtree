//! File maps in files, for [`BBTreeMapFile`](crate::BBTreeMapFile):
//! written once, then mapped into memory and used where they lie, or read
//! into it.

use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::ops::Range;
use std::path::Path;

use bbtree::file::{FileBuf, FileMap, FileWriter, Storable};
use bbtree::{Auto, InlineKeyStrategy};
use memmap2::{Mmap, MmapOptions};

/// A file map with keys `K`, values `V` and strategy `S`, from a file:
/// mapped into memory, so that lookups read the pages they reach from the
/// file as they first touch them, or read into memory whole. [`Self::map`]
/// is the map it holds.
///
/// A file map may be the whole file, or a range of it, as one embedded in a
/// Parquet file is. A range must start at a multiple of the file map's
/// alignment ([`FileMap::alignment`]), as [`Self::write_at`] places it.
pub(crate) struct MappedFile<K, V, S: InlineKeyStrategy<K> = Auto> {
    /// The map in `bytes`, opened once. It borrows `bytes` for as long as
    /// `self` lives, which no lifetime can name: [`Self::map`] lends it
    /// for as long as `self` is borrowed. Neither a mapping nor a buffer
    /// moves its bytes when `self` moves.
    map: FileMap<'static, K, V, S>,
    _bytes: FileMapBytes,
}

/// The OS pages a mapped file map comes into memory in: a choice between
/// the lookups of a cold file map and those of a hot one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OsPages {
    /// The system's own, 4 KiB on x86-64: a lookup that reaches a page not
    /// in memory reads a few KiB of the file. Once the file map is in memory,
    /// its lookups miss the TLB more often than a map's, and take a few
    /// percent longer.
    Small,
    /// 2 MiB pages, on Linux, which advises them (`MADV_HUGEPAGE`): once
    /// the file map is in memory, its lookups take a map's time, but a lookup
    /// that reaches a page not in memory reads 2 MiB of the file. Only the
    /// pages a lookup reads from the file through the mapping come so; an
    /// file map the page cache holds already, say from being written, stays in
    /// the pages it is in. Elsewhere, as [`Self::Small`].
    Huge,
}

/// Where a file map's bytes are: owned for the map that borrows them, which
/// is all that reads them.
#[expect(dead_code, reason = "kept for `MappedFile::map` to borrow, never read")]
enum FileMapBytes {
    Mapped(Mmap),
    Read(FileBuf),
}

impl<K: Storable, V: Storable, S: InlineKeyStrategy<K>> MappedFile<K, V, S> {
    /// Writes `writer`'s file map to the file at `path`, creating it, or
    /// truncating what it held.
    pub(crate) fn create(path: impl AsRef<Path>, writer: &FileWriter<K, V, S>) -> io::Result<()>
    where
        S: InlineKeyStrategy<K::InWriter>,
    {
        let mut file = File::create(path)?;
        Self::write_at(&mut file, 0, writer)?;
        Ok(())
    }

    /// Writes `writer`'s file map into `file` at `offset`, after zeros up to
    /// the next multiple of its alignment, in place of what lay there, and
    /// ends the file after it. Returns where it lies: the range
    /// [`Self::mmap`] and [`Self::read`] take.
    pub(crate) fn write_at(
        file: &mut File,
        offset: u64,
        writer: &FileWriter<K, V, S>,
    ) -> io::Result<Range<u64>>
    where
        S: InlineKeyStrategy<K::InWriter>,
    {
        let start = offset.next_multiple_of(FileMap::<K, V, S>::alignment() as u64);
        let end = start + writer.byte_len() as u64;
        file.set_len(offset)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut out = BufWriter::with_capacity(1 << 20, &mut *file);
        io::copy(&mut io::repeat(0).take(start - offset), &mut out)?;
        writer.write(|bytes| out.write_all(bytes))?;
        out.flush()?;
        Ok(start..end)
    }

    /// The file map in `range` of `file`, mapped into memory in `pages`.
    /// Checks its header alone, so it reads nothing of the file but the
    /// first page: lookups read the rest as they reach it.
    ///
    /// # Safety
    ///
    /// The range holds a file map an [`FileWriter`] wrote, unchanged, and
    /// nothing changes or truncates the file while it is mapped: as for
    /// [`FileMap::open_unchecked`], and for any file mapped into memory.
    pub(crate) unsafe fn mmap(file: &File, range: Range<u64>, pages: OsPages) -> io::Result<Self> {
        let len = usize::try_from(range.end - range.start).map_err(io::Error::other)?;
        // SAFETY: the caller's: nothing changes the file while it is mapped.
        let mapping = unsafe { MmapOptions::new().offset(range.start).len(len).map(file)? };
        // Advice only: when it is not taken, the pages are small.
        #[cfg(target_os = "linux")]
        if pages == OsPages::Huge {
            let _ = mapping.advise(memmap2::Advice::HugePage);
        }
        // SAFETY: the mapping holds what a writer wrote (caller), and stays
        // unchanged and in place for as long as `self` owns it.
        let map = unsafe { FileMap::open_unchecked(Self::forever(&mapping)) }
            .map_err(io::Error::other)?;
        Ok(Self {
            map,
            _bytes: FileMapBytes::Mapped(mapping),
        })
    }

    /// The file map in `range` of `file`, read into memory, after checking
    /// every page ([`FileMap::open`]).
    pub(crate) fn read(file: &mut File, range: Range<u64>) -> io::Result<Self> {
        let len = usize::try_from(range.end - range.start).map_err(io::Error::other)?;
        let mut buffer = FileBuf::zeroed(len, FileMap::<K, V, S>::alignment());
        file.seek(SeekFrom::Start(range.start))?;
        file.read_exact(&mut buffer)?;
        // SAFETY: the buffer stays unchanged and in place for as long as
        // `self` owns it.
        let map = FileMap::open(unsafe { Self::forever(&buffer) }).map_err(io::Error::other)?;
        Ok(Self {
            map,
            _bytes: FileMapBytes::Read(buffer),
        })
    }

    /// The whole file at `path`, a file map, read into memory; see
    /// [`Self::read`].
    pub(crate) fn read_path(path: impl AsRef<Path>) -> io::Result<Self> {
        let mut file = File::open(path)?;
        let len = file.metadata()?.len();
        Self::read(&mut file, 0..len)
    }

    /// The map the file map holds.
    pub(crate) fn map(&self) -> &FileMap<'_, K, V, S> {
        &self.map
    }

    /// `bytes`, for as long as the caller says.
    ///
    /// # Safety
    ///
    /// The bytes stay allocated, unchanged and in place for as long as the
    /// caller uses them: as `self.map` does those of `self._bytes`.
    unsafe fn forever(bytes: &[u8]) -> &'static [u8] {
        // SAFETY: the caller's.
        unsafe { std::slice::from_raw_parts(bytes.as_ptr(), bytes.len()) }
    }
}
