//! `fetch`: installs the datasets in the data directory. Most data files come
//! from the datasets' GitHub release; those that may not be redistributed are
//! derived here from their upstream source, by the same tool that derived the
//! others.

use std::fs;
use std::path::Path;
use std::process::Command;

use super::{DataDirectory, DataFile, DataOrigin};

/// The GitHub release that hosts the datasets, and the dataset tool. The
/// release's assets are each data file of `DataOrigin::Release` compressed
/// with zstd, as `<file name>.zst`; each dataset directory's `SOURCE.md`, as
/// `<directory>.SOURCE.md`; and `SHA256SUMS`, which `fetch` doesn't need, as
/// it checks the decompressed files against the manifest.
pub(crate) struct DatasetFetch;

impl DatasetFetch {
    /// `gh` downloads with the user's login, or with `GH_TOKEN` in CI.
    const REPOSITORY: &str = "XiangpengHao/bbtree";

    /// TODO(phase 2): the tag of the new release, which holds the assets of
    /// `bench-data-2026-10-07` and those of the datasets added since.
    const TAG: &str = "bench-data-2026-10-08";

    /// Where the assets are downloaded to, inside the data directory, and
    /// removed from once installed.
    const STAGING_DIRECTORY: &str = ".fetch";

    /// Derives every dataset from its upstream source; each `SOURCE.md` says
    /// how.
    const DATASET_TOOL: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tools/bbtree_datasets.py");

    /// Installs the release's files and derives the others, then verifies
    /// them all as `verify-data` does. Files already there are replaced.
    pub(crate) fn fetch_into(directory: &DataDirectory) -> Result<(), String> {
        Self::install_release(directory)?;
        for file in DataFile::ALL {
            if let DataOrigin::Upstream { tool_arguments } = file.origin() {
                Self::derive_from_upstream(directory, file, tool_arguments)?;
            }
        }
        directory.verify()
    }

    /// Downloads every asset with `gh`, then decompresses the data files with
    /// `zstd` and moves the `SOURCE.md` files into the data directory's
    /// layout.
    fn install_release(directory: &DataDirectory) -> Result<(), String> {
        let staging = directory.root().join(Self::STAGING_DIRECTORY);
        if staging.exists() {
            fs::remove_dir_all(&staging)
                .map_err(|error| format!("removing {}: {error}", staging.display()))?;
        }
        fs::create_dir_all(&staging)
            .map_err(|error| format!("creating {}: {error}", staging.display()))?;
        eprintln!(
            "downloading release {} of {} into {}",
            Self::TAG,
            Self::REPOSITORY,
            staging.display()
        );
        Self::run(
            Command::new("gh")
                .args(["release", "download", Self::TAG, "--repo", Self::REPOSITORY])
                .arg("--dir")
                .arg(&staging),
            "log in with `gh auth login`, or set GH_TOKEN, with read access to the repository",
        )?;
        for file in DataFile::ALL {
            if file.origin() == DataOrigin::Release {
                Self::install_data_file(directory, &staging, file)?;
            }
        }
        for dataset in Self::dataset_directories() {
            let source = staging.join(format!("{dataset}.SOURCE.md"));
            let destination = directory.root().join(dataset).join("SOURCE.md");
            Self::create_parent_of(&destination)?;
            fs::rename(&source, &destination).map_err(|error| {
                format!(
                    "moving {} to {}: {error}",
                    source.display(),
                    destination.display()
                )
            })?;
        }
        fs::remove_dir_all(&staging)
            .map_err(|error| format!("removing {}: {error}", staging.display()))
    }

    /// Decompresses `file`'s asset to its place in the data directory.
    fn install_data_file(
        directory: &DataDirectory,
        staging: &Path,
        file: DataFile,
    ) -> Result<(), String> {
        let destination = directory.path_of(file);
        let file_name = (destination.file_name()).expect("data files have names");
        let mut asset_name = file_name.to_owned();
        asset_name.push(".zst");
        Self::create_parent_of(&destination)?;
        eprintln!("decompressing {}", file.relative_path());
        Self::run(
            Command::new("zstd")
                .args(["--decompress", "--quiet", "--force"])
                .arg(staging.join(asset_name))
                .arg("-o")
                .arg(&destination),
            "the release's asset may be damaged",
        )
    }

    /// Derives `file` from upstream with the dataset tool, which downloads
    /// the source as it parses it and replaces the file only once it has
    /// written all of it.
    fn derive_from_upstream(
        directory: &DataDirectory,
        file: DataFile,
        tool_arguments: &[&str],
    ) -> Result<(), String> {
        let destination = directory.path_of(file);
        Self::create_parent_of(&destination)?;
        eprintln!("deriving {} from upstream", file.relative_path());
        Self::run(
            Command::new("python3")
                .arg("-I")
                .arg(Self::DATASET_TOOL)
                .args(tool_arguments)
                .arg(&destination),
            "the upstream source may be unreachable, or its format may have changed",
        )
    }

    fn create_parent_of(path: &Path) -> Result<(), String> {
        let parent = path.parent().expect("data files are in a directory");
        fs::create_dir_all(parent)
            .map_err(|error| format!("creating {}: {error}", parent.display()))
    }

    /// The datasets' directories, which the data files' paths start with.
    /// Each has a `SOURCE.md` in the release, those of files derived from
    /// upstream too.
    fn dataset_directories() -> Vec<&'static str> {
        let mut directories: Vec<&str> = (DataFile::ALL.iter())
            .map(|file| {
                let (directory, _name) = (file.relative_path().split_once('/'))
                    .expect("data files are in a dataset directory");
                directory
            })
            .collect();
        directories.dedup();
        directories
    }

    /// Runs a tool that `fetch` relies on; `hint` says what to try when it
    /// fails.
    fn run(command: &mut Command, hint: &str) -> Result<(), String> {
        let program = command.get_program().to_string_lossy().into_owned();
        let status = command.status().map_err(|error| {
            format!("starting {program}: {error}; the dev shell (`nix develop`) has it")
        })?;
        if !status.success() {
            return Err(format!("{program} exited with {status}; {hint}"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_dataset_directory_is_listed_once() {
        assert_eq!(
            DatasetFetch::dataset_directories(),
            [
                "crate-names",
                "linux-paths",
                "wiki-titles",
                "json-objects",
                "cc-urls",
                "ban-addresses",
                "geonames",
                "dblp-titles"
            ]
        );
    }

    #[test]
    fn the_dataset_tool_is_in_the_repository() {
        assert!(Path::new(DatasetFetch::DATASET_TOOL).is_file());
    }
}
