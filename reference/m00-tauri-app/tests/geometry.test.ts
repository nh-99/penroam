import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { stablePieceCount, strokePieces, widthFor, hitTest } from "../src/ink/geometry";
import { parseDocument } from "../src/ink/model";

const fixture = (p: string) => readFileSync(new URL(`../../../tests/fixtures/${p}`, import.meta.url), "utf8");

describe("shared geometry (fixtures/geometry-golden.json)", () => {
  const golden = JSON.parse(fixture("geometry-golden.json"));
  const strokes = parseDocument(fixture("sample-strokes.json"));

  it("width mapping matches", () => {
    for (const c of golden.widthCases) expect(widthFor(c.base, c.pressure, c.pressureEnabled)).toBeCloseTo(c.width, 12);
  });

  it("pieces match", () => {
    for (const g of golden.strokes) {
      const s = strokes.find((x) => x.id === g.id)!;
      const pieces = strokePieces(s.points, s.brush.width, s.brush.pressure);
      expect(pieces.length).toBe(g.pieces.length);
      pieces.forEach((p, i) => {
        for (const [k, v] of Object.entries(g.pieces[i])) {
          if (typeof v === "number") expect((p as any)[k]).toBeCloseTo(v, 9);
          else expect((p as any)[k]).toBe(v);
        }
      });
    }
  });

  it("stable pieces are not changed by appended points", () => {
    const pts = Array.from({ length: 20 }, (_, i) => ({ x: i * 3, y: (i % 4) * 2, pressure: 0.1 + i * 0.04, tiltX: 0, tiltY: 0, timeUs: i }));
    const full = strokePieces(pts, 3, true);
    for (let n = 3; n <= pts.length; n++) {
      const part = strokePieces(pts.slice(0, n), 3, true);
      for (let k = 0; k < stablePieceCount(n); k++) expect(part[k]).toEqual(full[k]);
    }
  });

  it("eraser hit test", () => {
    const s = { ...strokes[1] };
    expect(hitTest(s, 300, 130, 4)).toBe(true);
    expect(hitTest(s, 300, 200, 4)).toBe(false);
  });
});
