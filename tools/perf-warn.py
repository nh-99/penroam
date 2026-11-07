#!/usr/bin/env python3
"""Warns when a Workload D run is much slower than the baseline. It never fails a build.

    python3 tools/perf-warn.py baseline.json current.json

CI runners vary from run to run, so this catches large regressions only. Numbers for a device
come from the method in docs/benchmarks/protocol.md, not from this script. Warnings use the GitHub
Actions annotation format, and exit status is 0 whatever the result.
"""
import json, sys

# (timing, statistic, smallest change worth reporting, in ms)
WATCHED = [
    ("startup_to_first_frame_ms", "median", 200),
    ("frame_interval_ms", "p95", 4),
    ("full_rebuild_ms", "median", 5),
    ("preview_render_submitted_ms", "p95", 2),
]
RATIO = 1.25


def regressions(baseline, current):
    """(timing, before, after) for each watched timing that got clearly worse."""
    found = []
    for name, stat, floor in WATCHED:
        try:
            before = baseline["metrics"]["timings"][name][stat]
            after = current["metrics"]["timings"][name][stat]
        except KeyError:
            continue
        if after > before * RATIO and after - before >= floor:
            found.append((name, before, after))
    return found


def main(argv):
    if len(argv) != 3:
        sys.exit(__doc__)
    try:
        baseline, current = (json.load(open(p)) for p in argv[1:])
    except (OSError, ValueError) as e:
        print(f"No comparison: {e}")
        return
    found = regressions(baseline, current)
    for name, before, after in found:
        print(f"::warning title=Performance::{name} went from {before:.1f} ms to {after:.1f} ms")
    if not found:
        print("No large change against the baseline.")


if __name__ == "__main__":
    main(sys.argv)
