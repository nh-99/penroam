import { fixture } from "./fixture";
import { describe, expect, it } from "vitest";
import { inkToM00, m00ToInk } from "../src/platform/nativeInk";

describe("native adapter stroke interop (M00 canonical format)", () => {
  it("round-trips every point field and the input metadata", () => {
    const doc = JSON.parse(fixture("sample-strokes.json"));
    for (const raw of doc.strokes) {
      const s = m00ToInk(raw, 7, { tool: "pen", color: "#000000", width: 3, pressure: true, opacity: 1 });
      const back = inkToM00(s) as any;
      expect(back.points).toEqual(raw.points);
      expect(back.input).toEqual(raw.input);
      expect(back.brush.width).toBe(raw.brush.width);
      expect(back.brush.pressure).toBe(raw.brush.pressure);
      expect(s.z).toBe(7);
    }
  });
  it("keeps highlighter tool, colour and opacity from the native brush", () => {
    const s = m00ToInk({ id: "x", brush: { type: "highlighter", width: 14, color: "#FACC15", opacity: 0.35 }, points: [{ x: 1, y: 2, pressure: 0.5 }] }, 1, { tool: "pen", color: "#000000", width: 3, pressure: true, opacity: 1 });
    expect(s).toMatchObject({ tool: "highlighter", color: "#facc15", opacity: 0.35, pressure: false });
  });
});
