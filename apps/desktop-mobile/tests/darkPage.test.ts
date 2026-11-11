import { describe, expect, it } from "vitest";
import { PageStrip } from "../src/editor/pageStrip";
import { shownColor } from "../src/editor/renderer";

describe("dark theme page colours", () => {
  it("swaps black and white ink", () => {
    expect(shownColor("#000000", true)).toBe("#ffffff");
    expect(shownColor("#ffffff", true)).toBe("#000000");
    // the default pen colour is a near-black
    expect(shownColor("#111111", true)).toBe("#eeeeee");
  });

  it("leaves coloured ink alone", () => {
    for (const c of ["#1d4ed8", "#b91c1c", "#facc15", "#15803d"]) expect(shownColor(c, true)).toBe(c);
  });

  it("changes nothing outside the dark theme", () => {
    expect(shownColor("#111111", false)).toBe("#111111");
  });

  it("gives the stored colour back when applied twice", () => {
    for (const c of ["#000000", "#111111", "#7f7f7f", "#2a2d31", "#1d4ed8"]) expect(shownColor(shownColor(c, true), true)).toBe(c);
  });
});

describe("page strip placeholders", () => {
  it("shows the saved thumbnail in its own element until a picture arrives, and again after dropImages", () => {
    const strip = new PageStrip();
    strip.setPages([{ id: "a" }, { id: "b", placeholder: "blob:thumb-b" }]);
    strip.relayout("a", 100, 100);
    strip.update([0, -1000, 100, 1000], [0, -1000, 100, 1000]);
    const slot = strip.el.querySelector(".penroam-strip-page")!;
    expect(slot.querySelector<HTMLElement>(".penroam-strip-placeholder")?.style.backgroundImage).toContain("blob:thumb-b");

    strip.setImage("b", document.createElement("canvas"));
    expect(slot.querySelector(".penroam-strip-placeholder")).toBeNull();
    expect(slot.querySelector("canvas")).not.toBeNull();

    strip.dropImages();
    expect(strip.imageWidth("b")).toBe(0);
    expect(slot.querySelector(".penroam-strip-placeholder")).not.toBeNull();
  });
});
