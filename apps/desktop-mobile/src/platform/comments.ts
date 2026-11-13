// Comments and mentions in shared notebooks: typed wrappers over the Rust commands and events,
// and the pure helpers the UI builds on (filters, delivery wording, safe rendering of a body).
//
// Rust owns the comment stream, who wrote what (never a name the client supplies), queueing while
// offline and the delivery state of every comment. The interfaces mirror the Rust types (camelCase
// through serde); swap them for the ts-rs output in src/generated when it covers them.
//
// A comment body is plain text. It is only ever shown as text: `bodySegments` splits it into text,
// mention and link parts, and the templates print each with `{{ }}`, never `v-html`.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { cleanName, cleanNames, cleanText, revealedHost } from "./display";
import { CommandError, hasHost } from "./ipc";

/** A rectangle in page coordinates (points), top-left origin. */
export interface CommentRect {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** What a thread is attached to. Tagged JSON, mirroring the core `CommentAnchor` variants. */
export type AnchorView =
  | { kind: "point"; pageId: string; x: number; y: number }
  | { kind: "rect"; pageId: string; rect: CommentRect }
  | { kind: "element"; pageId: string; elementId: string; fallbackRect: CommentRect }
  | { kind: "text"; pageId: string; elementId: string; quote: string; fallbackRect: CommentRect; [sticky: string]: unknown }
  | { kind: "pdf"; pageId: string; pdfPageIndex: number; normalizedRect: CommentRect; rotation: number };

export type OrphanReason = "elementDeleted" | "pageDeleted" | "textChanged" | "missing";

/** Where a thread is now: on a page, or orphaned (its anchor is gone). */
export type Placement = { kind: "placed"; pageId: string; rect: CommentRect } | { kind: "orphaned"; reason: OrphanReason; lastPageId?: string; quote?: string };

/** Where a comment stands. `waiting` and `failed` mean it is NOT delivered. */
export type Delivery = "sent" | "waiting" | "failed" | "draft";

export interface CommentView {
  commentId: string;
  authorAccountId: string;
  /** Verified email, or empty when not known yet. */
  authorDisplay: string;
  body: string;
  mentions: string[];
  /** Unix ms. */
  at: number;
  edited: boolean;
  deleted: boolean;
  removedByOwner: boolean;
  delivery: Delivery;
}

export interface ThreadView {
  threadId: string;
  anchor: AnchorView;
  placement: Placement;
  status: "open" | "resolved";
  resolvedByDisplay?: string;
  comments: CommentView[];
  unread: boolean;
  mine: boolean;
  mentionsMe: boolean;
}

export interface CommentsView {
  /** This account may write comments now (owner, editor, commenter, with a current role). */
  canComment: boolean;
  isModerator: boolean;
  threads: ThreadView[];
}

export interface Mentionable {
  accountId: string;
  display: string;
}

export type NotificationKind = "mention" | "reply";

export interface NotificationItem {
  notificationId: string;
  notebookId: string;
  threadId: string;
  commentId: string;
  kind: NotificationKind;
  actorDisplay: string;
  at: number;
  read: boolean;
}

export interface NotificationsView {
  items: NotificationItem[];
  unread: number;
}

export interface NotificationPrefs {
  mutedNotebooks: string[];
  mutedThreads: { notebookId: string; threadId: string }[];
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!hasHost()) throw new CommandError("noHost", "Comments need the Penroam app.");
  try {
    return cleanNames(await invoke<T>(cmd, args));
  } catch (e: any) {
    if (e && typeof e === "object" && "code" in e) throw new CommandError(e.code, e.message ?? String(e));
    throw new CommandError("io", String(e));
  }
}

export const commentsApi = {
  list: (notebookId: string) => call<CommentsView>("comments_list", { notebookId }),
  /** The anchor for a range of a text block (UTF-16 offsets); Rust adds the sticky positions. */
  textAnchor: (notebookId: string, pageId: string, elementId: string, start: number, end: number) => call<AnchorView>("comments_text_anchor", { notebookId, pageId, elementId, start, end }),
  createThread: (notebookId: string, anchor: AnchorView, body: string) => call<ThreadView>("comments_create_thread", { notebookId, anchor, body }),
  reply: (notebookId: string, threadId: string, body: string, replyTo?: string) => call<void>("comments_reply", { notebookId, threadId, body, replyTo }),
  edit: (notebookId: string, commentId: string, body: string) => call<void>("comments_edit", { notebookId, commentId, body }),
  remove: (notebookId: string, commentId: string) => call<void>("comments_delete", { notebookId, commentId }),
  setResolved: (notebookId: string, threadId: string, resolved: boolean) => call<void>("comments_set_resolved", { notebookId, threadId, resolved }),
  reattach: (notebookId: string, threadId: string, anchor: AnchorView) => call<void>("comments_reattach", { notebookId, threadId, anchor }),
  mentionable: (notebookId: string) => call<Mentionable[]>("comments_mentionable", { notebookId }),
};

export const notificationsApi = {
  list: () => call<NotificationsView>("notifications_list"),
  markRead: (notificationId: string) => call<void>("notifications_mark_read", { notificationId }),
  prefs: () => call<NotificationPrefs>("notification_prefs"),
  setPrefs: (prefs: NotificationPrefs) => call<NotificationPrefs>("notification_prefs_set", { prefs }),
};

export const onCommentsChanged = (cb: (notebookId: string) => void): Promise<UnlistenFn> => listen<{ notebookId: string }>("comments://changed", (e) => cb(e.payload.notebookId));
export const onNotificationsChanged = (cb: () => void): Promise<UnlistenFn> => listen("notifications://changed", () => cb());

// ------------------------------------------------------------------ filters --

export type CommentFilter = "open" | "resolved" | "mine" | "mentions";
export const FILTERS: { id: CommentFilter; label: string }[] = [
  { id: "open", label: "Open" },
  { id: "resolved", label: "Resolved" },
  { id: "mine", label: "Mine" },
  { id: "mentions", label: "Mentions" },
];

/** Whether a thread passes a filter. Orphaned threads follow the same rules (they have their own section). */
export function matchesFilter(t: ThreadView, f: CommentFilter): boolean {
  switch (f) {
    case "open":
      return t.status === "open";
    case "resolved":
      return t.status === "resolved";
    case "mine":
      return t.mine;
    case "mentions":
      return t.mentionsMe;
  }
}

/** Threads split into those on a page and the orphaned ones, oldest first within each. */
export function splitThreads(threads: readonly ThreadView[], f: CommentFilter): { placed: ThreadView[]; orphaned: ThreadView[] } {
  const keep = threads.filter((t) => matchesFilter(t, f)).sort((a, b) => (a.comments[0]?.at ?? 0) - (b.comments[0]?.at ?? 0));
  return { placed: keep.filter((t) => t.placement.kind === "placed"), orphaned: keep.filter((t) => t.placement.kind === "orphaned") };
}

/** Threads with something new, for the badge. */
export const unreadCount = (threads: readonly ThreadView[]) => threads.filter((t) => t.unread).length;

// ----------------------------------------------------------------- wording --

/** How a comment's delivery is worded; null when it was sent (nothing to say). Never calls a queued comment delivered. */
export function deliveryLabel(d: Delivery): string | null {
  switch (d) {
    case "waiting":
      return "Waiting to send";
    case "failed":
      return "Failed — retry";
    case "draft":
      return "Draft (you can no longer comment)";
    default:
      return null;
  }
}

/** Who wrote a comment; a neutral name while the identity isn't known. */
export const authorName = (c: Pick<CommentView, "authorDisplay">) => cleanName(c.authorDisplay).trim() || "Collaborator";

/** Why a thread has no place on the page, in words. */
export function orphanReason(p: Extract<Placement, { kind: "orphaned" }>): string {
  switch (p.reason) {
    case "elementDeleted":
      return "What this was attached to was deleted.";
    case "pageDeleted":
      return "The page this was on was deleted.";
    case "textChanged":
      return "The text this was attached to changed.";
    default:
      return "This can't be placed on the page.";
  }
}

/** A short description of what a thread is attached to, for lists and screen readers. */
export function anchorLabel(a: AnchorView): string {
  switch (a.kind) {
    case "point":
      return "A point on the page";
    case "rect":
      return "An area of the page";
    case "element":
      return "A drawing or object";
    case "text":
      return a.quote ? `Text: “${a.quote.slice(0, 60)}”` : "Some text";
    case "pdf":
      return "A part of the PDF";
  }
}

// ------------------------------------------------------------ safe body text --

/**
 * Mentions in a body: `@[accountId]`. Only the account id is stored: names change and can be email
 * addresses, so they are looked up when shown (`mentionName`). Rust reads the same token to build
 * the notification side-car.
 */
export const MENTION_RE = /@\[([A-Za-z0-9_-]{1,128})\]/g;
/** What a mention of a person who is no longer listed, or never was, is called. */
export const FORMER_MEMBER = "Former member";
const URL_RE = /\bhttps?:\/\/[^\s<>"'`]{1,2000}/g;

export type BodyPart = { t: "text"; s: string } | { t: "mention"; id: string } | { t: "link"; href: string; s: string; /** The real host, shown when the link text doesn't already say it (an international look-alike). */ host?: string };

/** A person a body can name: the mentionable members, plus the authors of the thread. */
export type NameLookup = readonly Mentionable[];

/** The name to show for an account id; unknown or removed members are "Former member". */
export function mentionName(id: string, ...lists: (NameLookup | undefined)[]): string {
  for (const l of lists) {
    const hit = l?.find((m) => m.accountId === id && cleanName(m.display).trim());
    if (hit) return cleanName(hit.display).trim();
  }
  return FORMER_MEMBER;
}

/** An http(s) URL that is safe to open, or null. Anything else (javascript:, data:, …) stays plain text. */
export function safeHttpUrl(raw: string): string | null {
  try {
    const u = new URL(raw);
    return u.protocol === "http:" || u.protocol === "https:" ? u.href : null;
  } catch {
    return null;
  }
}

/** The token for a member: only their account id goes into the body. */
export const mentionToken = (m: Pick<Mentionable, "accountId">) => `@[${m.accountId}]`;

/**
 * Splits a body into text, mentions and links. Nothing here is HTML: every part is a string the
 * template prints as text, so markup in a comment stays visible as markup.
 */
export function bodySegments(rawBody: string): BodyPart[] {
  // controls that force a direction are dropped, so a comment can't reorder what is around it
  const body = cleanText(rawBody);
  const parts: BodyPart[] = [];
  const pushText = (s: string) => {
    let at = 0;
    for (const m of s.matchAll(URL_RE)) {
      // trailing punctuation belongs to the sentence, not the link
      const raw = m[0].replace(/[.,;:!?)\]]+$/u, "");
      const href = safeHttpUrl(raw);
      if (m.index! > at) parts.push({ t: "text", s: s.slice(at, m.index) });
      if (href) parts.push({ t: "link", href, s: raw, host: revealedHost(raw, href) ?? undefined });
      else parts.push({ t: "text", s: raw });
      at = m.index! + raw.length;
    }
    if (at < s.length) parts.push({ t: "text", s: s.slice(at) });
  };
  let at = 0;
  for (const m of body.matchAll(MENTION_RE)) {
    if (m.index! > at) pushText(body.slice(at, m.index));
    parts.push({ t: "mention", id: m[1] });
    at += m.index! - at + m[0].length;
  }
  if (at < body.length) pushText(body.slice(at));
  return parts;
}

/** The body with mention tokens replaced by `@Name`, for plain-text uses (previews, aria labels). */
export const plainBody = (body: string, ...lists: (NameLookup | undefined)[]) => body.replace(MENTION_RE, (_m, id: string) => `@${mentionName(id, ...lists)}`);

/** Mention candidates for text after an `@`, from the listed members only, best match first. */
export function suggestMentions(members: readonly Mentionable[], query: string): Mentionable[] {
  const q = query.trim().toLowerCase();
  return members
    .filter((m) => !q || m.display.toLowerCase().includes(q))
    .sort((a, b) => Number(b.display.toLowerCase().startsWith(q)) - Number(a.display.toLowerCase().startsWith(q)) || a.display.localeCompare(b.display))
    .slice(0, 6);
}

/** The longest body Rust accepts (8 KiB of UTF-8). */
export const MAX_COMMENT_BYTES = 8192;
export const commentTooLong = (body: string) => new TextEncoder().encode(body).length > MAX_COMMENT_BYTES;

// ------------------------------------------------------------ composer text --

/**
 * The composer shows `@Name` and keeps the chosen members beside the text, so what is typed
 * reads normally. This turns a stored body into that form (for editing a comment).
 */
export function tokensToDisplay(body: string, ...lists: (NameLookup | undefined)[]): { text: string; chosen: Mentionable[] } {
  const chosen = new Map<string, Mentionable>();
  const used = new Set<string>();
  const text = body.replace(MENTION_RE, (_m, id: string) => {
    let c = chosen.get(id);
    if (!c) {
      // two people with the same name (two former members) must stay apart on the way back
      const base = mentionName(id, ...lists);
      let name = base;
      for (let n = 2; used.has(name); n++) name = `${base} (${n})`;
      used.add(name);
      c = { accountId: id, display: name };
      chosen.set(id, c);
    }
    return `@${c.display}`;
  });
  return { text, chosen: [...chosen.values()] };
}

const escapeRe = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/** The inverse: `@Name` of a chosen member becomes the token Rust reads; other `@words` stay text. */
export function displayToTokens(text: string, chosen: readonly Mentionable[]): string {
  let out = text;
  // longest names first, so "@Ann Lee" is not read as "@Ann" plus " Lee"
  for (const m of [...chosen].sort((a, b) => b.display.length - a.display.length)) {
    out = out.replace(new RegExp(`@${escapeRe(m.display)}(?![\\p{L}\\p{N}_])`, "gu"), () => mentionToken(m));
  }
  return out;
}

/** The members still mentioned in the text (a chip whose `@Name` was deleted drops out). */
export const stillMentioned = (text: string, chosen: readonly Mentionable[]) => chosen.filter((m) => new RegExp(`@${escapeRe(m.display)}(?![\\p{L}\\p{N}_])`, "u").test(text));

/** The `@query` being typed just before the caret, or null. */
export function mentionQuery(text: string, caret: number): { start: number; query: string } | null {
  const m = /(^|\s)@([^\s@]{0,30})$/u.exec(text.slice(0, caret));
  return m ? { start: caret - m[2].length - 1, query: m[2] } : null;
}
