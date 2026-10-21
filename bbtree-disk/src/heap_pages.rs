//! [`HeapPages`]: what a [`BBTreeMapFile`](crate::BBTreeMapFile) keeps after
//! each leaf (`design/15-index-file.md`): the bytes of its keys and values
//! of any length ([`VarBytes`](crate::VarBytes)), in heap pages, as a
//! store's leaf keeps its records in a chain of its own (`bbtree-store`).
//!
//! - **Heap pages** are the map's node config's (`HEAP_PAGE_SIZE`, the
//!   leaves' size by default), right after their leaf, so that a lookup
//!   reads the leaf and its bytes from one place of the file.
//! - **A record** is an entry's key's bytes, then its value's, whichever
//!   of them a span keeps, in key order, the head's first.
//! - **A record stays in one page,** starting the next if it doesn't fit in
//!   what is left of one; one larger than a page starts on a page and runs
//!   on over the next.

use bbtree::InlineKeyStrategy;
use bbtree::file::{LeafBytes, WriterLeaf};

use crate::disk_type::DiskStored;
use crate::var_bytes::VarBytes;

/// The heap pages after each leaf: the [`LeafBytes`] of a
/// `BBTreeMapFile`'s writer.
pub(crate) struct HeapPages;

impl HeapPages {
    /// Lays out the records of `leaf`'s entries in its heap pages, calling
    /// `place` for each key and value a span keeps, with its slot, its
    /// bytes' offset from the first page, and its bytes; returns how many
    /// bytes the pages take.
    fn lay_out<K: DiskStored, V: DiskStored, S: InlineKeyStrategy<K::InWriter>>(
        leaf: &WriterLeaf<'_, K, V, S>,
        mut place: impl FnMut(usize, usize, &[u8]),
    ) -> usize {
        let page = leaf.heap_page_size();
        let mut top = 0;
        for entry in leaf.entries() {
            let key = entry.key().map_or(&[][..], K::leaf_bytes);
            let value = V::leaf_bytes(entry.value());
            let size = key.len() + value.len();
            if size > page - top % page {
                top = top.next_multiple_of(page);
            }
            if K::HAS_BYTES {
                let slot = entry.key_slot().expect("keys with bytes have inline keys");
                place(slot, top, key);
            }
            if V::HAS_BYTES {
                place(entry.value_slot(), top + key.len(), value);
            }
            top += size;
        }
        top.next_multiple_of(page)
    }
}

// SAFETY: every slot of a type with bytes gets a span to its bytes, which
// `write` copies into the leaf's heap pages, after the page, as long as
// `len` says: `VarBytes::check` finds them in the file map.
unsafe impl<K: DiskStored, V: DiskStored> LeafBytes<K, V> for HeapPages {
    fn len<S: InlineKeyStrategy<K::InWriter>>(leaf: &WriterLeaf<'_, K, V, S>) -> usize {
        Self::lay_out(leaf, |_, _, _| {})
    }

    fn write<S: InlineKeyStrategy<K::InWriter>>(
        leaf: &WriterLeaf<'_, K, V, S>,
        page: &mut [u8],
        bytes: &mut [u8],
    ) {
        let page_len = page.len();
        Self::lay_out(leaf, |slot, at, record| {
            let offset = u32::try_from(page_len + at - slot)
                .expect("a leaf's heap pages within 4 GiB of its slots");
            let len = u32::try_from(record.len()).expect("a key or value under 4 GiB");
            page[slot..slot + size_of::<VarBytes>()]
                .copy_from_slice(&VarBytes::encode(len, offset));
            bytes[at..at + record.len()].copy_from_slice(record);
        });
    }
}
