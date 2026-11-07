// Typed facade over the Linux AppImage's app-menu entry (src-tauri/src/desktop_entry.rs). The
// backend decides whether this build can have one, so the renderer only shows what it reports.
import { invoke } from "@tauri-apps/api/core";
import { hasHost } from "./ipc";

export interface DesktopEntryStatus {
  /** Running from an AppImage on Linux. Otherwise there is nothing to show. */
  supported: boolean;
  /** Penroam's entry is in the app menu. */
  installed: boolean;
}

export const desktopEntryApi = {
  status: async (): Promise<DesktopEntryStatus> =>
    hasHost() ? await invoke<DesktopEntryStatus>("desktop_entry_status") : { supported: false, installed: false },
  install: () => invoke<DesktopEntryStatus>("desktop_entry_install"),
  remove: () => invoke<DesktopEntryStatus>("desktop_entry_remove"),
};
