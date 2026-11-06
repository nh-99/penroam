import { describe, expect, it } from "vitest";
import { alignment, type TextEdges } from "../src/editor/snap";

const box = (x: number, y: number, w = 100, h = 40): TextEdges => ({ rect: [x, y, x + w, y + h], baseline: y + 16 });

describe("text alignment", () => {
  it("snaps a left edge to another block's and reports a guide across both", () => {
    const a = alignment([box(104, 300)], [box(100, 100)], 8, true);
    expect(a).toMatchObject({ dx: -4, dy: 0 });
    expect(a.guides).toContainEqual({ x0: 100, y0: 100, x1: 100, y1: 340 });
  });
  it("draws one guide for blocks of the same size", () => {
    expect(alignment([box(104, 300)], [box(100, 100)], 8, false).guides).toHaveLength(1);
  });
  it("lines up centres and right edges of different widths with their own kind", () => {
    expect(alignment([box(300, 300, 50)], [box(150, 100, 200)], 8, true).dx).toBe(0);
    expect(alignment([box(305, 300, 50)], [box(150, 100, 200)], 8, true).dx).toBe(-5); // right edges at 350
    expect(alignment([box(226, 300, 50)], [box(150, 100, 200)], 8, true).dx).toBe(-1); // centres at 250
  });
  it("doesn't align a left edge with a right edge", () => {
    expect(alignment([box(203, 300, 300)], [box(100, 100, 100)], 8, true).guides).toEqual([]);
  });
  it("takes the nearest match when several are in range", () => {
    expect(alignment([box(106, 300)], [box(100, 100), box(104, 200)], 8, true).dx).toBe(-2);
  });
  it("leaves blocks alone beyond the tolerance", () => {
    expect(alignment([box(120, 300)], [box(100, 100)], 8, true)).toEqual({ dx: 0, dy: 0, guides: [] });
  });
  it("aligns tops and baselines of blocks in different sizes", () => {
    const large: TextEdges = { rect: [400, 100, 500, 140], baseline: 131 };
    expect(alignment([box(100, 103)], [large], 8, true).dy).toBe(-3); // tops
    expect(alignment([box(100, 111)], [large], 8, true).dy).toBe(4); // baselines
  });
  it("keeps the vertical axis still when told to", () => {
    const a = alignment([box(104, 103)], [box(100, 100)], 8, false);
    expect(a).toMatchObject({ dx: -4, dy: 0 });
    expect(a.guides.every((g) => g.x0 === g.x1)).toBe(true);
  });
  it("aligns any block of a group", () => {
    expect(alignment([box(300, 300), box(204, 400)], [box(200, 100)], 8, true).dx).toBe(-4);
  });
});
