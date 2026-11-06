// UI preferences (per device, not notebook data): theme, input and tool
// defaults. Stored in localStorage; notebooks themselves live in Rust storage.
import { defineStore } from "pinia";
import { ref, watch } from "vue";
import type { PenOnlyMode } from "../editor/input";
import type { Paper } from "../generated/Paper";
import type { TemplateKind } from "../generated/TemplateKind";

export type ThemePref = "system" | "light" | "dark" | "epaper";

const KEY = "penroam.settings.v1";

interface Persisted {
  theme: ThemePref;
  penOnly: PenOnlyMode;
  /** A stylus has been used on this device, so "auto" gives fingers pan and zoom from the start. */
  penSeen: boolean;
  useNativeInk: boolean;
  pressure: boolean;
  penColor: string;
  penWidth: number;
  highlighterColor: string;
  highlighterWidth: number;
  textColor: string;
  textSize: number;
  /** New text is placed anywhere on ruled and dotted paper instead of on its rows. */
  textFreePlacement: boolean;
  paper: Paper;
  template: TemplateKind;
  sidebarOpen: boolean;
  epaperChosenAutomatically: boolean;
  /** Developer mode (debug builds only): choose another sync server. */
  developerMode: boolean;
}

const defaults: Persisted = {
  theme: "system",
  penOnly: "auto",
  penSeen: false,
  useNativeInk: true,
  pressure: true,
  penColor: "#111111",
  penWidth: 2.2,
  highlighterColor: "#facc15",
  highlighterWidth: 14,
  textColor: "#111111",
  textSize: 16,
  textFreePlacement: false,
  paper: "letter",
  template: "ruled",
  sidebarOpen: true,
  epaperChosenAutomatically: false,
  developerMode: false,
};

function load(): Persisted {
  try {
    const raw = localStorage.getItem(KEY);
    if (raw) return { ...defaults, ...JSON.parse(raw) };
  } catch {
    /* corrupted prefs: fall back to defaults */
  }
  return { ...defaults };
}

export const PEN_COLORS = ["#111111", "#1d4ed8", "#b91c1c", "#15803d", "#7c3aed", "#c2410c"];
export const HIGHLIGHTER_COLORS = ["#facc15", "#4ade80", "#f472b6", "#60a5fa"];
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

  return { s, effectiveTheme, preferEpaper };
});
