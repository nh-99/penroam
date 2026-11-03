#!/usr/bin/env python3
"""Semantic comparison of two .penroam archives (two-OS round-trip check).

    python3 tools/compare-penroam.py a.penroam b.penroam

Compares page order, page size/template/PDF background, and every element:
ink strokes point-by-point (x, y, pressure, tilt, timeUs, twist, flags),
brush/tool/colour/width/opacity, input metadata, text blocks (text, box,
font size, colour, alignment) and asset bytes (SHA-256). Exit code 1 on any
difference. Revisions/timestamps are reported but not required to match.
"""
import hashlib, json, struct, sys, zipfile, zlib


def decode_prp(b):
    """Independent decoder for the binary page encoding (docs/formats/penroam-binary-v1.md §2–§4)."""
    if b[:4] != b"PRPG":
        raise ValueError("not a PRPG page")
    if zlib.crc32(b[:-4]) != struct.unpack("<I", b[-4:])[0]:
        raise ValueError("checksum mismatch")
    version, flags, mlen = struct.unpack_from("<HHI", b, 4)
    if version != 1:
        raise ValueError(f"unsupported page version {version}")
    meta = b[12:12 + mlen]
    page = json.loads(zlib.decompress(meta) if flags & 1 else meta)
    i = 12 + mlen

    def varint():
        nonlocal i
        v = s = 0
        while True:
            c = b[i]; i += 1
            v |= (c & 0x7F) << s
            s += 7
            if c < 0x80:
                return v

    def zz():
        v = varint()
        return (v >> 1) ^ -(v & 1)

    def floats(n):
        nonlocal i
        tag = b[i]; i += 1
        if tag == 0:
            v = struct.unpack_from("<d", b, i)[0]; i += 8
            return [v] * n
        if tag == 1:
            k = b[i]; i += 1
            q, out = 0, []
            for _ in range(n):
                q += zz(); out.append(q / 10.0 ** k)
            return out
        out = list(struct.unpack_from(f"<{n}d", b, i)); i += 8 * n
        return out

    def ints(n):
        nonlocal i
        tag = b[i]; i += 1
        if tag == 0:
            return [zz()] * n
        v, out = 0, []
        for _ in range(n):
            v += zz(); out.append(v)
        return out

    def presence(n):
        nonlocal i
        tag = b[i]; i += 1
        if tag < 2:
            return [tag == 1] * n
        bits = b[i:i + (n + 7) // 8]; i += (n + 7) // 8
        return [bool(bits[j // 8] >> (j % 8) & 1) for j in range(n)]

    count = struct.unpack_from("<I", b, i)[0]; i += 4
    inks = [e for e in page["elements"] if e["type"] == "ink"]
    assert len(inks) == count
    for e in inks:
        n = varint()
        x, y, p, tx, ty = (floats(n) for _ in range(5))
        t = ints(n)
        twp = presence(n); tw = iter(floats(sum(twp)) if any(twp) else [])
        flp = presence(n); fl = iter(ints(sum(flp)) if any(flp) else [])
        pts = []
        for j in range(n):
            q = {"x": x[j], "y": y[j], "pressure": p[j], "tiltX": tx[j], "tiltY": ty[j], "timeUs": t[j]}
            if twp[j]: q["twist"] = next(tw)
            if flp[j]: q["flags"] = next(fl)
            pts.append(q)
        e["points"] = pts
    assert i == len(b) - 4
    return page


def load_page(data):
    return decode_prp(data) if data[:4] == b"PRPG" else json.loads(data)


def load(path):
    z = zipfile.ZipFile(path)
    m = json.loads(z.read("manifest.json"))
    pages = {p["id"]: load_page(z.read(p["path"])) for p in m["pages"]}
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
