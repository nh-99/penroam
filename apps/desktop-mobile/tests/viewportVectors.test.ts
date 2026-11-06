import { describe, expect, it } from "vitest";
import vectors from "../../../tests/fixtures/viewport-vectors.json";
import { toDoc, toScreen } from "../src/editor/viewport";

// The same file is checked by the Kotlin and Swift tests, so a document point lands on the same
// pixel on every platform, at any zoom, pan, display scale or rotation.
describe("shared viewport vectors", () => {
  it("doc to view", () => {
    for (const c of vectors.cases) {
      const [x, y] = toScreen({ scale: c.scale, ox: c.ox, oy: c.oy }, c.doc[0], c.doc[1]);
      expect(x * c.dpr).toBeCloseTo(c.view[0], 3);
      expect(y * c.dpr).toBeCloseTo(c.view[1], 3);
    }
  });
  it("view back to doc stays within capture precision", () => {
    for (const c of vectors.cases) {
      const [x, y] = toDoc({ scale: c.scale, ox: c.ox, oy: c.oy }, c.view[0] / c.dpr, c.view[1] / c.dpr);
      expect(Math.abs(x - c.doc[0])).toBeLessThan(0.001);
      expect(Math.abs(y - c.doc[1])).toBeLessThan(0.001);
    }
  });
});
