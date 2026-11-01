// Test A (iPad -> Linux), automated part: every PencilKit export pulled from
// the iPad must parse in the Tauri engine without loss, re-serialize
// identically, and produce finite geometry. Skips when no export exists.
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { parseDocument, toDocument } from "../src/ink/model";
import { strokeBounds, strokePieces } from "../src/ink/geometry";

const dir = join(__dirname, "../../../benchmarks/results/ipad");
let files: string[] = [];
try { files = readdirSync(dir).filter((f) => /^pencilkit-\d+(\.width-fixed)?\.json$/.test(f)); } catch { /* no results */ }

describe.skipIf(files.length === 0)("iPad PencilKit exports load in Tauri", () => {
  for (const f of files) {
    it(f, () => {
      const text = readFileSync(join(dir, f), "utf8");
      const raw = JSON.parse(text);
      expect(raw.format).toBe("m00-strokes");
      expect(raw.version).toBe(1);
      const strokes = parseDocument(text);
      expect(strokes.length).toBe(raw.strokes.length);
      strokes.forEach((s, i) => {
        const r = raw.strokes[i];
        expect(s.id).toBe(r.id);
        expect(s.brush.width).toBe(r.brush.width);
        expect(s.brush.pressure).toBe(r.brush.pressure);
        expect(s.points.length).toBe(r.points.length);
        s.points.forEach((p, j) => {
          const q = r.points[j];
          expect([p.x, p.y, p.pressure, p.tiltX, p.tiltY, p.timeUs]).toEqual([q.x, q.y, q.pressure, q.tiltX, q.tiltY, q.timeUs]);
          expect(p.pressure).toBeGreaterThanOrEqual(0);
          expect(p.pressure).toBeLessThanOrEqual(1);
        });
        expect(s.input.api).toBe("pencilkit");
        const b = strokeBounds(s);
        expect(b.every(Number.isFinite)).toBe(true);
        expect(strokePieces(s.points, s.brush.width, s.brush.pressure).length).toBeGreaterThan(0);
      });
      // Re-export from Tauri and parse again: unchanged.
      const again = parseDocument(JSON.stringify(toDocument(strokes, raw.meta)));
      expect(again).toEqual(strokes);
    });
  }
});
