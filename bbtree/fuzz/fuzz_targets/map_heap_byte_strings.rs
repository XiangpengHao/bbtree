//! Map operations against std's map, on std heap byte-string keys
//! (`Vec<u8>`): see `MapTarget::HeapByteStrings`.

#![no_main]

use bbtree_fuzz::map_differential::{MapInput, MapTarget};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: MapInput| MapTarget::HeapByteStrings.run(&input));
