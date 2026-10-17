#!/usr/bin/env bash
# Merges one machine's blog runs (bench/tools/blog-runs.sh, under the
# ignored bench/results/blog/<host>-<date>) into the files the post's charts
# compile in (vis/src/measurements.rs), under vis/data/<host>-<date>, which
# the repository keeps: throughput.csv, with the run each row came from
# (single-thread or concurrent); memory.csv; and the machine's
# description. Runs under reruns/ come last, and a cell measured
# again replaces what the first run measured of it. The integrations'
# results (integrations.csv) are written there by hand, from
# ~/coding/bbtree-integrations/*/RESULTS.md.
#
#   bench/tools/blog-collect.sh bench/results/blog/<host>-<date>
set -euo pipefail

dir=${1:?usage: $0 <results directory>}
out=$(cd "$(dirname "$0")/../.." && pwd)/vis/data/$(basename "$dir")
mkdir -p "$out"

# The rows of "$@" past their headers, each prefixed by $prefix, a cell's
# last row winning: a cell is a row's first $key_fields fields. Rows of
# maps the post no longer shows are left out: artmap, in both its forms,
# and DashMap (2026-10-16).
rows() {
    local prefix=$1 key_fields=$2
    shift 2
    for file in "$@"; do
        if [ -f "$file" ]; then tail -n +2 "$file" | { grep -v -e '^artmap' -e '^dashmap' || true; } | sed "s/^/$prefix/"; fi
    done | awk -F, -v n="$key_fields" '{
        key = $1; for (i = 2; i <= n; i++) key = key "," $i
        if (!(key in last)) order[++count] = key
        last[key] = $0
    } END { for (i = 1; i <= count; i++) print last[order[i]] }'
}

{
    echo "run,map,keys,workload,threads,operations,elapsed_ns,mops"
    rows "single-thread," 5 "$dir"/tab1-throughput-*/scaling.csv "$dir"/arch-*/scaling.csv \
        "$dir"/reruns/tab1-throughput-*/scaling.csv
    rows "concurrent," 5 "$dir"/tab2-*/scaling.csv "$dir"/reruns/tab2-*/scaling.csv
} > "$out/throughput.csv"

{
    echo "map,keys,entries,heap_bytes,heap_bytes_per_entry,resident_growth_bytes,peak_growth_bytes,peak_heap_bytes"
    rows "" 2 "$dir"/memory-*/memory.csv "$dir"/reruns/memory-*/memory.csv
} > "$out/memory.csv"

cp "$dir/machine.txt" "$out/machine.txt"
wc -l "$out"/*.csv
