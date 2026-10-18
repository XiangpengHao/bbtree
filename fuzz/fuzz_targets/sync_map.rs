//! The sync map's operations against std's map, on one thread, over every
//! row of the node plan it takes: see `bbtree_fuzz::sync_map_differential`.

#![no_main]

use bbtree_fuzz::sync_map_differential::SyncMapInput;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: SyncMapInput| input.run());
