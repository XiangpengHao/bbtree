//! Map operations against std's map, on keys without inline keys: see
//! `MapTarget::Plain`.

#![no_main]

use bbtree_fuzz::map_differential::{MapInput, MapTarget};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: MapInput| MapTarget::Plain.run(&input));
