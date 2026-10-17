//! The scaling binary's memory mode: the base allocator wrapped in a
//! counter of live bytes, which reports what each map holds once built.

use std::process::ExitCode;

use bbtree_bench::CountingAllocator;

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator::new();

fn main() -> ExitCode {
    bbtree_bench::run_scaling(Some(&ALLOCATOR))
}
