// Comment anchors drawn over the page: a marker for each thread on this page, and a preview while
// an area is being chosen.
//
// The layer is a child of the page's world element (page coordinates; the world's transform does
// the zooming and panning), separate from the ink layers and from the presence layer. Only the
// markers take the pointer, and they stop it before the editor sees it, so tapping one never
// starts a stroke. They are real buttons of at least 44 px on screen (the world's `--inv` scale
// keeps them that size at any zoom), carry a number and a state mark besides their colour, and
// never animate, so nothing moves over a stroke being drawn.
import { anchorLabel, authorName, type CommentRect, type ThreadView } from "../platform/comments";

const SIZE_PX = 44;
// Theme colours from styles/main.css: the dark theme's paper is black, so a fixed dark grey would
// disappear into it. The fallbacks are the light theme's values.
const INK = "var(--pr-comment-ink,#1b1b1b)";
const PAPER = "var(--pr-comment-paper,#fff)";
const TINT = "var(--pr-comment-tint,rgb(27 27 27 / 0.08))";

export class CommentMarkerLayer {
  readonly el: HTMLDivElement;

  constructor(parent: HTMLElement) {
    this.el = document.createElement("div");
    this.el.className = "penroam-comment-markers";
    this.el.style.cssText = "position:absolute;left:0;top:0;width:100%;height:100%;pointer-events:none;overflow:visible;z-index:4;";
    parent.appendChild(this.el);
  }

  /** Draws the threads placed on `pageId`; `selected` also gets an outline of what it is attached to. */
  setThreads(threads: readonly ThreadView[], pageId: string | null, selected: string | null, onOpen: (threadId: string) => void) {
    this.el.replaceChildren();
    if (!pageId) return;
    for (const t of threads) {
      if (t.placement.kind !== "placed" || t.placement.pageId !== pageId) continue;
      const r = t.placement.rect;
      if (t.threadId === selected) this.outline(r);
      // the marker sits at the top-right corner of the anchored area, or on the point itself
      const x = t.anchor.kind === "point" ? r.x : r.x + r.w;
      const y = r.y;
      this.el.appendChild(this.marker(t, x, y, t.threadId === selected, onOpen));
    }
  }

  private marker(t: ThreadView, x: number, y: number, selected: boolean, onOpen: (id: string) => void): HTMLButtonElement {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "penroam-comment-marker";
    b.dataset.thread = t.threadId;
    const n = t.comments.filter((c) => !c.deleted).length;
    const first = t.comments[0];
    const state = [t.status === "resolved" ? "resolved" : "open", t.unread ? "new activity" : "", t.comments.some((c) => c.delivery === "waiting") ? "waiting to send" : "", t.comments.some((c) => c.delivery === "failed") ? "failed to send" : ""].filter(Boolean).join(", ");
    b.setAttribute("aria-label", `Comment thread by ${first ? authorName(first) : "someone"} on ${anchorLabel(t.anchor).toLowerCase()}: ${n} comment${n === 1 ? "" : "s"}, ${state}`);
    if (selected) b.setAttribute("aria-current", "true");
    const filled = t.status === "open";
    b.style.cssText = [
      "position:absolute",
      `left:${x}px`,
      `top:${y}px`,
      `width:${SIZE_PX}px`,
      `height:${SIZE_PX}px`,
      `margin:${-SIZE_PX / 2}px 0 0 ${-SIZE_PX / 2}px`,
      "padding:0",
      "border:0",
      "background:transparent",
      "pointer-events:auto",
      "cursor:pointer",
      "touch-action:manipulation",
      "transform-origin:50% 50%",
      "transform:scale(var(--inv,1))",
    ].join(";");
    const dot = document.createElement("span");
    dot.setAttribute("aria-hidden", "true");
    dot.style.cssText = `display:flex;align-items:center;justify-content:center;width:26px;height:26px;margin:9px;box-sizing:border-box;border-radius:50% 50% 50% 0;font:700 12px system-ui,sans-serif;border:2px solid ${INK};color:${filled ? PAPER : INK};background:${filled ? INK : PAPER};${t.unread ? "outline:3px solid #d9480f;outline-offset:1px;" : ""}`;
    // the count, and a mark for the state: ✓ resolved, ! failed, … waiting
    const mark = t.comments.some((c) => c.delivery === "failed") ? "!" : t.comments.some((c) => c.delivery === "waiting") ? "…" : t.status === "resolved" ? "✓" : String(n);
    dot.textContent = mark;
    b.appendChild(dot);
    // never let the editor see this press as the start of a stroke or a pan
    for (const type of ["pointerdown", "pointerup", "pointermove", "touchstart"]) b.addEventListener(type, (e) => e.stopPropagation());
    b.addEventListener("click", (e) => {
      e.stopPropagation();
      onOpen(t.threadId);
    });
    return b;
  }

  private outline(r: CommentRect) {
    const d = document.createElement("div");
    d.className = "penroam-comment-outline";
    d.setAttribute("aria-hidden", "true");
    d.style.cssText = `position:absolute;left:${r.x}px;top:${r.y}px;width:${r.w}px;height:${r.h}px;box-sizing:border-box;border:calc(2px * var(--inv,1)) dashed ${INK};pointer-events:none;`;
    this.el.appendChild(d);
  }

  /** Shows the area being chosen; null clears it. */
  preview(r: CommentRect | null) {
    this.el.querySelector(".penroam-comment-preview")?.remove();
    if (!r) return;
    const d = document.createElement("div");
    d.className = "penroam-comment-preview";
    d.setAttribute("aria-hidden", "true");
    d.style.cssText = `position:absolute;left:${r.x}px;top:${r.y}px;width:${r.w}px;height:${r.h}px;box-sizing:border-box;border:calc(2px * var(--inv,1)) dashed ${INK};background:${TINT};pointer-events:none;`;
    this.el.appendChild(d);
  }

  destroy() {
    this.el.remove();
  }
}
