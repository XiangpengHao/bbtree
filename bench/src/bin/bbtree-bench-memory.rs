//! The memory binary: the base allocator wrapped in a counter of live bytes,
//! which every cell reads to report what its maps hold.

use std::process::ExitCode;

use bbtree_bench::CountingAllocator;

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator::new();

fn main() -> ExitCode {
    bbtree_bench::run(Some(&ALLOCATOR))
}
