//! The real-world datasets: the manifest that pins each file, the directory
//! the files live in, and the commands that manage them: `verify-data`
//! hashes every file against the manifest, and `fetch` (`fetch.rs`)
//! installs them.
//!
//! Cells read the files through `key_list.rs` (one key per line) and
//! `json_objects.rs` (the objects of JSON documents). Before reading, a cell
//! checks only that the file exists with its pinned size: hashing 400 MB
//! would take longer than many cells do.

mod fetch;
mod json_objects;
mod key_list;

use std::fs::{self, File};
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

pub(crate) use fetch::DatasetFetch;
pub(crate) use json_objects::JsonObjectCorpus;
pub(crate) use key_list::KeyList;

/// A file of the manifest. Each dataset's directory also holds a
/// `SOURCE.md`, which says where the data came from, how it was derived and
/// what its keys look like.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DataFile {
    /// Every crate name on crates.io (database dump of 2026-10-07), sorted.
    CrateNames,
    /// The regular files of the Linux 6.12 source tree, sorted.
    KernelPaths,
    /// English Wikipedia's article titles (namespace 0, dump 20260901),
    /// sorted.
    WikiTitles,
    /// The keys of every object in six JSON documents from simdjson's
    /// examples: one object per line, its keys tab-separated, in document
    /// order.
    JsonObjectKeys,
    /// The distinct keys of `JsonObjectKeys`, each with its count.
    JsonKeyVocabulary,
    /// The distinct URLs of the pages in random blocks of the index of
    /// Common Crawl's crawl CC-MAIN-2023-14, sorted. Derived from the index
    /// by `fetch`.
    CommonCrawlUrls,
    /// The distinct addresses of France's Base Adresse Nationale (export of
    /// 2026-10-07), as its geocoder labels them, sorted.
    BanAddresses,
    /// The distinct names of GeoNames' places (dump of 2026-10-08), sorted.
    GeoNames,
    /// The distinct titles of dblp's publications (snapshot of 2026-10-03),
    /// as plain text, sorted.
    DblpTitles,
}

/// Where `fetch` gets a data file from. Either way, the manifest pins the
/// file's exact contents.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DataOrigin {
    /// The datasets' GitHub release.
    Release,
    /// Its upstream source, which never changes, but whose terms don't let
    /// the release redistribute the file: `fetch` derives it by running the
    /// dataset tool with `tool_arguments`, then the file's path.
    Upstream {
        tool_arguments: &'static [&'static str],
    },
}

/// What the manifest pins of a data file.
struct PinnedFile {
    /// Relative to the data directory.
    relative_path: &'static str,
    bytes: u64,
    sha256: &'static str,
}

impl DataFile {
    pub(crate) const ALL: [Self; 9] = [
        Self::CrateNames,
        Self::KernelPaths,
        Self::WikiTitles,
        Self::JsonObjectKeys,
        Self::JsonKeyVocabulary,
        Self::CommonCrawlUrls,
        Self::BanAddresses,
        Self::GeoNames,
        Self::DblpTitles,
    ];

    /// The manifest, from the datasets' `SOURCE.md` files.
    fn pinned(self) -> PinnedFile {
        match self {
            Self::CrateNames => PinnedFile {
                relative_path: "crate-names/crate-names.txt",
                bytes: 4_754_469,
                sha256: "fb27de8ec996495e450f92f77d053b5788d733d09237955b77513b954a2b0bcb",
            },
            Self::KernelPaths => PinnedFile {
                relative_path: "linux-paths/linux-6.12-paths.txt",
                bytes: 3_411_142,
                sha256: "1250c86563ead7b34d855f96cd753c725a51227e7859ba265f7dbfc63fb55c44",
            },
            Self::WikiTitles => PinnedFile {
                relative_path: "wiki-titles/enwiki-20260901-titles.txt",
                bytes: 423_316_037,
                sha256: "27997e84a87a3916795548922776dfaa8443cf64a38c6dc7263af2ac933ab63f",
            },
            Self::JsonObjectKeys => PinnedFile {
                relative_path: "json-objects/object-keys.tsv",
                bytes: 569_084,
                sha256: "aa2ba80a035d168b65fe8a9ddf7effdb667604ed62c2b9f77ddb9fac31f86c1e",
            },
            Self::JsonKeyVocabulary => PinnedFile {
                relative_path: "json-objects/key-vocabulary.tsv",
                bytes: 17_999,
                sha256: "fbd52a705ce03bf94012d3cf932f3065245354afc620217b3ebad907194b546c",
            },
            // TODO(phase 2): pin the derived files.
            Self::CommonCrawlUrls => PinnedFile {
                relative_path: "cc-urls/cc-main-2023-14-urls.txt",
                bytes: 0,
                sha256: "0000000000000000000000000000000000000000000000000000000000000000",
            },
            Self::BanAddresses => PinnedFile {
                relative_path: "ban-addresses/ban-2026-10-07-addresses.txt",
                bytes: 1_064_160_095,
                sha256: "18072059a36e95e2c10f5871da1d1494c606b487d27497cfabaa8704e7e361de",
            },
            Self::GeoNames => PinnedFile {
                relative_path: "geonames/geonames-2026-10-08-names.txt",
                bytes: 0,
                sha256: "0000000000000000000000000000000000000000000000000000000000000000",
            },
            Self::DblpTitles => PinnedFile {
                relative_path: "dblp-titles/dblp-2026-10-03-titles.txt",
                bytes: 684_074_119,
                sha256: "b9c22b15b3075aec603b1310f9ab7e9a32a22e094929a2f857e699184b5ce11c",
            },
        }
    }

    fn origin(self) -> DataOrigin {
        match self {
            Self::CrateNames
            | Self::KernelPaths
            | Self::WikiTitles
            | Self::JsonObjectKeys
            | Self::JsonKeyVocabulary
            | Self::BanAddresses
            | Self::GeoNames
            | Self::DblpTitles => DataOrigin::Release,
            Self::CommonCrawlUrls => DataOrigin::Upstream {
                tool_arguments: &[
                    "cc-urls",
                    "https://data.commoncrawl.org/cc-index/collections/CC-MAIN-2023-14/indexes/cluster.idx",
                    "https://data.commoncrawl.org/cc-index/collections/CC-MAIN-2023-14/indexes",
                    "4500",
                    "42",
                ],
            },
        }
    }

    /// The file's path relative to the data directory: its dataset's
    /// directory, then its name.
    pub(crate) fn relative_path(self) -> &'static str {
        self.pinned().relative_path
    }
}

/// The directory the datasets live in: `$BBTREE_BENCH_DATA`, or else
/// `$XDG_CACHE_HOME/bbtree-bench`, or else `~/.cache/bbtree-bench`.
pub(crate) struct DataDirectory {
    root: PathBuf,
}

impl DataDirectory {
    pub(crate) const VARIABLE: &str = "BBTREE_BENCH_DATA";

    pub(crate) fn from_environment() -> Result<Self, String> {
        let variable = |name| {
            std::env::var_os(name)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        };
        let root = (variable(Self::VARIABLE))
            .or_else(|| variable("XDG_CACHE_HOME").map(|cache| cache.join("bbtree-bench")))
            .or_else(|| variable("HOME").map(|home| home.join(".cache/bbtree-bench")))
            .ok_or_else(|| {
                format!(
                    "neither {}, XDG_CACHE_HOME nor HOME is set, so there is no data directory",
                    Self::VARIABLE
                )
            })?;
        Ok(Self { root })
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn path_of(&self, file: DataFile) -> PathBuf {
        self.root.join(file.relative_path())
    }

    /// Checks that `file` exists with its pinned size, which is all that
    /// cells check; `verify-data` checks the contents.
    pub(crate) fn check_present(&self, file: DataFile) -> Result<PathBuf, String> {
        let path = self.path_of(file);
        let metadata =
            fs::metadata(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        let pinned_bytes = file.pinned().bytes;
        if metadata.len() != pinned_bytes {
            return Err(format!(
                "{}: {} bytes, but the manifest pins {pinned_bytes}",
                path.display(),
                metadata.len()
            ));
        }
        Ok(path)
    }

    /// Reads `file`, after checking its size.
    pub(crate) fn read(&self, file: DataFile) -> Result<String, String> {
        let path = self.check_present(file)?;
        fs::read_to_string(&path).map_err(|error| format!("reading {}: {error}", path.display()))
    }

    /// `verify-data`: hashes every data file and compares it with the
    /// manifest, printing a line per file.
    pub(crate) fn verify(&self) -> Result<(), String> {
        println!("data directory: {}", self.root.display());
        let mut failures = 0;
        for file in DataFile::ALL {
            match self.verify_file(file) {
                Ok(()) => println!("ok      {}", file.relative_path()),
                Err(problem) => {
                    failures += 1;
                    println!("FAILED  {problem}");
                }
            }
        }
        if failures > 0 {
            return Err(format!(
                "{failures} of {} data files failed verification; `bbtree-bench fetch` \
                 downloads them",
                DataFile::ALL.len()
            ));
        }
        Ok(())
    }

    fn verify_file(&self, file: DataFile) -> Result<(), String> {
        let path = self.check_present(file)?;
        let sha256 = Self::sha256_of(&path)?;
        let pinned_sha256 = file.pinned().sha256;
        if sha256 != pinned_sha256 {
            return Err(format!(
                "{}: SHA-256 {sha256}, but the manifest pins {pinned_sha256}",
                path.display()
            ));
        }
        Ok(())
    }

    /// The file's SHA-256, in lowercase hexadecimal.
    fn sha256_of(path: &Path) -> Result<String, String> {
        let file = File::open(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let mut hasher = Sha256::new();
        io::copy(&mut BufReader::with_capacity(1 << 20, file), &mut hasher)
            .map_err(|error| format!("reading {}: {error}", path.display()))?;
        Ok(format!("{:x}", hasher.finalize()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manifest_pins_distinct_files_by_sha256() {
        for (index, file) in DataFile::ALL.iter().enumerate() {
            let pinned = file.pinned();
            assert_eq!(pinned.sha256.len(), 64);
            assert!(pinned.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()));
            assert!(
                DataFile::ALL[..index]
                    .iter()
                    .all(|other| other.relative_path() != pinned.relative_path)
            );
        }
    }

    /// A data directory of its own under the system's temporary directory,
    /// removed when dropped.
    struct TemporaryDataDirectory {
        directory: DataDirectory,
    }

    impl TemporaryDataDirectory {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("bbtree-bench-{name}-{}", std::process::id()));
            Self {
                directory: DataDirectory { root },
            }
        }

        /// Writes `contents` as `file`.
        fn write(&self, file: DataFile, contents: &[u8]) {
            let path = self.directory.path_of(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
    }

    impl Drop for TemporaryDataDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(self.directory.root());
        }
    }

    #[test]
    fn files_must_have_their_pinned_contents() {
        let data = TemporaryDataDirectory::new("exact-pins");
        // From the release, and derived from upstream.
        for file in [DataFile::KernelPaths, DataFile::CommonCrawlUrls] {
            data.write(file, b"Makefile\n");
            assert!(data.directory.check_present(file).is_err());
            assert!(data.directory.verify_file(file).is_err());
        }
    }

    #[test]
    fn sha256_matches_a_known_digest() {
        let path = std::env::temp_dir().join(format!("bbtree-bench-sha256-{}", std::process::id()));
        fs::write(&path, b"abc").unwrap();
        let sha256 = DataDirectory::sha256_of(&path);
        fs::remove_file(&path).unwrap();
        assert_eq!(
            sha256.unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
