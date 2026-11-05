import { describe, expect, it } from "vitest";
import { describeState, formatBytes, highlightRects, type SearchHit, type SearchStatus } from "../src/platform/search";

const hit = (o: Partial<SearchHit>): SearchHit => ({
  notebookId: "n",
  notebookTitle: "N",
  pageId: "p",
  pageIndex: 0,
  kind: "ink",
  snippet: [],
  bounds: { x: 0, y: 0, w: 100, h: 20 },
  matchBounds: [],
  regionKey: "r",
  confidence: null,
  fuzzy: false,
  score: 1,
  ...o,
});

describe("search helpers", () => {
  it("outlines matching words, else the source, never a whole PDF page or a title", () => {
    const word = { x: 10, y: 2, w: 20, h: 16 };
    expect(highlightRects(hit({ matchBounds: [word] }))).toEqual([word]);
    expect(highlightRects(hit({}))).toEqual([{ x: 0, y: 0, w: 100, h: 20 }]);
    expect(highlightRects(hit({ kind: "pdf" }))).toEqual([]);
    expect(highlightRects(hit({ kind: "title" }))).toEqual([]);
  });

  it("describes worker states", () => {
    const s = { state: "indexing", queued: 1, lastError: null } as SearchStatus;
    expect(describeState(s)).toBe("Indexing: 1 page to check");
    expect(describeState({ ...s, state: "waiting-for-power", queued: 3 })).toContain("charger");
    expect(describeState({ ...s, state: "error", lastError: "disk full" })).toContain("disk full");
    expect(formatBytes(2_500_000)).toBe("2.4 MB");
  });
});
