// Deterministic benchmark scenes. Must produce exactly
// fixtures/benchmark-scenes/scene-<N>.json (tests/scene.test.ts), so the app
// does not have to bundle the 11-22 MB fixture files.
import type { InkDocument, InkStroke } from "./model";

export function mulberry32(seed: number): () => number {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

export const SCENE_WIDTH = 1600;
export const SCENE_HEIGHT = 1200;
const WIDTHS = [1.5, 3.0, 6.0];
const round2 = (v: number) => Math.round(v * 100) / 100;
const round3 = (v: number) => Math.round(v * 1000) / 1000;
const clamp = (v: number, lo: number, hi: number) => (v < lo ? lo : v > hi ? hi : v);

export function generateScene(count: number): InkDocument {
  const rnd = mulberry32(count);
  const strokes: InkStroke[] = [];
  for (let i = 0; i < count; i++) {
    let x = 40 + rnd() * (SCENE_WIDTH - 80);
    let y = 40 + rnd() * (SCENE_HEIGHT - 80);
    const n = 8 + Math.floor(rnd() * 40);
    let vx = (rnd() - 0.5) * 6;
    let vy = (rnd() - 0.5) * 6;
    let p = 0.3 + 0.5 * rnd();
    const width = WIDTHS[Math.floor(rnd() * 3)];
    const points = [];
    for (let j = 0; j < n; j++) {
      if (j > 0) {
        vx = vx * 0.85 + (rnd() - 0.5) * 3;
        vy = vy * 0.85 + (rnd() - 0.5) * 3;
        x = clamp(x + vx, 0, SCENE_WIDTH);
        y = clamp(y + vy, 0, SCENE_HEIGHT);
        p = p * 0.7 + 0.3 * (0.2 + 0.7 * rnd());
      }
      points.push({ x: round2(x), y: round2(y), pressure: round3(p), tiltX: 0, tiltY: 0, timeUs: j * 4000 });
    }
    strokes.push({
      id: `scene${count}-${i}`,
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
    meta: { generator: "fixtures/generate-fixtures.mjs", seed: count },
    strokes,
  };
}
