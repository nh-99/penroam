// Thin wrappers around the Rust backend and the BOOX Android plugin.
import { invoke, isTauri } from "@tauri-apps/api/core";

export const inTauri = (): boolean => {
  try {
    return isTauri();
  } catch {
    return false;
  }
};

/**
 * Host command bridge. In Tauri this is `invoke`. In the iOS WKWebView harness
 * (ios/M00iOS, a proxy for the Tauri iPad build) the same command names are
 * answered by a Swift WKScriptMessageHandlerWithReply named "m00".
 */
const iosBridge = (): { postMessage(m: unknown): Promise<any> } | undefined => (window as any).webkit?.messageHandlers?.m00;
export const hasHost = (): boolean => inTauri() || !!iosBridge();
export async function host<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  if (inTauri()) return invoke<T>(cmd, args);
  const b = iosBridge();
  if (b) return b.postMessage({ cmd, args });
  throw new Error(`no host for ${cmd}`);
}
export const hostName = (): string => (inTauri() ? "tauri" : (window as any).__M00_HOST__ ?? "browser");

export const isAndroid = () => /Android/i.test(navigator.userAgent);
export const isIOS = () => /iPad|iPhone|iPod/.test(navigator.userAgent) || (navigator.platform === "MacIntel" && navigator.maxTouchPoints > 1);

export interface SystemInfo {
  os: string;
  arch: string;
  family: string;
  kernel?: string;
  webviewVersion?: string;
  tauriVersion: string;
  appVersion: string;
  sessionType?: string;
  desktop?: string;
}

export interface ProcStats {
  available: boolean;
  pid: number;
  rssBytes: number;
  cpuSeconds: number;
  /** Includes WebKitWebProcess/GPU/network child processes on Linux. */
  treeRssBytes: number;
  treeCpuSeconds: number;
  processes: { pid: number; name: string; rssBytes: number; cpuSeconds: number }[];
  note?: string;
}

function download(name: string, text: string) {
  const a = document.createElement("a");
  a.href = URL.createObjectURL(new Blob([text], { type: "application/json" }));
  a.download = name;
  a.click();
  URL.revokeObjectURL(a.href);
}

export async function saveStrokes(json: string): Promise<string> {
  if (!hasHost()) {
    download("strokes.json", json);
    return "downloaded strokes.json";
  }
  return host<string>("save_strokes", { json });
}

export async function listStrokeFiles(): Promise<string[]> {
  if (!hasHost()) return [];
  return host<string[]>("list_strokes");
}

export async function loadStrokeFile(name: string): Promise<string> {
  return host<string>("load_strokes", { name });
}

export async function writePerf(label: string, json: string): Promise<string> {
  if (!hasHost()) {
    download(`perf-${label}.json`, json);
    return `downloaded perf-${label}.json`;
  }
  return host<string>("write_perf", { label, json });
}

export async function systemInfo(): Promise<SystemInfo | null> {
  return hasHost() ? host<SystemInfo>("system_info") : null;
}

export async function procStats(): Promise<ProcStats | null> {
  return hasHost() ? host<ProcStats>("proc_stats") : null;
}

export async function startupMs(): Promise<number | null> {
  return hasHost() ? host<number>("startup_ms") : null;
}

/** Reads a fixture from the app bundle (sample strokes). */
export async function fetchText(url: string): Promise<string> {
  const r = await fetch(url);
  if (!r.ok) throw new Error(`${url}: ${r.status}`);
  return r.text();
}

// Native ink plugins: see src/nativeInk.ts
