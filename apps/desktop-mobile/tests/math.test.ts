import { beforeAll, describe, expect, it } from "vitest";
import { ensureMath, mathBox, mathEpoch, normalizePath, parseTransform, renderedMath, renderMathTable } from "../src/editor/math";

const I: [number, number, number, number, number, number] = [1, 0, 0, 1, 0, 0];

describe("path flattening", () => {
  it("reads MathJax's transforms, including chained and space-separated ones", () => {
    expect(parseTransform("translate(2736,0) translate(0 -0.5)", I)).toEqual([1, 0, 0, 1, 2736, -0.5]);
    expect(parseTransform("scale(1,-1)", I)).toEqual([1, 0, 0, -1, 0, 0]);
    expect(parseTransform(undefined, I)).toEqual(I);
    expect(parseTransform("rotate(45)", I)).toBeNull();
  });
  it("turns SVG path data into absolute lines and curves in em", () => {
    expect(normalizePath("M100 200H300V400L0 0Z", I)).toEqual(["M 0.1 0.2", "L 0.3 0.2", "L 0.3 0.4", "L 0 0", "Z"]);
    const quad = normalizePath("M0 0Q300 0 300 300T600 600", I)!;
    expect(quad[1]).toBe("C 0.2 0 0.3 0.1 0.3 0.3");
    expect(quad[2]).toBe("C 0.3 0.5 0.4 0.6 0.6 0.6"); // T mirrors the last control point
    expect(normalizePath("A 1 1 0 0 0 1 1", I)).toBeNull();
  });
  it("applies the transform to every point", () => {
    expect(normalizePath("M100 100", [1, 0, 0, -1, 1000, 0])).toEqual(["M 1.1 -0.1"]);
  });
});

describe("rendering formulas", () => {
  beforeAll(ensureMath, 30000);

  it("draws TeX as a closed outline with a baseline", () => {
    const r = renderedMath("\\frac{a}{b}", true)!;
    expect(r.width).toBeGreaterThan(0.5);
    expect(r.height).toBeGreaterThan(0.5);
    expect(r.depth).toBeGreaterThan(0.3);
    expect(r.path).toMatch(/^M /);
    expect(r.path).toContain("Z");
    expect(r.path).toMatch(/^[MLCZ0-9 .e-]+$/);
  });

  it("places the outline inside the box it reports", () => {
    const r = renderedMath("x^2+\\sqrt{y}", false)!;
    const v = r.path.split(" ").filter((t) => /\d/.test(t)).map(Number);
    const ys = v.filter((_, i) => i % 2 === 1);
    expect(Math.min(...ys)).toBeGreaterThanOrEqual(-r.height - 0.05);
    expect(Math.max(...ys)).toBeLessThanOrEqual(r.depth + 0.05);
  });

  it("has no outline for invalid TeX or characters outside its fonts", () => {
    expect(mathBox("\\undefinedcommand", false)).toBeNull();
    expect(mathBox("\\frac{", false)).toBeNull();
    expect(mathBox("\\text{日本}", false)).toBeNull();
  });

  it("supports the AMS environments", () => {
    expect(mathBox("\\begin{pmatrix}1&2\\\\3&4\\end{pmatrix}", true)).not.toBeNull();
    expect(mathBox("\\begin{aligned}a&=b\\\\c&=d\\end{aligned}", true)).not.toBeNull();
  });

  it("changes the layout cache key once loaded", () => {
    expect(mathEpoch()).toBeGreaterThan(0);
  });

  it("skips formulas that can't be drawn when building an export's table", async () => {
    const t = await renderMathTable([{ tex: "x", display: false }, { tex: "\\nope", display: false }]);
    expect(t.map((m) => m.tex)).toEqual(["x"]);
  });
});
