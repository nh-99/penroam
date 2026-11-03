#!/usr/bin/env node
// Deterministic generator for the shared stroke, geometry and scene fixtures.
//
//   node fixtures/generate-fixtures.mjs          # writes everything
//   node fixtures/generate-fixtures.mjs --check  # verifies files on disk match
//
// This file is the *reference implementation* for three things that every
// framework port (TypeScript, Dart, Kotlin) must reproduce exactly:
//
//   1. mulberry32 PRNG + benchmark-scene generator  (scene-<N>.json)
//   2. pressure -> width mapping                    (widthFor)
//   3. midpoint-quadratic stroke geometry           (strokePieces)
//
// The ports are checked against fixtures/geometry-golden.json and
// fixtures/benchmark-scenes/scene-100.json in each app's test suite, so the
// apps render identical geometry from identical data.

import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, writeFileSync, existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));

// ---------------------------------------------------------------- PRNG ----
export function mulberry32(seed) {
  let a = seed >>> 0;
  return function next() {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const round2 = (v) => Math.round(v * 100) / 100;
const round3 = (v) => Math.round(v * 1000) / 1000;
const clamp = (v, lo, hi) => (v < lo ? lo : v > hi ? hi : v);

export const SCENE_WIDTH = 1600;
export const SCENE_HEIGHT = 1200;
export const SCENE_WIDTHS = [1.5, 3.0, 6.0];

// ------------------------------------------------------ scene generator ----
export function generateScene(strokeCount) {
  const rnd = mulberry32(strokeCount);
  const strokes = [];
  for (let i = 0; i < strokeCount; i++) {
    let x = 40 + rnd() * (SCENE_WIDTH - 80);
    let y = 40 + rnd() * (SCENE_HEIGHT - 80);
    const n = 8 + Math.floor(rnd() * 40);
    let vx = (rnd() - 0.5) * 6;
    let vy = (rnd() - 0.5) * 6;
    let p = 0.3 + 0.5 * rnd();
    const width = SCENE_WIDTHS[Math.floor(rnd() * 3)];
    const points = [];
    for (let j = 0; j < n; j++) {
      if (j > 0) {
        vx = vx * 0.85 + (rnd() - 0.5) * 3;
        vy = vy * 0.85 + (rnd() - 0.5) * 3;
        x = clamp(x + vx, 0, SCENE_WIDTH);
        y = clamp(y + vy, 0, SCENE_HEIGHT);
        p = p * 0.7 + 0.3 * (0.2 + 0.7 * rnd());
      }
      points.push({
        x: round2(x),
        y: round2(y),
        pressure: round3(p),
        tiltX: 0,
        tiltY: 0,
        timeUs: j * 4000,
      });
    }
    strokes.push({
      id: `scene${strokeCount}-${i}`,
      source: "fixture",
      input: { api: "generated", device: "none" },
      brush: { type: "pen", width, color: "#000000", pressure: true },
      points,
    });
  }
  return {
    format: "m00-strokes",
    version: 1,
    canvas: { width: SCENE_WIDTH, height: SCENE_HEIGHT },
    meta: { generator: "fixtures/generate-fixtures.mjs", seed: strokeCount },
    strokes,
  };
}

// ----------------------------------------------------- brush geometry ----
// Width in document units for a sample with normalized pressure p (0..1).
// p = 0.5 yields exactly the nominal brush width; range 0.4x .. 1.6x.
export function widthFor(baseWidth, pressure, pressureEnabled) {
  if (!pressureEnabled) return baseWidth;
  const p = clamp(pressure, 0, 1);
  return Math.max(0.5, baseWidth * (0.4 + 1.2 * p));
}

// Splits a stroke into drawable pieces using midpoint quadratic smoothing.
//   n == 1 : one dot (filled circle, radius = width/2)
//   n == 2 : one line
//   n >= 3 : line P0->M01, quads (M(i-1,i) --Pi--> M(i,i+1)) for i=1..n-2,
//            line M(n-2,n-1)->P(n-1)
// Each piece carries the width of the sample it is anchored to. Pieces are
// stroked with round caps/joins. Piece k only depends on points <= k+1, so
// when point k+1 arrives, pieces 0..k-1 are final (incremental rendering).
export function strokePieces(points, baseWidth, pressureEnabled) {
  const n = points.length;
  const w = (pt) => widthFor(baseWidth, pt.pressure, pressureEnabled);
  const mid = (a, b) => ({ x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 });
  if (n === 0) return [];
  if (n === 1) return [{ kind: "dot", x: points[0].x, y: points[0].y, width: w(points[0]) }];
  if (n === 2) {
    return [{ kind: "line", x0: points[0].x, y0: points[0].y, x1: points[1].x, y1: points[1].y, width: w(points[0]) }];
  }
  const out = [];
  const m01 = mid(points[0], points[1]);
  out.push({ kind: "line", x0: points[0].x, y0: points[0].y, x1: m01.x, y1: m01.y, width: w(points[0]) });
  for (let i = 1; i <= n - 2; i++) {
    const a = mid(points[i - 1], points[i]);
    const b = mid(points[i], points[i + 1]);
    out.push({ kind: "quad", x0: a.x, y0: a.y, cx: points[i].x, cy: points[i].y, x1: b.x, y1: b.y, width: w(points[i]) });
  }
  const last = mid(points[n - 2], points[n - 1]);
  out.push({ kind: "line", x0: last.x, y0: last.y, x1: points[n - 1].x, y1: points[n - 1].y, width: w(points[n - 1]) });
  return out;
}

// ------------------------------------------------------ sample strokes ----
function sampleStrokes() {
  const mk = (id, source, api, width, pressure, pts) => ({
    id,
    source,
    input: { api, device: source === "mouse" ? "mouse" : "fixture-stylus" },
    brush: { type: "pen", width, color: "#000000", pressure },
    points: pts.map(([x, y, p, t]) => ({ x, y, pressure: p, tiltX: 0, tiltY: 0, timeUs: t })),
  });
  // A small "hello"-ish set: dot, straight line, curve with rising pressure,
  // a closed loop, and a constant-width mouse stroke.
  const curve = [];
  for (let i = 0; i <= 24; i++) {
    const t = i / 24;
    curve.push([round2(100 + t * 300), round2(300 + 60 * (4 * t * (1 - t)) * (i % 2 ? 1 : 0.92)), round3(0.1 + 0.85 * t), i * 4000]);
  }
  const loop = [];
  // closed loop approximated with a fixed polygon (no trig => portable)
  const poly = [[600, 200], [660, 180], [710, 210], [730, 270], [700, 330], [640, 350], [585, 320], [565, 260], [600, 200]];
  poly.forEach(([x, y], i) => loop.push([x, y, 0.5, i * 8000]));
  return {
    format: "m00-strokes",
    version: 1,
    canvas: { width: SCENE_WIDTH, height: SCENE_HEIGHT },
    meta: { generator: "fixtures/generate-fixtures.mjs", description: "hand-authored sample strokes" },
    strokes: [
      mk("sample-dot", "stylus", "fixture", 3.0, true, [[80, 80, 0.62, 0]]),
      mk("sample-line", "stylus", "fixture", 3.0, true, [[100, 120, 0.5, 0], [500, 140, 0.5, 16000]]),
      mk("sample-curve", "stylus", "fixture", 6.0, true, curve),
      mk("sample-loop", "stylus", "fixture", 1.5, true, loop),
      mk("sample-mouse", "mouse", "fixture", 3.0, false, [[100, 500, 0.5, 0], [180, 520, 0.5, 8000], [260, 480, 0.5, 16000], [340, 530, 0.5, 24000]]),
    ],
  };
}

// ------------------------------------------------------------ outputs ----
function goldenFor(sample) {
  return {
    description:
      "Expected strokePieces() output for fixtures/sample-strokes.json. Every port must match within 1e-9.",
    widthCases: [0, 0.25, 0.5, 0.75, 1, 1.5, -1].flatMap((p) =>
      [1.5, 3, 6].flatMap((base) => [
        { base, pressure: p, pressureEnabled: true, width: widthFor(base, p, true) },
        { base, pressure: p, pressureEnabled: false, width: widthFor(base, p, false) },
      ]),
    ),
    strokes: sample.strokes.map((s) => ({
      id: s.id,
      pieces: strokePieces(s.points, s.brush.width, s.brush.pressure),
    })),
  };
}

const SCENE_SIZES = [100, 1000, 5000, 10000];

function outputs() {
  const sample = sampleStrokes();
  const files = {
    "sample-strokes.json": JSON.stringify(sample, null, 2) + "\n",
    "geometry-golden.json": JSON.stringify(goldenFor(sample), null, 2) + "\n",
  };
  for (const n of SCENE_SIZES) {
    files[`benchmark-scenes/scene-${n}.json`] = JSON.stringify(generateScene(n)) + "\n";
  }
  return files;
}

function main() {
  const check = process.argv.includes("--check");
  const files = outputs();
  const manifest = {};
  let bad = 0;
  for (const [rel, text] of Object.entries(files)) {
    const abs = join(here, rel);
    const sha = createHash("sha256").update(text).digest("hex");
    manifest[rel] = { sha256: sha, bytes: Buffer.byteLength(text) };
    if (check) {
      const ok = existsSync(abs) && readFileSync(abs, "utf8") === text;
      if (!ok) bad++;
      console.log(`${ok ? "ok  " : "DIFF"} ${rel}`);
    } else {
      mkdirSync(dirname(abs), { recursive: true });
      writeFileSync(abs, text);
      console.log(`wrote ${rel} (${manifest[rel].bytes} bytes)`);
    }
  }
  if (!check) writeFileSync(join(here, "manifest.json"), JSON.stringify(manifest, null, 2) + "\n");
  if (bad) process.exit(1);
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) main();
