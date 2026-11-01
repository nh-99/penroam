#!/usr/bin/env python3
"""Semantic comparison of two .penroam archives (two-OS round-trip check).

    python3 tools/compare-penroam.py a.penroam b.penroam

Compares page order, page size/template/PDF background, and every element:
ink strokes point-by-point (x, y, pressure, tilt, timeUs, twist, flags),
brush/tool/colour/width/opacity, input metadata, text blocks (text, box,
font size, colour, alignment) and asset bytes (SHA-256). Exit code 1 on any
difference. Revisions/timestamps are reported but not required to match.
"""
import hashlib, json, sys, zipfile


def load(path):
    z = zipfile.ZipFile(path)
    m = json.loads(z.read("manifest.json"))
    pages = {p["id"]: json.loads(z.read(p["path"])) for p in m["pages"]}
    assets = {a["sha256"]: hashlib.sha256(z.read(a["path"])).hexdigest() for a in m["assets"]}
    return m, pages, assets


def main():
    a, b = sys.argv[1], sys.argv[2]
    ma, pa, aa = load(a)
    mb, pb, ab = load(b)
    diffs = []
    oa, ob = ma["notebook"]["pageOrder"], mb["notebook"]["pageOrder"]
    if oa != ob:
        diffs.append(f"page order differs: {oa} vs {ob}")
    for pid in oa:
        x, y = pa.get(pid), pb.get(pid)
        if y is None:
            diffs.append(f"page {pid} missing in {b}")
            continue
        for k in ("size", "paper", "template", "background"):
            if x.get(k) != y.get(k):
                diffs.append(f"page {pid} {k}: {x.get(k)} vs {y.get(k)}")
        ex = {e["id"]: e for e in x["elements"]}
        ey = {e["id"]: e for e in y["elements"]}
        if [e["id"] for e in x["elements"]] != [e["id"] for e in y["elements"]]:
            diffs.append(f"page {pid}: element ids/order differ ({len(ex)} vs {len(ey)})")
        for eid, e in ex.items():
            f = ey.get(eid)
            if f is None:
                continue
            if e != f:
                keys = sorted(k for k in set(e) | set(f) if e.get(k) != f.get(k))
                diffs.append(f"page {pid} element {eid} ({e.get('type')}): fields differ {keys}")
    if aa != ab:
        diffs.append(f"assets differ: {sorted(aa)} vs {sorted(ab)}")
    n_ink = sum(1 for p in pa.values() for e in p["elements"] if e["type"] == "ink")
    n_pts = sum(len(e["points"]) for p in pa.values() for e in p["elements"] if e["type"] == "ink")
    n_txt = sum(1 for p in pa.values() for e in p["elements"] if e["type"] == "text")
    print(f"{a}: generator={ma['generator']} pages={len(oa)} strokes={n_ink} points={n_pts} text={n_txt} assets={len(aa)}")
    print(f"{b}: generator={mb['generator']} pages={len(ob)}")
    if diffs:
        print("DIFFERENCES:")
        for d in diffs:
            print("  " + d)
        sys.exit(1)
    print("IDENTICAL (semantic content, all point fields, text layout inputs, asset bytes)")


if __name__ == "__main__":
    main()
