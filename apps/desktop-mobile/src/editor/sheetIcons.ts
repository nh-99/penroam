// Icons for the spreadsheet grid, built as plain SVG so the editor stays free of the UI framework.
//
// The bar and menus use the app's own icon set, Lucide (the shapes below are copied from
// @lucide/vue 1.66.0, ISC licence: https://lucide.dev/license), drawn the way the app's
// toolbar draws them: a 24 × 24 grid, 2 px round strokes in the text colour. The sort arrows and
// menu button inside header cells are part of the note rather than the interface, so they are
// drawn as if by hand to sit with the hand font.

type Shape = [string, Record<string, string>];

/** Lucide icons, by what the grid uses them for. */
const LUCIDE = {
  bold: [["path", { d: "M6 12h9a4 4 0 010 8H7a1 1 0 01-1-1V5a1 1 0 011-1h7a4 4 0 010 8" }]],
  italic: [["line", { x1: "19", x2: "10", y1: "4", y2: "4" }], ["line", { x1: "14", x2: "5", y1: "20", y2: "20" }], ["line", { x1: "15", x2: "9", y1: "4", y2: "20" }]],
  strike: [["path", { d: "M16 4H9a3 3 0 0 0-2.83 4" }], ["path", { d: "M14 12a4 4 0 0 1 0 8H6" }], ["line", { x1: "4", x2: "20", y1: "12", y2: "12" }]],
  currency: [["path", { d: "M12 3v18" }], ["path", { d: "M17 5H9.5a1 1 0 000 7h5a1 1 0 010 7H6" }]],
  percent: [["line", { x1: "19", x2: "5", y1: "5", y2: "19" }], ["circle", { cx: "6.5", cy: "6.5", r: "2.5" }], ["circle", { cx: "17.5", cy: "17.5", r: "2.5" }]],
  decimalsLess: [["path", { d: "m13 21-3-3 3-3" }], ["path", { d: "M20 18H10" }], ["path", { d: "M3 11h.01" }], ["rect", { x: "6", y: "3", width: "5", height: "8", rx: "2.5" }]],
  decimalsMore: [["path", { d: "M10 18h10" }], ["path", { d: "m17 21 3-3-3-3" }], ["path", { d: "M3 11h.01" }], ["rect", { x: "15", y: "3", width: "5", height: "8", rx: "2.5" }], ["rect", { x: "6", y: "3", width: "5", height: "8", rx: "2.5" }]],
  alignLeft: [["path", { d: "M21 5H3" }], ["path", { d: "M15 12H3" }], ["path", { d: "M17 19H3" }]],
  alignCenter: [["path", { d: "M21 5H3" }], ["path", { d: "M17 12H7" }], ["path", { d: "M19 19H5" }]],
  alignRight: [["path", { d: "M21 5H3" }], ["path", { d: "M21 12H9" }], ["path", { d: "M21 19H7" }]],
  filter: [["path", { d: "M10 20a1 1 0 0 0 .553.895l2 1A1 1 0 0 0 14 21v-7a2 2 0 0 1 .517-1.341L21.74 4.67A1 1 0 0 0 21 3H3a1 1 0 0 0-.742 1.67l7.225 7.989A2 2 0 0 1 10 14z" }]],
  addRow: [["rect", { width: "13", height: "7", x: "8", y: "3", rx: "1" }], ["path", { d: "m2 9 3 3-3 3" }], ["rect", { width: "13", height: "7", x: "8", y: "14", rx: "1" }]],
  addColumn: [["rect", { width: "7", height: "13", x: "3", y: "8", rx: "1" }], ["path", { d: "m15 2-3 3-3-3" }], ["rect", { width: "7", height: "13", x: "14", y: "8", rx: "1" }]],
  markdown: [["path", { d: "m18 16 4-4-4-4" }], ["path", { d: "m6 8-4 4 4 4" }], ["path", { d: "m14.5 4-5 16" }]],
  sortAsc: [["path", { d: "m3 8 4-4 4 4" }], ["path", { d: "M7 4v16" }], ["path", { d: "M11 12h4" }], ["path", { d: "M11 16h7" }], ["path", { d: "M11 20h10" }]],
  sortDesc: [["path", { d: "m3 16 4 4 4-4" }], ["path", { d: "M7 20V4" }], ["path", { d: "M11 4h10" }], ["path", { d: "M11 8h7" }], ["path", { d: "M11 12h4" }]],
  chevron: [["path", { d: "m6 9 6 6 6-6" }]],
  trash: [["path", { d: "M10 11v6" }], ["path", { d: "M14 11v6" }], ["path", { d: "M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6" }], ["path", { d: "M3 6h18" }], ["path", { d: "M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" }]],
  number: [["line", { x1: "4", x2: "20", y1: "9", y2: "9" }], ["line", { x1: "4", x2: "20", y1: "15", y2: "15" }], ["line", { x1: "10", x2: "8", y1: "3", y2: "21" }], ["line", { x1: "16", x2: "14", y1: "3", y2: "21" }]],
} satisfies Record<string, Shape[]>;

/** Hand-drawn icons on a 20 × 20 grid: strokes that wobble a little, with round ends. */
const HAND = {
  sortAsc: ["M10 16.6c-.4-3.7.3-7.5-.1-11.8", "M5.6 8.7c1.6-1.5 2.9-3 4.3-4 1.4 1.1 2.7 2.6 4.3 4.1"],
  sortDesc: ["M10 3.4c.4 3.7-.3 7.5.1 11.8", "M5.6 11.3c1.6 1.5 2.9 3 4.3 4 1.4-1.1 2.7-2.6 4.3-4.1"],
  sortNone: ["M6.8 15.8c-.3-3.4.2-7-.1-10.8", "M4.3 7.6c.9-.9 1.7-1.8 2.5-2.7.9.8 1.7 1.8 2.6 2.7", "M13.2 4.2c.3 3.4-.2 7 .1 10.8", "M10.7 12.4c.9.9 1.7 1.8 2.5 2.7.9-.8 1.7-1.8 2.6-2.7"],
  menu: ["M5.4 8.1c1.6 1.4 3.1 2.9 4.6 4.4 1.5-1.5 2.9-3 4.5-4.5"],
} satisfies Record<string, string[]>;

/** A name in the interface icon set. */
export type UiIcon = keyof typeof LUCIDE;
/** A name in the hand-drawn set. */
export type HandIcon = keyof typeof HAND;

const SVG = "http://www.w3.org/2000/svg";

function svg(viewBox: string, cls: string): SVGSVGElement {
  const el = document.createElementNS(SVG, "svg");
  el.setAttribute("viewBox", viewBox);
  el.setAttribute("aria-hidden", "true");
  el.setAttribute("class", cls);
  return el;
}

/** An interface icon, styled like the app toolbar's (see `.cm-sheet-icon` in sheetGrid.ts). */
export function uiIcon(name: UiIcon): SVGSVGElement {
  const el = svg("0 0 24 24", "cm-sheet-icon");
  for (const [tag, attrs] of LUCIDE[name] as Shape[]) {
    const shape = document.createElementNS(SVG, tag);
    for (const [k, v] of Object.entries(attrs)) shape.setAttribute(k, v);
    el.append(shape);
  }
  return el;
}

/** A hand-drawn icon, in the note's ink. */
export function handIcon(name: HandIcon): SVGSVGElement {
  const el = svg("0 0 20 20", "cm-sheet-icon cm-sheet-hand");
  for (const d of HAND[name]) {
    const path = document.createElementNS(SVG, "path");
    path.setAttribute("d", d);
    el.append(path);
  }
  return el;
}
