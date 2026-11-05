import { describe, expect, it } from "vitest";
import { describeState, formatBytes, highlightRects, recognizerSummary, type RecognizerChoice, type SearchHit, type SearchSettings, type SearchStatus } from "../src/platform/search";

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

describe("recognizerSummary", () => {
  const platform = { id: "os", modelVersion: "1", name: "Windows handwriting recognition", languages: [], license: "" };
  const model = { id: "m", version: "1", name: "M", license: "", languages: [], bytes: 1, provenance: null };
  const with_ = (recognizer: RecognizerChoice, p: boolean, m: boolean) =>
    recognizerSummary({ platform: p ? platform : null, model: m ? model : null, settings: { recognizer } as SearchSettings });

  it("names both in automatic mode", () => {
    expect(with_("auto", true, true)).toContain("as a fallback");
  });
  it("follows an explicit choice", () => {
    expect(with_("platform", true, true)).toBe("Uses Windows handwriting recognition on this device.");
    expect(with_("model", true, true)).toBe("Uses the Penroam handwriting model on this device.");
  });
  it("falls back like the backend when the choice cannot be honoured", () => {
    expect(with_("model", true, false)).toBe("Uses Windows handwriting recognition on this device.");
    expect(with_("platform", false, true)).toBe("Uses the Penroam handwriting model on this device.");
    expect(with_("auto", false, false)).toBeNull();
  });
});
