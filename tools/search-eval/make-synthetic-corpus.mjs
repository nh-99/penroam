#!/usr/bin/env node
// Generates a synthetic handwriting corpus for the search evaluation
// (crates/penroam-search/examples/eval.rs). Pages are written with single-stroke Hershey
// fonts, distorted per page, word and sample (slant, size, baseline drift, wobble, timing),
// and stored as Penroam page JSON with the ground truth of every line and word.
//
// This is a stand-in for plumbing and regression checks only. It is not real handwriting:
// accuracy on it says little about accuracy on people's notes. Real measurements need a
// consented corpus in the same format (docs/development.md, "Search evaluation").
//
// The font data is not part of this repository. Get `hersheytext.json` from the MIT-licensed
// `hersheytext` npm package (`npm pack hersheytext`, then extract) and pass its path:
//
//   node tools/search-eval/make-synthetic-corpus.mjs --font <hersheytext.json> --out <dir>
//        [--pages 10] [--seed 1]
//
// Output: <dir>/corpus.json  { generator, seed, pages: [{ id, style, page, lines, }], distractors }
//   lines: [{ text, bounds: {x,y,w,h}, words: [{ text, strokeIds, bounds }] }]
import { readFileSync, mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const args = Object.fromEntries(
  process.argv.slice(2).reduce((acc, a, i, all) => (a.startsWith("--") ? [...acc, [a.slice(2), all[i + 1]]] : acc), []),
);
if (!args.font || !args.out) {
  console.error("usage: make-synthetic-corpus.mjs --font <hersheytext.json> --out <dir> [--pages N] [--seed S]");
  process.exit(2);
}
const PAGES = Number(args.pages ?? 10);
const SEED = Number(args.seed ?? 1);
const fonts = JSON.parse(readFileSync(args.font, "utf8"));
const STYLES = { print: fonts.futural, cursive: fonts.scripts };

// deterministic PRNG (mulberry32)
let state = SEED >>> 0;
function rand() {
  state = (state + 0x6d2b79f5) >>> 0;
  let t = state;
  t = Math.imul(t ^ (t >>> 15), t | 1);
  t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
  return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
}
const between = (a, b) => a + (b - a) * rand();
const pick = (xs) => xs[Math.floor(rand() * xs.length)];

// everyday, technical and proper-noun vocabulary (lowercase; some lines are capitalised)
const WORDS = `meeting budget review project deadline quarterly invoice schedule notes agenda follow
up call email draft report summary design sketch idea question answer problem solution kernel
latency throughput database index query vector stroke pressure tablet battery charger window
monday tuesday friday morning evening tomorrow yesterday weekend holiday travel flight hotel
coffee lunch dinner groceries milk bread apples pharmacy doctor dentist appointment garden
river mountain forest library museum concert ticket birthday present family friends alice
bob carol london paris berlin tokyo toronto chapter lecture exam homework theorem proof lemma
integral matrix vertex graph tree sorting compiler rust python server client protocol cache
memory thread signal network router printer scanner camera photo album recipe oven salt pepper
sugar flour butter cheese tomato onion garlic paint brush canvas pencil marker eraser paper`
  .split(/\s+/)
  .filter(Boolean);
const DISTRACTORS = `zebra quokka saxophone glacier volcano xylophone trombone avalanche tangerine
pomegranate submarine labyrinth hurricane porcupine walrus nebula`
  .split(/\s+/)
  .filter(Boolean);

function parsePath(d) {
  // "M x,y L x,y x,y M ..." -> [[[x,y],...], ...]
  const strokes = [];
  let cur = null;
  for (const tok of d.trim().split(/\s+/)) {
    let t = tok;
    if (t[0] === "M") {
      cur = [];
      strokes.push(cur);
      t = t.slice(1);
    } else if (t[0] === "L") t = t.slice(1);
    if (!t) continue;
    const [x, y] = t.split(",").map(Number);
    cur.push([x, y]);
  }
  return strokes.filter((s) => s.length > 0);
}

// Glyph table: index = char code - 33 ("!" first). Coordinates are font units, y down,
// baseline at y = 22, cap height 21; a glyph's advance is about twice its `o` value.
const BASELINE = 22;
function glyph(font, ch) {
  const g = font.chars[ch.charCodeAt(0) - 33];
  return g ? { strokes: parsePath(g.d || ""), width: Number(g.o) * 2 } : { strokes: [], width: 10 };
}

/** Resample a polyline at ~`step` spacing (duplicates for single-point dots). */
function resample(pts, step) {
  if (pts.length === 1) return [pts[0], [pts[0][0] + 0.3, pts[0][1] + 0.3]];
  const out = [pts[0]];
  for (let i = 1; i < pts.length; i++) {
    const [ax, ay] = pts[i - 1], [bx, by] = pts[i];
    const n = Math.max(1, Math.ceil(Math.hypot(bx - ax, by - ay) / step));
    for (let k = 1; k <= n; k++) out.push([ax + ((bx - ax) * k) / n, ay + ((by - ay) * k) / n]);
  }
  return out;
}

let idCounter = 0;
const uuid = () => {
  idCounter++;
  const hex = (SEED * 1e6 + idCounter).toString(16).padStart(12, "0");
  return `00000000-0000-4000-8000-${hex.slice(-12)}`;
};

function makePage(style, pageNo) {
  const font = STYLES[style];
  const page = {
    schemaVersion: 1,
    id: uuid(),
    paper: "letter",
    size: { width: 612, height: 792 },
    template: { kind: "ruled", spacing: 24 },
    revision: 1,
    createdAt: 0,
    updatedAt: 0,
    elements: [],
  };
  const lines = [];
  const slant = between(-0.1, 0.3); // shear: x += slant * (baseline - y)
  const size = between(0.65, 0.95); // pt per font unit
  let t = 0;
  let z = 0;
  for (let y = 108, row = 0; y < 720; y += between(34, 44), row++) {
    const nWords = 2 + Math.floor(rand() * 3);
    const words = Array.from({ length: nWords }, () => pick(WORDS));
    if (rand() < 0.3) words[0] = words[0][0].toUpperCase() + words[0].slice(1);
    let x = 72 + between(-4, 8);
    const baseline = y;
    const line = { text: words.join(" "), words: [] };
    for (const w of words) {
      const ws = size * between(0.9, 1.1);
      const wb = baseline + between(-1.5, 1.5);
      const word = { text: w, strokeIds: [], bounds: null };
      for (const ch of w) {
        const g = glyph(font, ch);
        for (const s of g.strokes) {
          const pts = resample(
            s.map(([gx, gy]) => {
              const lx = x + gx * ws;
              const ly = wb + (gy - BASELINE) * ws;
              return [lx + slant * (wb - ly), ly];
            }),
            1.0,
          );
          const phase = rand() * 6.28;
          const points = pts.map(([px, py], i) => {
            t += 6600; // ~150 pt/s at 1 pt spacing
            return {
              x: +(px + 0.5 * Math.sin(i / 9 + phase)).toFixed(2),
              y: +(py + 0.5 * Math.cos(i / 11 + phase)).toFixed(2),
              pressure: +between(0.45, 0.7).toFixed(3),
              tiltX: 0,
              tiltY: 0,
              timeUs: t,
            };
          });
          t += 120000; // pen up between strokes
          const id = uuid();
          page.elements.push({ type: "ink", id, z: ++z, tool: "pen", color: "#1f2937", opacity: 1, width: 1.6, pressure: true, source: "synthetic", input: { generator: "hershey", style }, points });
          word.strokeIds.push(id);
          const xs = points.map((p) => p.x), ys = points.map((p) => p.y);
          const b = { x0: Math.min(...xs), y0: Math.min(...ys), x1: Math.max(...xs), y1: Math.max(...ys) };
          word.bounds = word.bounds
            ? { x0: Math.min(word.bounds.x0, b.x0), y0: Math.min(word.bounds.y0, b.y0), x1: Math.max(word.bounds.x1, b.x1), y1: Math.max(word.bounds.y1, b.y1) }
            : b;
        }
        // cursive letters are drawn closer so their joining strokes meet
        x += g.width * ws * (style === "cursive" ? 0.85 : 1) * between(0.95, 1.08);
      }
      x += between(9, 16); // word gap
      if (word.bounds) line.words.push(word);
      t += 300000;
    }
    if (x > 560) {
      // line ran off the right margin: drop it (keeps ground truth honest)
      const drop = new Set(line.words.flatMap((w) => w.strokeIds));
      page.elements = page.elements.filter((e) => !drop.has(e.id));
      continue;
    }
    const bs = line.words.map((w) => w.bounds);
    const lb = { x0: Math.min(...bs.map((b) => b.x0)), y0: Math.min(...bs.map((b) => b.y0)), x1: Math.max(...bs.map((b) => b.x1)), y1: Math.max(...bs.map((b) => b.y1)) };
    const rect = (b) => ({ x: b.x0, y: b.y0, w: b.x1 - b.x0, h: b.y1 - b.y0 });
    lines.push({ text: line.text, bounds: rect(lb), words: line.words.map((w) => ({ text: w.text, strokeIds: w.strokeIds, bounds: rect(w.bounds) })) });
  }
  return { id: `${style}-${pageNo}`, style, page, lines };
}

const pages = [];
for (const style of Object.keys(STYLES)) for (let i = 0; i < PAGES; i++) pages.push(makePage(style, i + 1));
mkdirSync(args.out, { recursive: true });
writeFileSync(
  join(args.out, "corpus.json"),
  JSON.stringify({ generator: "make-synthetic-corpus.mjs (Hershey single-stroke fonts, synthetic)", seed: SEED, pages, distractors: DISTRACTORS }),
);
const lineCount = pages.reduce((n, p) => n + p.lines.length, 0);
console.log(`wrote ${pages.length} pages, ${lineCount} lines to ${join(args.out, "corpus.json")}`);
