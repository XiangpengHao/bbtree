//! Maps whose node config (D59) sets other page sizes match std: internal
//! pages of 64 B, which make trees deep, and large pages of every kind, with
//! keys that have inline keys and keys that have none.

use std::string::String;

use super::differential::Workload;
use crate::inline_key::{Auto, WithInlineKey};
use crate::node_config::Configured;
use crate::tree::test_support::{FromNumber, LargePages, SmallInternalPages};

/// A key [`Auto`] doesn't recognize: its nodes keep no inline keys, and its
/// internal nodes keep its keys, in pages of `PLAIN_INTERNAL_PAGE_SIZE`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct PlainKey(u64);

impl FromNumber for PlainKey {
    fn from_number(number: usize) -> Self {
        Self(number as u64)
    }
}

#[test]
fn small_internal_pages_match_std() {
    type Strategy = Configured<Auto, SmallInternalPages>;
    let workload = Workload::new(10_000, 1 << 13);
    workload.run::<u64, u64, Strategy>(110);
    workload.run::<String, u64, Strategy>(111);
    workload.run::<PlainKey, String, Strategy>(112);
    workload.run::<i64, u64, Configured<WithInlineKey, SmallInternalPages>>(113);
}

/// The ordered inserts push 4 KiB leaves 1 below the root: 2 would take
/// some 50 000 entries under internal nodes of 2 KiB.
#[test]
fn large_pages_match_std() {
    type Strategy = Configured<Auto, LargePages>;
    let workload = Workload::shallow(10_000, 1 << 13);
    workload.run::<u64, u64, Strategy>(114);
    workload.run::<String, u64, Strategy>(115);
    workload.run::<PlainKey, u64, Strategy>(116);
}
