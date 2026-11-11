#!/usr/bin/env python3
"""Rebuilds apps/desktop-mobile/src/assets/fonts/Excalifont-Regular.ttf.

Upstream ships Excalifont only as Unicode-range WOFF2 subsets
(excalidraw/excalidraw, packages/excalidraw/fonts/Excalifont). Penroam needs one
TrueType file: the text layout reads its advance widths
(tools/gen-font-metrics.mjs) and the PDF export embeds it. This merges the
subsets back together without touching the glyphs.

    pip install fonttools brotli
    python tools/merge-excalifont.py <dir with the .woff2 subsets> <out.ttf>
"""
import glob
import os
import sys
import tempfile

from fontTools.merge import Merger
from fontTools.ttLib import TTFont

src, out = sys.argv[1], sys.argv[2]
with tempfile.TemporaryDirectory() as tmp:
    parts = []
    for i, path in enumerate(sorted(glob.glob(os.path.join(src, "*.woff2")))):
        font = TTFont(path)
        font.flavor = None
        part = os.path.join(tmp, f"part{i}.ttf")
        font.save(part)
        parts.append(part)
    if not parts:
        sys.exit(f"no .woff2 files in {src}")
    Merger().merge(parts).save(out)
print(f"wrote {out}: {len(TTFont(out).getBestCmap())} code points")
