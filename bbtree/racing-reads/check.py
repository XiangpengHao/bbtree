#!/usr/bin/env python3
"""Builds the probes (src/lib.rs) to assembly for each target, with the
cargo on PATH, and checks their racing reads (checker.py): every read of a
node after one of its version checks is vouched for by another check.

With --planted, it then plants each reload in PLANTED in a copy of the
repository, and checks that the checker flags it in the probes that reach
it: a checker that has gone blind to a compiler's code fails instead of
passing. Exits 1 on anything flagged or missed.
"""

import argparse
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

import checker

CRATE = Path(__file__).resolve().parent
REPOSITORY = CRATE.parents[1]

TARGETS = {
    "x86-64": ("x86_64-unknown-linux-gnu", "-C llvm-args=-x86-asm-syntax=intel"),
    # bbtree picks its AVX-512 search by `cfg(target_feature)`, so the flags
    # go to every crate, as RUSTFLAGS.
    "x86-64-v4": ("x86_64-unknown-linux-gnu", "-C llvm-args=-x86-asm-syntax=intel -C target-cpu=x86-64-v4"),
    "aarch64": ("aarch64-unknown-linux-gnu", ""),
}

PROBES = ["probe_get_u64", "probe_get_string", "probe_get_many_u64", "probe_insert_u64"]

# Each reload: a racing read done again after the check that covers it, as
# a compiler relying on the read's being race-free could; the file, the code
# it replaces and the code that replaces it, and the probes that reach it.
PLANTED = {
    "the leaf's entry, after the leaf's check": (
        "bbtree/src/tree/latched/lookup.rs",
        """\
            if !at.is_unchanged() {
                return LeafFind::Changed;
            }
""",
        """\
            if !at.is_unchanged() {
                return LeafFind::Changed;
            }
            let read = match candidate {
                Some((index, prefix_len)) => {
                    RacingRead::of_candidate(leaf, key, index, prefix_len).unwrap_or(read)
                }
                None => read,
            };
""",
        ["probe_get_u64", "probe_get_string", "probe_get_many_u64"],
    ),
    "the edge, after the node's second check": (
        "bbtree/src/tree/latched/descent.rs",
        """\
                        let child_version = child.latch().read_version();
                        if !node.latch().is_unchanged(version) {
                            continue 'restart;
                        }
                        (node, version) = (child, child_version);
""",
        """\
                        let child_version = child.latch().read_version();
                        if !node.latch().is_unchanged(version) {
                            continue 'restart;
                        }
                        let Some(Node::Internal(child)) = node.racing_child(routed.edge) else {
                            continue 'restart;
                        };
                        (node, version) = (child, child_version);
""",
        PROBES,
    ),
    # The lanes' read loop, the file's last: the lookups' (get_many_copied).
    "a lane's edge, after the node's second check": (
        "bbtree/src/tree/latched/batch.rs",
        """\
                            let child_version = child.latch().read_version();
                            if !node.latch().is_unchanged(version) {
                                continue 'restart;
                            }
                            (node, version) = (child, child_version);
""",
        """\
                            let child_version = child.latch().read_version();
                            if !node.latch().is_unchanged(version) {
                                continue 'restart;
                            }
                            let Some(Node::Internal(child)) = node.racing_child(usize::from(edge))
                            else {
                                continue 'restart;
                            };
                            (node, version) = (child, child_version);
""",
        ["probe_get_many_u64"],
    ),
}


def build(crate, target, target_dir):
    """The assembly file of the probes in crate, built for target."""
    triple, flags = TARGETS[target]
    env = dict(os.environ, RUSTFLAGS=flags, CARGO_TARGET_DIR=str(target_dir))
    subprocess.run(
        ["cargo", "rustc", "--quiet", "--release", "--locked", "--lib", "--target", triple, "--", "--emit=asm"],
        cwd=crate, env=env, check=True)
    files = sorted(Path(target_dir).rglob("bbtree_racing_reads-*.s"), key=lambda f: f.stat().st_mtime)
    return files[-1]


def check_probes(assembly, probes):
    """For each probe, its report: the reads and checks the checker flags."""
    reports = {}
    for probe in probes:
        checks, report = checker.check(assembly, [probe])
        if checks == 0:
            report = [f"{probe}: no version check found"]
        reports[probe] = report
    return reports


def copy_with(planted, into):
    """A copy of the repository's files at into, with the reload planted."""
    path, old, new, _ = PLANTED[planted]
    files = subprocess.run(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
        cwd=REPOSITORY, check=True, capture_output=True).stdout.decode().split("\0")
    for name in filter(None, files):
        if (REPOSITORY / name).is_file():
            (into / name).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(REPOSITORY / name, into / name)
    text = (into / path).read_text()
    if old not in text:
        sys.exit(f"{planted}: the code to replace isn't in {path} any more; update PLANTED in check.py")
    at = text.rindex(old)
    (into / path).write_text(text[:at] + new + text[at + len(old):])
    return into / CRATE.relative_to(REPOSITORY)


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--planted", action="store_true", help="also check that planted reloads are flagged")
    parser.add_argument("--target", action="append", choices=TARGETS, help="a target to check (default: all)")
    parser.add_argument("--target-dir", default=CRATE / "target", type=Path)
    args = parser.parse_args()
    failed = False
    for target in args.target or TARGETS:
        # A target directory for each tree of sources, so that the newest
        # assembly file in it is that tree's.
        reports = check_probes(build(CRATE, target, args.target_dir / target / "unchanged"), PROBES)
        for probe, report in reports.items():
            print(f"{target} {probe}: {'flagged' if report else 'ok'}")
            for line in report:
                print("  " + line.replace("\n", "\n  "))
            failed |= bool(report)
        if not args.planted:
            continue
        for planted, (_, _, _, reaching) in PLANTED.items():
            with tempfile.TemporaryDirectory() as copy:
                crate = copy_with(planted, Path(copy))
                reports = check_probes(build(crate, target, args.target_dir / target / "planted"), reaching)
            for probe in reaching:
                caught = any("return reached unchecked" in line for line in reports[probe])
                print(f"{target} {probe}, planted {planted}: {'flagged' if caught else 'MISSED'}")
                failed |= not caught
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
