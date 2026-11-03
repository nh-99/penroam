#!/usr/bin/env python3
"""Converts a Hershey SVG font (e.g. Inkscape's svg_fonts/HersheyScript1.svg)
into the compact JSON stroke font used by the demo library generator.

    python3 tools/screenshots/extract_hershey.py HersheyScript1.svg tools/screenshots/hershey-script.json

Output: {"license": [...], "units_per_em": N, "glyphs": {"a": {"adv": 378,
"strokes": [[[x, y], ...], ...]}}} with y pointing up (SVG font space).
"""
import json, re, sys
import xml.etree.ElementTree as ET

ACK = [
    "The Hershey Fonts were originally created by Dr. A. V. Hershey while working at the U. S. National Bureau of Standards.",
    "The format of the Font data in this distribution was originally created by James Hurt, Cognition, Inc., 900 Technology Park Drive, Billerica, MA 01821 (mit-eddie!ci-dandelion!hurt).",
    "SVG font conversion by Windell H. Oskay (www.evilmadscientist.com), adapted from emergent.unpythonic.net/software/hershey via 'Hershey Fonts in SVG' by Marty McGuire.",
]

def strokes(d):
    out, cur = [], None
    for cmd, x, y in re.findall(r"([ML])\s*(-?[\d.]+)[\s,]+(-?[\d.]+)", d):
        p = [round(float(x), 2), round(float(y), 2)]
        if cmd == "M":
            cur = [p]
            out.append(cur)
        else:
            cur.append(p)
    return [s for s in out if len(s) > 1]

def main(src, dst):
    ns = {"s": "http://www.w3.org/2000/svg"}
    root = ET.parse(src).getroot()
    font = root.find(".//s:font", ns)
    face = root.find(".//s:font-face", ns)
    glyphs = {}
    for g in root.iter("{http://www.w3.org/2000/svg}glyph"):
        u = g.get("unicode")
        if u is None or len(u) != 1:
            continue
        glyphs[u] = {"adv": float(g.get("horiz-adv-x", font.get("horiz-adv-x"))),
                     "strokes": strokes(g.get("d", ""))}
    json.dump({"license": ACK, "units_per_em": float(face.get("units-per-em", 1000)),
               "ascent": float(face.get("ascent", 800)), "glyphs": glyphs},
              open(dst, "w"), separators=(",", ":"))
    print(f"{len(glyphs)} glyphs -> {dst}")

if __name__ == "__main__":
    main(*sys.argv[1:3])
