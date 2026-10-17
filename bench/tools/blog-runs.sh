#!/usr/bin/env bash
# The blog post's benchmark runs (doc/14-blogpost.md), on whatever machine
# this runs on: tab 1, one thread, the mean time of each operation; tab 2,
# throughput as threads are added; each map's memory, at the end of its
# build and at the build's peak; and arch, the architectures card's cells
# alone, for every other machine (Linux or macOS). Every invocation stays
# under five minutes, and each cell runs once. The post shows one bbtree
# map, `bbtree::BBTreeMap`, which threads share, as "bbtree" in every tab;
# bbtree's unsync map runs in none.
#
#   bench/tools/blog-runs.sh [tab1|tab2|memory|arch|all] [keys...]
#
# Environment:
#   ENTRIES      entries per map (10000000)
#   THREADS      tab 2's thread counts (1,2,4,8,12,16,24, or every power of
#                two up to the CPUs, and the CPUs)
#   SINGLE_CPU   the CPU single-thread runs are pinned to, where taskset
#                exists (4; one away from CPU 0's interrupts)
#   OUT          results directory (bench/results/blog/<host>-<date>)
#   TIMED, BUILD prefixes for timed and build commands (lock wrappers, or
#                nothing)
set -euo pipefail

bench=$(cd "$(dirname "$0")/.." && pwd)
what=${1:-all}
shift || true
keys=("$@")
if [ ${#keys[@]} -eq 0 ]; then
    # The architectures card shows UUIDs; every other run, all four kinds.
    if [ "$what" = arch ]; then keys=(uuid); else keys=(u64 uuid wiki-titles str-json); fi
fi

entries=${ENTRIES:-10000000}
cpus=$(nproc 2>/dev/null || sysctl -n hw.ncpu)
if [ -z "${THREADS:-}" ]; then
    if [ "$cpus" -eq 24 ]; then
        THREADS=1,2,4,8,12,16,24
    else
        THREADS=1
        for ((count = 2; count < cpus; count *= 2)); do THREADS+=",$count"; done
        THREADS+=",$cpus"
    fi
fi
single_cpu=${SINGLE_CPU:-4}
# Pinning, where the system has taskset (not macOS).
pin=()
command -v taskset >/dev/null && pin=(taskset -c "$single_cpu")
out=${OUT:-$bench/results/blog/$(hostname)-$(date +%Y%m%d)}
timed=${TIMED:-}
scaling=$bench/target/release/bbtree-scaling
memory=$bench/target/release/bbtree-scaling-memory

mkdir -p "$out"
# Only the binaries the run needs: each is a fat-LTO link.
case $what in
    tab1 | tab2 | arch) bins=(--bin bbtree-scaling) ;;
    memory) bins=(--bin bbtree-scaling-memory) ;;
    *) bins=(--bin bbtree-scaling --bin bbtree-scaling-memory) ;;
esac
(cd "$bench" && ${BUILD:-} cargo build --release "${bins[@]}")
{
    echo "host: $(hostname)"
    echo "date: $(date -Iseconds)"
    echo "bbtree: $(git -C "$bench/.." rev-parse HEAD) $(git -C "$bench/.." status --porcelain | wc -l) files changed"
    echo "rustc: $(rustc --version)"
    uname -a
    if command -v lscpu >/dev/null; then
        lscpu | grep -E 'Model name|^CPU\(s\)|Thread|Core|Socket|L1d|L2|L3|MHz'
        grep MemTotal /proc/meminfo
    else
        sysctl -n machdep.cpu.brand_string hw.memsize 2>/dev/null || true
    fi
} > "$out/machine.txt"

# Tab 1: one thread, pinned. bbtree, used from one thread, against the maps
# a program without threads would pick, and the ordered maps it might pick
# instead, each as its crate ships it: std's B-tree, crossbeam's skiplist,
# scc's TreeIndex and concurrent-map (concurrent maps, used from one
# thread); and std's hash map beside them, which keeps no order and can't
# scan. The mean time of each operation, back to back: lookups half of
# which miss, inserts, updates and 100-entry scans; bbtree's lookups and
# inserts also through its batch API, in an invocation of its own.
tab1() {
    local key=$1
    $timed ${pin[@]+"${pin[@]}"} "$scaling" --entries "$entries" --threads 1 --keys "$key" \
        --maps bbtree --workloads get-half,get-half-batch,insert,insert-batch,update,scan \
        --measure throughput --seconds 1 --out "$out/tab1-throughput-$key-bbtree"
    $timed ${pin[@]+"${pin[@]}"} "$scaling" --entries "$entries" --threads 1 --keys "$key" \
        --maps std-btree,crossbeam-skipmap,scc-treeindex,concurrent-map,std-hash \
        --workloads get-half,insert,update,scan --measure throughput \
        --seconds 1 --out "$out/tab1-throughput-$key"
}

# Tab 2: the maps threads share, as threads are added: lookups half of
# which miss, updates, a 90/10 lookup/update mix and a 90/10 scan/update
# mix in one invocation; bbtree's batched lookups in another; inserts in a
# third.
tab2() {
    local key=$1
    local maps=bbtree,std-btree-rwlock,scc-treeindex,concurrent-map,crossbeam-skipmap
    $timed "$scaling" --entries "$entries" --threads "$THREADS" --keys "$key" --maps "$maps" \
        --workloads get-half,update,mixed,scan-mixed --measure throughput --seconds 0.5 \
        --out "$out/tab2-reads-$key"
    $timed "$scaling" --entries "$entries" --threads "$THREADS" --keys "$key" --maps bbtree \
        --workloads get-half-batch --measure throughput --seconds 0.5 \
        --out "$out/tab2-batch-$key"
    $timed "$scaling" --entries "$entries" --threads "$THREADS" --keys "$key" --maps "$maps" \
        --workloads insert --measure throughput --out "$out/tab2-inserts-$key"
}

# Architectures: the single-thread gets of bbtree against std's B-tree
# alone, on a machine other than the one the post was written on: lookups
# half of which miss, of UUIDs unless other keys are named.
arch() {
    local key=$1
    $timed ${pin[@]+"${pin[@]}"} "$scaling" --entries "$entries" --threads 1 --keys "$key" \
        --maps bbtree,std-btree --workloads get-half \
        --measure throughput --seconds 1 --out "$out/arch-$key"
}

# The bytes each map holds once built, keys included, and the most it held
# at once while it was built.
memory_of() {
    local key=$1
    $timed "$memory" --entries "$entries" --keys "$key" \
        --maps bbtree,std-btree,crossbeam-skipmap,scc-treeindex,concurrent-map \
        --measure memory --out "$out/memory-$key"
}

for key in "${keys[@]}"; do
    case $what in
        tab1) tab1 "$key" ;;
        tab2) tab2 "$key" ;;
        memory) memory_of "$key" ;;
        arch) arch "$key" ;;
        all) tab1 "$key"; tab2 "$key"; memory_of "$key" ;;
        *) echo "usage: $0 [tab1|tab2|memory|arch|all] [keys...]" >&2; exit 2 ;;
    esac
done
echo "results in $out; merge them for the post with bench/tools/blog-collect.sh $out"
