#!/usr/bin/env python3
"""Draws race_bench's CSV as the README's race, in the style of upstream's
`OG-design/assets/readme-race.svg`: one bouncing ball per map, in two heats,
tree maps and hash maps. Each ball's trip takes 0.6 s times the fastest
map's throughput over its own, so balls compare across heats. A gold
medal marks each heat's fastest map.

usage: race_bench ... | race_svg.py "<machine>" > race.svg
"""
import csv
import sys
from xml.sax.saxutils import escape

TREE_MAPS = "tree maps (ordered)"
HASH_MAPS = "hash maps (unordered)"

# race_bench's names: the chart's label, heat, and whether the map is ours.
LABELS = {
    "shoal::TreeMap (bulk)": ("shoal tree, bulk", TREE_MAPS, True),
    "shoal::TreeMap": ("shoal tree", TREE_MAPS, True),
    "tbb::concurrent_map": ("tbb concurrent_map", TREE_MAPS, False),
    "folly::ConcurrentSkipList": ("folly SkipList", TREE_MAPS, False),
    "absl::btree_map + shared_mutex": ("absl btree + rwlock", TREE_MAPS, False),
    "std::map + shared_mutex": ("std::map + rwlock", TREE_MAPS, False),
    "shoal::HashMap (bulk)": ("shoal hash, bulk", HASH_MAPS, True),
    "shoal::HashMap": ("shoal hash", HASH_MAPS, True),
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
# The medal's center: past the longest label, before the track.
MEDAL_X = TRACK_START - 26
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
    .medal-ribbon { fill: #2563eb; }
    .medal { fill: #f5b301; stroke: #a16207; stroke-width: 1.5; }
    .medal-rank { font-size: 10px; font-weight: 800; fill: #713f12; text-anchor: middle; }
    .ball { animation: bounce linear infinite alternate; }
    @keyframes bounce { to { transform: translateX(%dpx); } }
  </style>""" % (TRACK_END - TRACK_START - 22)


class RaceChart:
    def __init__(self, rows, machine):
        self.rows = rows
        self.machine = machine
        self.threads = int(rows[0]["threads"])
        self.entries = int(rows[0]["entries"])
        # Runs from before the column found half their keys.
        self.hit_percent = int(rows[0].get("hit_percent") or 50)
        self.fastest = max(float(row["mops"]) for row in rows)

    @staticmethod
    def count_text(count):
        return f"{count // 1_000_000}M" if count >= 1_000_000 else f"{count // 1000}K"

    def keys_text(self):
        missing = 100 - self.hit_percent
        share = "half" if missing == 50 else "none" if missing == 0 else f"{missing}%"
        return f"{self.count_text(self.entries)} UUID keys, {share} missing"

    def title_text(self):
        if self.threads == 1:
            return "get, 1 thread"
        return f"concurrent get, {self.threads} threads"

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

    @staticmethod
    def winners(rows):
        """The rows that ran fastest: the heat's gold, ties included."""
        fastest = max(float(row["mops"]) for row in rows)
        return [row for row in rows if float(row["mops"]) == fastest]

    @staticmethod
    def medal_lines(y):
        """A gold medal on its ribbon, centered on the row's track line."""
        cy = y - 6
        return [
            f'  <path class="medal-ribbon" d="M{MEDAL_X - 6} {cy - 15} h5 l3 8 -3 3 z '
            f'M{MEDAL_X + 6} {cy - 15} h-5 l-3 8 3 3 z"/>',
            f'  <circle class="medal" cx="{MEDAL_X}" cy="{cy}" r="8"/>',
            f'  <text class="medal-rank" x="{MEDAL_X}" y="{cy + 3.5}">1</text>',
        ]

    def row_lines(self, row, y, winner):
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
        if winner:
            lines += self.medal_lines(y)
        return lines

    def svg(self):
        heats = [(name, self.heat(name)) for name in (TREE_MAPS, HASH_MAPS) if self.heat(name)]
        height = 84 + sum(HEAT_GAP + ROW * len(rows) for _, rows in heats) + 16
        summary = ", ".join(f"{LABELS[row['map']][0]} {self.value_text(float(row['mops']))}"
                            for row in self.rows)
        gold = "; ".join(f"{name}, {' and '.join(LABELS[row['map']][0] for row in self.winners(rows))}"
                         for name, rows in heats)
        keys = self.keys_text()
        out = [
            f'<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{height}" '
            f'viewBox="0 0 {WIDTH} {height}" role="img" aria-label="{self.title_text().capitalize()} '
            f'on {keys}, {escape(self.machine)}, in million operations per second: '
            f'{escape(summary)}. Fastest in {escape(gold)}.">',
            "  <!-- bench/race/race_bench.cpp, drawn by bench/race/race_svg.py. -->",
            STYLE,
            f'  <rect x="0.5" y="0.5" width="{WIDTH - 1}" height="{height - 1}" rx="12" '
            'fill="#fafafa" stroke="#d4d4d8"/>',
            "",
            f'  <text class="title" x="28" y="40">{self.title_text()}</text>',
            f'  <text class="muted" x="28" y="66">{keys} · {escape(self.machine)} · M ops/s</text>',
        ]
        y = 84
        for index, (name, rows) in enumerate(heats):
            if index > 0:
                out += ["", f'  <line class="rule" x1="28" y1="{y - 4}" x2="{VALUE_X}" y2="{y - 4}"/>']
            out += ["", f'  <text class="heat" x="28" y="{y + 18}">{escape(name.upper())}</text>']
            y += HEAT_GAP
            winners = self.winners(rows)
            for row in rows:
                out += self.row_lines(row, y + 22, row in winners)
                y += ROW
        out.append("</svg>")
        return "\n".join(out)


if __name__ == "__main__":
    machine = sys.argv[1] if len(sys.argv) > 1 else ""
    print(RaceChart(list(csv.DictReader(sys.stdin)), machine).svg())
