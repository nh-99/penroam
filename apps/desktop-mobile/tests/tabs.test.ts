import { describe, expect, it } from "vitest";
import { tabsScroll } from "../src/components/ui/tabsScroll";

describe("tabsScroll", () => {
  it("hides the arrows when everything fits", () => {
    expect(tabsScroll(0, 400, 400).overflowing).toBe(false);
    expect(tabsScroll(0, 400, 400.5).overflowing).toBe(false);
  });

  it("offers only the way that still has tabs", () => {
    expect(tabsScroll(0, 300, 400)).toEqual({ overflowing: true, canLeft: false, canRight: true });
    expect(tabsScroll(50, 300, 400)).toEqual({ overflowing: true, canLeft: true, canRight: true });
    expect(tabsScroll(100, 300, 400)).toEqual({ overflowing: true, canLeft: true, canRight: false });
  });

  it("treats a fractional scroll position at the far end as the end", () => {
    expect(tabsScroll(99.5, 300, 400).canRight).toBe(false);
    expect(tabsScroll(0.5, 300, 400).canLeft).toBe(false);
  });
});
