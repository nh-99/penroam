// UI preferences (per device, not notebook data): theme, input and tool
// defaults. Stored in localStorage; notebooks themselves live in Rust storage.
import { defineStore } from "pinia";
import { ref, watch } from "vue";
import type { Tool } from "../editor/engine";
import type { PenOnlyMode } from "../editor/input";
import type { Paper } from "../generated/Paper";
import type { TemplateKind } from "../generated/TemplateKind";
import type { UpdateChannel } from "../platform/updates";
import { normalizeHex } from "../editor/color";

export interface AutoBackup {
  target: "off" | "folder" | "webdav";
  dir: string;
  url: string;
  user: string;
  /** Last successful automatic backup (ms since the epoch). */
  lastAt: number;
}

export type ThemePref = "system" | "light" | "dark" | "epaper";

/** Which set of toolbar colours is meant. Text and recolouring a selection use the pen's. */
export type PaletteKind = "pen" | "highlighter";

const KEY = "penroam.settings.v1";

/** The colours the toolbar offers until the user changes them. */
export const PEN_COLORS = ["#111111", "#1d4ed8", "#b91c1c", "#15803d", "#7c3aed", "#c2410c"];
export const HIGHLIGHTER_COLORS = ["#facc15", "#4ade80", "#f472b6", "#60a5fa"];
/** Keeps the colours from pushing the rest of the toolbar out of sight. */
export const MAX_PALETTE_COLORS = 12;

interface Persisted {
  theme: ThemePref;
  /** The last tool used on this device. */
  tool: Tool;
  penOnly: PenOnlyMode;
  /** A stylus has been used on this device, so "auto" gives fingers pan and zoom from the start. */
  penSeen: boolean;
  /** Don't ask before exporting notes that sync keeps encrypted. */
  plaintextExportOk: boolean;
  /** Automatic backups. The WebDAV password is in the keychain, not here. */
  autoBackup: AutoBackup;
  useNativeInk: boolean;
  pressure: boolean;
  penColor: string;
  penWidth: number;
  highlighterColor: string;
  highlighterWidth: number;
  textColor: string;
  textSize: number;
  /** The toolbar's pen colours: distinct `#rrggbb` values, never empty. */
  penPalette: string[];
  /** The toolbar's highlighter colours: distinct `#rrggbb` values, never empty. */
  highlighterPalette: string[];
  /** New text is placed anywhere on ruled and dotted paper instead of on its rows. */
  textFreePlacement: boolean;
  paper: Paper;
  template: TemplateKind;
  sidebarOpen: boolean;
  /** The editor shows every page in one scrolling column instead of one page at a time. */
  continuousScroll: boolean;
  epaperChosenAutomatically: boolean;
  /** Check for an update at start-up. Off until the user turns it on: a check is a network request. */
  updateCheck: boolean;
  updateChannel: UpdateChannel;
  /** Developer mode (debug builds only): choose another sync server. */
  developerMode: boolean;
}

const defaults: Persisted = {
  theme: "system",
  tool: "pen",
  penOnly: "auto",
  penSeen: false,
  plaintextExportOk: false,
  autoBackup: { target: "off", dir: "", url: "", user: "", lastAt: 0 },
  useNativeInk: true,
  pressure: true,
  penColor: "#111111",
  penWidth: 2.2,
  highlighterColor: "#facc15",
  highlighterWidth: 14,
  textColor: "#111111",
  textSize: 16,
  penPalette: PEN_COLORS,
  highlighterPalette: HIGHLIGHTER_COLORS,
  textFreePlacement: false,
  paper: "letter",
  template: "ruled",
  sidebarOpen: true,
  continuousScroll: true,
  epaperChosenAutomatically: false,
  updateCheck: false,
  updateChannel: "stable",
  developerMode: false,
};

/** A saved palette held to its invariants. Always a new array, so edits can't reach the built-in lists. */
function cleanPalette(saved: unknown, fallback: string[]): string[] {
  const colors = Array.isArray(saved) ? saved.flatMap((c) => (typeof c === "string" ? (normalizeHex(c) ?? []) : [])) : [];
  const distinct = [...new Set(colors)].slice(0, MAX_PALETTE_COLORS);
  return distinct.length ? distinct : [...fallback];
}

function load(): Persisted {
  let saved: Partial<Persisted> = {};
  try {
    saved = JSON.parse(localStorage.getItem(KEY) ?? "{}") ?? {};
  } catch {
    /* corrupted prefs: fall back to defaults */
  }
  return {
    ...defaults,
    ...saved,
    penPalette: cleanPalette(saved.penPalette, PEN_COLORS),
    highlighterPalette: cleanPalette(saved.highlighterPalette, HIGHLIGHTER_COLORS),
  };
}

export const PEN_WIDTHS = [1.2, 2.2, 3.5, 6];
export const HIGHLIGHTER_WIDTHS = [8, 14, 22];
export const TEXT_SIZES = [12, 16, 22, 32];

export const useSettings = defineStore("settings", () => {
  const s = ref<Persisted>(load());
  watch(
    s,
    (v) => {
      try {
        localStorage.setItem(KEY, JSON.stringify(v));
      } catch {
        /* storage full or disabled: prefs stay in memory */
      }
    },
    { deep: true },
  );

  const media = typeof matchMedia !== "undefined" ? matchMedia("(prefers-color-scheme: dark)") : null;
  const systemDark = ref(media?.matches ?? false);
  media?.addEventListener("change", (e) => (systemDark.value = e.matches));

  function effectiveTheme(): "light" | "dark" | "epaper" {
    const t = s.value.theme;
    if (t === "system") return systemDark.value ? "dark" : "light";
    return t;
  }

  function applyTheme() {
    document.documentElement.dataset.theme = effectiveTheme();
  }
  watch([() => s.value.theme, systemDark], applyTheme, { immediate: true });

  /** On BOOX the first launch switches to the ePaper theme (user can change it). */
  function preferEpaper() {
    if (!s.value.epaperChosenAutomatically) {
      s.value.epaperChosenAutomatically = true;
      if (s.value.theme === "system") s.value.theme = "epaper";
    }
  }

  function palette(kind: PaletteKind): string[] {
    return kind === "pen" ? s.value.penPalette : s.value.highlighterPalette;
  }

  /** Adds a toolbar colour. Does nothing if it is already there or the palette is full. */
  function addPaletteColor(kind: PaletteKind, color: string) {
    const p = palette(kind);
    if (p.length < MAX_PALETTE_COLORS && !p.includes(color)) p.push(color);
  }

  /** Changes a toolbar colour in place. If the new colour is already in the palette, the two become one. */
  function replacePaletteColor(kind: PaletteKind, old: string, color: string) {
    const p = palette(kind);
    const i = p.indexOf(old);
    if (i < 0 || old === color) return;
    if (p.includes(color)) p.splice(i, 1);
    else p[i] = color;
    // A tool using this swatch keeps using it, rather than being left on a colour no longer offered.
    if (kind === "highlighter") {
      if (s.value.highlighterColor === old) s.value.highlighterColor = color;
    } else {
      if (s.value.penColor === old) s.value.penColor = color;
      if (s.value.textColor === old) s.value.textColor = color;
    }
  }

  /** Removes a toolbar colour, unless it is the last one. */
  function removePaletteColor(kind: PaletteKind, color: string) {
    const p = palette(kind);
    if (p.length > 1 && p.includes(color)) p.splice(p.indexOf(color), 1);
  }

  /** Puts a palette back to the colours the app ships with. */
  function resetPalette(kind: PaletteKind) {
    if (kind === "pen") s.value.penPalette = [...PEN_COLORS];
    else s.value.highlighterPalette = [...HIGHLIGHTER_COLORS];
  }

  return { s, effectiveTheme, preferEpaper, palette, addPaletteColor, replacePaletteColor, removePaletteColor, resetPalette };
});
