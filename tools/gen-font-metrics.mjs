#!/usr/bin/env node
// Extracts advance widths (per 1000 em) for the code points Penroam's text
// layout measures, from the bundled Liberation Sans Regular (metric-compatible
// with Helvetica/Arial). Output is the single source of truth used by both the
// Rust text layout (penroam-core) and the TypeScript renderer.
//
//   node tools/gen-font-metrics.mjs apps/desktop-mobile/src/assets/fonts/LiberationSans-Regular.ttf \
//        crates/penroam-core/data/text-metrics.json
import { readFileSync, writeFileSync } from "node:fs";

const [src, out] = process.argv.slice(2);
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
// Basic Latin + Latin-1 + the extra Windows-1252 characters (the PDF export
// uses WinAnsiEncoding for the standard Helvetica font).
const cps = [];
for (let c = 0x20; c <= 0x7e; c++) cps.push(c);
for (let c = 0xa0; c <= 0xff; c++) cps.push(c);
cps.push(0x152, 0x153, 0x160, 0x161, 0x178, 0x17d, 0x17e, 0x192, 0x2c6, 0x2dc, 0x2013, 0x2014, 0x2018, 0x2019,
  0x201a, 0x201c, 0x201d, 0x201e, 0x2020, 0x2021, 0x2022, 0x2026, 0x2030, 0x2039, 0x203a, 0x20ac, 0x2122);
for (const cp of cps) {
  const g = glyph(cp);
  if (g) widths[cp] = Math.round((advance(g) * 1000) / unitsPerEm);
}
const doc = {
  font: "Liberation Sans Regular (metric-compatible with Helvetica)",
  unitsPerEm: 1000,
  ascent: Math.round((ascender * 1000) / unitsPerEm),
  descent: Math.round((descender * 1000) / unitsPerEm),
  lineHeight: 1250,
  firstBaseline: 970,
  fallbackWidth: 600,
  widths,
};
writeFileSync(out, JSON.stringify(doc, null, 1) + "\n");
console.log(`wrote ${out}: ${Object.keys(widths).length} widths, ascent ${doc.ascent}, descent ${doc.descent}`);
