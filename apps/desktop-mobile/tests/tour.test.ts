import { beforeEach, describe, expect, it } from "vitest";
import { flushPromises, mount } from "@vue/test-utils";
import { createPinia, setActivePinia } from "pinia";
import { useTour } from "../src/stores/tour";
import { useSettings } from "../src/stores/settings";
import Tour from "../src/components/Tour.vue";
import { TOURS } from "../src/components/tourSteps";

describe("tour", () => {
  beforeEach(() => {
    localStorage.clear();
    setActivePinia(createPinia());
  });

  it("starts by itself only on a first start with an empty library", () => {
    const tour = useTour();
    tour.offerAtStartup(true);
    expect(tour.active).toBe("library");
    tour.stop(true);
    // the next start, even with the library still empty, doesn't offer it again
    tour.offerAtStartup(true);
    expect(tour.active).toBeNull();
  });

  it("never interrupts an existing library", () => {
    const tour = useTour();
    tour.offerAtStartup(false);
    expect(tour.active).toBeNull();
    tour.offerInEditor();
    expect(tour.active).toBeNull();
    expect(useSettings().s.tourOffered).toBe(true);
  });

  it("follows a finished library tour with the editor tour, once", () => {
    const tour = useTour();
    tour.offerAtStartup(true);
    tour.stop(true);
    tour.offerInEditor();
    expect(tour.active).toBe("editor");
    tour.stop(true);
    tour.offerInEditor();
    expect(tour.active).toBeNull();
  });

  it("drops the editor tour when the library tour is skipped", () => {
    const tour = useTour();
    tour.offerAtStartup(true);
    tour.stop(false);
    tour.offerInEditor();
    expect(tour.active).toBeNull();
  });

  it("shows the steps whose targets are on screen and closes on Escape", async () => {
    document.body.innerHTML = `<header><button aria-label="Search notebooks">s</button></header>`;
    // happy-dom has no layout: give the target a size so it counts as visible
    const btn = document.querySelector("button")!;
    btn.getClientRects = () => [{}] as unknown as DOMRectList;
    btn.getBoundingClientRect = () => ({ left: 10, top: 10, width: 40, height: 40, right: 50, bottom: 50, x: 10, y: 10, toJSON() {} }) as DOMRect;
    const tour = useTour();
    tour.start("library");
    const w = mount(Tour, { props: { name: "library" }, attachTo: document.body });
    await new Promise((r) => setTimeout(r, 300));
    await flushPromises();
    // the welcome step (no target) and the search step; the rest have no target here
    expect(w.text()).toContain("1 of 2");
    expect(w.text()).toContain(TOURS.library[0].title);
    await w.find("button.bg-brand, button:last-child").trigger("click");
    expect(w.text()).toContain("Find anything");
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    expect(tour.active).toBeNull();
    w.unmount();
  });
});
