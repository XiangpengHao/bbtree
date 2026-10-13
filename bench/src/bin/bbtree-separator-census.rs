//! The separator census (`separator_census.rs`): untimed, on plain mimalloc.

use std::process::ExitCode;

use bbtree_bench::BaseAllocator;

#[global_allocator]
static ALLOCATOR: BaseAllocator = BaseAllocator;

fn main() -> ExitCode {
    bbtree_bench::separator_census::run()
}
