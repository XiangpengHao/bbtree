//! The fuzz targets' harnesses (D5, `design/08-fuzzing-verification.md`).
//!
//! Fuzzing time goes where a bug would reach the most users and do the
//! most harm: the node code every operation shares, under every key type's
//! node plan, and the unsafe code under user code that misbehaves. Each
//! target is a thin binary in `fuzz_targets/` over one of these harnesses:
//!
//! - [`map_differential`]: map operations against std's map, one target per
//!   row of the node plan, which is where key types take different paths
//!   through the shared node code;
//! - [`sync_map_differential`]: the sync map's operations against std's
//!   map, on one thread, one target over every row it takes: the latched
//!   layer over the same nodes;
//! - [`set_algebra`]: the operations on two sets against std's sets, the
//!   only set code that is not the map's;
//! - [`user_code_panics`]: keys and values whose callbacks panic or lie,
//!   for memory safety and panic isolation.
//!
//! Every harness checks our tree's invariants (`check_invariants`, compiled
//! with `--cfg fuzzing`, which `cargo fuzz` and this crate's
//! `.cargo/config.toml` set) besides std's answers: a corrupt node is
//! caught when it is made, not when a later lookup happens to cross it.

pub mod key_recipe;
pub mod map_differential;
mod operation_list;
mod position_mask;
pub mod set_algebra;
pub mod sync_map_differential;
pub mod user_code_panics;

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use libfuzzer_sys::arbitrary::{Arbitrary, Unstructured};

    use crate::map_differential::{MapInput, MapTarget};
    use crate::set_algebra::SetInput;
    use crate::sync_map_differential::SyncMapInput;
    use crate::user_code_panics::HostileInput;

    /// Inputs of pseudo-random bytes, of every length up to 16 KiB, the
    /// same every time: a check that each harness agrees with itself on
    /// what std and our collections do, so that what the fuzzer reports is
    /// theirs to explain.
    struct RandomInputs {
        state: u64,
        left: usize,
    }

    impl RandomInputs {
        fn new(count: usize) -> Self {
            Self {
                state: 0x2545_F491_4F6C_DD1D,
                left: count,
            }
        }

        fn next_word(&mut self) -> u64 {
            self.state ^= self.state << 13;
            self.state ^= self.state >> 7;
            self.state ^= self.state << 17;
            self.state
        }

        /// Runs `run` on each input, decoded as the fuzzer decodes it.
        fn run_each<T: for<'a> Arbitrary<'a>>(self, run: impl Fn(&T)) {
            for bytes in self {
                let input = T::arbitrary_take_rest(Unstructured::new(&bytes));
                run(&input.expect("every input decodes"));
            }
        }
    }

    impl Iterator for RandomInputs {
        type Item = Vec<u8>;

        fn next(&mut self) -> Option<Vec<u8>> {
            self.left = self.left.checked_sub(1)?;
            let len = (self.next_word() % (16 << 10)) as usize;
            Some((0..len).map(|_| self.next_word() as u8).collect())
        }
    }

    /// A crash the fuzzer found, minimized, kept as
    /// `regressions/<target>/<what it found>`.
    struct Regression {
        path: PathBuf,
    }

    impl Regression {
        /// Every regression, of every target.
        fn all() -> Vec<Self> {
            let regressions = Path::new(env!("CARGO_MANIFEST_DIR")).join("regressions");
            let targets = fs::read_dir(regressions).expect("regressions/ is readable");
            (targets.flat_map(|target| fs::read_dir(target.unwrap().path()).unwrap()))
                .map(|input| Self {
                    path: input.unwrap().path(),
                })
                .collect()
        }

        fn decode<T: for<'a> Arbitrary<'a>>(bytes: &[u8]) -> T {
            T::arbitrary_take_rest(Unstructured::new(bytes)).expect("every input decodes")
        }

        /// Runs the input on the target it was found by, named by its
        /// directory, as the fuzzer runs it.
        fn run(&self) {
            let bytes = fs::read(&self.path).unwrap();
            let target = self.path.parent().and_then(Path::file_name).unwrap();
            let map_target = match target.to_str().unwrap() {
                "map_heap_byte_strings" => MapTarget::HeapByteStrings,
                "map_integers" => MapTarget::Integers,
                "map_user_inline_keys" => MapTarget::UserInlineKeys,
                "map_byte_arrays" => MapTarget::ByteArrays,
                "map_plain_keys" => MapTarget::Plain,
                "sync_map" => return Self::decode::<SyncMapInput>(&bytes).run(),
                "set_algebra" => return Self::decode::<SetInput>(&bytes).run(),
                "user_code_panics" => return Self::decode::<HostileInput>(&bytes).run(),
                other => panic!("no fuzz target is named {other}"),
            };
            map_target.run(&Self::decode(&bytes));
        }
    }

    /// Every crash the fuzzer found stays fixed. A failure names its input:
    /// `cargo fuzz run <target> <input>` replays it with the sanitizer.
    #[test]
    fn regressions_run_clean() {
        HostileInput::silence_hostile_panics();
        for regression in Regression::all() {
            eprintln!("replaying {}", regression.path.display());
            regression.run();
        }
    }

    #[test]
    fn map_targets_accept_random_inputs() {
        RandomInputs::new(64).run_each(|input: &MapInput| {
            MapTarget::ALL.iter().for_each(|target| target.run(input));
        });
    }

    #[test]
    fn sync_map_target_accepts_random_inputs() {
        RandomInputs::new(256).run_each(SyncMapInput::run);
    }

    #[test]
    fn set_target_accepts_random_inputs() {
        RandomInputs::new(256).run_each(SetInput::run);
    }

    #[test]
    fn hostile_target_accepts_random_inputs() {
        HostileInput::silence_hostile_panics();
        RandomInputs::new(256).run_each(HostileInput::run);
    }
}
