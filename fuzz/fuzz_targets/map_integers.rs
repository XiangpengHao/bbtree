//! Map operations against std's map, on std integer keys (`u64`): see
//! `MapTarget::Integers`.

#![no_main]

use bbtree_fuzz::map_differential::{MapInput, MapTarget};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: MapInput| MapTarget::Integers.run(&input));
