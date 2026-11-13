// Live collaboration in shared notebooks: who else is here and where (presence), whether the live
// channel is connected, and the awareness this device sends. Rust owns the channel, the encryption
// and the durable sync; this module is the typed edge of it for the UI.
//
// The interfaces mirror the Rust types (camelCase through serde); swap them for the ts-rs output in
// src/generated when it covers them. Awareness is only ever a hint about where somebody is: it
// never counts as proof that anything was saved or shared.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { cleanName, cleanNames } from "./display";
import { CommandError, hasHost } from "./ipc";
import type { SharedRole } from "./sharing";

/** A rectangle in page coordinates. */
export interface PageRect {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** A caret in page coordinates: the top of the line it is on, and the line's height. */
export interface PageCaret {
  x: number;
  y: number;
  h: number;
}

/** What a peer is showing: they are on a page, have a caret or selection, or are looking at an area. */
export type AwarenessKind = "here" | "caret" | "selection" | "viewport";

/** One other person's session in a shared notebook. */
export interface PresencePeer {
  sessionId: string;
  /** Verified email, or empty while it isn't known: shown as a neutral label. */
  memberDisplay: string;
  role: SharedRole;
  pageId: string;
  kind: AwarenessKind;
  rect?: PageRect;
  caret?: PageCaret;
  /** The text block a caret or selection is in. */
  blockId?: string;
  /** Index into the palette, stable for the person during the session. */
  colorIndex: number;
  /** Unix ms after which this entry is stale. */
  expiresAt: number;
}

export interface PresenceEvent {
  notebookId: string;
  peers: PresencePeer[];
}

/** State of the live channel for a notebook. */
export type LiveState = "connecting" | "live" | "offline" | "disabled";

export interface LiveStatusEvent {
  notebookId: string;
  state: LiveState;
}

/** What this device tells the others; the page is always given, the rest depends on `kind`. */
export interface AwarenessPayload {
  pageId: string;
  kind: AwarenessKind;
  rect?: PageRect;
  caret?: PageCaret;
  blockId?: string;
}

/** Device preference for presence. */
export interface LivePrefs {
  sharePresence: boolean;
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!hasHost()) throw new CommandError("noHost", "Live collaboration needs the Penroam app.");
  try {
    return cleanNames(await invoke<T>(cmd, args));
  } catch (e: any) {
    if (e && typeof e === "object" && "code" in e) throw new CommandError(e.code, e.message ?? String(e));
    throw new CommandError("io", String(e));
  }
}

export const liveApi = {
  awareness: (notebookId: string, payload: AwarenessPayload) => call<void>("live_awareness", { notebookId, payload }),
  /** The presence preference; with `prefs` it is set first. */
  prefs: (prefs?: LivePrefs) => call<LivePrefs>("live_prefs", prefs ? { prefs } : undefined),
};

export const onLivePresence = (cb: (e: PresenceEvent) => void): Promise<UnlistenFn> => listen<PresenceEvent>("live://presence", (e) => cb(cleanNames(e.payload)));
export const onLiveStatus = (cb: (e: LiveStatusEvent) => void): Promise<UnlistenFn> => listen<LiveStatusEvent>("live://status", (e) => cb(e.payload));

/** What the sync indicator says about the live channel of a shared notebook; null when there is nothing to say. */
export function liveStatusLabel(s: LiveState | undefined): string | null {
  switch (s) {
    case "live":
      return "Live";
    case "connecting":
      return "Connecting…";
    case "offline":
      return "Live disconnected — changes will sync when online";
    default:
      return null;
  }
}

/** What a peer is called: their email, or a neutral name when this device doesn't know it yet. */
export const peerName = (p: Pick<PresencePeer, "memberDisplay">): string => cleanName(p.memberDisplay).trim() || "Collaborator";

/** One or two letters for an avatar; "?" for an unknown person. */
export function peerInitials(p: Pick<PresencePeer, "memberDisplay">): string {
  const name = cleanName(p.memberDisplay).trim();
  if (!name) return "?";
  const local = name.split("@")[0];
  const words = local.split(/[._\-\s]+/u).filter(Boolean);
  const letters = words.length > 1 ? [words[0], words[1]] : [local];
  return Array.from(letters.map((w) => Array.from(w)[0] ?? "").join("")).slice(0, 2).join("").toUpperCase() || "?";
}

/** Palette for remote people; shapes accompany the colours so they are never the only cue. */
export const PEER_COLORS = ["#d9480f", "#1971c2", "#2f9e44", "#9c36b5", "#c2255c", "#0c8599", "#e8590c", "#5f3dc4"] as const;
export const PEER_SHAPES = ["●", "■", "▲", "◆", "★", "✚", "⬟", "⬢"] as const;
export const peerColor = (i: number) => PEER_COLORS[((i % PEER_COLORS.length) + PEER_COLORS.length) % PEER_COLORS.length];
export const peerShape = (i: number) => PEER_SHAPES[((i % PEER_SHAPES.length) + PEER_SHAPES.length) % PEER_SHAPES.length];

/** Peers that haven't expired, one per session, in a stable order. */
export function activePeers(peers: readonly PresencePeer[], now = Date.now()): PresencePeer[] {
  return peers.filter((p) => p.expiresAt > now).sort((a, b) => a.sessionId.localeCompare(b.sessionId));
}

/** The longest an awareness packet waits: at most 15 are sent per second. */
export const AWARENESS_MIN_INTERVAL_MS = Math.ceil(1000 / 15);

/**
 * Sends awareness at a bounded rate. Newer frames of a kind replace older ones that haven't gone
 * out, so a drag of the view costs one frame per interval, not one per event. While `canSend` says
 * no (a stroke is being drawn) nothing goes out and nothing is dropped: the latest frames follow
 * once it says yes. Pen samples never pass through here.
 */
export class AwarenessSender {
  private pending = new Map<AwarenessKind, AwarenessPayload>();
  private last = -Infinity;
  private timer: ReturnType<typeof setTimeout> | null = null;

  constructor(
    private send: (p: AwarenessPayload) => void | Promise<unknown>,
    private canSend: () => boolean = () => true,
    private now: () => number = () => Date.now(),
    private minInterval = AWARENESS_MIN_INTERVAL_MS,
  ) {}

  /** Queues a frame. A page change drops caret and selection frames from the page before. */
  push(p: AwarenessPayload) {
    if (p.kind === "here") for (const [k, q] of this.pending) if (q.pageId !== p.pageId) this.pending.delete(k);
    this.pending.set(p.kind, p);
    this.schedule();
  }

  /** Forgets what hasn't been sent (leaving the notebook, or presence turned off). */
  clear() {
    this.pending.clear();
    if (this.timer) clearTimeout(this.timer);
    this.timer = null;
  }

  private schedule() {
    if (this.timer || !this.pending.size) return;
    const wait = Math.max(0, this.last + this.minInterval - this.now());
    this.timer = setTimeout(() => this.flush(), this.canSend() ? wait : Math.max(wait, 100));
  }

  private flush() {
    this.timer = null;
    if (!this.pending.size) return;
    if (!this.canSend()) return this.schedule();
    // one frame per interval, in the order that matters: where I am, then what I'm doing there
    const order: AwarenessKind[] = ["here", "selection", "caret", "viewport"];
    const kind = order.find((k) => this.pending.has(k))!;
    const p = this.pending.get(kind)!;
    this.pending.delete(kind);
    this.last = this.now();
    void Promise.resolve(this.send(p)).catch(() => {});
    this.schedule();
  }
}
