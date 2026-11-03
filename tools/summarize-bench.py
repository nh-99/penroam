#!/usr/bin/env python3
"""Aggregates raw Workload D result files into a markdown table.

    python3 tools/summarize-bench.py benchmarks/results/linux > summary.md

Medians are taken across all trials of all sessions; raw files are never modified.
"""
import glob, json, os, statistics, sys

def med(xs):
    xs = [x for x in xs if x is not None]
    return statistics.median(xs) if xs else None

def fmt(v, unit=""):
    return "–" if v is None else (f"{v:.0f}{unit}" if abs(v) >= 10 else f"{v:.1f}{unit}")

def main(dirs):
    files = sorted(f for d in dirs for f in glob.glob(os.path.join(d, "perf-*-workload-d-*.json")))
    by_app = {}
    for f in files:
        d = json.load(open(f))
        by_app.setdefault(d["app"], []).append(d)
    print(f"Source files: {len(files)}\n")
    for app, sessions in sorted(by_app.items()):
        s0 = sessions[0]
        sysinfo = s0.get("system") or {}
        print(f"### {app} — {len(sessions)} session(s)\n")
        print(f"- system: `{json.dumps(sysinfo)}`")
        print(f"- viewport: `{json.dumps(s0.get('viewport'))}`")
        startups = [x.get("metrics", {}).get("info", {}).get("startupMs") for x in sessions]
        print(f"- startup (process start → first frame/IPC), per session ms: {[fmt(v) for v in startups]}\n")
        rows = {}
        for s in sessions:
            for r in s["results"]:
                rows.setdefault(r["strokes"], []).append(r)
        mem = lambda r: (r.get("treeRssBytes") or r.get("physFootprintBytes") or r.get("rssBytes") or 0) / 2**20
        if "replayIntervalMedianMs" in sessions[0]["results"][0]:
            print("| strokes | trials | generate ms | full redraw ms (GPU-flushed) | replay frame interval median/max ms | memory MB |")
            print("|---:|---:|---:|---:|---:|---:|")
            for n, rs in sorted(rows.items()):
                print(f"| {n} | {len(rs)} | {fmt(med([r['generateMs'] for r in rs]))} | {fmt(med([r['rebuildMs'] for r in rs]))} | "
                      f"{fmt(med([r['replayIntervalMedianMs'] for r in rs]))}/{fmt(med([r['replayIntervalMaxMs'] for r in rs]))} | "
                      f"{fmt(med([mem(r) for r in rs]))} |")
        elif "rebuildMs" in sessions[0]["results"][0]:
            mem_label = "app footprint MB (WebContent excluded)" if "wkwebview" in app else ("phys footprint MB" if "ios" in app else "process-tree RSS MB")
            print(f"| strokes | trials | generate ms | full redraw ms (flushed) | replay frame median/max ms | {mem_label} |")
            print("|---:|---:|---:|---:|---:|---:|")
            for n, rs in sorted(rows.items()):
                print(f"| {n} | {len(rs)} | {fmt(med([r['generateMs'] for r in rs]))} | {fmt(med([r['rebuildMs'] for r in rs]))} | "
                      f"{fmt(med([r['replayFrameMedianMs'] for r in rs]))}/{fmt(med([r['replayFrameMaxMs'] for r in rs]))} | "
                      f"{fmt(med([mem(r) for r in rs]))} |")
        else:
            print("| strokes | trials | generate ms | picture record ms | load→frame end ms | replay frame median/max ms | RSS MB |")
            print("|---:|---:|---:|---:|---:|---:|---:|")
            for n, rs in sorted(rows.items()):
                print(f"| {n} | {len(rs)} | {fmt(med([r['generateMs'] for r in rs]))} | {fmt(med([r['recordMs'] for r in rs]))} | "
                      f"{fmt(med([r['loadToFrameEndMs'] for r in rs]))} | {fmt(med([r['replayFrameMedianMs'] for r in rs]))}/{fmt(med([r['replayFrameMaxMs'] for r in rs]))} | "
                      f"{fmt(med([(r['rssBytes'] or 0)/2**20 for r in rs]))} |")
        fi = [x["metrics"]["timings"].get("frame_interval_ms") or x["metrics"]["timings"].get("frame_total_span_ms") for x in sessions]
        print(f"\nframe timing per session (median/p95/max ms): {[(fmt(f.get('median')), fmt(f.get('p95')), fmt(f.get('max'))) for f in fi if f]}\n")

if __name__ == "__main__":
    main(sys.argv[1:] or ["benchmarks/results/linux"])
