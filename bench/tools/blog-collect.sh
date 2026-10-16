#!/usr/bin/env bash
# Merges one machine's blog runs (bench/tools/blog-runs.sh) into the three
# files the post's charts read (vis/src/measurements.rs): latency.csv,
# throughput.csv, with the run each row came from (one-thread for tab 1,
# threads for tab 2), and memory.csv. Runs under reruns/ come last, and a
# cell measured again replaces what the first run measured of it.
#
#   bench/tools/blog-collect.sh bench/results/blog/<host>-<date>
set -euo pipefail

dir=${1:?usage: $0 <results directory>}

# The rows of "$@" past their headers, each prefixed by $prefix, a cell's
# last row winning: a cell is a row's first $key_fields fields.
rows() {
    local prefix=$1 key_fields=$2
    shift 2
    for file in "$@"; do
        if [ -f "$file" ]; then tail -n +2 "$file" | sed "s/^/$prefix/"; fi
    done | awk -F, -v n="$key_fields" '{
        key = $1; for (i = 2; i <= n; i++) key = key "," $i
        if (!(key in last)) order[++count] = key
        last[key] = $0
    } END { for (i = 1; i <= count; i++) print last[order[i]] }'
}

{
    echo "map,keys,workload,threads,operations,p50_ns,p90_ns,p99_ns,p999_ns,max_ns"
    rows "" 4 "$dir"/tab1-latency-*/latency.csv "$dir"/reruns/tab1-latency-*/latency.csv
} > "$dir/latency.csv"

{
    echo "run,map,keys,workload,threads,operations,elapsed_ns,mops"
    {
        rows "one-thread," 5 "$dir"/tab1-throughput-*/scaling.csv "$dir"/reruns/tab1-throughput-*/scaling.csv
        rows "threads," 5 "$dir"/tab2-*/scaling.csv "$dir"/reruns/tab2-*/scaling.csv
    }
} > "$dir/throughput.csv"

{
    echo "map,keys,entries,heap_bytes,heap_bytes_per_entry,resident_growth_bytes,peak_growth_bytes"
    rows "" 2 "$dir"/memory-*/memory.csv "$dir"/reruns/memory-*/memory.csv
} > "$dir/memory.csv"

wc -l "$dir"/latency.csv "$dir"/throughput.csv "$dir"/memory.csv
