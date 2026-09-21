//! Datasets of one key per line, and the keys that a one-map cell takes from
//! them.
//!
//! A tenth of the keys are held out: maps never hold them, and lookups that
//! miss probe with them. They are chosen by a hash of their bytes, so the
//! choice is the same at every size and doesn't depend on the file's order.
//!
//! The files hold up to 26M keys, so taking a cell's keys runs on all cores
//! (`parallel_setup.rs`), with the same result as on one.

use rayon::prelude::*;
use siphasher::sip::SipHasher13;

use super::{DataDirectory, DataFile};
use crate::command_line::Named;
use crate::operations::SingleMapKeys;
use crate::parallel_setup::ParallelSetup;
use crate::random::{RandomStream, SeededRng};
use crate::workload::{MapSize, Scale};

/// A dataset file of one distinct key per line.
pub(crate) struct KeyList {
    file: DataFile,
    contents: String,
}

impl KeyList {
    /// One key in this many is held out.
    const HOLD_OUT_ONE_IN: u64 = 10;

    pub(crate) fn load(directory: &DataDirectory, file: DataFile) -> Result<Self, String> {
        Ok(Self {
            file,
            contents: directory.read(file)?,
        })
    }

    /// The keys of one map of `size`, and the held-out keys that its lookups
    /// that miss probe with.
    ///
    /// - The map holds a seeded sample of the keys that aren't held out, or
    ///   all of them at size `all` (a tenth of them under `--quick`). The
    ///   samples nest: a smaller map's keys are among a larger one's.
    /// - Misses are a seeded sample of the held-out keys, as many as the map
    ///   has entries or as the cell's lookups, whichever is fewer.
    pub(crate) fn single_map_keys(
        &self,
        size: MapSize,
        scale: Scale,
    ) -> Result<SingleMapKeys<String>, String> {
        let (mut in_set, mut held_out): (Vec<&str>, Vec<&str>) =
            (self.contents.par_lines()).partition(|key| !Self::is_held_out(key));
        // Sorted first, so that the samples don't depend on the file's order.
        ParallelSetup::sort_unstable_by(&mut in_set, Ord::cmp);
        ParallelSetup::sort_unstable_by(&mut held_out, Ord::cmp);
        let entries = match size {
            MapSize::All => scale.shrink(in_set.len()),
            _ => (size.fixed_shape(scale))
                .filter(|shape| shape.map_count() == 1)
                .ok_or_else(|| format!("key lists make one map, not {}", size.name()))?
                .entries_per_map(),
        };
        if entries > in_set.len() {
            return Err(format!(
                "{} has {} keys that aren't held out, fewer than a {} map holds",
                self.file.relative_path(),
                in_set.len(),
                size.name()
            ));
        }
        let miss_count = (entries.min(scale.operation_budget())).min(held_out.len());
        Ok(SingleMapKeys::new(
            Self::sample(&in_set, entries),
            Self::sample(&held_out, miss_count),
        ))
    }

    /// Whether `key` is held out: whether a fixed-key SipHash-1-3 of its
    /// bytes is a multiple of `HOLD_OUT_ONE_IN`.
    fn is_held_out(key: &str) -> bool {
        SipHasher13::new()
            .hash(key.as_bytes())
            .is_multiple_of(Self::HOLD_OUT_ONE_IN)
    }

    /// `count` of `keys`: a seeded sample, or all of them, in order. The
    /// first steps of the sampling shuffle don't depend on `count`, so a
    /// smaller sample is the start of a larger one.
    fn sample(keys: &[&str], count: usize) -> Vec<String> {
        let chosen: Vec<usize> = if count < keys.len() {
            SeededRng::new(RandomStream::DatasetSample).choose_indices(keys.len(), count)
        } else {
            (0..count).collect()
        };
        ParallelSetup::map(&chosen, |_, &index| keys[index].to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_list(keys: &[String]) -> KeyList {
        KeyList {
            file: DataFile::CrateNames,
            contents: keys.join("\n") + "\n",
        }
    }

    fn crate_like_names(count: usize) -> Vec<String> {
        (0..count)
            .map(|index| format!("crate-{index:06}"))
            .collect()
    }

    #[test]
    fn a_tenth_of_the_keys_are_held_out_by_a_pinned_hash() {
        let names = crate_like_names(100_000);
        let held_out = names.iter().filter(|key| KeyList::is_held_out(key)).count();
        assert!((9_500..=10_500).contains(&held_out), "{held_out} held out");
        // SipHash-1-3 with zero keys: these change only if the hash does.
        assert_eq!(SipHasher13::new().hash(b""), 0xd1fb_a762_150c_532c);
        assert_eq!(SipHasher13::new().hash(b"serde"), 0xe88e_ff3c_6215_a6f9);
    }

    #[test]
    fn maps_hold_no_held_out_key_and_misses_only_held_out_keys() {
        let keys = key_list(&crate_like_names(20_000))
            .single_map_keys(MapSize::OneThousand, Scale::Full)
            .unwrap();
        assert_eq!(keys.present().len(), 1_000);
        assert_eq!(keys.absent().len(), 1_000);
        assert!(keys.present().iter().all(|key| !KeyList::is_held_out(key)));
        assert!(keys.absent().iter().all(|key| KeyList::is_held_out(key)));
    }

    #[test]
    fn all_holds_every_key_that_isnt_held_out() {
        let names = crate_like_names(20_000);
        let in_set = names
            .iter()
            .filter(|key| !KeyList::is_held_out(key))
            .count();
        let keys = key_list(&names)
            .single_map_keys(MapSize::All, Scale::Full)
            .unwrap();
        assert_eq!(keys.present().len(), in_set);
        assert_eq!(keys.absent().len(), names.len() - in_set);
    }

    #[test]
    fn keys_dont_depend_on_the_files_order() {
        let mut names = crate_like_names(20_000);
        let sorted = key_list(&names).single_map_keys(MapSize::OneThousand, Scale::Full);
        names.reverse();
        let reversed = key_list(&names).single_map_keys(MapSize::OneThousand, Scale::Full);
        let (sorted, reversed) = (sorted.unwrap(), reversed.unwrap());
        assert_eq!(sorted.present(), reversed.present());
        assert_eq!(sorted.absent(), reversed.absent());
    }

    #[test]
    fn smaller_samples_are_the_start_of_larger_ones() {
        let names = crate_like_names(1_000);
        let keys: Vec<&str> = names.iter().map(String::as_str).collect();
        let small = KeyList::sample(&keys, 10);
        let large = KeyList::sample(&keys, 100);
        assert_eq!(small[..], large[..10]);
    }

    #[test]
    fn sizes_larger_than_the_dataset_are_refused() {
        let keys =
            key_list(&crate_like_names(500)).single_map_keys(MapSize::OneThousand, Scale::Full);
        assert!(keys.is_err());
    }
}
