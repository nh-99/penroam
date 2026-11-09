import { flushPromises, mount, type VueWrapper } from "@vue/test-utils";
import { createPinia, setActivePinia } from "pinia";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import ColorDialog from "../src/components/ColorDialog.vue";
import Toolbar from "../src/components/Toolbar.vue";
import { hexToHsv, hsvToHex, normalizeHex } from "../src/editor/color";
import { HIGHLIGHTER_COLORS, MAX_PALETTE_COLORS, PEN_COLORS, useSettings } from "../src/stores/settings";

const KEY = "penroam.settings.v1";

beforeEach(() => {
  localStorage.clear();
  setActivePinia(createPinia());
});

describe("colour conversion", () => {
  it("accepts short and long hex with or without the hash", () => {
    expect(normalizeHex("#ABC")).toBe("#aabbcc");
    expect(normalizeHex(" 1D4ED8 ")).toBe("#1d4ed8");
  });

  it("rejects anything else", () => {
    for (const bad of ["", "#12", "#12345", "#1234567", "#gggggg", "red"]) expect(normalizeHex(bad)).toBeNull();
  });

  it("gives back exactly the colour it was given", () => {
    for (let r = 0; r < 256; r += 17) {
      for (let g = 0; g < 256; g += 17) {
        for (let b = 0; b < 256; b += 17) {
          const hex = `#${[r, g, b].map((n) => n.toString(16).padStart(2, "0")).join("")}`;
          expect(hsvToHex(hexToHsv(hex))).toBe(hex);
        }
      }
    }
    for (const hex of [...PEN_COLORS, ...HIGHLIGHTER_COLORS, "#123456"]) expect(hsvToHex(hexToHsv(hex))).toBe(hex);
  });

  it("treats a hue of 360 as red", () => {
    expect(hsvToHex({ h: 360, s: 100, v: 100 })).toBe("#ff0000");
  });
});

describe("palettes", () => {
  it("starts on the built-in colours without sharing their arrays", () => {
    const s = useSettings();
    s.addPaletteColor("pen", "#abcdef");
    expect(s.s.penPalette).toContain("#abcdef");
    expect(PEN_COLORS).not.toContain("#abcdef");
  });

  it("gives settings saved before palettes existed the built-in colours and keeps the rest", () => {
    localStorage.setItem(KEY, JSON.stringify({ penColor: "#123456" }));
    const s = useSettings();
    expect(s.s.penPalette).toEqual(PEN_COLORS);
    expect(s.s.penColor).toBe("#123456");
  });

  it("repairs a damaged saved palette", () => {
    localStorage.setItem(KEY, JSON.stringify({ penPalette: ["#ABCDEF", "#abcdef", "nope", 7], highlighterPalette: [] }));
    const s = useSettings();
    expect(s.s.penPalette).toEqual(["#abcdef"]);
    expect(s.s.highlighterPalette).toEqual(HIGHLIGHTER_COLORS);
  });

  it("does not add duplicates or go past the limit", () => {
    const s = useSettings();
    s.addPaletteColor("pen", PEN_COLORS[0]);
    expect(s.s.penPalette).toHaveLength(PEN_COLORS.length);
    for (let i = 0; i < 40; i++) s.addPaletteColor("pen", `#0000${i.toString(16).padStart(2, "0")}`);
    expect(s.s.penPalette).toHaveLength(MAX_PALETTE_COLORS);
  });

  it("moves tools on the edited colour to its replacement", () => {
    const s = useSettings();
    s.s.penColor = "#1d4ed8";
    s.s.textColor = "#1d4ed8";
    s.replacePaletteColor("pen", "#1d4ed8", "#000080");
    expect(s.s.penPalette[1]).toBe("#000080");
    expect(s.s.penColor).toBe("#000080");
    expect(s.s.textColor).toBe("#000080");
  });

  it("merges a swatch edited into one that already exists", () => {
    const s = useSettings();
    s.replacePaletteColor("pen", "#1d4ed8", "#b91c1c");
    expect(s.s.penPalette.filter((c) => c === "#b91c1c")).toHaveLength(1);
    expect(s.s.penPalette).toHaveLength(PEN_COLORS.length - 1);
  });

  it("never removes the last colour", () => {
    const s = useSettings();
    for (const c of [...HIGHLIGHTER_COLORS]) s.removePaletteColor("highlighter", c);
    expect(s.s.highlighterPalette).toEqual([HIGHLIGHTER_COLORS[3]]);
  });

  it("resets to the built-in colours", () => {
    const s = useSettings();
    s.removePaletteColor("pen", PEN_COLORS[0]);
    s.resetPalette("pen");
    expect(s.s.penPalette).toEqual(PEN_COLORS);
  });
});

let wrapper: VueWrapper | null = null;
afterEach(() => {
  wrapper?.unmount();
  wrapper = null;
});

describe("colour dialog", () => {
  const dialog = (color: string) => {
    wrapper = mount(ColorDialog, { props: { title: "Custom colour", color, confirm: "Use colour" }, global: { plugins: [createPinia()] }, attachTo: document.body });
    return wrapper;
  };
  const slider = (i: number) => wrapper!.findAll<HTMLInputElement>('input[type="range"]')[i];
  const hex = () => wrapper!.find<HTMLInputElement>("input:not([type])");

  it("makes the hue visible when starting from black", async () => {
    dialog("#111111");
    await slider(0).setValue("240");
    await wrapper!.find("form").trigger("submit");
    expect(wrapper!.emitted("pick")![0][0]).toBe("#0000ff");
  });

  it("keeps the hue through zero saturation", async () => {
    dialog("#0000ff");
    await slider(1).setValue("0");
    expect(hex().element.value).toBe("#ffffff");
    await slider(1).setValue("100");
    expect(hex().element.value).toBe("#0000ff");
  });

  it("refuses to submit a colour that isn't one", async () => {
    dialog("#0000ff");
    await hex().setValue("#12");
    await wrapper!.find("form").trigger("submit");
    expect(wrapper!.emitted("pick")).toBeUndefined();
    expect(wrapper!.text()).toContain("Enter a colour");
  });

  it("returns a typed colour exactly", async () => {
    dialog("#0000ff");
    await hex().setValue("123456");
    await wrapper!.find("form").trigger("submit");
    expect(wrapper!.emitted("pick")![0][0]).toBe("#123456");
  });
});

describe("toolbar colours", () => {
  const state = { tool: "pen", scale: 1, canUndo: false, canRedo: false, selectionCount: 0, selectionHasText: false, selectionTextFree: true, pageHasRows: false, editingText: false, textFormats: [] };
  const toolbar = (tool = "pen") => {
    const pinia = createPinia();
    setActivePinia(pinia);
    wrapper = mount(Toolbar, { props: { state: { ...state, tool } as never }, global: { plugins: [pinia] }, attachTo: document.body });
    return useSettings();
  };
  const custom = () => wrapper!.find('[aria-label="Custom colour"]');

  it("offers the user's palette for the current tool", async () => {
    const s = toolbar("highlighter");
    s.addPaletteColor("highlighter", "#abcdef");
    await flushPromises();
    expect(wrapper!.find('[aria-label="Colour #abcdef"]').exists()).toBe(true);
    expect(wrapper!.find('[aria-label="Colour #1d4ed8"]').exists()).toBe(false);
  });

  it("marks the custom button only for a colour without a swatch", async () => {
    const s = toolbar();
    expect(custom().attributes("aria-pressed")).toBe("false");
    s.s.penColor = "#abcdef";
    await flushPromises();
    expect(custom().attributes("aria-pressed")).toBe("true");
  });
});
