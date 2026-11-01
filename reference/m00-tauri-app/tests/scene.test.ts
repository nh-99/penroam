import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { generateScene } from "../src/ink/scene";
import { parseDocument, toDocument } from "../src/ink/model";

const fixture = (p: string) => readFileSync(new URL(`../../../tests/fixtures/${p}`, import.meta.url), "utf8");

describe("benchmark scenes", () => {
  for (const n of [100, 1000]) {
    it(`generateScene(${n}) reproduces fixtures/benchmark-scenes/scene-${n}.json`, () => {
      expect(generateScene(n)).toEqual(JSON.parse(fixture(`benchmark-scenes/scene-${n}.json`)));
    });
  }

  it("stroke JSON round-trips", () => {
    const strokes = parseDocument(fixture("sample-strokes.json"));
    expect(parseDocument(JSON.stringify(toDocument(strokes)))).toEqual(strokes);
  });
});
