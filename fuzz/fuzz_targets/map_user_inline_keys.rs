//! Map operations against std's map, on keys with a user's `InlineKey`
//! impl under `WithInlineKey`: see `MapTarget::UserInlineKeys`.

#![no_main]

use bbtree_fuzz::map_differential::{MapInput, MapTarget};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: MapInput| MapTarget::UserInlineKeys.run(&input));
