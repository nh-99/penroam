// TypeScript port of crates/penroam-core/src/sheet.rs: spreadsheet values and formulas in
// Markdown tables. The finished layout (markdown.ts) draws formula results and checkboxes with
// it, and the table editor (sheetGrid.ts) shows the same results while you type. Both ports are
// checked against tests/fixtures/sheet-golden.json; keep the two files in step. The module docs
// there give the rules: dependency-order evaluation with no recursion across cells, a cap on
// nesting and on range work, and number printing that uses no platform float formatting.

/** Deepest nesting of parentheses, signs and function calls in a formula. */
export const MAX_NESTING = 32;
/** Most cells the ranges in one table's formulas may cover, all told. */
export const MAX_RANGE_CELLS = 200_000;
/** The currency symbols a number may start with. */
export const CURRENCIES = ["$", "€", "£", "¥"];

/** How a number was written, so a result can be shown the same way. */
export interface NumberFormat {
  currency: string | null;
  /** The currency symbol comes after the digits (`5 €`). */
  suffix: boolean;
  /** A space separates the currency symbol from the digits. */
  spaced: boolean;
  percent: boolean;
  grouped: boolean;
  decimals: number;
}

/** The meaning of a cell. */
export type Value =
  | { kind: "blank" }
  | { kind: "number"; n: number; fmt: NumberFormat }
  | { kind: "bool"; b: boolean }
  | { kind: "text"; text: string }
  | { kind: "error"; code: string };

const ERROR = "#ERROR!", NAME = "#NAME?", REF = "#REF!", VALUE = "#VALUE!", DIV0 = "#DIV/0!", NUM = "#NUM!", CYCLE = "#CYCLE!", LIMIT = "#LIMIT!";

const PLAIN: NumberFormat = { currency: null, suffix: false, spaced: false, percent: false, grouped: false, decimals: 0 };
export const plainFormat = (): NumberFormat => ({ ...PLAIN });
const isPlain = (f: NumberFormat) => f.currency === null && !f.percent;
const isDigit = (c: string | undefined) => c !== undefined && c >= "0" && c <= "9";
const isAlpha = (c: string | undefined) => c !== undefined && ((c >= "a" && c <= "z") || (c >= "A" && c <= "Z"));

/** A cell's text without the whitespace around it, as markdown-it trims table cells. */
export const trim = (raw: string) => raw.trim();

/** Emphasis wrapped around a whole cell, which a cell's value looks through. */
export interface CellStyle {
  bold: boolean;
  italic: boolean;
  strike: boolean;
}

/** A cell's text without the emphasis wrapped around all of it, and that emphasis (`unstyle` in penroam-core). */
export function unstyle(raw: string): { text: string; style: CellStyle } {
  let s = trim(raw);
  const style: CellStyle = { bold: false, italic: false, strike: false };
  for (let n = 0; n < 3; n++) {
    const m = ["**", "__", "~~", "*", "_"].find((m) => {
      if (s.length <= 2 * m.length || !s.startsWith(m) || !s.endsWith(m)) return false;
      const inner = s.slice(m.length, -m.length);
      return !inner.includes(m) && inner === inner.trim();
    });
    if (!m) break;
    if (m === "**" || m === "__") style.bold = true;
    else if (m === "~~") style.strike = true;
    else style.italic = true;
    s = s.slice(m.length, -m.length);
  }
  return { text: s, style };
}

/** `true`/`false` when the cell is a checkbox: exactly `[ ]`, `[x]` or `[X]`. */
export function checkbox(raw: string): boolean | null {
  const s = trim(raw);
  return s === "[ ]" ? false : s === "[x]" || s === "[X]" ? true : null;
}

/** Whether a cell's source is a formula: its text, inside any emphasis, starts with `=`. */
export const isFormula = (raw: string) => unstyle(raw).text.startsWith("=");

/** Reads a number as people write one in a spreadsheet (`1,234.5`, `-$3`, `5 €`, `($3.00)`, `12%`), or null. */
export function parseNumber(s: string): { n: number; fmt: NumberFormat } | null {
  const all = Array.from(s);
  const bracketed = all.length > 2 && all[0] === "(" && all[all.length - 1] === ")";
  const c = bracketed ? all.slice(1, -1) : all;
  const skipSpaces = (at: number) => {
    while (c[at] === " ") at++;
    return at;
  };
  let i = 0, neg = false, signed = false;
  const fmt = plainFormat();
  if (c[0] === "+" || c[0] === "-") {
    neg = c[0] === "-";
    signed = true;
    i++;
  }
  if (c[i] !== undefined && CURRENCIES.includes(c[i])) {
    fmt.currency = c[i];
    const after = skipSpaces(i + 1);
    fmt.spaced = after > i + 1;
    i = after;
    if (!signed && (c[i] === "+" || c[i] === "-")) {
      neg = c[i++] === "-";
      signed = true;
    }
  }
  const intStart = i;
  while (i < c.length && (isDigit(c[i]) || c[i] === ",")) i++;
  const int = c.slice(intStart, i).join("");
  if (int.includes(",")) {
    const groups = int.split(",");
    if (groups[0].length < 1 || groups[0].length > 3 || groups.slice(1).some((g) => g.length !== 3)) return null;
    fmt.grouped = true;
  }
  let frac = "";
  if (c[i] === ".") {
    i++;
    while (i < c.length && isDigit(c[i])) frac += c[i++];
    if (!frac) return null;
  }
  if (!int && !frac) return null;
  if (c[i] === "%") {
    fmt.percent = true;
    i++;
  }
  if (fmt.currency === null) {
    const at = skipSpaces(i);
    if (c[at] !== undefined && CURRENCIES.includes(c[at])) {
      fmt.currency = c[at];
      fmt.suffix = true;
      fmt.spaced = at > i;
      i = at + 1;
    }
  }
  // a sign inside brackets would make the brackets mean nothing
  if (i !== c.length || (fmt.percent && fmt.currency !== null) || (bracketed && signed)) return null;
  neg ||= bracketed;
  fmt.decimals = frac.length;
  let n = Number(`${int ? int.replaceAll(",", "") : "0"}.${frac || "0"}`);
  if (fmt.percent) n /= 100;
  return { n: neg ? -n : n, fmt };
}

/** The value of a cell that isn't a formula. */
export function parseValue(raw: string): Value {
  const s = unstyle(raw).text;
  if (!s) return { kind: "blank" };
  const box = checkbox(s);
  if (box !== null) return { kind: "bool", b: box };
  const lower = s.length === 4 || s.length === 5 ? s.replace(/[A-Z]/g, (l) => l.toLowerCase()) : "";
  if (lower === "true") return { kind: "bool", b: true };
  if (lower === "false") return { kind: "bool", b: false };
  const num = parseNumber(s);
  return num ? { kind: "number", ...num } : { kind: "text", text: s };
}

/** Above this, a double no longer holds every integer, so digits can't be exact. */
const EXACT = 9007199254740992;

/** Prints a number the way `fmt` says it was written (`format_number` in penroam-core). */
export function formatNumber(n: number, fmt: NumberFormat): string {
  const v = fmt.percent ? n * 100 : n;
  if (!Number.isFinite(v) || Math.abs(v) >= EXACT) return NUM;
  let places = fmt.currency !== null ? 2 : 6;
  let scale = fmt.currency !== null ? 100 : 1e6;
  if (Math.abs(v) * scale >= EXACT) {
    places = 0;
    scale = 1;
  }
  const scaled = Math.floor(Math.abs(v) * scale + 0.5);
  const digits = String(scaled).padStart(places + 1, "0");
  const int = digits.slice(0, digits.length - places);
  let frac = digits.slice(digits.length - places);
  if (fmt.currency === null) {
    const keep = Math.min(fmt.decimals, places);
    while (frac.length > keep && frac.endsWith("0")) frac = frac.slice(0, -1);
  }
  let out = v < 0 && scaled > 0 ? "-" : "";
  if (fmt.currency !== null && !fmt.suffix) out += fmt.currency + (fmt.spaced ? " " : "");
  if (fmt.grouped) {
    for (let i = 0; i < int.length; i++) {
      if (i > 0 && (int.length - i) % 3 === 0) out += ",";
      out += int[i];
    }
  } else out += int;
  if (frac) out += `.${frac}`;
  if (fmt.percent) out += "%";
  if (fmt.currency !== null && fmt.suffix) out += (fmt.spaced ? " " : "") + fmt.currency;
  return out;
}

/** What a value shows as in a cell. */
export function display(v: Value): string {
  switch (v.kind) {
    case "blank":
      return "0";
    case "number":
      return formatNumber(v.n, v.fmt);
    case "bool":
      return v.b ? "TRUE" : "FALSE";
    case "text":
      return v.text;
    case "error":
      return v.code;
  }
}

type Func = "SUM" | "AVERAGE" | "MIN" | "MAX" | "COUNT" | "PRODUCT" | "ROUND" | "ABS";
/** The functions formulas can call. */
export const FUNCTIONS: readonly Func[] = ["SUM", "AVERAGE", "MIN", "MAX", "COUNT", "PRODUCT", "ROUND", "ABS"];

function funcNamed(name: string): Func | null {
  const up = name.toUpperCase();
  if (up === "AVG") return "AVERAGE";
  return (FUNCTIONS as readonly string[]).includes(up) ? (up as Func) : null;
}

type Expr =
  | { k: "num"; n: number }
  | { k: "ref"; r: number; c: number }
  | { k: "range"; r0: number; c0: number; r1: number; c1: number }
  | { k: "neg"; a: Expr }
  | { k: "bin"; op: string; a: Expr; b: Expr }
  | { k: "call"; f: Func; args: Expr[] };

/** A formula token. `from`/`to` are code-unit offsets in the formula body, for rewriting refs. */
export type Tok = ({ t: "num"; n: number } | { t: "ref"; r: number; c: number } | { t: "func"; f: Func } | { t: "op"; op: string }) & { from: number; to: number };

class FormulaError {
  constructor(readonly code: string) {}
}

/** Column letters to a column index from 0: `A` is 0, `AA` is 26. */
export function columnIndex(letters: string): number {
  let n = 0;
  for (const l of letters.toUpperCase()) n = n * 26 + (l.charCodeAt(0) - 64);
  return n - 1;
}

/** A column index from 0 to its letters. */
export function columnName(c: number): string {
  let s = "";
  for (let n = c + 1; n > 0; n = Math.floor((n - 1) / 26)) s = String.fromCharCode(65 + ((n - 1) % 26)) + s;
  return s;
}

/** Splits a formula body (after its `=`) into tokens; throws a FormulaError with the error code. */
export function tokenize(src: string): Tok[] {
  const out: Tok[] = [];
  let i = 0;
  while (i < src.length) {
    const ch = src[i];
    if (ch === " " || ch === "\t") i++;
    else if (isDigit(ch) || (ch === "." && isDigit(src[i + 1]))) {
      const start = i;
      while (isDigit(src[i])) i++;
      if (src[i] === "." && isDigit(src[i + 1])) {
        i++;
        while (isDigit(src[i])) i++;
      }
      const text = src.slice(start, i);
      out.push({ t: "num", n: Number(text.startsWith(".") ? `0${text}` : text), from: start, to: i });
    } else if (isAlpha(ch)) {
      const start = i;
      while (isAlpha(src[i])) i++;
      const letters = src.slice(start, i);
      const digitStart = i;
      while (isDigit(src[i])) i++;
      const digits = src.slice(digitStart, i);
      const next = src[i];
      if (!digits) {
        if (next !== "(") throw new FormulaError(NAME);
        const f = funcNamed(letters);
        if (!f) throw new FormulaError(NAME);
        out.push({ t: "func", f, from: start, to: i });
      } else {
        if (letters.length > 3 || digits.length > 7 || (next !== undefined && (isAlpha(next) || isDigit(next) || next === "_" || next === "("))) throw new FormulaError(NAME);
        const row = Number(digits);
        // row 0 is no row; MAX_SAFE_INTEGER is outside every table
        out.push({ t: "ref", r: row === 0 ? Number.MAX_SAFE_INTEGER : row - 1, c: columnIndex(letters), from: start, to: i });
      }
    } else if (src.startsWith(REF, i)) {
      // what deleting a referenced row or column leaves behind
      out.push({ t: "ref", r: Number.MAX_SAFE_INTEGER, c: 0, from: i, to: i + REF.length });
      i += REF.length;
    } else if ("+-*/^(),:".includes(ch)) {
      out.push({ t: "op", op: ch, from: i, to: i + 1 });
      i++;
    } else throw new FormulaError(ERROR);
  }
  return out;
}

class Parser {
  at = 0;
  depth = 0;
  constructor(private toks: Tok[]) {}

  done() {
    return this.at === this.toks.length;
  }
  private eat(op: string) {
    const t = this.toks[this.at];
    if (t?.t === "op" && t.op === op) {
      this.at++;
      return true;
    }
    return false;
  }
  private nest() {
    if (++this.depth > MAX_NESTING) throw new FormulaError(ERROR);
  }

  expr(): Expr {
    let left = this.term();
    for (;;) {
      const op = this.eat("+") ? "+" : this.eat("-") ? "-" : null;
      if (!op) return left;
      left = { k: "bin", op, a: left, b: this.term() };
    }
  }
  private term(): Expr {
    let left = this.power();
    for (;;) {
      const op = this.eat("*") ? "*" : this.eat("/") ? "/" : null;
      if (!op) return left;
      left = { k: "bin", op, a: left, b: this.power() };
    }
  }
  private power(): Expr {
    let left = this.unary();
    while (this.eat("^")) left = { k: "bin", op: "^", a: left, b: this.unary() };
    return left;
  }
  private unary(): Expr {
    const neg = this.eat("-") ? true : this.eat("+") ? false : null;
    if (neg === null) return this.primary();
    this.nest();
    const inner = this.unary();
    this.depth--;
    return neg ? { k: "neg", a: inner } : inner;
  }
  private primary(): Expr {
    const tok = this.toks[this.at];
    if (!tok) throw new FormulaError(ERROR);
    this.at++;
    switch (tok.t) {
      case "num":
        return { k: "num", n: tok.n };
      case "ref": {
        if (!this.eat(":")) return { k: "ref", r: tok.r, c: tok.c };
        const end = this.toks[this.at];
        if (end?.t !== "ref") throw new FormulaError(ERROR);
        this.at++;
        return { k: "range", r0: Math.min(tok.r, end.r), c0: Math.min(tok.c, end.c), r1: Math.max(tok.r, end.r), c1: Math.max(tok.c, end.c) };
      }
      case "func": {
        this.nest();
        if (!this.eat("(")) throw new FormulaError(ERROR);
        const args: Expr[] = [];
        if (!this.eat(")")) {
          for (;;) {
            args.push(this.expr());
            if (this.eat(")")) break;
            if (!this.eat(",")) throw new FormulaError(ERROR);
          }
        }
        const ok = tok.f === "ROUND" ? args.length >= 1 && args.length <= 2 : tok.f === "ABS" ? args.length === 1 : args.length > 0;
        if (!ok) throw new FormulaError(ERROR);
        this.depth--;
        return { k: "call", f: tok.f, args };
      }
      case "op":
        if (tok.op !== "(") throw new FormulaError(ERROR);
        this.nest();
        {
          const inner = this.expr();
          if (!this.eat(")")) throw new FormulaError(ERROR);
          this.depth--;
          return inner;
        }
    }
  }
}

function parseFormula(src: string): Expr {
  const s = unstyle(src).text;
  if (!s.startsWith("=")) throw new FormulaError(ERROR);
  const p = new Parser(tokenize(s.slice(1)));
  const e = p.expr();
  if (!p.done()) throw new FormulaError(ERROR);
  return e;
}

/** The cells a formula reads inside the table; false when the range budget runs out. */
function reads(e: Expr, rows: number, cols: number, budget: { left: number }, out: [number, number][]): boolean {
  switch (e.k) {
    case "num":
      return true;
    case "ref":
      if (e.r < rows && e.c < cols) out.push([e.r, e.c]);
      return true;
    case "range":
      if (e.r1 < rows && e.c1 < cols) {
        const area = (e.r1 - e.r0 + 1) * (e.c1 - e.c0 + 1);
        if (area > budget.left) return false;
        budget.left -= area;
        for (let r = e.r0; r <= e.r1; r++) for (let c = e.c0; c <= e.c1; c++) out.push([r, c]);
      }
      return true;
    case "neg":
      return reads(e.a, rows, cols, budget, out);
    case "bin":
      return reads(e.a, rows, cols, budget, out) && reads(e.b, rows, cols, budget, out);
    case "call":
      return e.args.every((a) => reads(a, rows, cols, budget, out));
  }
}

function hasRange(e: Expr): boolean {
  switch (e.k) {
    case "range":
      return true;
    case "num":
    case "ref":
      return false;
    case "neg":
      return hasRange(e.a);
    case "bin":
      return hasRange(e.a) || hasRange(e.b);
    case "call":
      return e.args.some(hasRange);
  }
}

/** `b` to the integer power `e` by squaring: the same multiplications in every port. */
function powi(b: number, e: number): number {
  let n = Math.abs(e);
  let result = 1, base = b;
  while (n > 0) {
    if (n % 2 === 1) result *= base;
    base *= base;
    n = Math.floor(n / 2);
  }
  return e < 0 ? 1 / result : result;
}

/** `x` rounded to `places` decimals (negative: tens, hundreds...), halves away from zero. */
export function roundTo(x: number, places: number): number {
  const p = Math.min(Math.max(Math.trunc(places), -10), 10);
  const f = powi(10, Math.abs(p));
  const r = (v: number) => {
    const a = Math.floor(Math.abs(v) + 0.5);
    return v < 0 ? -a : a;
  };
  return p >= 0 ? r(x * f) / f : r(x / f) * f;
}

type Num = { n: number; fmt: NumberFormat };

class Eval {
  constructor(
    private cells: string[][],
    private cols: number,
    private done: Map<number, Value>,
  ) {}

  key(r: number, c: number) {
    return r * this.cols + c;
  }

  cell(r: number, c: number): Value {
    if (r >= this.cells.length || c >= this.cols) return { kind: "error", code: REF };
    const v = this.done.get(this.key(r, c));
    if (v) return v;
    const raw = this.cells[r][c];
    if (raw === undefined) return { kind: "blank" };
    return isFormula(raw) ? { kind: "error", code: CYCLE } : parseValue(raw);
  }

  static number(v: Value): Num {
    switch (v.kind) {
      case "blank":
        return { n: 0, fmt: plainFormat() };
      case "number":
        return { n: v.n, fmt: v.fmt };
      case "bool":
        return { n: v.b ? 1 : 0, fmt: plainFormat() };
      case "text":
        throw new FormulaError(VALUE);
      case "error":
        throw new FormulaError(v.code);
    }
  }

  num(e: Expr): Num {
    const x = Eval.number(this.value(e));
    if (!Number.isFinite(x.n)) throw new FormulaError(NUM);
    return x;
  }

  value(e: Expr): Value {
    try {
      return this.scalar(e);
    } catch (err) {
      if (err instanceof FormulaError) return { kind: "error", code: err.code };
      throw err;
    }
  }

  scalar(e: Expr): Value {
    switch (e.k) {
      case "num":
        return { kind: "number", n: e.n, fmt: plainFormat() };
      case "ref":
        return this.cell(e.r, e.c);
      case "range":
        throw new FormulaError(VALUE);
      case "neg": {
        const x = this.num(e.a);
        return { kind: "number", n: -x.n, fmt: x.fmt };
      }
      case "bin": {
        const x = this.num(e.a);
        const y = this.num(e.b);
        const pick = isPlain(x.fmt) ? y.fmt : x.fmt;
        let n: number, fmt: NumberFormat;
        switch (e.op) {
          case "+":
            [n, fmt] = [x.n + y.n, pick];
            break;
          case "-":
            [n, fmt] = [x.n - y.n, pick];
            break;
          case "*":
            n = x.n * y.n;
            fmt = x.fmt.percent && !y.fmt.percent ? y.fmt : !x.fmt.percent && y.fmt.percent ? x.fmt : pick;
            break;
          case "/": {
            if (y.n === 0) throw new FormulaError(DIV0);
            const same = x.fmt.currency !== null && x.fmt.currency === y.fmt.currency;
            n = x.n / y.n;
            fmt = same || isPlain(x.fmt) ? plainFormat() : x.fmt;
            break;
          }
          default:
            n = Number.isInteger(y.n) && Math.abs(y.n) <= 1024 ? powi(x.n, y.n) : Math.pow(x.n, y.n);
            fmt = x.fmt;
        }
        if (!Number.isFinite(n)) throw new FormulaError(NUM);
        return { kind: "number", n, fmt };
      }
      case "call":
        return this.call(e.f, e.args);
    }
  }

  numbers(args: Expr[], count: boolean): Num[] {
    const out: Num[] = [];
    for (const a of args) {
      let area: [number, number, number, number] | null = null;
      if (a.k === "range") {
        if (a.r1 >= this.cells.length || a.c1 >= this.cols) {
          if (count) continue;
          throw new FormulaError(REF);
        }
        area = [a.r0, a.c0, a.r1, a.c1];
      } else if (a.k === "ref") area = [a.r, a.c, a.r, a.c];
      if (area) {
        for (let r = area[0]; r <= area[2]; r++) {
          for (let c = area[1]; c <= area[3]; c++) {
            const v = this.cell(r, c);
            if (v.kind === "number") out.push({ n: v.n, fmt: v.fmt });
            else if (v.kind === "error" && !count) throw new FormulaError(v.code);
          }
        }
      } else {
        try {
          out.push(this.num(a));
        } catch (err) {
          if (!count || !(err instanceof FormulaError)) throw err;
        }
      }
    }
    return out;
  }

  call(f: Func, args: Expr[]): Value {
    const finish = (n: number, fmt: NumberFormat): Value => {
      if (!Number.isFinite(n)) throw new FormulaError(NUM);
      return { kind: "number", n, fmt };
    };
    if (f === "ROUND" || f === "ABS") {
      const x = this.num(args[0]);
      const fmt = { ...x.fmt };
      if (f === "ABS") return finish(Math.abs(x.n), fmt);
      const places = args[1] ? this.num(args[1]).n : 0;
      // shows the decimals it rounded to, not the ones it was given
      fmt.decimals = Math.min(Math.max(Math.trunc(places), 0), 10);
      return finish(roundTo(x.n, places), fmt);
    }
    if (f === "COUNT") return { kind: "number", n: this.numbers(args, true).length, fmt: plainFormat() };
    const nums = this.numbers(args, false);
    const fmt = nums[0]?.fmt ?? plainFormat();
    const ns = nums.map((x) => x.n);
    switch (f) {
      case "SUM":
        return finish(ns.reduce((a, b) => a + b, 0), fmt);
      case "PRODUCT":
        return finish(ns.length ? ns.slice(1).reduce((a, b) => a * b, ns[0]) : 0, fmt);
      case "MIN":
        return finish(ns.length ? ns.reduce((a, b) => Math.min(a, b)) : 0, fmt);
      case "MAX":
        return finish(ns.length ? ns.reduce((a, b) => Math.max(a, b)) : 0, fmt);
      default:
        if (!ns.length) throw new FormulaError(DIV0);
        return finish(ns.reduce((a, b) => a + b, 0) / ns.length, fmt);
    }
  }
}

/**
 * Evaluates every formula in a table. `cells` are the cells' source, row by row, the header
 * first. The result has the same shape, with each formula cell's value and null elsewhere.
 */
export function evaluateValues(cells: string[][]): (Value | null)[][] {
  const rows = cells.length;
  const cols = cells.reduce((n, r) => Math.max(n, r.length), 0);
  const budget = { left: MAX_RANGE_CELLS };
  const out: (Value | null)[][] = cells.map((r) => r.map(() => null));
  const formulas: [number, number][] = [];
  const parsed: (Expr | null)[] = [];
  const index = new Map<number, number>();
  const done = new Map<number, Value>();
  const key = (r: number, c: number) => r * cols + c;
  cells.forEach((row, r) =>
    row.forEach((raw, c) => {
      if (!isFormula(raw)) return;
      index.set(key(r, c), formulas.length);
      formulas.push([r, c]);
      try {
        parsed.push(parseFormula(raw));
      } catch (err) {
        if (!(err instanceof FormulaError)) throw err;
        done.set(key(r, c), { kind: "error", code: err.code });
        parsed.push(null);
      }
    }),
  );
  const waiting = new Array<number>(formulas.length).fill(0);
  const dependents: number[][] = formulas.map(() => []);
  let exhausted = false;
  parsed.forEach((expr, i) => {
    if (!expr) return;
    const read: [number, number][] = [];
    if ((exhausted && hasRange(expr)) || !reads(expr, rows, cols, budget, read)) {
      exhausted = true;
      done.set(key(...formulas[i]), { kind: "error", code: LIMIT });
      parsed[i] = null;
      return;
    }
    for (const [r, c] of read) {
      const j = index.get(key(r, c));
      if (j !== undefined) {
        waiting[i]++;
        dependents[j].push(i);
      }
    }
  });
  const ready: number[] = [];
  waiting.forEach((w, i) => w === 0 && ready.push(i));
  const finished = new Array<boolean>(formulas.length).fill(false);
  const ev = new Eval(cells, cols, done);
  for (let q = 0; q < ready.length; q++) {
    const i = ready[q];
    finished[i] = true;
    const expr = parsed[i];
    if (expr) done.set(key(...formulas[i]), ev.value(expr));
    for (const j of dependents[i]) if (--waiting[j] === 0) ready.push(j);
  }
  formulas.forEach(([r, c], i) => {
    out[r][c] = (finished[i] && done.get(key(r, c))) || { kind: "error", code: CYCLE };
  });
  return out;
}

/** [`evaluateValues`], with each result as it should be shown (`evaluate` in penroam-core). */
export function evaluate(cells: string[][]): (string | null)[][] {
  return evaluateValues(cells).map((row) => row.map((v) => (v ? display(v) : null)));
}
