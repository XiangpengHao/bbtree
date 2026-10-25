#!/usr/bin/env python3
"""Draws race_bench's CSV as the README's race, in the style of upstream's
`design/assets/readme-race.svg`: one bouncing ball per map, in two heats,
tree maps and hash maps. Each ball's trip takes 0.6 s times the fastest
map's throughput over its own, so balls compare across heats.

usage: race_bench ... | race_svg.py "<machine>" > race.svg
"""
import csv
import sys
from xml.sax.saxutils import escape

TREE_MAPS = "tree maps (ordered)"
HASH_MAPS = "hash maps (unordered)"

# race_bench's names: the chart's label, heat, and whether the map is ours.
LABELS = {
    "bbtree (bulk)": ("bbtree, bulk", TREE_MAPS, True),
    "bbtree": ("bbtree", TREE_MAPS, True),
    "tbb::concurrent_map": ("tbb concurrent_map", TREE_MAPS, False),
    "folly::ConcurrentSkipList": ("folly SkipList", TREE_MAPS, False),
    "absl::btree_map + shared_mutex": ("absl btree + rwlock", TREE_MAPS, False),
    "std::map + shared_mutex": ("std::map + rwlock", TREE_MAPS, False),
    "bbtree::BBHashMap (bulk)": ("bbtree hash, bulk", HASH_MAPS, True),
    "bbtree::BBHashMap": ("bbtree hash", HASH_MAPS, True),
    "boost::concurrent_flat_map": ("boost flat_map, bulk", HASH_MAPS, False),
    "folly::ConcurrentHashMap": ("folly ConcurrentHashMap", HASH_MAPS, False),
    "tbb::concurrent_hash_map": ("tbb concurrent_hash_map", HASH_MAPS, False),
    "tbb::concurrent_unordered_map": ("tbb concurrent_unordered", HASH_MAPS, False),
    "absl::flat_hash_map + shared_mutex": ("absl flat_hash + rwlock", HASH_MAPS, False),
    "std::unordered_map + shared_mutex": ("std::unordered + rwlock", HASH_MAPS, False),
}

WIDTH = 760
ROW = 40
HEAT_GAP = 30
TRACK_START, TRACK_END, VALUE_X = 334, 650, 732
CHAR_WIDTH = 11
STYLE = """  <style>
    text { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 18px; font-weight: 600; fill: #18181b; }
    .title { font-weight: 800; }
    .muted { fill: #52525b; }
    .heat { fill: #52525b; font-size: 15px; font-weight: 700; letter-spacing: 0.04em; }
    .ours { font-weight: 800; }
    .value { text-anchor: end; }
    .highlight { fill: #ffd60a; }
    .track { stroke: #a1a1aa; stroke-width: 3; stroke-linecap: round; }
    .rule { stroke: #d4d4d8; stroke-width: 1.5; stroke-dasharray: 4 6; }
    .ball { animation: bounce linear infinite alternate; }
    @keyframes bounce { to { transform: translateX(%dpx); } }
  </style>""" % (TRACK_END - TRACK_START - 22)


class RaceChart:
    def __init__(self, rows, machine):
        self.rows = rows
        self.machine = machine
        self.threads = rows[0]["threads"]
        self.entries = int(rows[0]["entries"])
        self.fastest = max(float(row["mops"]) for row in rows)

    @staticmethod
    def count_text(count):
        return f"{count // 1_000_000}M" if count >= 1_000_000 else f"{count // 1000}K"

    @staticmethod
    def value_text(mops):
        return f"{mops:.0f}" if mops >= 100 else f"{mops:.1f}"

    def heat(self, name):
        """The heat's rows: ours first, as race_bench ran them, then the
        others fastest first."""
        rows = [row for row in self.rows if LABELS[row["map"]][1] == name]
        ours = [row for row in rows if LABELS[row["map"]][2]]
        theirs = sorted((row for row in rows if not LABELS[row["map"]][2]),
                        key=lambda row: -float(row["mops"]))
        return ours + theirs

    def row_lines(self, row, y):
        label, _, ours = LABELS[row["map"]]
        mops = float(row["mops"])
        value = self.value_text(mops)
        trip = 0.6 * self.fastest / mops
        lines = [""]
        if ours:
            lines.append(f'  <rect class="highlight" x="22" y="{y - 20}" '
                         f'width="{CHAR_WIDTH * len(label) + 12}" height="26" rx="4"/>')
            value_width = CHAR_WIDTH * len(value) + 16
            lines.append(f'  <rect class="highlight" x="{VALUE_X + 8 - value_width}" y="{y - 20}" '
                         f'width="{value_width}" height="26" rx="4"/>')
        ball = 'fill="#ffd60a" stroke="#18181b" stroke-width="2.5"' if ours else 'fill="#3f3f46"'
        text_class = ' class="ours"' if ours else ""
        value_class = "ours value" if ours else "value"
        lines += [
            f'  <text{text_class} x="28" y="{y}">{escape(label)}</text>',
            f'  <line class="track" x1="{TRACK_START}" y1="{y - 8}" x2="{TRACK_END}" y2="{y - 8}"/>',
            f'  <circle class="ball" cx="{TRACK_START + 11}" cy="{y - 8}" r="11" {ball} '
            f'style="animation-duration: {trip:.2f}s"/>',
            f'  <text class="{value_class}" x="{VALUE_X}" y="{y}">{value}</text>',
        ]
        return lines

    def svg(self):
        heats = [(name, self.heat(name)) for name in (TREE_MAPS, HASH_MAPS) if self.heat(name)]
        height = 84 + sum(HEAT_GAP + ROW * len(rows) for _, rows in heats) + 16
        summary = ", ".join(f"{LABELS[row['map']][0]} {self.value_text(float(row['mops']))}"
                            for row in self.rows)
        keys = f"{self.count_text(self.entries)} UUID keys, half missing"
        out = [
            f'<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{height}" '
            f'viewBox="0 0 {WIDTH} {height}" role="img" aria-label="Concurrent get on {keys}, '
            f'{self.threads} threads, {escape(self.machine)}, in million operations per second: '
            f'{escape(summary)}.">',
            "  <!-- bench/race/race_bench.cpp, drawn by bench/race/race_svg.py. -->",
            STYLE,
            f'  <rect x="0.5" y="0.5" width="{WIDTH - 1}" height="{height - 1}" rx="12" '
            'fill="#fafafa" stroke="#d4d4d8"/>',
            "",
            f'  <text class="title" x="28" y="40">concurrent get, {self.threads} threads</text>',
            f'  <text class="muted" x="28" y="66">{keys} · {escape(self.machine)} · M ops/s</text>',
        ]
        y = 84
        for index, (name, rows) in enumerate(heats):
            if index > 0:
                out += ["", f'  <line class="rule" x1="28" y1="{y - 4}" x2="{VALUE_X}" y2="{y - 4}"/>']
            out += ["", f'  <text class="heat" x="28" y="{y + 18}">{escape(name.upper())}</text>']
            y += HEAT_GAP
            for row in rows:
                out += self.row_lines(row, y + 22)
                y += ROW
        out.append("</svg>")
        return "\n".join(out)


if __name__ == "__main__":
    machine = sys.argv[1] if len(sys.argv) > 1 else ""
    print(RaceChart(list(csv.DictReader(sys.stdin)), machine).svg())
