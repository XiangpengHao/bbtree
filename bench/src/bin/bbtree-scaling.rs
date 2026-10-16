//! The scaling binary: throughput as threads are added, on plain mimalloc.

use std::process::ExitCode;

use bbtree_bench::BaseAllocator;

#[global_allocator]
static ALLOCATOR: BaseAllocator = BaseAllocator;

fn main() -> ExitCode {
    bbtree_bench::run_scaling(None)
}
