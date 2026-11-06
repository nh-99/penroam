import { fixture } from "./fixture";
import { describe, expect, it } from "vitest";
import { extractMath, layoutMarkdown, MAX_DEPTH, rowsFor, type MathBox } from "../src/editor/markdown";

const golden = JSON.parse(fixture("markdown-layout-golden.json"));

// the lookup used to generate the golden file (crates/penroam-core/tests/markdown.rs)
const math = (tex: string, display: boolean): MathBox | null =>
  tex.includes("bad") ? null : { width: tex.includes("wide") ? 40 : 2, height: display ? 1.5 : 0.8, depth: display ? 0.6 : 0.25 };



describe("Markdown layout (same golden as penroam-core)", () => {
  for (const c of golden) {
    it(`${JSON.stringify(c.text).slice(0, 60)} at ${c.fontSize} pt, ${c.width} wide, pitch ${c.rowPitch}`, () => {
      const p = layoutMarkdown(c.text, c.fontSize, c.width, c.rowPitch, c.align, math);
      expect(p).toEqual(c.paint);
    });
  }
});

describe("formula scanning", () => {
  const tex = (s: string) => extractMath(s).spans.map((m) => m.tex);
  it("finds formulas and leaves prices, escapes and code alone", () => {
    expect(tex("$x$ and $$y$$")).toEqual(["x", "y"]);
    expect(tex("$5 and $10")).toEqual([]);
    expect(tex("\\$x\\$")).toEqual([]);
    expect(tex("`$x$`")).toEqual([]);
    expect(tex("```\n$x$\n```")).toEqual([]);
    expect(tex("$$a\n\nb$$")).toEqual([]);
    expect(tex("$$a\nb$$")).toEqual(["a\nb"]);
  });
  it("replaces forged slots", () => {
    const { source, spans } = extractMath("\u{F0000} $x$");
    expect(spans).toHaveLength(1);
    expect(source.startsWith("�")).toBe(true);
  });
});

describe("row locking", () => {
  it("gives large text more rows", () => {
    expect(rowsFor(16, 24)).toBe(1);
    expect(rowsFor(22, 24)).toBe(1);
    expect(rowsFor(32, 24)).toBe(2);
    expect(rowsFor(22, 20)).toBe(2);
  });
});

describe("hostile input", () => {
  it("survives deep nesting", () => {
    expect(() => layoutMarkdown(`${"> ".repeat(100000)}x`, 16, 400, null, "left", math)).not.toThrow();
    expect(() => layoutMarkdown(`${"- ".repeat(5000)}x`, 16, 400, null, "left", math)).not.toThrow();
    expect(MAX_DEPTH).toBe(16);
  });
});
