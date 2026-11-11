// A Markdown table as a spreadsheet the table editor can change: reading the source into cells,
// writing cells back as tidy GFM, and the edits a spreadsheet offers (sorting, filtering,
// inserting and deleting rows and columns, filling and pasting).
//
// The Markdown stays the only thing stored, so every edit here produces new table source. Cells
// hold their unescaped text; `|` is escaped again on the way out and line breaks become spaces,
// as a GFM cell can't hold either. Formulas are rewritten the way spreadsheets rewrite them: when
// rows or columns move, references follow the cells they pointed at, and references to a deleted
// cell become `#REF!`. Sorting never moves a row whose formulas read other rows (a totals row),
// so it can't end up summing itself.
import type { TextAlign } from "../generated/TextAlign";
import { checkbox, columnName, display, evaluateValues, formatNumber, isFormula, parseNumber, parseValue, roundTo, tokenize, unstyle, type CellStyle, type Value } from "./sheet";

/** A table: `cells[0]` is the header row, and every row has one cell per column. */
export interface SheetTable {
  aligns: TextAlign[];
  cells: string[][];
}

/** Splits one table row at the pipes GFM splits at, unescaping `\|`. */
export function splitRow(line: string): string[] {
  let s = line.trim();
  if (s.startsWith("|")) s = s.slice(1);
  const cells: string[] = [];
  let cur = "";
  let closed = false;
  for (let i = 0; i < s.length; i++) {
    const ch = s[i];
    closed = false;
    if (ch === "\\" && s[i + 1] === "|") {
      cur += "|";
      i++;
    } else if (ch === "\\" && i + 1 < s.length) {
      cur += ch + s[++i];
    } else if (ch === "|") {
      cells.push(cur.trim());
      cur = "";
      closed = true;
    } else cur += ch;
  }
  // a trailing pipe closes the last cell rather than opening an empty one
  if (!closed) cells.push(cur.trim());
  return cells;
}

function alignOf(spec: string): TextAlign {
  const s = spec.trim();
  const left = s.startsWith(":"), right = s.endsWith(":");
  return left && right ? "center" : right ? "right" : "left";
}

/** Reads a table's source lines (header, delimiter row, then rows) into cells. */
export function parseTable(source: string): SheetTable | null {
  const lines = source.split(/\r\n?|\n/);
  if (lines.length < 2) return null;
  const header = splitRow(lines[0]);
  const specs = splitRow(lines[1]);
  if (!header.length || specs.length !== header.length || !specs.every((d) => /^:?-+:?$/.test(d))) return null;
  const width = header.length;
  const fit = (row: string[]) => Array.from({ length: width }, (_, i) => row[i] ?? "");
  return { aligns: specs.map(alignOf), cells: [header, ...lines.slice(2).map((l) => fit(splitRow(l)))] };
}

const escapeCell = (text: string) => text.replace(/\r\n?|\n/g, " ").replaceAll("|", "\\|").trim();
/** Width a cell is padded to; past it, columns stop lining up rather than balloon. */
const PAD_LIMIT = 24;
const visibleLength = (s: string) => Array.from(s).length;

/** Writes a table as GFM, with its columns lined up so the source stays readable. */
export function printTable(t: SheetTable): string {
  const width = t.aligns.length;
  const rows = t.cells.map((r) => Array.from({ length: width }, (_, i) => escapeCell(r[i] ?? "")));
  const widths = t.aligns.map((_, c) => Math.min(PAD_LIMIT, Math.max(3, ...rows.map((r) => visibleLength(r[c])))));
  const pad = (s: string, c: number, align: TextAlign) => {
    const gap = Math.max(0, widths[c] - visibleLength(s));
    return align === "right" ? " ".repeat(gap) + s : s + " ".repeat(gap);
  };
  const line = (cells: string[]) => `| ${cells.join(" | ")} |`;
  const rule = t.aligns.map((a, c) => {
    const dashes = "-".repeat(Math.max(1, widths[c] - (a === "center" ? 2 : a === "right" ? 1 : 0)));
    return a === "center" ? `:${dashes}:` : a === "right" ? `${dashes}:` : dashes;
  });
  return [line(rows[0].map((s, c) => pad(s, c, "left"))), line(rule), ...rows.slice(1).map((r) => line(r.map((s, c) => pad(s, c, t.aligns[c]))))].join("\n");
}

/** A new table with `columns` columns and `rows` empty data rows. */
export function blankTable(columns: number, rows: number): SheetTable {
  return {
    aligns: Array.from({ length: columns }, () => "left" as TextAlign),
    cells: [Array.from({ length: columns }, (_, c) => `Column ${c + 1}`), ...Array.from({ length: rows }, () => Array.from({ length: columns }, () => ""))],
  };
}

const clone = (t: SheetTable): SheetTable => ({ aligns: [...t.aligns], cells: t.cells.map((r) => [...r]) });

/** The A1-style name of a cell: row `r` and column `c` from 0. */
export const cellName = (r: number, c: number) => `${columnName(c)}${r + 1}`;

/** A reference outside every table: a `#REF!`, or row 0. */
const dead = (r: number) => r >= Number.MAX_SAFE_INTEGER;

type RefMap = (r: number, c: number, end: "single" | "start" | "end") => [number, number] | null;

/**
 * Rewrites the references in a formula. `map` gets each reference (ranges as their two corners)
 * and returns where it should point, or null for `#REF!`. A formula still being written moves
 * too, as long as its tokens read; one with characters formulas can't hold is left alone, and so
 * is every cell that isn't a formula.
 */
export function mapRefs(raw: string, map: RefMap): string {
  if (!isFormula(raw)) return raw;
  const at = raw.indexOf("=") + 1;
  const body = raw.slice(at);
  let toks;
  try {
    toks = tokenize(body);
  } catch {
    return raw;
  }
  let out = "";
  let last = 0;
  for (let i = 0; i < toks.length; i++) {
    const t = toks[i];
    if (t.t !== "ref") continue;
    const colon = toks[i + 1], end = toks[i + 2];
    const isRange = colon?.t === "op" && colon.op === ":" && end?.t === "ref";
    // a `#REF!` (or row 0) points nowhere, and stays as written
    if (dead(t.r) || (isRange && dead(end.r))) continue;
    if (isRange) {
      const a = map(t.r, t.c, "start"), b = map(end.r, end.c, "end");
      const bad = !a || !b || b[0] < a[0] || b[1] < a[1];
      out += body.slice(last, t.from) + (bad ? "#REF!" : `${cellName(...a)}:${cellName(...b)}`);
      last = end.to;
      i += 2;
    } else {
      const m = map(t.r, t.c, "single");
      out += body.slice(last, t.from) + (m ? cellName(...m) : "#REF!");
      last = t.to;
    }
  }
  return raw.slice(0, at) + out + body.slice(last);
}

function mapAll(t: SheetTable, map: RefMap): SheetTable {
  return { aligns: [...t.aligns], cells: t.cells.map((row) => row.map((raw) => mapRefs(raw, map))) };
}

/** Where references to `index` go when `count` lines are inserted before line `at`. */
const inserted = (index: number, at: number, count: number) => (index >= at ? index + count : index);

/** Where references to `index` go when line `at` is deleted: a single reference to it breaks, and ranges shrink. */
function deleted(index: number, at: number, end: "single" | "start" | "end"): number | null {
  if (index < at) return index;
  if (index > at) return index - 1;
  return end === "single" ? null : end === "start" ? at : at - 1;
}

/** Inserts empty rows before row `at` (1 is the first data row), moving references with the cells. */
export function insertRows(t: SheetTable, at: number, count = 1): SheetTable {
  const next = mapAll(t, (r, c) => [inserted(r, at, count), c]);
  next.cells.splice(at, 0, ...Array.from({ length: count }, () => t.aligns.map(() => "")));
  return next;
}

/** Deletes data row `at`; references to it become `#REF!`. The header row stays. */
export function deleteRow(t: SheetTable, at: number): SheetTable {
  if (at < 1 || at >= t.cells.length) return t;
  const next = mapAll(t, (r, c, end) => {
    const m = deleted(r, at, end);
    return m === null ? null : [m, c];
  });
  next.cells.splice(at, 1);
  return next;
}

/** Inserts an empty column before column `at`, moving references with the cells. */
export function insertColumn(t: SheetTable, at: number, header = ""): SheetTable {
  const next = mapAll(t, (r, c) => [r, inserted(c, at, 1)]);
  next.aligns.splice(at, 0, "left");
  next.cells.forEach((row, r) => row.splice(at, 0, r === 0 ? header : ""));
  return next;
}

/** Deletes column `at` (a table keeps at least one); references to it become `#REF!`. */
export function deleteColumn(t: SheetTable, at: number): SheetTable {
  if (t.aligns.length <= 1 || at < 0 || at >= t.aligns.length) return t;
  const next = mapAll(t, (r, c, end) => {
    const m = deleted(c, at, end);
    return m === null ? null : [r, m];
  });
  next.aligns.splice(at, 1);
  next.cells.forEach((row) => row.splice(at, 1));
  return next;
}

/** Sets one cell. */
export function setCell(t: SheetTable, r: number, c: number, text: string): SheetTable {
  const next = clone(t);
  next.cells[r][c] = text;
  return next;
}

/** Sets a column's alignment. */
export function setAlign(t: SheetTable, c: number, align: TextAlign): SheetTable {
  const next = clone(t);
  next.aligns[c] = align;
  return next;
}

/** A formula moved by (`dr`, `dc`): every reference moves with it, and any that falls off the sheet is `#REF!`. */
export function offsetFormula(raw: string, dr: number, dc: number): string {
  return mapRefs(raw, (r, c) => {
    const m: [number, number] = [r + dr, c + dc];
    return m[0] < 0 || m[1] < 0 ? null : m;
  });
}

/**
 * Writes `block` (rows of cell text) with its top left at (`r`, `c`), growing the table as
 * needed. With `from`, the block was copied from that cell and its formulas move with it.
 */
export function pasteBlock(t: SheetTable, r: number, c: number, block: string[][], from?: [number, number]): SheetTable {
  let next = clone(t);
  const width = Math.max(0, ...block.map((row) => row.length));
  while (next.aligns.length < c + width) next = insertColumn(next, next.aligns.length, `Column ${next.aligns.length + 1}`);
  while (next.cells.length < r + block.length) next.cells.push(next.aligns.map(() => ""));
  block.forEach((row, i) =>
    row.forEach((text, j) => {
      next.cells[r + i][c + j] = from ? offsetFormula(text, r - from[0], c - from[1]) : text;
    }),
  );
  return next;
}

/** Fills rows `r0 + 1` to `r1` of columns `c0` to `c1` with the cells of row `r0`, as spreadsheets fill down. */
export function fillDown(t: SheetTable, r0: number, c0: number, r1: number, c1: number): SheetTable {
  const next = clone(t);
  for (let r = r0 + 1; r <= r1; r++) for (let c = c0; c <= c1; c++) next.cells[r][c] = offsetFormula(t.cells[r0][c], r - r0, 0);
  return next;
}

/** A cell's value for sorting, filtering and showing: formulas evaluated, checkboxes as booleans. */
export interface CellView {
  value: Value;
  /** What the cell shows. Formulas show their result; other cells their source. */
  text: string;
  /** Days since 1970 for a date (`2026-11-10`, `11/10/2026`), which sorts and filters as one. */
  date: number | null;
}

/** Reads `2026-11-10` or `11/10/2026` (month first) as days since 1970. */
export function parseDate(s: string): number | null {
  let m = /^(\d{4})-(\d{1,2})-(\d{1,2})$/.exec(s.trim());
  let y: number, mo: number, d: number;
  if (m) [y, mo, d] = [Number(m[1]), Number(m[2]), Number(m[3])];
  else if ((m = /^(\d{1,2})\/(\d{1,2})\/(\d{4})$/.exec(s.trim()))) [y, mo, d] = [Number(m[3]), Number(m[1]), Number(m[2])];
  else return null;
  const ms = Date.UTC(y, mo - 1, d);
  const back = new Date(ms);
  // rejects 2026-02-30 and the like, which Date.UTC rolls over
  if (back.getUTCFullYear() !== y || back.getUTCMonth() !== mo - 1 || back.getUTCDate() !== d) return null;
  return ms / 86_400_000;
}

/** Every cell's value and text. */
export function viewCells(t: SheetTable): CellView[][] {
  const results = evaluateValues(t.cells);
  return t.cells.map((row, r) =>
    row.map((raw, c) => {
      const result = results[r][c];
      const value = result ?? parseValue(raw);
      const text = result ? display(result) : raw;
      return { value, text, date: value.kind === "text" ? parseDate(value.text) : null };
    }),
  );
}

function sortRank(v: CellView): number {
  if (v.value.kind === "number") return 0;
  if (v.date !== null) return 1;
  if (v.value.kind === "bool") return 2;
  if (v.value.kind === "text") return 3;
  return 4; // errors, then blanks
}

const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: "base" });

/** Orders two cells: numbers, dates, checkboxes, text, errors. Blanks go last whatever the direction. */
export function compareCells(a: CellView, b: CellView, descending = false): number {
  const blankA = a.value.kind === "blank", blankB = b.value.kind === "blank";
  if (blankA || blankB) return Number(blankA) - Number(blankB);
  const ra = sortRank(a), rb = sortRank(b);
  let d = ra - rb;
  if (!d) {
    if (a.value.kind === "number" && b.value.kind === "number") d = a.value.n - b.value.n;
    else if (a.date !== null && b.date !== null) d = a.date - b.date;
    else if (a.value.kind === "bool" && b.value.kind === "bool") d = Number(a.value.b) - Number(b.value.b);
    else d = collator.compare(a.text, b.text);
  }
  return descending ? -d : d;
}

/** Rows whose formulas read another row, such as a totals row. Sorting leaves them where they are. */
export function pinnedRows(t: SheetTable): Set<number> {
  const pinned = new Set<number>();
  t.cells.forEach((row, r) => {
    if (r === 0) return;
    for (const raw of row) {
      mapRefs(raw, (rr, c) => {
        if (rr !== r) pinned.add(r);
        return [rr, c];
      });
    }
  });
  return pinned;
}

/**
 * Sorts the data rows by column `c`, keeping equal rows in order and pinned rows in place. A
 * moved row's references to its own cells move with it, so `=B3*C3` stays `=B5*C5` on row 5.
 */
export function sortBy(t: SheetTable, c: number, descending: boolean): SheetTable {
  const views = viewCells(t);
  const pinned = pinnedRows(t);
  const movable = t.cells.map((_, r) => r).filter((r) => r > 0 && !pinned.has(r));
  const order = [...movable].sort((a, b) => compareCells(views[a][c], views[b][c], descending) || a - b);
  const next = clone(t);
  movable.forEach((slot, i) => {
    const from = order[i];
    next.cells[slot] = t.cells[from].map((raw) => mapRefs(raw, (r, cc) => [r === from ? slot : r, cc]));
  });
  return next;
}

/** The direction column `c`'s data rows are already sorted in, if they are and aren't all equal. */
export function sortedDirection(t: SheetTable, views: CellView[][], c: number): "asc" | "desc" | null {
  const pinned = pinnedRows(t);
  const rows = views.slice(1).filter((_, i) => !pinned.has(i + 1)).map((r) => r[c]);
  if (rows.length < 2) return null;
  let asc = true, desc = true, varied = false;
  for (let i = 1; i < rows.length; i++) {
    const up = compareCells(rows[i - 1], rows[i]);
    if (up > 0) asc = false;
    if (compareCells(rows[i - 1], rows[i], true) > 0) desc = false;
    if (up !== 0) varied = true;
  }
  return !varied ? null : asc ? "asc" : desc ? "desc" : null;
}

const YES = new Set(["x", "yes", "y", "true", "checked", "done", "[x]", "✓"]);
const NO = new Set(["no", "n", "false", "unchecked", "not done", "[ ]", "open"]);

/**
 * Whether a cell passes a column filter. A filter is text to look for (any case), or a
 * comparison: `>10`, `<=$5`, `>=2026-01-01`, `=pear`, `!=pear` (or `<>pear`). Numbers and dates
 * compare as numbers and dates; a checkbox column takes `yes`/`no` (or `x`, `done`, ...).
 */
export function matchesFilter(cell: CellView, filter: string): boolean {
  const f = filter.trim();
  if (!f) return true;
  const lower = f.toLowerCase();
  if (cell.value.kind === "bool" && (YES.has(lower) || NO.has(lower))) return cell.value.b === YES.has(lower);
  const m = /^(>=|<=|<>|!=|>|<|=)\s*(.*)$/.exec(f);
  if (!m) return cell.text.toLowerCase().includes(lower);
  const [, op, target] = m;
  const num = parseNumber(target.trim());
  const date = parseDate(target);
  let a: number | null = null, b: number | null = null;
  if (num && cell.value.kind === "number") [a, b] = [cell.value.n, num.n];
  else if (date !== null && cell.date !== null) [a, b] = [cell.date, date];
  if (a === null || b === null) {
    const same = cell.text.trim().toLowerCase() === target.trim().toLowerCase();
    return op === "=" ? same : op === "!=" || op === "<>" ? !same : false;
  }
  switch (op) {
    case ">":
      return a > b;
    case ">=":
      return a >= b;
    case "<":
      return a < b;
    case "<=":
      return a <= b;
    case "=":
      return a === b;
    default:
      return a !== b;
  }
}

/** The data rows (1 and up) that pass every column's filter. */
export function visibleRows(views: CellView[][], filters: string[]): number[] {
  const rows: number[] = [];
  for (let r = 1; r < views.length; r++) if (filters.every((f, c) => !f || matchesFilter(views[r][c], f))) rows.push(r);
  return rows;
}

/** Whether a cell is a checkbox, and ticked. */
export const cellCheckbox = checkbox;

/** Text pasted from another spreadsheet: tab-separated rows, as every spreadsheet copies. */
export function parseClipboard(text: string): string[][] {
  const rows = text.replace(/\r\n?/g, "\n").replace(/\n$/, "").split("\n");
  return rows.map((r) => r.split("\t"));
}

/** A formatting command from the grid's toolbar. */
export type CellFormat =
  | { kind: "currency"; symbol: string }
  | { kind: "percent" }
  | { kind: "plain" }
  | { kind: "grouping"; on: boolean }
  | { kind: "decimals"; delta: number }
  | { kind: "style"; style: keyof CellStyle; on: boolean };

/** `text` with emphasis wrapped around it, bold outermost, as [`unstyle`] reads it back. */
function styled(text: string, s: CellStyle): string {
  let t = text;
  if (s.strike) t = `~~${t}~~`;
  if (s.italic) t = `*${t}*`;
  if (s.bold) t = `**${t}**`;
  return t;
}

/**
 * One cell with a format applied. Number formats rewrite numbers only, keeping their value:
 * `5` as currency is `$5.00`, `0.12` as a percentage is `12%`, and fewer decimals round. They
 * leave text, checkboxes and formulas alone, as a formula's result takes the format of what it
 * reads. Bold, italic and strikethrough wrap any cell but a checkbox.
 */
export function formatCell(raw: string, f: CellFormat): string {
  const { text, style } = unstyle(raw);
  if (f.kind === "style") return !text || checkbox(raw) !== null ? raw : styled(text, { ...style, [f.style]: f.on });
  const num = isFormula(raw) ? null : parseNumber(text);
  if (!num) return raw;
  const fmt = { ...num.fmt };
  let n = num.n;
  switch (f.kind) {
    case "currency":
      if (fmt.currency === null) [fmt.suffix, fmt.spaced] = [false, false];
      fmt.currency = f.symbol;
      fmt.percent = false;
      break;
    case "percent":
      fmt.percent = true;
      fmt.currency = null;
      break;
    case "plain":
      fmt.currency = null;
      fmt.percent = false;
      break;
    case "grouping":
      fmt.grouped = f.on;
      break;
    case "decimals": {
      const d = Math.min(Math.max(fmt.decimals + f.delta, 0), 6);
      n = fmt.percent ? roundTo(n * 100, d) / 100 : roundTo(n, d);
      fmt.decimals = d;
      break;
    }
  }
  return styled(formatNumber(n, fmt), style);
}

/** A format applied to every cell of the range from (`r0`, `c0`) to (`r1`, `c1`). */
export function formatRange(t: SheetTable, r0: number, c0: number, r1: number, c1: number, f: CellFormat): SheetTable {
  const next = clone(t);
  for (let r = r0; r <= r1; r++) for (let c = c0; c <= c1; c++) next.cells[r][c] = formatCell(t.cells[r][c], f);
  return next;
}

/** What the toolbar shows as on for a range: a style when every styled cell has it, a number format when every number does. */
export function rangeFormat(t: SheetTable, r0: number, c0: number, r1: number, c1: number) {
  const style: CellStyle = { bold: true, italic: true, strike: true };
  let texts = 0, numbers = 0, currency: string | null | undefined, percent = true, grouped = true;
  for (let r = r0; r <= r1; r++) {
    for (let c = c0; c <= c1; c++) {
      const raw = t.cells[r][c];
      const u = unstyle(raw);
      if (!u.text || checkbox(raw) !== null) continue;
      texts++;
      for (const k of ["bold", "italic", "strike"] as const) style[k] &&= u.style[k];
      const num = isFormula(raw) ? null : parseNumber(u.text);
      if (!num) continue;
      numbers++;
      currency = currency === undefined || currency === num.fmt.currency ? num.fmt.currency : null;
      percent &&= num.fmt.percent;
      grouped &&= num.fmt.grouped;
    }
  }
  if (!texts) style.bold = style.italic = style.strike = false;
  return { style, numbers, currency: numbers ? (currency ?? null) : null, percent: numbers > 0 && percent, grouped: numbers > 0 && grouped };
}
