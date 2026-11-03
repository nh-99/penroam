// Human-readable conflict descriptions (pure; unit-tested).
import type { SyncConflict } from "./sync";

const GROUPS: Record<number, string> = { 1: "position", 2: "content", 4: "style" };

export function conflictTitle(c: SyncConflict): string {
  switch (c.target.kind) {
    case "notebookTitle":
      return "Notebook title";
    case "notebookCover":
      return "Notebook cover colour";
    case "notebookTrash":
      return "Notebook moved to the trash on one device and restored on another";
    case "pageOrder":
      return "Page position";
    case "pageTemplate":
      return "Page template";
    case "pageTrash":
      return "Page deleted on one device and restored on another";
    case "element":
      return `Element ${GROUPS[c.target.group ?? 0] ?? "properties"} changed on two devices`;
    case "elementTrash":
      return "Element deleted on one device and restored on another";
    case "editedWhileDeleted":
      return "Element edited on one device while deleted on another";
  }
}

export function resolvable(c: SyncConflict): boolean {
  return ["notebookTitle", "notebookCover", "element", "editedWhileDeleted"].includes(c.target.kind);
}

export function conflictValuePreview(c: SyncConflict, value: unknown): string {
  if (typeof value === "string") return value;
  if (typeof value === "boolean") return value ? "Kept" : "Deleted";
  if (value && typeof value === "object") {
    const v = value as Record<string, unknown>;
    if (v.type === "text" && typeof v.text === "string") return v.text.length > 400 ? `${v.text.slice(0, 400)}…` : v.text;
    if (v.type === "ink") return `Handwritten stroke (${Array.isArray(v.points) ? v.points.length : 0} points, ${String(v.color ?? "")})`;
    if (v.type === "image") return "Image";
    if (typeof v.kind === "string") return `Template: ${v.kind}`;
  }
  return c.target.kind === "pageOrder" ? "A different position" : "A different version";
}
