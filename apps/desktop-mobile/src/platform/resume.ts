// Where the user left off: the folder being browsed, the open notebook and page, and how the
// page was scrolled and zoomed. The app writes this as the user moves around and reads it once at
// start-up, so quitting and reopening comes back to the same place.
//
// It is per-device UI state, like the settings, so it lives in localStorage and never syncs.
// Everything in it is a hint: a notebook, page or folder that is gone by the next start (trashed,
// deleted on another device, in a locked vault) is skipped, and the library opens instead.

const KEY = "penroam.resume.v1";

/** How a page was shown: the zoom and the document point (pt, from the page's corner) at the view's top-left. */
export interface ViewSpot {
  scale: number;
  x: number;
  y: number;
}

/** A page's view, with the page it belongs to. */
export interface SavedView extends ViewSpot {
  notebookId: string;
  pageId: string;
}

/** The place to come back to. `null` notebook means the library. */
export interface Resume {
  notebookId: string | null;
  pageId: string | null;
  /** The folder the library was showing; `null` is the top level. */
  folderId: string | null;
  view: SavedView | null;
}

const empty: Resume = { notebookId: null, pageId: null, folderId: null, view: null };

const str = (v: unknown) => (typeof v === "string" && v ? v : null);
const num = (v: unknown) => typeof v === "number" && Number.isFinite(v);

/** Reads the saved place; anything missing or malformed reads as the library's top level. */
export function readResume(): Resume {
  let saved: Record<string, unknown>;
  try {
    saved = JSON.parse(localStorage.getItem(KEY) ?? "{}") ?? {};
  } catch {
    return { ...empty };
  }
  const v = saved.view as Record<string, unknown> | null | undefined;
  const view =
    v && str(v.notebookId) && str(v.pageId) && num(v.scale) && (v.scale as number) > 0 && num(v.x) && num(v.y)
      ? { notebookId: v.notebookId as string, pageId: v.pageId as string, scale: v.scale as number, x: v.x as number, y: v.y as number }
      : null;
  return { notebookId: str(saved.notebookId), pageId: str(saved.pageId), folderId: str(saved.folderId), view };
}

/** Updates part of the saved place. Storage that is full or turned off just forgets it. */
export function writeResume(patch: Partial<Resume>) {
  try {
    localStorage.setItem(KEY, JSON.stringify({ ...readResume(), ...patch }));
  } catch {
    /* nothing to come back to next time; nothing else depends on it */
  }
}

/** The view to put back on the first page opened after start-up, until the editor takes it. */
let pending: SavedView | null = null;

/** Sets the view the editor should restore (at start-up, from `readResume`). */
export function setPendingView(v: SavedView | null) {
  pending = v;
}

/**
 * The view to restore for this page, if start-up left one for it. One-shot: the first page the
 * editor opens takes or drops it, so moving around later always starts from the usual fit.
 */
export function takePendingView(notebookId: string, pageId: string): ViewSpot | null {
  const v = pending;
  pending = null;
  return v && v.notebookId === notebookId && v.pageId === pageId ? v : null;
}
