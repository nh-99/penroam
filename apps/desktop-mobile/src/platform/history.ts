// Version history for display. Every pen stroke is a version, far too many to list, so
// versions close together become one writing session, shown as its final state.
import { invoke } from "@tauri-apps/api/core";
import { cleanName, cleanNames } from "./display";
import { CommandError, hasHost, type HistoryRow } from "./ipc";
import type { ConflictTarget } from "./sync";
import type { ActivityEntryView } from "../generated/ActivityEntryView";
import type { ActivityKindView } from "../generated/ActivityKindView";
import type { ActivityPage as GeneratedActivityPage } from "../generated/ActivityPage";
import type { AuthorView as GeneratedAuthor } from "../generated/AuthorView";
import type { DiffRun } from "../generated/DiffRun";
import type { RestoreItemKind as GeneratedItemKind } from "../generated/RestoreItemKind";
import type { RestoreItemView } from "../generated/RestoreItemView";
import type { RestorePreview } from "../generated/RestorePreview";
import type { VersionComparison } from "../generated/VersionComparison";
import type { VersionRef as GeneratedVersionRef } from "../generated/VersionRef";

/** A pause this long (ms) starts a new session. */
const SESSION_GAP = 2 * 60 * 1000;

export interface HistorySession {
  /** The version at the end of the session, on this device. */
  revision: number | null;
  /** The same, in the account's operation log. */
  position: number | null;
  at: number;
  added: number;
  removed: number;
  /** Who made the changes in it, without repeats; absent when nothing says. */
  authors?: AuthorView[];
}

/** Whether a session came from the account rather than this device's own record. */
export const fromAccount = (s: HistorySession): boolean => s.revision === null;

/**
 * Groups versions (oldest first) into sessions, oldest first. The very first version stands
 * alone: it's the state before any recorded change, the one thing a session's end can't show.
 * A session never spans the two sources, because a version is restored from one or the other.
 */
export function groupSessions(points: HistoryRow[]): HistorySession[] {
  const out: HistorySession[] = [];
  let lastAt = -Infinity;
  for (const p of points) {
    const s = out[out.length - 1];
    if (s && out.length > 1 && p.at - lastAt < SESSION_GAP && (s.revision === null) === (p.revision === null)) {
      s.revision = p.revision;
      s.position = p.position;
      s.at = p.at;
      s.added += p.added;
      s.removed += p.removed;
      if (p.authors?.length) s.authors = mergeAuthors(s.authors, p.authors);
    } else {
      out.push({ revision: p.revision, position: p.position, at: p.at, added: p.added, removed: p.removed, ...(p.authors?.length ? { authors: p.authors } : {}) });
    }
    lastAt = p.at;
  }
  return out;
}

// ------------------------------------------------------------------ authors --

/** Who a change is attributed to. The server's record decides, never the device that relayed it; `kind` says how sure that is. */
export type AuthorView = GeneratedAuthor;

/** What to call an author. Never invents a name: unknown stays unknown. */
export function authorLabel(a: AuthorView): string {
  switch (a.kind) {
    case "member":
      return cleanName(a.display).trim() || "Collaborator";
    case "former":
      return "Former member";
    case "erased":
      return "Former collaborator";
    case "beforeSharing":
      return "Before sharing";
    case "unverified":
      return "Unverified author";
    default:
      return "Unknown";
  }
}

const authorKey = (a: AuthorView) => `${a.kind}:${a.accountId ?? a.display}`;

/** Both lists of authors, each once. */
export function mergeAuthors(a: readonly AuthorView[] | undefined, b: readonly AuthorView[]): AuthorView[] {
  const seen = new Map<string, AuthorView>();
  for (const x of [...(a ?? []), ...b]) seen.set(authorKey(x), x);
  return [...seen.values()];
}

/** A list of authors in one phrase, for a version row. */
export const authorsLine = (authors: readonly AuthorView[] | undefined) => (authors?.length ? authors.map(authorLabel).join(", ") : "");

// ----------------------------------------------------------------- activity --

export type ActivityKind = ActivityKindView;
export type ActivityEntry = ActivityEntryView;
export type ActivityPage = GeneratedActivityPage;

const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

/** One line for an entry, from its kind and count only: nothing is read into the encrypted content. */
export function activityText(e: Pick<ActivityEntry, "kind" | "count">): string {
  switch (e.kind) {
    case "strokesAdded":
      return `Added ${plural(e.count, "stroke")}`;
    case "strokesErased":
      return `Erased ${plural(e.count, "stroke")}`;
    case "textEdited":
      return `Edited ${plural(e.count, "text block")}`;
    case "imagesAdded":
      return `Added ${plural(e.count, "image")}`;
    case "elementsChanged":
      return `Changed ${plural(e.count, "element")}`;
    case "pageAdded":
      return `Added ${plural(e.count, "page")}`;
    case "pageMoved":
      return `Moved ${plural(e.count, "page")}`;
    case "pageDeleted":
      return `Deleted ${plural(e.count, "page")}`;
    case "pageRestored":
      return `Restored ${plural(e.count, "page")}`;
    case "template":
      return "Changed the paper";
    case "renamed":
      return "Renamed the notebook";
    case "pdfImported":
      return `Imported ${plural(e.count, "PDF page")}`;
    case "notebook":
      return "Changed the notebook";
  }
}

export const ACTIVITY_KINDS: { id: ActivityKind; label: string }[] = [
  { id: "strokesAdded", label: "Strokes added" },
  { id: "strokesErased", label: "Strokes erased" },
  { id: "textEdited", label: "Text edited" },
  { id: "imagesAdded", label: "Images added" },
  { id: "elementsChanged", label: "Elements changed" },
  { id: "pageAdded", label: "Pages added" },
  { id: "pageMoved", label: "Pages moved" },
  { id: "pageDeleted", label: "Pages deleted" },
  { id: "pageRestored", label: "Pages restored" },
  { id: "template", label: "Paper changed" },
  { id: "renamed", label: "Renamed" },
  { id: "pdfImported", label: "PDF imported" },
  { id: "notebook", label: "Notebook changes" },
];

export interface ActivityFilter {
  pageId: string | "";
  /** `authorKey` of one author, or "". */
  person: string;
  kind: ActivityKind | "";
}

/** The key the person filter uses for an author. */
export const personKey = authorKey;

/** Entries that pass the filters, newest first. */
export function filterActivity(entries: readonly ActivityEntry[], f: ActivityFilter): ActivityEntry[] {
  return entries
    .filter((e) => (!f.pageId || e.pageId === f.pageId) && (!f.person || authorKey(e.author) === f.person) && (!f.kind || e.kind === f.kind))
    .sort((a, b) => (b.acceptedAt ?? b.at) - (a.acceptedAt ?? a.at));
}

/** The people in a list of entries, each once, for the filter. */
export function activityPeople(entries: readonly ActivityEntry[]): AuthorView[] {
  return mergeAuthors(undefined, entries.map((e) => e.author));
}

// ------------------------------------------------------------------ compare --

export type TextDiffPart = DiffRun;
export type VersionCompare = VersionComparison;

// ------------------------------------------------------------------ restore --

export type RestoreItemKind = GeneratedItemKind;
export type RestoreItem = RestoreItemView;
export type RestorePlan = RestorePreview;

/** Where an item is, for its words: the page number when the plan is about one page of a notebook. */
export interface RestoreContext {
  pageNumbers?: Record<string, number>;
}

const things = (n: number | undefined) => (n === 1 ? "1 thing" : `${n ?? "some"} things`);

/** What a restore item does, in words. */
export function restoreItemText(i: RestoreItem, ctx: RestoreContext = {}): string {
  const n = i.pageId ? ctx.pageNumbers?.[i.pageId] : undefined;
  const page = n ? `page ${n}` : "a page";
  switch (i.kind) {
    case "pageContent":
      return `Bring back ${n ? `page ${n}'s` : "a page's"} content${i.count ? ` (${i.count} ${i.count === 1 ? "change" : "changes"})` : ""}`;
    case "removeAdditions":
      return i.pageId ? `Remove ${things(i.count)} added to ${page} since then` : `Remove ${things(i.count)} added since then`;
    case "trashNewer":
      return n ? `Move page ${n}, added since then, to the trash` : "Move a page added since then to the trash";
    case "pageOrder":
      return "Put the pages back in their old order";
    case "readd":
      return i.pageId && !i.elementId ? "Bring back a deleted page" : "Bring back something that was deleted";
    case "revert":
      return "Put an element back the way it was";
    case "keptAddition":
      return "Added since then: stays as it is";
    case "recoveredCopy":
      return "Restore the old text as a recovered copy next to the current one";
    case "template":
      return "Put the paper back the way it was";
    case "title":
      return "Put the notebook's title back";
    case "unavailable":
      return "Can't be brought back";
  }
}

/**
 * Ticking one of "put it back" and "restore as a copy" for the same block untick the other: both
 * together are refused. Returns the ticked ids after `id` was switched on or off.
 */
export function toggleItem(plan: RestorePlan, ticked: ReadonlySet<string>, id: string, on: boolean): Set<string> {
  const next = new Set(ticked);
  if (!on) {
    next.delete(id);
    return next;
  }
  next.add(id);
  const item = plan.items.find((i) => i.itemId === id);
  if (item?.elementId && (item.kind === "revert" || item.kind === "recoveredCopy")) {
    const other = item.kind === "revert" ? "recoveredCopy" : "revert";
    for (const o of plan.items) if (o.kind === other && o.elementId === item.elementId) next.delete(o.itemId);
  }
  return next;
}

/** The items to tick first: the plan's choice, with a block's revert and copy never both. */
export function defaultTicks(plan: RestorePlan): Set<string> {
  let ticked = new Set<string>();
  // copies go first, so that where both were preselected the plain revert is the one that stays
  const order = [...plan.items].sort((a, b) => Number(b.kind === "recoveredCopy") - Number(a.kind === "recoveredCopy"));
  for (const i of order) if (i.selected && i.editable && i.kind !== "keptAddition" && i.kind !== "unavailable") ticked = toggleItem(plan, ticked, i.itemId, true);
  return ticked;
}

/** Whether a failed restore commit means the page moved on after the preview was made. */
export const changedSincePreview = (e: unknown): boolean => /^changed_since_preview:/.test((e as Error)?.message ?? String(e));

/** Items other people changed since: they are what the warning is about. */
export const othersChanged = (plan: RestorePlan) => plan.items.filter((i) => (i.kind === "revert" || i.kind === "recoveredCopy") && i.authors.length > 0);

/** Items that are information only: nothing is sent for them. */
export const informational = (i: RestoreItem) => i.kind === "keptAddition" || i.kind === "unavailable";

// ---------------------------------------------------------------- commands --

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!hasHost()) throw new CommandError("noHost", "History needs the Penroam app.");
  try {
    return cleanNames(await invoke<T>(cmd, args));
  } catch (e: any) {
    if (e && typeof e === "object" && "code" in e) throw new CommandError(e.code, e.message ?? String(e));
    throw new CommandError("io", String(e));
  }
}

/** A version of a page: its place in the account's log, a revision on this device, or the current page. */
export type VersionRef = GeneratedVersionRef;

/** The reference for a listed version, or null when it has neither a position nor a revision. */
export function versionRef(v: { position: number | null; revision: number | null }): VersionRef | null {
  return v.position !== null ? { position: v.position } : v.revision !== null ? { revision: v.revision } : null;
}

export const historyApi = {
  activity: (notebookId: string, cursor?: string) => call<ActivityPage>("history_activity", { notebookId, cursor }),
  compare: (notebookId: string, pageId: string, from: VersionRef, to: VersionRef) => call<VersionCompare>("version_compare", { notebookId, pageId, from, to }),
  restorePreview: (notebookId: string, pageId: string, ref: { position?: number; revision?: number }, elementIds?: string[]) => call<RestorePlan>("restore_preview", { notebookId, pageId, position: ref.position, revision: ref.revision, elementIds }),
  restoreCommit: (notebookId: string, planId: string, selectedItemIds: string[]) => call<void>("restore_commit", { notebookId, planId, selectedItemIds }),
  notebookRestorePreview: (notebookId: string, at: number) => call<RestorePlan>("notebook_restore_preview", { notebookId, at }),
  notebookRestoreCommit: (notebookId: string, planId: string, selected: string[]) => call<void>("notebook_restore_commit", { notebookId, planId, selected }),
  /** Keeps the shown version and makes the alternate `opId` a copy next to it. */
  keepBoth: (notebookId: string, target: ConflictTarget, opId: string) => call<void>("sync_keep_both", { notebookId, target, opId }),
};
