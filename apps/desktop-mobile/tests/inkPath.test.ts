import { afterEach, describe, expect, it, vi } from "vitest";
import { describeInkPath } from "../src/platform/inkPath";

const ua = (s: string) => vi.spyOn(navigator, "userAgent", "get").mockReturnValue(s);

describe("ink path label", () => {
  afterEach(() => void vi.restoreAllMocks());
  it("names the native surfaces", () => {
    expect(describeInkPath("boox", null).label).toBe("BOOX hardware pen");
    expect(describeInkPath("ios-pencil", null).label).toBe("Native Apple Pencil layer");
  });
  it("names the S Pen only on a Samsung device with a stylus", () => {
    ua("Mozilla/5.0 (Linux; Android 15)");
    expect(describeInkPath(null, { device: { manufacturer: "samsung", stylus: true } }).label).toBe("S Pen on the web canvas");
    expect(describeInkPath(null, { device: { manufacturer: "Google", stylus: true } }).label).toBe("Stylus on the web canvas");
    expect(describeInkPath(null, { device: { manufacturer: "samsung", stylus: false } }).label).toBe("Web canvas");
    expect(describeInkPath(null, null).label).toBe("Web canvas");
  });
});
