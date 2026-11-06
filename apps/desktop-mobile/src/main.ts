import { createApp } from "vue";
import { createPinia } from "pinia";
import App from "./App.vue";
import "./styles/main.css";
import { invoke, isTauri } from "@tauri-apps/api/core";
import { isMobile } from "./platform/device";
import { startPowerReports } from "./platform/power";

// Release WebViews on Android/iOS have no visible console: forward warnings,
// errors and unhandled rejections to the Rust log (logcat / device console).
if (isMobile()) {
  try {
    if (isTauri()) {
      for (const level of ["info", "warn", "error"] as const) {
        const orig = console[level].bind(console);
        console[level] = (...args: unknown[]) => {
          orig(...args);
          const message = args.map((a) => (a instanceof Error ? `${a.message}\n${a.stack}` : typeof a === "string" ? a : JSON.stringify(a))).join(" ");
          void invoke("log_line", { level, message }).catch(() => {});
        };
      }
      window.addEventListener("unhandledrejection", (e) => console.error("unhandled rejection", e.reason));
      window.addEventListener("error", (e) => console.error("uncaught", e.message, e.filename, e.lineno));
    }
  } catch {
    /* not in Tauri */
  }
}

// Text blocks are measured with bundled metrics and drawn with the bundled
// fonts; wait for them so the first canvas paint uses the right glyphs.
const fontReady = document.fonts
  ? Promise.all(["16px PenroamSans", "bold 16px PenroamSans", "italic 16px PenroamSans", "italic bold 16px PenroamSans", "16px PenroamMono"].map((f) => document.fonts.load(f))).catch(() => undefined)
  : undefined;

void Promise.resolve(fontReady).finally(() => {
  createApp(App).use(createPinia()).mount("#app");
  startPowerReports();
});
