// Passes the mobile OS's power state to Rust, which cannot read it there (src-tauri/src/power.rs).
// The search and sync workers schedule from it. Failure only means "unknown", and work then runs
// as it would on a desktop that reports nothing.
import { invoke } from "@tauri-apps/api/core";
import { hasHost } from "./ipc";
import { isMobile } from "./device";

const REPORT_MS = 60_000;

async function report() {
  try {
    const p = await invoke<{ onBattery?: boolean; saver?: boolean }>("plugin:native-ink|call", { method: "getPower", args: {} });
    await invoke("power_report", { onBattery: p.onBattery ?? null, saver: p.saver === true });
  } catch (e) {
    console.warn("power state", e);
  }
}

/** Starts reporting on Android and iOS: every minute, and when the app comes back. */
export function startPowerReports() {
  if (!hasHost() || !isMobile()) return;
  void report();
  setInterval(() => void report(), REPORT_MS);
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible") void report();
  });
}
