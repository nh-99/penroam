import { describe, expect, it } from "vitest";
import { generateScene } from "../src/bench/scene";
import { fixture } from "./fixture";

describe("benchmark scenes (frozen fixtures)", () => {
  for (const n of [100, 1000]) {
    it(`generateScene(${n}) reproduces scene-${n}.json`, () => {
      expect(generateScene(n)).toEqual(JSON.parse(fixture(`benchmark-scenes/scene-${n}.json`)));
    });
  }
});
