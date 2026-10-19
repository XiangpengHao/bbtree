//! Predicates from fuzzer input for `retain` and `extract_if`.

use libfuzzer_sys::arbitrary::{self, Arbitrary};

/// A mask that picks the items of a walk by position: item `i` if bit
/// `i % 64` is set. Std's collections and ours visit items in the same
/// order, ascending, so two pickers of one mask, one for each, pick the same
/// items.
#[derive(Arbitrary, Debug, Clone, Copy)]
pub struct PositionMask(u64);

impl PositionMask {
    /// Answers, one call per item, whether to pick it.
    pub fn picker(self) -> impl FnMut() -> bool {
        let mut position = 0_u64;
        move || {
            let picked = self.0 >> (position % 64) & 1 == 1;
            position += 1;
            picked
        }
    }
}
