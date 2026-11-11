// Typed facade over desktop self-update (src-tauri/src/updates.rs). The backend picks the feed and
// verifies the signature, so the renderer only ever names a channel. No request is made unless
// the user asks or has turned on the automatic check.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { hasHost } from "./ipc";
import { flushEverything } from "./lifecycle";
import type { useUi } from "../stores/ui";

export type UpdateChannel = "stable" | "beta";

export interface OfferedUpdate {
  version: string;
  notes: string | null;
}

export interface UpdateProgress {
  received: number;
  total: number | null;
}

export const updatesApi = {
  /** False for mobile builds and for builds without an update key. */
  supported: async () => hasHost() && (await invoke<{ enabled: boolean }>("update_support")).enabled,
  check: (channel: UpdateChannel) => invoke<OfferedUpdate | null>("update_check", { channel }),
  /** Saves the editor's work, then installs and restarts. */
  install: async (channel: UpdateChannel) => {
    await flushEverything();
    await invoke<void>("update_install", { channel });
  },
  onProgress: (fn: (p: UpdateProgress) => void): Promise<UnlistenFn> => listen<UpdateProgress>("updates://progress", (e) => fn(e.payload)),
};

/**
 * Installs `version` from a toast: shows download progress in a sticky toast and reports a
 * failure as an error toast. On success the app restarts, so nothing after `install` runs.
 */
export async function installFromToast(ui: ReturnType<typeof useUi>, channel: UpdateChannel, version: string): Promise<void> {
  const id = ui.toast("info", `Downloading Penroam ${version}…`, { sticky: true });
  const stop = await updatesApi.onProgress((p) => {
    if (p.total) ui.retext(id, `Downloading Penroam ${version}: ${Math.round((p.received / p.total) * 100)}%`);
  });
  try {
    await updatesApi.install(channel);
  } catch (e: any) {
    ui.error(`The update could not be installed: ${e?.message ?? e}`);
  } finally {
    stop();
    ui.dismiss(id);
  }
}

/**
 * The start-up check. Returns the offer, or null when the setting is off, this build can't update,
 * or the request failed: being offline is normal and never worth a message.
 */
export async function checkAtStartup(enabled: boolean, channel: UpdateChannel): Promise<OfferedUpdate | null> {
  if (!enabled) return null;
  try {
    return (await updatesApi.supported()) ? await updatesApi.check(channel) : null;
  } catch {
    return null;
  }
}
