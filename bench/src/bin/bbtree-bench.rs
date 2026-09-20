//! The timing binary: every cell is timed with the plain base allocator.

use std::process::ExitCode;

use bbtree_bench::BaseAllocator;

#[global_allocator]
static ALLOCATOR: BaseAllocator = BaseAllocator;

fn main() -> ExitCode {
    bbtree_bench::run(None)
}
