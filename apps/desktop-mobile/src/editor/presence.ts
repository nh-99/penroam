// Other people's carets, selections and viewports drawn over the page.
//
// The layer is a child of the page's world element, so page coordinates are all it needs: the
// world's transform does the zooming and panning (and any change of it) for free. It is separate
// from the ink layers, sits above them, ignores the pointer, and is hidden from assistive
// technology (the collaborator strip carries the same facts as text). Markers never animate: there
// is no pulse or fade, so reduced-motion and the ePaper theme need no special case, and each marker
// has a label and a line style besides its colour.
import { peerColor, peerName, peerShape, type PageRect, type PresencePeer } from "../platform/live";

/** Page-space size of marker labels is kept constant on screen with the world's `--inv` scale. */
const LABEL = "position:absolute;left:0;top:-1.4em;white-space:nowrap;font:600 11px system-ui,sans-serif;padding:1px 4px;border-radius:4px;color:#fff;unicode-bidi:isolate;transform-origin:0 100%;transform:scale(var(--inv,1));pointer-events:none;";

export class PresenceLayer {
  readonly el: HTMLDivElement;

  constructor(parent: HTMLElement) {
    this.el = document.createElement("div");
    this.el.className = "penroam-presence";
    this.el.setAttribute("aria-hidden", "true");
    this.el.style.cssText = "position:absolute;left:0;top:0;width:100%;height:100%;pointer-events:none;overflow:visible;z-index:5;";
    parent.appendChild(this.el);
  }

  /**
   * Draws the peers that are on `pageId`. `blockRect` finds a text block's bounds when a peer
   * has a caret or selection in it but sent no rectangle of its own.
   */
  setPeers(peers: readonly PresencePeer[], pageId: string | null, blockRect?: (id: string) => PageRect | null) {
    this.el.replaceChildren();
    if (!pageId) return;
    for (const p of peers) {
      if (p.pageId !== pageId || p.kind === "here") continue;
      const color = peerColor(p.colorIndex);
      const name = `${peerShape(p.colorIndex)} ${peerName(p)}`;
      if (p.kind === "caret" && p.caret) {
        const box = this.box({ x: p.caret.x - 1, y: p.caret.y, w: 2, h: Math.max(p.caret.h, 8) }, color, "solid");
        box.style.background = color;
        this.label(box, name, color);
      } else if (p.kind === "selection") {
        const r = p.rect ?? (p.blockId ? blockRect?.(p.blockId) : null);
        if (!r) continue;
        const box = this.box(r, color, "solid");
        // a hatch as well as a tint, so the selection isn't only a colour
        box.style.background = `repeating-linear-gradient(135deg, ${color}33 0 6px, ${color}1a 6px 12px)`;
        this.label(box, name, color);
      } else if (p.kind === "viewport" && p.rect) {
        const box = this.box(p.rect, color, "dashed");
        this.label(box, `${name} is looking here`, color);
      } else if (p.kind === "caret" && p.blockId) {
        // no exact caret: outline the block they are typing in
        const r = blockRect?.(p.blockId);
        if (!r) continue;
        const box = this.box(r, color, "solid");
        this.label(box, `${name} is typing`, color);
      }
    }
  }

  private box(r: PageRect, color: string, style: "solid" | "dashed"): HTMLDivElement {
    const d = document.createElement("div");
    d.className = "penroam-presence-marker";
    d.style.cssText = `position:absolute;left:${r.x}px;top:${r.y}px;width:${r.w}px;height:${r.h}px;box-sizing:border-box;border:calc(2px * var(--inv,1)) ${style} ${color};pointer-events:none;`;
    this.el.appendChild(d);
    return d;
  }

  private label(box: HTMLElement, text: string, color: string) {
    const l = document.createElement("span");
    l.style.cssText = `${LABEL}background:${color};`;
    l.textContent = text;
    box.appendChild(l);
  }

  destroy() {
    this.el.remove();
  }
}
