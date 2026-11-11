#!/usr/bin/env node
// Extracts advance widths (per 1000 em) for the code points Penroam's text
// layout measures, from the bundled Liberation faces (metric-compatible with
// Helvetica, its bold and oblique cuts, and Courier) and from Excalifont, the
// handwritten font (rebuilt by tools/merge-excalifont.py). Output is the single
// source of truth used by both the Rust text layout (penroam-core) and the
// TypeScript renderer.
//
//   node tools/gen-font-metrics.mjs apps/desktop-mobile/src/assets/fonts \
//        crates/penroam-core/data/text-metrics.json
import { readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

// Basic Latin + Latin-1 + the extra Windows-1252 characters (the PDF export
// uses WinAnsiEncoding for the standard fonts).
const cps = [];
for (let c = 0x20; c <= 0x7e; c++) cps.push(c);
for (let c = 0xa0; c <= 0xff; c++) cps.push(c);
cps.push(0x152, 0x153, 0x160, 0x161, 0x178, 0x17d, 0x17e, 0x192, 0x2c6, 0x2dc, 0x2013, 0x2014, 0x2018, 0x2019,
  0x201a, 0x201c, 0x201d, 0x201e, 0x2020, 0x2021, 0x2022, 0x2026, 0x2030, 0x2039, 0x203a, 0x20ac, 0x2122);

/** Reads the hhea metrics and the widths of `cps` from one TrueType file. */
function readFace(src) {
  const buf = readFileSync(src);
  const u16 = (o) => buf.readUInt16BE(o);
  const i16 = (o) => buf.readInt16BE(o);
  const u32 = (o) => buf.readUInt32BE(o);
  const tables = {};
  for (let i = 0, n = u16(4); i < n; i++) {
    const r = 12 + i * 16;
    tables[buf.toString("latin1", r, r + 4)] = u32(r + 8);
  }
  const unitsPerEm = u16(tables.head + 18);
  const ascender = i16(tables.hhea + 4);
  const descender = i16(tables.hhea + 6);
  const numHMetrics = u16(tables.hhea + 34);
  const advance = (gid) => u16(tables.hmtx + 4 * Math.min(gid, numHMetrics - 1));

  // cmap format 4 (BMP)
  let sub = -1;
  for (let i = 0, n = u16(tables.cmap + 2); i < n; i++) {
    const r = tables.cmap + 4 + i * 8;
    const pid = u16(r), eid = u16(r + 2);
    if (pid === 3 && eid === 1) sub = tables.cmap + u32(r + 4);
  }
  if (sub < 0 || u16(sub) !== 4) throw new Error("no cmap (3,1) format 4");
  const segX2 = u16(sub + 6);
  const ends = sub + 14, starts = ends + segX2 + 2, deltas = starts + segX2, ranges = deltas + segX2;
  function glyph(cp) {
    for (let s = 0; s < segX2 / 2; s++) {
      const end = u16(ends + 2 * s);
      if (cp > end) continue;
      const start = u16(starts + 2 * s);
      if (cp < start) return 0;
      const delta = i16(deltas + 2 * s), ro = u16(ranges + 2 * s);
      if (ro === 0) return (cp + delta) & 0xffff;
      const g = u16(ranges + 2 * s + ro + 2 * (cp - start));
      return g === 0 ? 0 : (g + delta) & 0xffff;
    }
    return 0;
  }
  const widths = {};
  for (const cp of cps) {
    const g = glyph(cp);
    if (g) widths[cp] = Math.round((advance(g) * 1000) / unitsPerEm);
  }
  return { unitsPerEm, ascender, descender, widths };
}

const [dir, out] = process.argv.slice(2);
const face = (file) => readFace(join(dir, file));
const regular = face("LiberationSans-Regular.ttf");
const hand = face("Excalifont-Regular.ttf");
const em = (v, f) => Math.round((v * 1000) / f.unitsPerEm);
const doc = {
  font: "Liberation Sans Regular (metric-compatible with Helvetica)",
  unitsPerEm: 1000,
  ascent: Math.round((regular.ascender * 1000) / regular.unitsPerEm),
  descent: Math.round((regular.descender * 1000) / regular.unitsPerEm),
  lineHeight: 1250,
  firstBaseline: 970,
  fallbackWidth: 600,
  widths: regular.widths,
  // The other faces share the vertical metrics above: a line's height comes
  // from its size, never its style.
  faces: {
    bold: face("LiberationSans-Bold.ttf").widths,
    italic: face("LiberationSans-Italic.ttf").widths,
    boldItalic: face("LiberationSans-BoldItalic.ttf").widths,
    mono: face("LiberationMono-Regular.ttf").widths,
  },
  // The handwritten font (Excalifont). It has a single face: bold and italic are
  // drawn from it by thickening and slanting, so they share these widths. Text
  // it has no glyph for is set in the faces above. Line heights stay those of
  // the faces above; its own extents only matter for how far ink reaches.
  hand: {
    font: "Excalifont Regular",
    ascent: em(hand.ascender, hand),
    descent: em(hand.descender, hand),
    widths: hand.widths,
  },
};
writeFileSync(out, JSON.stringify(doc, null, 1) + "\n");
console.log(`wrote ${out}: ${Object.keys(regular.widths).length} widths, ascent ${doc.ascent}, descent ${doc.descent}`);
