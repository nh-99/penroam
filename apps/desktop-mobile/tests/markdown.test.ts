import { fixture } from "./fixture";
import { describe, expect, it } from "vitest";
import { extractMath, layoutMarkdown, layoutMarkdownWithTargets, MAX_DEPTH, rowsFor, toggledTask, type MathBox } from "../src/editor/markdown";

const golden = JSON.parse(fixture("markdown-layout-golden.json"));

// the lookup used to generate the golden file (crates/penroam-core/tests/markdown.rs)
const math = (tex: string, display: boolean): MathBox | null =>
  tex.includes("bad") ? null : { width: tex.includes("wide") ? 40 : 2, height: display ? 1.5 : 0.8, depth: display ? 0.6 : 0.25 };



describe("task checkboxes", () => {
  const math = () => null;
  const tasks = (text: string) => layoutMarkdownWithTargets(text, "formatted", 12, 400, null, "left", math).tasks;
  const tick = (text: string, i = 0) => toggledTask(text, tasks(text)[i]);

  it("ticks and unticks the marker in the source and nothing else", () => {
    expect(tick("- [ ] a [ ] b")).toBe("- [x] a [ ] b");
    expect(tick("- [x] done")).toBe("- [ ] done");
    expect(tick("- [X] done")).toBe("- [ ] done");
    expect(tick("- [ ] a\n- [ ] b", 1)).toBe("- [ ] a\n- [x] b");
  });

  it("finds markers in nested containers and across line endings", () => {
    expect(tick("> - [ ] quoted")).toBe("> - [x] quoted");
    expect(tick("1. x\n   - [ ] nested")).toBe("1. x\n   - [x] nested");
    expect(tick("- a\r\n- [ ] crlf\r\n")).toBe("- a\r\n- [x] crlf\r\n");
  });

  it("maps past formulas extracted from earlier lines", () => {
    const text = "$$a\nb$$\n\n- [ ] $x$ and more\n- [ ] \u{1F600} $y$";
    expect(tick(text)).toBe("$$a\nb$$\n\n- [x] $x$ and more\n- [ ] \u{1F600} $y$");
    expect(tick(text, 1)).toBe("$$a\nb$$\n\n- [ ] $x$ and more\n- [x] \u{1F600} $y$");
  });

  it("ticks checkboxes in table cells", () => {
    expect(tick("| a | b |\n|---|---|\n| [ ] | x |")).toBe("| a | b |\n|---|---|\n| [x] | x |");
    // past an escaped pipe, and on a later row
    expect(tick("|a|b|\n|-|-|\n|x \\| y|[x]|\n|z|[ ]|", 1)).toBe("|a|b|\n|-|-|\n|x \\| y|[x]|\n|z|[x]|");
    expect(tick("| $x$ | [X] |\n|---|---|")).toBe("| $x$ | [ ] |\n|---|---|");
    expect(tasks("| a |\n|---|\n| [x] done |")).toEqual([]);
  });

  it("gives no box to anything that isn't a marker it can find in the source", () => {
    expect(tasks("[ ] not in a list")).toEqual([]);
    expect(tasks("- plain")).toEqual([]);
    expect(tasks("- &#91; ] entity [ ] later")).toEqual([]);
  });
});

describe("Markdown layout (same golden as penroam-core)", () => {
  for (const c of golden) {
    it(`${JSON.stringify(c.text).slice(0, 60)} ${c.font} at ${c.fontSize} pt, ${c.width} wide, pitch ${c.rowPitch}`, () => {
      const p = layoutMarkdown(c.text, c.font, c.fontSize, c.width, c.rowPitch, c.align, math);
      expect(p).toEqual(c.paint);
    });
  }
});

describe("link boxes", () => {
  const links = (text: string, width = 400) => layoutMarkdownWithTargets(text, "formatted", 12, width, null, "left", math).links;
  it("covers the link text and carries the address", () => {
    const [box, ...rest] = links("see [the docs](https://penroam.ink/docs) here");
    expect(rest).toEqual([]);
    expect(box.href).toBe("https://penroam.ink/docs");
    const runs = layoutMarkdown("see [the docs](https://penroam.ink/docs) here", "formatted", 12, 400, null, "left", math).runs;
    expect(box.x0).toBeGreaterThan(runs[0].x);
    expect(box.y0).toBeLessThan(runs[0].y);
    expect(box.y1).toBeGreaterThan(runs[0].y);
  });
  it("splits a wrapped link per line and keeps neighbours apart", () => {
    expect(links("[one two three four five six](https://a.example)", 60).length).toBeGreaterThan(1);
    expect(links("[a](https://a.example)[b](https://b.example)").map((l) => l.href)).toEqual(["https://a.example", "https://b.example"]);
  });
  it("finds autolinks and links around images, and no unsafe ones", () => {
    expect(links("<https://a.example>")[0].href).toBe("https://a.example");
    expect(links("[![alt](x.png)](https://a.example)")[0].href).toBe("https://a.example");
    expect(links("[x](javascript:alert(1))")).toEqual([]);
    expect(links("plain https://a.example")).toEqual([]);
  });
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
    expect(() => layoutMarkdown(`${"> ".repeat(100000)}x`, "formatted", 16, 400, null, "left", math)).not.toThrow();
    expect(() => layoutMarkdown(`${"- ".repeat(5000)}x`, "formatted", 16, 400, null, "left", math)).not.toThrow();
    expect(MAX_DEPTH).toBe(16);
  });
});
