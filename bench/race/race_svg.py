#!/usr/bin/env python3
"""Draws race_bench's CSV as the README's race, in the style of upstream's
`design/assets/readme-race.svg`: one bouncing ball per map, each trip taking
0.6 s times bbtree's throughput over the map's own.

usage: race_bench ... | race_svg.py "<machine>" > race.svg
"""
import csv
import sys
from xml.sax.saxutils import escape

# race_bench's names, the labels the chart gives them, and whether a row is
# ours or the unordered reference below the line.
LABELS = {
    "bbtree (batched)": ("bbtree, batched", "ours"),
    "bbtree": ("bbtree", "ours"),
    "tbb::concurrent_map": ("tbb concurrent_map", "theirs"),
    "folly::ConcurrentSkipList": ("folly SkipList", "theirs"),
    "absl::btree_map + shared_mutex": ("absl btree + rwlock", "theirs"),
    "std::map + shared_mutex": ("std::map + rwlock", "theirs"),
    "bbtree::BBHashMap (unordered)": ("bbtree hash (unordered)", "unordered"),
    "boost::concurrent_flat_map (unordered)": ("boost hash (unordered)", "unordered"),
}

WIDTH = 760
ROW = 40
TRACK_START, TRACK_END, VALUE_X = 318, 650, 732
TRIP = 332  # the ball's travel, track end less its start


def count_text(count):
    return f"{count // 1_000_000}M" if count >= 1_000_000 else f"{count // 1000}K"


def value_text(mops):
    return f"{mops:.0f}" if mops >= 100 else f"{mops:.1f}"


def main():
    machine = sys.argv[1] if len(sys.argv) > 1 else ""
    rows = list(csv.DictReader(sys.stdin))
    threads = rows[0]["threads"]
    entries = int(rows[0]["entries"])
    fastest = max(float(row["mops"]) for row in rows if LABELS[row["map"]][1] == "ours")
    ordered = [row for row in rows if LABELS[row["map"]][1] != "unordered"]
    unordered = [row for row in rows if LABELS[row["map"]][1] == "unordered"]
    height = 86 + ROW * len(ordered) + (16 + ROW * len(unordered) if unordered else 0) + 14

    summary = ", ".join(f"{LABELS[row['map']][0]} {value_text(float(row['mops']))}" for row in rows)
    out = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{height}" '
        f'viewBox="0 0 {WIDTH} {height}" role="img" aria-label="Concurrent get on '
        f'{count_text(entries)} UUID keys, half missing, {threads} threads, {escape(machine)}, '
        f'in million operations per second: {escape(summary)}.">',
        "  <!-- bench/race/race_bench.cpp, drawn by bench/race/race_svg.py. -->",
        "  <style>",
        "    text { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; "
        "font-size: 18px; font-weight: 600; fill: #18181b; }",
        "    .title { font-weight: 800; }",
        "    .muted { fill: #52525b; }",
        "    .faint { fill: #71717a; }",
        "    .ours { font-weight: 800; }",
        "    .value { text-anchor: end; }",
        "    .highlight { fill: #ffd60a; }",
        "    .track { stroke: #a1a1aa; stroke-width: 3; stroke-linecap: round; }",
        "    .dashed { stroke-dasharray: 4 6; }",
        "    .ball { animation: bounce linear infinite alternate; }",
        f"    @keyframes bounce {{ to {{ transform: translateX({TRIP - 22}px); }} }}",
        "  </style>",
        f'  <rect x="0.5" y="0.5" width="{WIDTH - 1}" height="{height - 1}" rx="12" '
        'fill="#fafafa" stroke="#d4d4d8"/>',
        "",
        f'  <text class="title" x="28" y="40">concurrent get, {threads} threads</text>',
        f'  <text class="muted" x="28" y="66">{count_text(entries)} UUID keys, half missing'
        f" · {escape(machine)} · M ops/s</text>",
    ]

    def draw(row, y, kind):
        label, _ = LABELS[row["map"]]
        mops = float(row["mops"])
        trip = 0.6 * fastest / mops
        text_class = {"ours": "ours", "theirs": "", "unordered": "faint"}[kind]
        class_attr = f' class="{text_class}"' if text_class else ""
        value_class = f"{text_class} value".strip()
        track_class = "track dashed" if kind == "unordered" else "track"
        ball = 'fill="#ffd60a" stroke="#18181b" stroke-width="2.5"' if kind == "ours" else 'fill="#3f3f46"'
        if kind == "unordered":
            ball = 'fill="#a1a1aa"'
        lines = [""]
        if kind == "ours":
            lines.append(f'  <rect class="highlight" x="22" y="{y - 20}" width="{11 * len(label) + 12}" '
                         'height="26" rx="4"/>')
            value_width = 11 * len(value_text(mops)) + 16
            lines.append(f'  <rect class="highlight" x="{VALUE_X + 8 - value_width}" y="{y - 20}" '
                         f'width="{value_width}" height="26" rx="4"/>')
        lines += [
            f'  <text{class_attr} x="28" y="{y}">{escape(label)}</text>',
            f'  <line class="{track_class}" x1="{TRACK_START}" y1="{y - 8}" x2="{TRACK_END}" y2="{y - 8}"/>',
            f'  <circle class="ball" cx="{TRACK_START + 11}" cy="{y - 8}" r="11" {ball} '
            f'style="animation-duration: {trip:.2f}s"/>',
            f'  <text class="{value_class}" x="{VALUE_X}" y="{y}">{value_text(mops)}</text>',
        ]
        out.extend(lines)

    y = 106
    for row in ordered:
        draw(row, y, "ours" if LABELS[row["map"]][1] == "ours" else "theirs")
        y += ROW
    if unordered:
        out += ["", f'  <line class="track dashed" x1="28" y1="{y - 24}" x2="{VALUE_X}" y2="{y - 24}" '
                'stroke-width="1.5"/>']
        y += 16
        for row in unordered:
            draw(row, y, "unordered")
            y += ROW
    out.append("</svg>")
    print("\n".join(out))


if __name__ == "__main__":
    main()
