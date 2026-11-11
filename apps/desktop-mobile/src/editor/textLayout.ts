// TypeScript port of crates/penroam-core/src/text.rs: font metrics and plain
// wrapping. Both read the same metrics file, so on-screen wrapping equals PDF
// export wrapping on every OS. Rendered text goes through ./markdown.ts; plain
// wrapping sizes the source view while a block is edited. Verified against
// tests/fixtures/text-layout-golden.json.
//
// Excalifont, the handwritten font, has one face. Its bold and italic are drawn
// by stroking and shearing that face (see `drawRun`), exactly as the PDF export
// does, so they keep its widths. `styledFace` sets a character Excalifont lacks
// in the formatted font instead, so nothing is measured in one font and drawn in
// another.
import metricsJson from "../../../../crates/penroam-core/data/text-metrics.json";
import type { TextFont } from "../generated/TextFont";

type FormattedFace = "regular" | "bold" | "italic" | "boldItalic" | "mono";

interface Metrics {
  unitsPerEm: number;
  ascent: number;
  descent: number;
  lineHeight: number;
  firstBaseline: number;
  fallbackWidth: number;
  widths: Record<string, number>;
  faces: Record<Exclude<FormattedFace, "regular">, Record<string, number>>;
  hand: { ascent: number; descent: number; widths: Record<string, number> };
}

/** A bundled typeface: the formatted faces have PDF standard fonts with the same widths, the hand faces are Excalifont. */
export type Face = FormattedFace | "hand" | "handBold" | "handItalic" | "handBoldItalic";

const M = metricsJson as Metrics;
const codes = (widths: Record<string, number>) => new Map<number, number>(Object.entries(widths).map(([k, v]) => [Number(k), v]));
const byCode = codes(M.widths);
const byHand = codes(M.hand.widths);
const byFace: Record<Face, Map<number, number>> = {
  regular: byCode,
  bold: codes(M.faces.bold),
  italic: codes(M.faces.italic),
  boldItalic: codes(M.faces.boldItalic),
  mono: codes(M.faces.mono),
  hand: byHand,
  handBold: byHand,
  handItalic: byHand,
  handBoldItalic: byHand,
};

export const metrics = { unitsPerEm: M.unitsPerEm, ascent: M.ascent, descent: M.descent };
export const TEXT_FONT_FAMILY = "PenroamSans";
export const MONO_FONT_FAMILY = "PenroamMono";
export const HAND_FONT_FAMILY = "PenroamHand";
/** Size of code relative to the text around it. */
export const MONO_SCALE = 0.9;
/** Shear of the slanted hand faces (`HAND_SLANT` in penroam-core). */
export const HAND_SLANT = 0.2;
/** Outline stroke that thickens the bold hand faces, in em (`HAND_BOLD_STROKE` in penroam-core). */
export const HAND_BOLD_STROKE = 0.045;

export const isHand = (face: Face) => face === "hand" || face === "handBold" || face === "handItalic" || face === "handBoldItalic";

/** The CSS font family a block's text is set in, for the DOM editor and previews. */
export const fontFamilyOf = (font: TextFont | undefined) => (font === "handwritten" ? `${HAND_FONT_FAMILY}, ${TEXT_FONT_FAMILY}` : TEXT_FONT_FAMILY);

/** Ascender height of a font's text faces, in font units. */
export const ascentOf = (font: TextFont) => (font === "handwritten" ? M.hand.ascent : M.ascent);
/** Descender depth of a font's text faces (negative), in font units. */
export const descentOf = (font: TextFont) => (font === "handwritten" ? M.hand.descent : M.descent);

/** The face for character `c` (`Face::styled` in penroam-core). */
export function styledFace(font: TextFont, bold: boolean, italic: boolean, mono: boolean, c: string): Face {
  if (mono) return "mono";
  const cp = c.codePointAt(0)!;
  const hand = font === "handwritten" && (byHand.has(cp) || (c === "\t" && byHand.has(32)));
  if (hand) return bold && italic ? "handBoldItalic" : bold ? "handBold" : italic ? "handItalic" : "hand";
  return bold && italic ? "boldItalic" : bold ? "bold" : italic ? "italic" : "regular";
}

/**
 * Canvas font string for a face. The hand faces always name the one Excalifont face: their
 * weight and slant come from `drawRun`, never from the browser's synthesis, whose widths
 * differ between engines.
 */
export function canvasFont(face: Face, size: number): string {
  if (face === "mono") return `${size}px ${MONO_FONT_FAMILY}`;
  if (isHand(face)) return `${size}px ${HAND_FONT_FAMILY}`;
  const style = face === "italic" || face === "boldItalic" ? "italic " : "";
  const weight = face === "bold" || face === "boldItalic" ? "bold " : "";
  return `${style}${weight}${size}px ${TEXT_FONT_FAMILY}`;
}

/** Draws one laid-out run with its baseline at (`x`, `y`), the same way the PDF export does. */
export function drawRun(ctx: CanvasRenderingContext2D, face: Face, size: number, text: string, x: number, y: number): void {
  ctx.font = canvasFont(face, size);
  if (!isHand(face)) {
    ctx.fillText(text, x, y);
    return;
  }
  // The layout knows nothing of kerning or ligatures, and neither does the PDF.
  ctx.save();
  ctx.fontKerning = "none";
  if ("textRendering" in ctx) ctx.textRendering = "optimizeSpeed";
  ctx.translate(x, y);
  if (face === "handItalic" || face === "handBoldItalic") ctx.transform(1, 0, -HAND_SLANT, 1, 0, 0);
  ctx.fillText(text, 0, 0);
  if (face === "handBold" || face === "handBoldItalic") {
    ctx.lineWidth = HAND_BOLD_STROKE * size;
    ctx.lineJoin = "round";
    ctx.strokeText(text, 0, 0);
  }
  ctx.restore();
}

export function advance(ch: string): number {
  if (ch === "\t") return advance(" ") * 4;
  return byCode.get(ch.codePointAt(0)!) ?? M.fallbackWidth;
}

export function advanceIn(face: Face, ch: string): number {
  if (ch === "\t") return advanceIn(face, " ") * 4;
  return byFace[face].get(ch.codePointAt(0)!) ?? M.fallbackWidth;
}

export const lineHeight = (fontSize: number) => (fontSize * M.lineHeight) / M.unitsPerEm;
export const firstBaseline = (fontSize: number) => (fontSize * M.firstBaseline) / M.unitsPerEm;

export interface Line {
  start: number; // code point index
  end: number;
  text: string;
  width: number;
}

export function measure(s: string, fontSize: number): number {
  let w = 0;
  for (const c of s) w += advance(c);
  return (w * fontSize) / M.unitsPerEm;
}

const round6 = (v: number) => Math.round(v * 1e6) / 1e6 + 0;

export function layout(text: string, fontSize: number, boxWidth: number): Line[] {
  const chars = Array.from(text);
  const scale = fontSize / M.unitsPerEm;
  const lines: Line[] = [];
  let paraStart = 0;
  for (;;) {
    let paraEnd = chars.indexOf("\n", paraStart);
    if (paraEnd < 0) paraEnd = chars.length;
    wrap(chars, paraStart, paraEnd, boxWidth, scale, lines);
    if (paraEnd >= chars.length) break;
    paraStart = paraEnd + 1;
  }
  return lines;
}

function wrap(chars: string[], start: number, end: number, boxW: number, scale: number, out: Line[]) {
  const push = (s: number, e: number) => {
    let te = e;
    while (te > s && chars[te - 1] === " ") te--;
    let w = 0;
    for (let i = s; i < te; i++) w += advance(chars[i]);
    out.push({ start: s, end: e, text: chars.slice(s, e).join(""), width: round6(w * scale) });
  };
  if (start === end) return push(start, end);
  let lineStart = start;
  let i = start;
  let width = 0;
  let lastBreak: number | null = null;
  while (i < end) {
    const c = chars[i];
    const w = advance(c) * scale;
    if (c === " ") {
      width += w;
      i++;
      lastBreak = i;
      continue;
    }
    if (width + w > boxW && i > lineStart) {
      if (lastBreak !== null && lastBreak > lineStart) {
        push(lineStart, lastBreak);
        lineStart = lastBreak;
      } else {
        push(lineStart, i);
        lineStart = i;
      }
      lastBreak = null;
      let acc = 0;
      for (let k = lineStart; k < i; k++) acc += advance(chars[k]);
      width = acc * scale;
      continue;
    }
    width += w;
    i++;
  }
  push(lineStart, end);
}

/** Height of `text` as plain source, one line per wrapped line of `lineH` points. */
export function contentHeight(text: string, fontSize: number, boxWidth: number, lineH = lineHeight(fontSize)): number {
  return Math.max(1, layout(text, fontSize, boxWidth).length) * lineH;
}
