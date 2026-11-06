// Backup plumbing shared by the app shell and Settings: long operations report progress over an
// event from Rust, and automatic backups run on a timer while the app is open.
import { listen } from "@tauri-apps/api/event";
import { api, hasHost, type AutoTarget } from "./ipc";
import { flushEverything } from "./lifecycle";
import { useLibrary } from "../stores/library";
import { useSettings, type AutoBackup } from "../stores/settings";
import { useUi } from "../stores/ui";

const DAY = 24 * 60 * 60 * 1000;
/** How often to see whether a backup is due. */
const CHECK_EVERY = 15 * 60 * 1000;
/** Leaves the first moments after launch to the editor and to sync. */
const FIRST_CHECK = 2 * 60 * 1000;

interface ProgressEvent {
  label: string;
  done: number;
  total: number;
}

export function watchProgress(lib: ReturnType<typeof useLibrary>) {
  if (!hasHost()) return;
  void listen<ProgressEvent>("portability-progress", ({ payload: p }) => {
    // an automatic backup runs behind the scenes, with no busy screen to fill
    if (!lib.busy) return;
    lib.progress = { label: p.label, fraction: p.total ? p.done / p.total : null };
  });
}

/** The destination configured in settings, or null when automatic backups are off. */
export function autoTarget(a: AutoBackup): AutoTarget | null {
  if (a.target === "folder" && a.dir) return { kind: "folder", dir: a.dir };
  if (a.target === "webdav" && a.url) return { kind: "webdav", url: a.url, user: a.user };
  return null;
}

/** Whether a backup is due. */
export const backupDue = (a: AutoBackup, now: number) => autoTarget(a) !== null && now - a.lastAt >= DAY;

let running = false;

/** Runs a backup to the configured destination now. Throws what went wrong. */
export async function runAutoBackup() {
  const settings = useSettings();
  const target = autoTarget(settings.s.autoBackup);
  if (!target || running) return;
  running = true;
  try {
    await flushEverything();
    await api.autoBackup(target);
    settings.s.autoBackup = { ...settings.s.autoBackup, lastAt: Date.now() };
  } finally {
    running = false;
  }
}

async function check() {
  const settings = useSettings();
  if (!backupDue(settings.s.autoBackup, Date.now())) return;
  try {
    await runAutoBackup();
  } catch (e) {
    useUi().error(`Automatic backup failed: ${(e as Error)?.message ?? e}`);
  }
}

export function startAutoBackup() {
  if (!hasHost()) return;
  setTimeout(() => {
    void check();
    setInterval(() => void check(), CHECK_EVERY);
  }, FIRST_CHECK);
}
