import { describe, expect, it } from "vitest";
import { analyzeSamples, summarize } from "../src/ink/metrics";

describe("metrics", () => {
  it("summarize computes median/p95", () => {
    const s = summarize(Array.from({ length: 100 }, (_, i) => i + 1));
    expect(s.median).toBe(51);
    expect(s.p95).toBe(95);
    expect(s.max).toBe(100);
  });
  it("detects gaps and spatial jumps", () => {
    const g = analyzeSamples([0, 4, 8, 200, 204], [0, 1, 2, 3, 300], [0, 0, 0, 0, 0]);
    expect(g.timeGaps).toBe(1);
    expect(g.spatialJumps).toBe(1);
  });
});
