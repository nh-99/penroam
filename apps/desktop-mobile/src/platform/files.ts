// System file pickers. The renderer only ever holds the path/URI the user
// picked and hands it straight back to Rust; it never reads files itself.
// On Android the dialogs return content:// URIs; folder picking is not used.
import { open, save } from "@tauri-apps/plugin-dialog";
import { hasHost } from "./ipc";
import { isMobile } from "./device";

export interface Picked {
  path: string;
  name: string;
}

function baseName(p: string): string {
  try {
    const decoded = decodeURIComponent(p);
    return decoded.split(/[\\/:]/).filter(Boolean).pop() ?? decoded;
  } catch {
    return p.split(/[\\/]/).pop() ?? p;
  }
}

export async function pickFile(kind: "pdf" | "penroam" | "model"): Promise<Picked | null> {
  if (!hasHost()) return null;
  const filters =
    kind === "pdf"
      ? [{ name: "PDF document", extensions: ["pdf"] }]
      : kind === "model"
        ? [{ name: "Penroam handwriting model", extensions: ["penroam-model"] }]
        : [{ name: "Penroam notebook", extensions: ["penroam"] }];
  // Android's picker filters by MIME type; our extensions have no registered type, so allow any file there.
  const path = await open({ multiple: false, directory: false, filters: isMobile() && kind !== "pdf" ? [] : filters });
  if (!path || Array.isArray(path)) return null;
  return { path, name: baseName(path) };
}

export async function pickSaveLocation(kind: "pdf" | "penroam", suggested: string): Promise<string | null> {
  if (!hasHost()) return null;
  const ext = kind === "pdf" ? "pdf" : "penroam";
  const path = await save({
    defaultPath: suggested.endsWith(`.${ext}`) ? suggested : `${suggested}.${ext}`,
    filters: [{ name: kind === "pdf" ? "PDF document" : "Penroam notebook", extensions: [ext] }],
  });
  return path ?? null;
}

export function safeFileName(title: string): string {
  const s = Array.from(title, (c) => (c.charCodeAt(0) < 32 ? " " : c))
    .join("")
    .replace(/[\\/:*?"<>|]+/g, " ")
    .replace(/\s+/g, " ")
    .trim();
  return s.slice(0, 80) || "Notebook";
}
