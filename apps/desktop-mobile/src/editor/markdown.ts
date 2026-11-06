// TypeScript port of crates/penroam-core/src/markdown.rs: Markdown text blocks
// to a Paint of positioned runs, line segments and formulas. Both sides read
// the same metrics and are checked against
// tests/fixtures/markdown-layout-golden.json, so a block looks the same on
// screen and in an exported PDF. Keep the two files in step; the module docs
// there give the rules and invariants.
//
// markdown-it stands in for pulldown-cmark. The tree it builds here matches the
// Rust one: soft breaks are line breaks, HTML is literal text, images show their
// alt text, and containers past MAX_DEPTH are flattened.
import MarkdownIt from "markdown-it";
import type Token from "markdown-it/lib/token.mjs";
import type { TextAlign } from "../generated/TextAlign";
import { advanceIn, firstBaseline, lineHeight, metrics, MONO_SCALE, type Face } from "./textLayout";

export const MAX_DEPTH = 16;

const SLOT_FIRST = 0xf0000;
const SLOT_LAST = 0xffffd;
const HEADING_SCALE = [1.75, 1.5, 1.25, 1.1, 1, 0.9];
const ROW_FRAME_DROP = 0.22;
const STROKE_EM = 0.06;

export interface MathSpan {
  tex: string;
  display: boolean;
  source: string;
}
export interface MathBox {
  width: number;
  height: number;
  depth: number;
}
export type MathLookup = (tex: string, display: boolean) => MathBox | null;

export interface Run {
  x: number;
  y: number;
  face: Face;
  size: number;
  text: string;
}
export interface Segment {
  x0: number;
  y0: number;
  x1: number;
  y1: number;
  width: number;
}
export interface MathMark {
  x: number;
  y: number;
  size: number;
  tex: string;
  display: boolean;
}
export interface Paint {
  height: number;
  bottom: number;
  runs: Run[];
  segments: Segment[];
  math: MathMark[];
}

const isSlot = (c: string) => {
  const cp = c.codePointAt(0)!;
  return cp >= SLOT_FIRST && cp <= SLOT_LAST;
};
const slotIndex = (c: string) => c.codePointAt(0)! - SLOT_FIRST;
const isSpace = (c: string | undefined) => c !== undefined && /^\s$/u.test(c);
const isDigit = (c: string | undefined) => c !== undefined && c >= "0" && c <= "9";

function fenceAt(line: string[]): { ch: string; len: number; bare: boolean } | null {
  const start = line.findIndex((c) => c !== " ");
  if (start < 0 || start > 3 || (line[start] !== "`" && line[start] !== "~")) return null;
  const ch = line[start];
  let len = 0;
  while (line[start + len] === ch) len++;
  if (len < 3) return null;
  return { ch, len, bare: line.slice(start + len).every(isSpace) };
}

function scanMath(chars: string[], i: number): [number, number, number, boolean] | null {
  const n = chars.length;
  if (chars[i + 1] === "$") {
    let j = i + 2;
    while (j < n) {
      if (chars[j] === "\\") j += 2;
      else if (chars[j] === "$" && chars[j + 1] === "$") return [i + 2, j, j + 2, true];
      else if (chars[j] === "\n") {
        let k = j + 1;
        while (k < n && chars[k] !== "\n" && isSpace(chars[k])) k++;
        if (k >= n || chars[k] === "\n") return null;
        j++;
      } else j++;
    }
    return null;
  }
  if (i + 1 >= n || isSpace(chars[i + 1])) return null;
  let j = i + 1;
  while (j < n) {
    const c = chars[j];
    if (c === "\n") return null;
    if (c === "\\") j += 2;
    else if (c === "$") return !isSpace(chars[j - 1]) && !isDigit(chars[j + 1]) ? [i + 1, j, j + 1, false] : null;
    else j++;
  }
  return null;
}

function findBackticks(chars: string[], from: number, run: number): number | null {
  let j = from;
  while (j < chars.length && chars[j] !== "\n") {
    if (chars[j] === "`") {
      let len = 0;
      while (chars[j + len] === "`") len++;
      if (len === run) return j + len;
      j += len;
    } else j++;
  }
  return null;
}

export function extractMath(src: string): { source: string; spans: MathSpan[] } {
  const chars = Array.from(src, (c) => (isSlot(c) ? "�" : c));
  const n = chars.length;
  const out: string[] = [];
  const spans: MathSpan[] = [];
  let fence: { ch: string; len: number } | null = null;
  let lineStart = true;
  let i = 0;
  while (i < n) {
    if (lineStart) {
      lineStart = false;
      let end = chars.indexOf("\n", i);
      if (end < 0) end = n;
      const marker = fenceAt(chars.slice(i, end));
      const wasFenced = fence !== null;
      if (!fence && marker) fence = marker;
      else if (fence && marker && marker.bare && marker.ch === fence.ch && marker.len >= fence.len) fence = null;
      if (wasFenced || fence) {
        out.push(...chars.slice(i, end));
        if (end < n) out.push("\n");
        i = end + 1;
        lineStart = true;
        continue;
      }
    }
    const c = chars[i];
    if (c === "\n") {
      out.push(c);
      lineStart = true;
      i++;
    } else if (c === "\\") {
      out.push(c);
      i++;
      if (i < n && chars[i] !== "\n") out.push(chars[i++]);
    } else if (c === "`") {
      let run = 0;
      while (chars[i + run] === "`") run++;
      const end = findBackticks(chars, i + run, run) ?? i + run;
      out.push(...chars.slice(i, end));
      i = end;
    } else if (c === "$") {
      const found = scanMath(chars, i);
      const tex = found ? chars.slice(found[0], found[1]).join("").trim() : "";
      if (found && tex && spans.length <= SLOT_LAST - SLOT_FIRST) {
        out.push(String.fromCodePoint(SLOT_FIRST + spans.length));
        spans.push({ tex, display: found[3], source: chars.slice(i, found[2]).join("") });
        i = found[2];
      } else {
        out.push(c);
        i++;
      }
    } else {
      out.push(c);
      i++;
    }
  }
  return { source: out.join(""), spans };
}

const BOLD = 1, ITALIC = 2, MONO = 4, STRIKE = 8, UNDERLINE = 16;

type Inline = { kind: "text"; text: string; flags: number } | { kind: "break" } | { kind: "math"; index: number };
interface Item {
  task: boolean | null;
  blocks: Block[];
}
type Block =
  | { kind: "para"; inlines: Inline[]; level: number }
  | { kind: "code"; text: string }
  | { kind: "rule" }
  | { kind: "quote"; blocks: Block[] }
  | { kind: "list"; start: number | null; tight: boolean; items: Item[] }
  | { kind: "table"; aligns: TextAlign[]; rows: Inline[][][] };

type Frame =
  | { kind: "root"; blocks: Block[] }
  | { kind: "quote"; blocks: Block[] }
  | { kind: "list"; start: number | null; tight: boolean; items: Item[] }
  | { kind: "item"; item: Item };

const md = new MarkdownIt("commonmark", { html: true, linkify: false, typographer: false }).enable(["table", "strikethrough"]);
// The preset's limit is below MAX_DEPTH once lists and items each count as a level.
(md.options as { maxNesting?: number }).maxNesting = 64;

const TILDE = 0x7e;
/** Delimiters pair by marker, so a lone tilde only closes a lone tilde. */
const LONE_TILDE = 0x7f;

// markdown-it strikes only `~~`; GFM and pulldown-cmark also strike `~`.
md.inline.ruler.at("strikethrough", (state, silent) => {
  if (silent || state.src.charCodeAt(state.pos) !== TILDE) return false;
  // Like pulldown-cmark, `~~` may fall inside a word and `~` may not.
  const single = state.src.charCodeAt(state.pos + 1) !== TILDE;
  const scanned = state.scanDelims(state.pos, !single);
  const token = state.push("text", "", 0);
  token.content = "~".repeat(scanned.length);
  if (scanned.length <= 2) {
    state.delimiters.push({
      marker: scanned.length === 1 ? LONE_TILDE : TILDE,
      length: 0,
      token: state.tokens.length - 1,
      end: -1,
      open: scanned.can_open,
      close: scanned.can_close,
    });
  }
  state.pos += scanned.length;
  return true;
});
md.inline.ruler2.at("strikethrough", (state) => {
  const all = [state.delimiters, ...state.tokens_meta.map((m) => m?.delimiters ?? [])];
  for (const delimiters of all) {
    for (const open of delimiters) {
      if ((open.marker !== TILDE && open.marker !== LONE_TILDE) || open.end === -1) continue;
      const close = delimiters[open.end];
      state.tokens[open.token].type = "s_open";
      state.tokens[open.token].content = "";
      state.tokens[close.token].type = "s_close";
      state.tokens[close.token].content = "";
    }
  }
  return false;
});

class Builder {
  stack: Frame[] = [{ kind: "root", blocks: [] }];
  pushed: boolean[] = [];

  private lines: string[];

  constructor(
    private spans: MathSpan[],
    source: string,
  ) {
    this.lines = source.split("\n");
  }

  private restore(text: string): string {
    let out = "";
    for (const c of text) {
      const m = isSlot(c) ? this.spans[slotIndex(c)] : undefined;
      out += m ? m.source : c;
    }
    return out;
  }

  private blocks(): Block[] {
    const f = this.stack[this.stack.length - 1];
    if (f.kind === "item") return f.item.blocks;
    if (f.kind === "list") {
      if (!f.items.length) f.items.push({ task: null, blocks: [] });
      return f.items[f.items.length - 1].blocks;
    }
    return f.blocks;
  }

  private open(frame: Frame) {
    const room = this.stack.length <= MAX_DEPTH;
    this.pushed.push(room);
    if (room) this.stack.push(frame);
  }

  private close() {
    if (!this.pushed.pop()) return;
    const f = this.stack.pop()!;
    if (f.kind === "quote") this.blocks().push({ kind: "quote", blocks: f.blocks });
    else if (f.kind === "list") this.blocks().push({ kind: "list", start: f.start, tight: f.tight, items: f.items });
    else if (f.kind === "item") {
      const top = this.stack[this.stack.length - 1];
      if (top.kind === "list") top.items.push(f.item);
      else this.blocks().push(...f.item.blocks);
    }
  }

  private pushText(out: Inline[], text: string, flags: number) {
    let cur = "";
    for (const c of text) {
      if (isSlot(c) && slotIndex(c) < this.spans.length) {
        if (cur) out.push({ kind: "text", text: cur, flags });
        cur = "";
        out.push({ kind: "math", index: slotIndex(c) });
      } else cur += c;
    }
    if (cur) out.push({ kind: "text", text: cur, flags });
  }

  /** `flags` are the open styles, shared across an image's alt text. */
  private inlines(tokens: Token[], out: Inline[], style: number[]) {
    const flags = () => style.reduce((f, n, i) => (n > 0 ? f | (1 << i) : f), 0);
    for (const t of tokens) {
      switch (t.type) {
        case "text":
          this.pushText(out, t.content, flags());
          break;
        case "softbreak":
        case "hardbreak":
          out.push({ kind: "break" });
          break;
        case "code_inline":
          out.push({ kind: "text", text: this.restore(t.content), flags: flags() | MONO });
          break;
        case "html_inline":
          out.push({ kind: "text", text: this.restore(t.content), flags: flags() });
          break;
        case "strong_open":
          style[0]++;
          break;
        case "strong_close":
          style[0]--;
          break;
        case "em_open":
          style[1]++;
          break;
        case "em_close":
          style[1]--;
          break;
        case "s_open":
          style[3]++;
          break;
        case "s_close":
          style[3]--;
          break;
        case "link_open":
          style[4]++;
          break;
        case "link_close":
          style[4]--;
          break;
        case "image":
          this.inlines(t.children ?? [], out, style);
          break;
      }
    }
  }

  private inline(t: Token): Inline[] {
    const out: Inline[] = [];
    this.inlines(t.children ?? [], out, [0, 0, 0, 0, 0]);
    return out;
  }

  build(tokens: Token[]): Block[] {
    for (let i = 0; i < tokens.length; i++) {
      const t = tokens[i];
      switch (t.type) {
        case "paragraph_open":
        case "heading_open": {
          const heading = t.type === "heading_open";
          if (!heading && !t.hidden) {
            const parent = this.stack[this.stack.length - 2];
            if (parent?.kind === "list") parent.tight = false;
          }
          const inline = tokens[++i];
          const top = this.stack[this.stack.length - 1];
          // the task marker is the start of the item's first paragraph
          if (!heading && top.kind === "item" && !top.item.blocks.length && top.item.task === null) {
            const first = inline.children?.[0];
            const next = inline.children?.[1];
            const m = first?.type === "text" ? /^\[([ xX\t])\](?:[ \t]+|$)/.exec(first.content) : null;
            // markdown-it trims the line, so the space a marker needs is in the source
            const line = inline.map?.[0] ?? 0;
            const spaced = first!.content.length > 3 || line < this.lines.length - 1 || /\[[ xX\t]\][ \t]/.test(this.lines[line] ?? "");
            if (m && first && spaced && (first.content.length > 3 || !next || next.type === "softbreak")) {
              top.item.task = m[1] !== " " && m[1] !== "\t";
              first.content = first.content.slice(m[0].length);
              // text on the next line follows the marker, it doesn't start a line of its own
              if (!first.content && next?.type === "softbreak") {
                next.type = "text";
                next.content = "";
              }
            }
          }
          const inlines = this.inline(inline);
          i++; // the closing token
          if (inlines.length) this.blocks().push({ kind: "para", inlines, level: heading ? Number(t.tag.slice(1)) : 0 });
          break;
        }
        case "blockquote_open":
          this.open({ kind: "quote", blocks: [] });
          break;
        case "bullet_list_open":
          this.open({ kind: "list", start: null, tight: true, items: [] });
          break;
        case "ordered_list_open":
          this.open({ kind: "list", start: Number(t.attrGet("start") ?? 1), tight: true, items: [] });
          break;
        case "list_item_open":
          this.open({ kind: "item", item: { task: null, blocks: [] } });
          break;
        case "blockquote_close":
        case "bullet_list_close":
        case "ordered_list_close":
        case "list_item_close":
          this.close();
          break;
        case "fence":
        case "code_block":
          this.blocks().push({ kind: "code", text: this.restore(t.content.replace(/\n$/, "")) });
          break;
        case "html_block": {
          const inlines: Inline[] = [];
          this.restore(t.content.trimEnd()).split("\n").forEach((line, n) => {
            if (n > 0) inlines.push({ kind: "break" });
            const text = line.trimStart();
            if (text) inlines.push({ kind: "text", text, flags: 0 });
          });
          this.blocks().push({ kind: "para", inlines, level: 0 });
          break;
        }
        case "hr":
          this.blocks().push({ kind: "rule" });
          break;
        case "table_open": {
          const aligns: TextAlign[] = [];
          const rows: Inline[][][] = [];
          let row: Inline[][] = [];
          for (i++; tokens[i].type !== "table_close"; i++) {
            const c = tokens[i];
            if (c.type === "th_open" && rows.length === 0) {
              const style = c.attrGet("style") ?? "";
              aligns.push(style.includes("center") ? "center" : style.includes("right") ? "right" : "left");
            } else if (c.type === "inline") row.push(this.inline(c));
            else if (c.type === "tr_close") {
              rows.push(row);
              row = [];
            }
          }
          this.blocks().push({ kind: "table", aligns, rows });
          break;
        }
      }
    }
    return (this.stack[0] as { blocks: Block[] }).blocks;
  }
}

const faceOf = (flags: number): Face => {
  if (flags & MONO) return "mono";
  const b = flags & BOLD, i = flags & ITALIC;
  return b && i ? "boldItalic" : b ? "bold" : i ? "italic" : "regular";
};

type Atom =
  | { kind: "char"; c: string; flags: number; size: number; width: number }
  | { kind: "math"; index: number; size: number; width: number; height: number; depth: number };

const atomIsSpace = (a: Atom) => a.kind === "char" && a.c === " ";
const atomIsBlank = (a: Atom) => a.kind === "char" && (a.c === " " || a.c === "\t");
const round6 = (v: number) => Math.round(v * 1e6) / 1e6 + 0;
const sum = (atoms: Atom[], from: number, to: number) => {
  let w = 0;
  for (let i = from; i < to; i++) w += atoms[i].width;
  return w;
};

function charAtom(c: string, flags: number, size: number): Atom {
  const s = flags & MONO ? size * MONO_SCALE : size;
  return { kind: "char", c, flags, size: s, width: (advanceIn(faceOf(flags), c) * s) / metrics.unitsPerEm };
}

class Layout {
  y = 0;
  listDepth = 0;
  marker: { x: number; text: string | null; task: boolean | null } | null = null;
  paint: Paint = { height: 0, bottom: 0, runs: [], segments: [], math: [] };

  constructor(
    private fs: number,
    private row: number | null,
    private align: TextAlign,
    private spans: MathSpan[],
    private lookup: MathLookup,
  ) {}

  gap() {
    return this.row ?? lineHeight(this.fs);
  }
  drop() {
    return this.row !== null ? ROW_FRAME_DROP * this.fs : 0;
  }
  segment(x0: number, y0: number, x1: number, y1: number, width: number) {
    this.paint.segments.push({ x0: round6(x0), y0: round6(y0), x1: round6(x1), y1: round6(y1), width: round6(width) });
  }
  run(x: number, y: number, face: Face, size: number, text: string) {
    this.paint.runs.push({ x: round6(x), y: round6(y), face, size: round6(size), text });
  }

  sourceAtoms(index: number, size: number, out: Atom[]) {
    for (const c of this.spans[index].source) out.push(charAtom(c === "\n" ? " " : c, MONO, size));
  }

  mathAtom(index: number, size0: number, avail: number, out: Atom[]) {
    const s = this.spans[index];
    const b = this.lookup(s.tex, s.display);
    if (!b) return this.sourceAtoms(index, size0, out);
    const wide = b.width * size0;
    const fit = wide > avail && wide > 0 ? Math.max(avail, 1) / wide : 1;
    const size = size0 * fit;
    out.push({ kind: "math", index, size, width: b.width * size, height: b.height * size, depth: b.depth * size });
  }

  place(line: Atom[], x: number, avail: number, align: TextAlign, base: number) {
    let trimmed = line.length;
    while (trimmed > 0 && atomIsBlank(line[trimmed - 1])) trimmed--;
    const atoms = line.slice(0, trimmed);
    const width = sum(atoms, 0, atoms.length);
    let size = 0, tall = 0, deep = 0;
    for (const a of atoms) {
      if (a.kind === "char") size = Math.max(size, a.size);
      else {
        tall = Math.max(tall, a.height);
        deep = Math.max(deep, a.depth);
      }
    }
    if (size === 0) size = base;
    let baseline: number, advance: number;
    if (this.row !== null) {
      const need = Math.max(size, tall + deep);
      const rows = Math.max(Math.ceil(need / this.row - 1e-9), 1);
      baseline = this.y + rows * this.row;
      advance = rows * this.row;
    } else {
      const first = firstBaseline(size);
      const up = Math.max(first, tall);
      const down = Math.max(lineHeight(size) - first, deep);
      baseline = this.y + up;
      advance = up + down;
    }
    const dx = align === "center" ? (avail - width) / 2 : align === "right" ? avail - width : 0;
    let cx = x + dx;
    let i = 0;
    while (i < atoms.length) {
      const a = atoms[i];
      if (a.kind === "math") {
        const s = this.spans[a.index];
        this.paint.math.push({ x: round6(cx), y: round6(baseline), size: round6(a.size), tex: s.tex, display: s.display });
        cx += a.width;
        i++;
        continue;
      }
      const start = cx;
      let text = "";
      for (; i < atoms.length; i++) {
        const b = atoms[i];
        if (b.kind !== "char" || b.flags !== a.flags || b.size !== a.size) break;
        text += b.c === "\t" ? "    " : b.c;
        cx += b.width;
      }
      this.run(start, baseline, faceOf(a.flags), a.size, text);
      const weight = STROKE_EM * a.size;
      if (a.flags & UNDERLINE) {
        const y = baseline + 0.12 * a.size;
        this.segment(start, y, cx, y, weight);
      }
      if (a.flags & STRIKE) {
        const y = baseline - 0.28 * a.size;
        this.segment(start, y, cx, y, weight);
      }
    }
    const mk = this.marker;
    if (mk) {
      this.marker = null;
      if (mk.text !== null) this.run(mk.x, baseline, "regular", this.fs, mk.text);
      if (mk.task !== null) {
        const side = 0.68 * this.fs;
        const x0 = mk.x, y0 = baseline - side, x1 = mk.x + side, y1 = baseline;
        const w = STROKE_EM * this.fs;
        this.segment(x0, y0, x1, y0, w);
        this.segment(x1, y0, x1, y1, w);
        this.segment(x1, y1, x0, y1, w);
        this.segment(x0, y1, x0, y0, w);
        if (mk.task) {
          const mx = x0 + side * 0.4, my = y1 - side * 0.2;
          this.segment(x0 + side * 0.18, y1 - side * 0.5, mx, my, w * 1.4);
          this.segment(mx, my, x1 - side * 0.14, y0 + side * 0.2, w * 1.4);
        }
      }
    }
    const bottom = baseline + Math.max((-metrics.descent * size) / metrics.unitsPerEm, deep);
    this.paint.bottom = Math.max(this.paint.bottom, round6(bottom));
    this.y += advance;
  }

  wrap(source: Atom[], x: number, avail: number, align: TextAlign, base: number) {
    let end = source.length;
    while (end > 0 && atomIsBlank(source[end - 1])) end--;
    const atoms = source.slice(0, end);
    if (!atoms.length) return this.place(atoms, x, avail, align, base);
    let lineStart = 0, i = 0, width = 0;
    let lastBreak: number | null = null;
    while (i < atoms.length) {
      const w = atoms[i].width;
      if (atomIsSpace(atoms[i])) {
        width += w;
        i++;
        lastBreak = i;
        continue;
      }
      if (width + w > avail && i > lineStart) {
        const at: number = lastBreak !== null && lastBreak > lineStart ? lastBreak : i;
        this.place(atoms.slice(lineStart, at), x, avail, align, base);
        lineStart = at;
        lastBreak = null;
        width = sum(atoms, lineStart, i);
        continue;
      }
      width += w;
      i++;
    }
    this.place(atoms.slice(lineStart), x, avail, align, base);
  }

  flow(inlines: Inline[], x: number, avail: number, align: TextAlign, size: number, extra: number) {
    let line: Atom[] = [];
    let placed = false;
    let afterDisplay = false;
    inlines.forEach((item, i) => {
      if (item.kind === "text") {
        for (const c of item.text) line.push(charAtom(c, item.flags | extra, size));
        afterDisplay = false;
      } else if (item.kind === "break") {
        const next = inlines[i + 1];
        const nextDisplay = next?.kind === "math" && this.spans[next.index].display;
        if (!(afterDisplay || (!line.length && nextDisplay))) {
          this.wrap(line, x, avail, align, size);
          placed = true;
        }
        line = [];
        afterDisplay = false;
      } else if (this.spans[item.index].display) {
        if (line.length) {
          this.wrap(line, x, avail, align, size);
          line = [];
        }
        const atoms: Atom[] = [];
        this.mathAtom(item.index, size, avail, atoms);
        this.wrap(atoms, x, avail, "center", size);
        placed = true;
        afterDisplay = true;
      } else {
        this.mathAtom(item.index, size, avail, line);
        afterDisplay = false;
      }
    });
    if (line.length || !placed) this.wrap(line, x, avail, align, size);
  }

  blocks(blocks: Block[], x: number, avail: number, gaps: boolean) {
    blocks.forEach((b, i) => {
      if (i > 0 && gaps) this.y += this.gap();
      this.block(b, x, avail);
    });
  }

  block(b: Block, x: number, avail: number) {
    switch (b.kind) {
      case "para": {
        const size = b.level === 0 ? this.fs : this.fs * HEADING_SCALE[b.level - 1];
        this.flow(b.inlines, x, avail, this.align, size, b.level === 0 ? 0 : BOLD);
        break;
      }
      case "code": {
        const pad = 0.4 * this.fs;
        const top = this.y + this.drop();
        for (const line of b.text.split("\n")) {
          const atoms = Array.from(line, (c) => charAtom(c, MONO, this.fs));
          this.wrap(atoms, x + pad, avail - 2 * pad, "left", this.fs * MONO_SCALE);
        }
        const bottom = this.y + this.drop();
        const right = x + avail;
        const w = STROKE_EM * this.fs;
        this.segment(x, top, right, top, w);
        this.segment(right, top, right, bottom, w);
        this.segment(right, bottom, x, bottom, w);
        this.segment(x, bottom, x, top, w);
        this.paint.bottom = Math.max(this.paint.bottom, round6(bottom));
        break;
      }
      case "rule": {
        const h = this.gap();
        const y = this.y + h / 2;
        this.segment(x, y, x + avail, y, STROKE_EM * this.fs);
        this.y += h;
        break;
      }
      case "quote": {
        const top = this.y + this.drop();
        const indent = this.fs;
        this.blocks(b.blocks, x + indent, avail - indent, true);
        if (!b.blocks.length) this.y += this.gap();
        const bx = x + 0.35 * this.fs;
        const bottom = this.y + this.drop();
        this.segment(bx, top, bx, bottom, 0.12 * this.fs);
        break;
      }
      case "list": {
        const gutter = 1.5 * this.fs;
        this.listDepth++;
        b.items.forEach((item, i) => {
          if (i > 0 && !b.tight) this.y += this.gap();
          const label = item.task !== null ? null : b.start !== null ? `${b.start + i}.` : this.listDepth % 2 === 1 ? "•" : "–";
          let w = 0.68 * this.fs;
          if (label !== null) {
            w = 0;
            for (const c of label) w += advanceIn("regular", c);
            w = (w * this.fs) / metrics.unitsPerEm;
          }
          this.marker = { x: Math.max(x, x + gutter - 0.4 * this.fs - w), text: label, task: item.task };
          this.blocks(item.blocks, x + gutter, avail - gutter, !b.tight);
          if (this.marker) this.place([], x + gutter, avail - gutter, "left", this.fs);
        });
        this.listDepth--;
        break;
      }
      case "table":
        this.table(b.aligns, b.rows, x, avail);
        break;
    }
  }

  table(aligns: TextAlign[], rows: Inline[][][], x: number, avail: number) {
    const columns = rows.reduce((n, r) => Math.max(n, r.length), 0);
    if (columns === 0) return;
    const pad = 0.4 * this.fs;
    const widths = new Array<number>(columns).fill(2 * pad);
    rows.forEach((row, r) => {
      row.forEach((cell, c) => {
        widths[c] = Math.max(widths[c], this.naturalWidth(cell, r === 0 ? BOLD : 0) + 2 * pad);
      });
    });
    const total = widths.reduce((a, b) => a + b, 0);
    if (total > avail) for (let c = 0; c < columns; c++) widths[c] *= avail / total;
    const tableW = widths.reduce((a, b) => a + b, 0);
    const weight = STROKE_EM * this.fs;
    const drop = this.drop();
    const top = this.y + drop;
    this.segment(x, top, x + tableW, top, weight);
    rows.forEach((row, r) => {
      const start = this.y;
      let end = start;
      let cx = x;
      for (let c = 0; c < columns; c++) {
        this.y = start;
        this.flow(row[c] ?? [], cx + pad, widths[c] - 2 * pad, aligns[c] ?? "left", this.fs, r === 0 ? BOLD : 0);
        end = Math.max(end, this.y);
        cx += widths[c];
      }
      this.y = end;
      this.segment(x, end + drop, x + tableW, end + drop, weight);
    });
    const bottom = this.y + drop;
    let cx = x;
    this.segment(cx, top, cx, bottom, weight);
    for (const w of widths) {
      cx += w;
      this.segment(cx, top, cx, bottom, weight);
    }
    this.paint.bottom = Math.max(this.paint.bottom, round6(bottom));
  }

  naturalWidth(cell: Inline[], extra: number): number {
    let best = 0, line = 0;
    for (const item of cell) {
      const atoms: Atom[] = [];
      if (item.kind === "text") for (const c of item.text) atoms.push(charAtom(c, item.flags | extra, this.fs));
      else if (item.kind === "math") this.mathAtom(item.index, this.fs, Infinity, atoms);
      else {
        best = Math.max(best, line);
        line = 0;
      }
      line += sum(atoms, 0, atoms.length);
    }
    return Math.max(best, line);
  }
}

/** Lays Markdown out. `rowPitch` locks lines to rows; `lookup` sizes formulas. */
export function layoutMarkdown(text: string, fontSize: number, boxWidth: number, rowPitch: number | null, align: TextAlign, lookup: MathLookup): Paint {
  const { source, spans } = extractMath(text);
  const blocks = new Builder(spans, source).build(md.parse(source, {}));
  const l = new Layout(fontSize, rowPitch, align, spans, lookup);
  l.blocks(blocks, 0, boxWidth, true);
  if (!blocks.length) l.place([], 0, boxWidth, align, fontSize);
  l.paint.height = round6(l.y);
  l.paint.bottom = Math.max(l.paint.bottom, l.paint.height);
  return l.paint;
}

/** Number of rows a line of `fontSize` text takes at `pitch`. */
export const rowsFor = (fontSize: number, pitch: number) => Math.max(Math.ceil(fontSize / pitch - 1e-9), 1);
