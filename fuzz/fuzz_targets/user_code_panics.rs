//! The map under keys and values whose callbacks panic or lie: see
//! `bbtree_fuzz::user_code_panics`.

#![no_main]

use bbtree_fuzz::user_code_panics::HostileInput;
use libfuzzer_sys::fuzz_target;

fuzz_target!(
    init: HostileInput::silence_hostile_panics(),
    |input: HostileInput| input.run()
);
