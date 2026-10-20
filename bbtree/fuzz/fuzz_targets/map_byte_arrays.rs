//! Map operations against std's map, on std byte-array keys (`[u8; 16]`):
//! see `MapTarget::ByteArrays`.

#![no_main]

use bbtree_fuzz::map_differential::{MapInput, MapTarget};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: MapInput| MapTarget::ByteArrays.run(&input));
