// TypeScript port of crates/penroam-core/src/text.rs. Both read the same
// metrics file, so on-screen wrapping equals PDF export wrapping on every OS.
// Verified against tests/fixtures/text-layout-golden.json.
import metricsJson from "../../../../crates/penroam-core/data/text-metrics.json";

interface Metrics {
  unitsPerEm: number;
  ascent: number;
  descent: number;
  lineHeight: number;
  firstBaseline: number;
  fallbackWidth: number;
  widths: Record<string, number>;
}

const M = metricsJson as Metrics;
const byCode = new Map<number, number>(Object.entries(M.widths).map(([k, v]) => [Number(k), v]));

export const TEXT_FONT_FAMILY = "PenroamSans";

export function advance(ch: string): number {
  if (ch === "\t") return advance(" ") * 4;
  return byCode.get(ch.codePointAt(0)!) ?? M.fallbackWidth;
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

export function contentHeight(text: string, fontSize: number, boxWidth: number): number {
  return Math.max(1, layout(text, fontSize, boxWidth).length) * lineHeight(fontSize);
}
