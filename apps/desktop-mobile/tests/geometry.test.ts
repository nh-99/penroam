import { fixture } from "./fixture";
import { describe, expect, it } from "vitest";
import { hitTest, hitTestSegment, stablePieceCount, strokePieces, widthFor } from "../src/editor/geometry";
import { strokeDocToInk } from "../src/platform/nativeInk";

const tool = { tool: "pen" as const, color: "#000000", width: 3, pressure: true, opacity: 1 };

describe("geometry golden (tests/fixtures/geometry-golden.json)", () => {
  const golden = JSON.parse(fixture("geometry-golden.json"));
  const strokes = JSON.parse(fixture("sample-strokes.json")).strokes.map((s: any) => ({ srcId: s.id, ...strokeDocToInk({ ...s, id: s.id }, 1, tool) }));

  it("width mapping", () => {
    for (const c of golden.widthCases) expect(widthFor(c.base, c.pressure, c.pressureEnabled)).toBeCloseTo(c.width, 12);
  });

  it("pieces", () => {
    for (const g of golden.strokes) {
      const s = strokes.find((x: any) => x.srcId === g.id)!;
      const pieces = strokePieces(s.points, s.width, s.pressure);
      expect(pieces.length).toBe(g.pieces.length);
      pieces.forEach((p, i) => {
        for (const [k, v] of Object.entries(g.pieces[i])) {
          if (typeof v === "number") expect((p as any)[k]).toBeCloseTo(v, 9);
          else expect((p as any)[k]).toBe(v);
        }
      });
    }
  });

  it("stable pieces never change when points are appended (incremental preview)", () => {
    const pts = Array.from({ length: 20 }, (_, i) => ({ x: i * 3, y: (i % 4) * 2, pressure: 0.1 + i * 0.04, tiltX: 0, tiltY: 0, timeUs: i }));
    const full = strokePieces(pts, 3, true);
    for (let n = 3; n <= pts.length; n++) {
      const part = strokePieces(pts.slice(0, n), 3, true);
      for (let k = 0; k < stablePieceCount(n); k++) expect(part[k]).toEqual(full[k]);
    }
  });

  it("whole-stroke eraser hit test, including fast sweeps", () => {
    const s = strokes[1];
    expect(hitTest(s, 300, 130, 4)).toBe(true);
    expect(hitTest(s, 300, 200, 4)).toBe(false);
    // a sweep that jumps over the stroke between two samples still hits it
    const line = { ...s, points: [{ x: 0, y: 50, pressure: 0.5, tiltX: 0, tiltY: 0, timeUs: 0 }, { x: 100, y: 50, pressure: 0.5, tiltX: 0, tiltY: 0, timeUs: 1 }] };
    expect(hitTest(line, 50, 0, 4) || hitTest(line, 50, 100, 4)).toBe(false);
    expect(hitTestSegment(line, 50, 0, 50, 100, 4)).toBe(true);
  });
});
