import { fixture } from "./fixture";
import { describe, expect, it } from "vitest";
import { contentHeight, layout, lineHeight, measure } from "../src/editor/textLayout";

const golden = JSON.parse(fixture("text-layout-golden.json"));

describe("deterministic text layout (same golden as penroam-core)", () => {
  it("matches the Rust layout exactly", () => {
    for (const c of golden) {
      const lines = layout(c.text, c.fontSize, c.width);
      expect(lines.map((l) => ({ start: l.start, end: l.end, text: l.text, width: l.width }))).toEqual(
        c.lines.map((l: any) => ({ start: l.start, end: l.end, text: l.text, width: l.width })),
      );
    }
  });
  it("line metrics", () => {
    expect(lineHeight(16)).toBe(20);
    expect(contentHeight("a\nb\nc", 16, 100)).toBe(60);
    expect(contentHeight("", 16, 100)).toBe(20);
    expect(measure("Hello", 10)).toBeCloseTo(22.78, 6);
  });
});
