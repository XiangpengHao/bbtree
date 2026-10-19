//! Writes the post's figures (doc/14-blogpost.md, "How it works") as SVG
//! files into `vis/assets/figures/`, where the site embeds them and from
//! where any other page can take them as they are. The figures of a
//! mechanism are drawn from a few fixed keys; those of what bbtree does
//! read real trees, built here; the charts read the measurements in
//! `vis/data/`.
//!
//! ```text
//! cargo run --release --manifest-path vis/figures/Cargo.toml
//! ```

mod batch_lanes;
mod concurrent_gets;
mod integer_keys;
mod key_count;
mod lookup_counts;
mod prefix_keys;
mod search_space;
mod string_keys;
mod svg;
mod version_lock;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use bbtree::unsync::BBTreeMap;

use crate::batch_lanes::BatchLanes;
use crate::concurrent_gets::ConcurrentGets;
use crate::integer_keys::IntegerLeaf;
use crate::key_count::KeyCount;
use crate::lookup_counts::{LookupCount, LookupCounts};
use crate::prefix_keys::SeparatedKeys;
use crate::search_space::SearchSpace;
use crate::string_keys::StringNode;
use crate::svg::Svg;
use crate::version_lock::VersionLock;

/// Random `u64` keys, the same at every run: SplitMix64 from a fixed seed.
struct RandomKeys {
    state: u64,
}

impl Iterator for RandomKeys {
    type Item = u64;

    fn next(&mut self) -> Option<u64> {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        Some(z ^ (z >> 31))
    }
}

/// Where the figures come from and where they go.
struct PostFigures {
    data: PathBuf,
    out: PathBuf,
}

impl PostFigures {
    /// The tree the figures of real lookups read: 10M random `u64` keys,
    /// as the post's benchmarks have them.
    const ENTRIES: usize = 10_000_000;

    fn write_all(&self) -> io::Result<()> {
        fs::create_dir_all(&self.out)?;
        let keys: Vec<u64> = RandomKeys { state: 1 }.take(Self::ENTRIES).collect();
        // One insert at a time, as the benchmarks build their maps: nodes
        // fill as random inserts leave them, not full as a bulk build does.
        let mut map = BBTreeMap::new();
        for &key in &keys {
            map.insert(key, key);
        }
        let lines = self.data.join("9900x-20261018/lookups.csv");
        let figures: Vec<(&str, Svg)> = vec![
            ("search-space", SearchSpace::new().svg()),
            ("string-keys", StringNode::plain().svg()),
            ("inline-keys", StringNode::with_inline_keys().svg()),
            ("prefix-keys", SeparatedKeys::new().svg()),
            ("integer-keys", IntegerLeaf::of_lookup(&map, keys[0]).svg()),
            (
                "cache-lines",
                LookupCounts::read(&lines, LookupCount::LinesFromMemory)?.svg(),
            ),
            (
                "batch-lanes",
                BatchLanes::of_lookups(&map, [keys[1], keys[2], keys[3], keys[4]]).svg(),
            ),
            ("key-count", KeyCount::new().svg()),
            (
                "instructions",
                LookupCounts::read(&lines, LookupCount::Instructions)?.svg(),
            ),
            ("version-lock", VersionLock::svg()),
            (
                "concurrent-gets",
                ConcurrentGets::read(&self.data.join("aws-c8a-20261017/throughput.csv"))?.svg(),
            ),
        ];
        for (name, svg) in figures {
            let path = self.out.join(format!("{name}.svg"));
            svg.write(&path)?;
            println!("wrote {}", path.display());
        }
        Ok(())
    }
}

fn main() -> io::Result<()> {
    let vis = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    PostFigures {
        data: vis.join("data"),
        out: vis.join("assets/figures"),
    }
    .write_all()
}
