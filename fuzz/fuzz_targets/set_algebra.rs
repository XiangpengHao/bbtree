//! The operations on two sets against std's sets: see `bbtree_fuzz::set_algebra`.

#![no_main]

use bbtree_fuzz::set_algebra::SetInput;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: SetInput| input.run());
