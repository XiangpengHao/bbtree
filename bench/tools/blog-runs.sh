#!/usr/bin/env bash
# The blog post's benchmark runs (doc/14-blogpost.md), on whatever machine
# this runs on: tab 1, one thread, each operation's latency and the mean;
# tab 2, throughput as threads are added; and each map's memory. Every
# invocation stays under five minutes, and each cell runs once.
#
#   bench/tools/blog-runs.sh [tab1|tab2|memory|all] [keys...]
#
# Environment:
#   ENTRIES      entries per map (10000000)
#   THREADS      tab 2's thread counts (1,2,4,8,12,16,24, or every power of
#                two up to the CPUs, and the CPUs)
#   SINGLE_CPU   tab 1's CPU (4; one away from CPU 0's interrupts)
#   OUT          results directory (bench/results/blog/<host>-<date>)
#   TIMED, BUILD prefixes for timed and build commands (lock wrappers, or
#                nothing)
set -euo pipefail

bench=$(cd "$(dirname "$0")/.." && pwd)
what=${1:-all}
shift || true
keys=("$@")
[ ${#keys[@]} -eq 0 ] && keys=(u64 uuid wiki-titles str-json)

entries=${ENTRIES:-10000000}
cpus=$(nproc)
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
out=${OUT:-$bench/results/blog/$(hostname)-$(date +%Y%m%d)}
timed=${TIMED:-}
scaling=$bench/target/release/bbtree-scaling
memory=$bench/target/release/bbtree-scaling-memory

mkdir -p "$out"
(cd "$bench" && ${BUILD:-} cargo build --release --bins)
{
    echo "host: $(hostname)"
    echo "date: $(date -Iseconds)"
    echo "bbtree: $(git -C "$bench/.." rev-parse HEAD) $(git -C "$bench/.." status --porcelain | wc -l) files changed"
    echo "rustc: $(rustc --version)"
    uname -a
    lscpu | grep -E 'Model name|^CPU\(s\)|Thread|Core|Socket|L1d|L2|L3|MHz'
    grep MemTotal /proc/meminfo
} > "$out/machine.txt"

# Tab 1: one thread, pinned. The maps a program without threads would pick,
# and the fastest ordered maps there are: bbtree, std's B-tree, crossbeam's
# skiplist, artmap in its arena. Latencies of lookups, misses and inserts;
# then the mean of each, batched lookups included.
tab1() {
    local key=$1
    $timed taskset -c "$single_cpu" "$scaling" --entries "$entries" --threads 1 --keys "$key" \
        --maps bbtree,std-btree,crossbeam-skipmap,artmap-arena \
        --workloads get,get-miss,insert --measure latency --seconds 1 \
        --out "$out/tab1-latency-$key"
    $timed taskset -c "$single_cpu" "$scaling" --entries "$entries" --threads 1 --keys "$key" \
        --maps bbtree,std-btree,bctree,crossbeam-skipmap,artmap-arena \
        --workloads get,get-batch,get-miss,get-miss-batch,insert --measure throughput \
        --seconds 1 --out "$out/tab1-throughput-$key"
}

# Tab 2: the maps threads share, as threads are added: lookups, a 90/10
# read/update mix and scans in one invocation, inserts in another.
tab2() {
    local key=$1
    local maps=bctree,std-btree-rwlock,bbtree-rwlock,crossbeam-skipmap,concurrent-map,scc-treeindex,artmap
    $timed "$scaling" --entries "$entries" --threads "$THREADS" --keys "$key" --maps "$maps" \
        --workloads get,get-batch,mixed,scan --measure throughput --seconds 0.5 \
        --out "$out/tab2-reads-$key"
    $timed "$scaling" --entries "$entries" --threads "$THREADS" --keys "$key" --maps "$maps" \
        --workloads insert --measure throughput --out "$out/tab2-inserts-$key"
}

# Bytes per entry once built, keys included. artmap's arena reserves 4 GiB
# up front, so it is left out.
memory_of() {
    local key=$1
    $timed "$memory" --entries "$entries" --keys "$key" \
        --maps bbtree,std-btree,bctree,crossbeam-skipmap,scc-treeindex,artmap,concurrent-map \
        --measure memory --out "$out/memory-$key"
}

for key in "${keys[@]}"; do
    case $what in
        tab1) tab1 "$key" ;;
        tab2) tab2 "$key" ;;
        memory) memory_of "$key" ;;
        all) tab1 "$key"; tab2 "$key"; memory_of "$key" ;;
        *) echo "usage: $0 [tab1|tab2|memory|all] [keys...]" >&2; exit 2 ;;
    esac
done
echo "results in $out"
