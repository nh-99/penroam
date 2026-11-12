// System file pickers. The renderer only ever holds the path/URI the user
// picked and hands it straight back to Rust; it never reads files itself.
// On Android the dialogs return content:// URIs; folder picking is not used.
import { open, save } from "@tauri-apps/plugin-dialog";
import { api, hasHost } from "./ipc";
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

export type OpenKind = "pdf" | "penroam" | "model" | "backup";
export type SaveKind = "pdf" | "penroam" | "backup" | "md" | "txt" | "png" | "svg";

const OPEN_FILTERS: Record<OpenKind, { name: string; extensions: string[] }> = {
  pdf: { name: "PDF document", extensions: ["pdf"] },
  penroam: { name: "Penroam notebook", extensions: ["penroam"] },
  model: { name: "Penroam handwriting model", extensions: ["penroam-model"] },
  backup: { name: "Penroam backup", extensions: ["penroam-backup"] },
};

const SAVE_FILTERS: Record<SaveKind, { name: string; ext: string }> = {
  pdf: { name: "PDF document", ext: "pdf" },
  penroam: { name: "Penroam notebook", ext: "penroam" },
  backup: { name: "Penroam backup", ext: "penroam-backup" },
  md: { name: "Markdown", ext: "md" },
  txt: { name: "Text", ext: "txt" },
  png: { name: "PNG image", ext: "png" },
  svg: { name: "SVG image", ext: "svg" },
};

/** Everything the library's Import button takes, in the order the picker lists it. */
const IMPORT_FILTER = { name: "Notebooks and documents", extensions: ["penroam", "pdf", "xopp", "xoj", "enex", "jex", "md", "markdown", "txt"] };

export async function pickFile(kind: OpenKind): Promise<Picked | null> {
  if (!hasHost()) return null;
  // Android's picker filters by MIME type; our extensions have no registered type, so allow any file there.
  const path = await open({ multiple: false, directory: false, filters: isMobile() && kind !== "pdf" ? [] : [OPEN_FILTERS[kind]] });
  if (!path || Array.isArray(path)) return null;
  return { path, name: baseName(path) };
}

/** Files to import. Several can be chosen at once; each gets its own result. */
export async function pickImportFiles(): Promise<Picked[]> {
  if (!hasHost()) return [];
  const paths = await open({ multiple: true, directory: false, filters: isMobile() ? [] : [IMPORT_FILTER] });
  const list = paths === null ? [] : Array.isArray(paths) ? paths : [paths];
  return list.map((path) => ({ path, name: baseName(path) }));
}

/** A folder, on desktop systems. Mobile has no folder access here, so it returns null. */
export async function pickFolder(): Promise<string | null> {
  if (!hasHost() || isMobile()) return null;
  const path = await open({ multiple: false, directory: true });
  if (typeof path !== "string") return null;
  // The sandboxed Mac App Store build loses access to the folder when it quits unless a bookmark
  // is saved now, while the picker's grant still holds. Without one, the folder still works until
  // then, so a failure here isn't worth stopping for.
  await api.rememberFolder(path).catch(() => undefined);
  return path;
}

export async function pickSaveLocation(kind: SaveKind, suggested: string): Promise<string | null> {
  if (!hasHost()) return null;
  const { name, ext } = SAVE_FILTERS[kind];
  const path = await save({
    defaultPath: suggested.endsWith(`.${ext}`) ? suggested : `${suggested}.${ext}`,
    filters: [{ name, extensions: [ext] }],
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
