// The spreadsheet editor for Markdown tables in a text box.
//
// While a box is edited, each table at the top level of its Markdown shows as this grid instead
// of its source (markdownPreview.ts decides which). It is a CodeMirror block widget: the document
// is still the Markdown, and every change in the grid is a transaction that replaces the table's
// source with sheetTable.ts's printed table. Undo, the page's history and sync all see ordinary
// text edits, and a note's Markdown stays readable anywhere.
//
// The grid works the way spreadsheets do. A cell is selected, or being edited in an input. Arrow
// keys, Tab and Enter move; typing replaces a cell and Enter or F2 edits it; Delete clears;
// Ctrl+C / Ctrl+V copy and paste tab-separated cells, so blocks move to and from other
// spreadsheets; Ctrl+D fills down; Space ticks a checkbox. While a formula is being typed, a click
// on another cell puts its reference in. Each header cell has a sort arrow and a menu button; the
// column letters and row numbers open menus to sort, align, insert and delete; and the filter row
// hides rows that don't match. Filters only change what the grid shows: they aren't saved, and the
// finished note shows every row. The bar above the grid shows the selected cell's source and
// formats the selection (bold, italic, strikethrough, currency, percent, thousands separators,
// decimals, alignment); the editor's own formatting buttons reach it too, through [`focusedGrid`].
//
// The editor ends a text edit when focus leaves the box, so the grid never lets focus fall out of
// it: pressing its buttons doesn't take focus, and before a focused control is redrawn or a menu
// closes, focus moves to the grid itself.
//
// The cells inherit the box's font, size and colour, so a table in a handwritten box is written
// in the hand font here as it is on the page, and the sort arrows and menu buttons in its header
// cells are drawn as if by hand to match. Everything around the cells (the bar, menus, column
// letters and row numbers) is interface, and looks like the rest of the app: its font, icons,
// colours and theme tokens, at a size that doesn't change with zoom.
import { redo, undo } from "@codemirror/commands";
import { StateEffect } from "@codemirror/state";
import { EditorView, WidgetType } from "@codemirror/view";
import MarkdownIt from "markdown-it";
import type { TextAlign } from "../generated/TextAlign";
import type { FormatAction } from "./markdownFormat";
import { columnName, isFormula, unstyle } from "./sheet";
import { handIcon, uiIcon, type UiIcon } from "./sheetIcons";
import {
  cellCheckbox,
  cellName,
  deleteColumn,
  deleteRow,
  fillDown,
  formatRange,
  insertColumn,
  insertRows,
  parseClipboard,
  parseTable,
  pasteBlock,
  printTable,
  rangeFormat,
  setAlign,
  setCell,
  sortBy,
  sortedDirection,
  viewCells,
  visibleRows,
  type CellFormat,
  type CellView,
  type SheetTable,
} from "./sheetTable";

/** Inline Markdown in a cell, as HTML. `html: false` escapes any tags in the source. */
const inlineMd = new MarkdownIt("commonmark", { html: false, linkify: false, typographer: false }).enable(["strikethrough"]);

/** The last cells copied from a grid, so pasting them moves their formulas' references. */
let copied: { text: string; cells: string[][]; from: [number, number] } | null = null;

const grids = new WeakMap<HTMLElement, SheetGrid>();

/** A table shown as a spreadsheet. `source` is the table's Markdown, whole lines. */
export class SheetWidget extends WidgetType {
  constructor(readonly source: string) {
    super();
  }
  eq(other: SheetWidget) {
    return other.source === this.source;
  }
  toDOM(view: EditorView) {
    return new SheetGrid(view, this.source).root;
  }
  updateDOM(dom: HTMLElement) {
    const grid = grids.get(dom);
    if (!grid) return false;
    grid.setSource(this.source);
    return true;
  }
  get estimatedHeight() {
    return this.source.split("\n").length * 24;
  }
  /** The grid handles its own events; CodeMirror must not move its selection for them. */
  ignoreEvent() {
    return true;
  }
  destroy(dom: HTMLElement) {
    grids.get(dom)?.destroy();
  }
}

type Cell = [number, number];

interface Editing {
  r: number;
  c: number;
  input: HTMLInputElement;
}

const el = <K extends keyof HTMLElementTagNameMap>(tag: K, cls?: string, text?: string) => {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
};

const button = (label: string | Node, cls: string, run: (b: HTMLButtonElement) => void, title?: string) => {
  const b = el("button", cls);
  b.append(label);
  b.type = "button";
  if (title) {
    b.title = title;
    b.setAttribute("aria-label", title);
  }
  b.addEventListener("click", (e) => {
    e.preventDefault();
    run(b);
  });
  return b;
};

/** A menu entry: a label, what it does, an optional icon, and whether it destroys something. */
interface MenuEntry {
  label: string;
  run: () => void;
  icon?: UiIcon;
  danger?: boolean;
}

/** The grid that has focus, if any, so the editor's formatting buttons can format its cells. */
export function focusedGrid(): SheetGrid | null {
  const root = (document.activeElement as HTMLElement | null)?.closest?.<HTMLElement>(".cm-sheet");
  return (root && grids.get(root)) ?? null;
}

/** The grid for one table: its DOM, selection and edits. */
export class SheetGrid {
  readonly root: HTMLDivElement;
  private table: SheetTable;
  private views: CellView[][] = [];
  private active: Cell = [1, 0];
  private anchor: Cell = [1, 0];
  private editing: Editing | null = null;
  private filters: string[] = [];
  private filterOpen = false;
  /** The column last sorted here, which shows which way it was sorted. */
  private sortedColumn: number | null = null;
  private menu: HTMLElement | null = null;
  private dragging = false;
  private bar: HTMLDivElement;
  private scroll: HTMLDivElement;
  private body: HTMLTableSectionElement | null = null;
  private status: HTMLDivElement;

  constructor(
    private view: EditorView,
    private source: string,
  ) {
    this.table = parseTable(source) ?? { aligns: ["left"], cells: [[""]] };
    if (this.table.cells.length < 2) this.active = this.anchor = [0, 0];
    this.root = el("div", "cm-sheet");
    this.root.contentEditable = "false";
    this.root.tabIndex = 0;
    this.root.setAttribute("role", "grid");
    this.root.setAttribute("aria-label", "Spreadsheet");
    this.bar = el("div", "cm-sheet-bar");
    this.scroll = el("div", "cm-sheet-scroll");
    this.status = el("div", "cm-sheet-status");
    this.root.append(this.bar, this.scroll, this.status);
    grids.set(this.root, this);
    this.root.addEventListener("pointerdown", (e) => this.onPointerDown(e));
    this.root.addEventListener("pointermove", (e) => this.onPointerMove(e));
    this.root.addEventListener("pointerup", () => (this.dragging = false));
    this.root.addEventListener("dblclick", (e) => this.onDoubleClick(e));
    this.root.addEventListener("keydown", (e) => this.onKeyDown(e));
    this.root.addEventListener("copy", (e) => this.onCopy(e, false));
    this.root.addEventListener("cut", (e) => this.onCopy(e, true));
    this.root.addEventListener("paste", (e) => this.onPaste(e));
    this.root.addEventListener("focusin", () => this.root.classList.add("is-focused"));
    this.root.addEventListener("focusout", (e) => {
      if (!this.root.contains(e.relatedTarget as Node | null)) {
        this.root.classList.remove("is-focused");
        this.closeMenu();
      }
    });
    this.render();
  }

  // ------------------------------------------------------------ model --

  private get rows() {
    return this.table.cells.length;
  }
  private get cols() {
    return this.table.aligns.length;
  }

  /** The table's source changed under the grid: an undo, or the grid's own edit coming back. */
  setSource(source: string) {
    if (source === this.source) return;
    const t = parseTable(source);
    if (!t) return;
    this.source = source;
    this.table = t;
    if (this.editing) this.editing = null;
    this.clampSelection();
    this.render();
  }

  private clampSelection() {
    const fit = ([r, c]: Cell): Cell => [Math.min(r, this.rows - 1), Math.min(c, this.cols - 1)];
    this.active = fit(this.active);
    this.anchor = fit(this.anchor);
    this.filters.length = Math.min(this.filters.length, this.cols);
  }

  /** Replaces the table in the document with `t`. */
  private apply(t: SheetTable) {
    const next = printTable(t);
    if (next === this.source) {
      this.table = t;
      return;
    }
    let from: number;
    try {
      from = this.view.posAtDOM(this.root);
    } catch {
      return;
    }
    const to = from + this.source.length;
    // the widget covers exactly its source; anything else means the document moved on under us
    if (this.view.state.sliceDoc(from, to) !== this.source) {
      console.warn("spreadsheet edit dropped: the table changed under the grid");
      return;
    }
    this.table = t;
    this.source = next;
    this.clampSelection();
    this.view.dispatch({ changes: { from, to, insert: next }, userEvent: "sheet" });
    this.render();
  }

  // ---------------------------------------------------------- render --

  /** Moves focus to the grid when a control inside it has it, before that control goes away. */
  private holdFocus() {
    const active = document.activeElement;
    if (active && active !== this.root && this.root.contains(active)) this.root.focus({ preventScroll: true });
  }

  private render() {
    this.holdFocus();
    this.views = viewCells(this.table);
    this.renderBar();
    const table = el("table", "cm-sheet-table");
    const head = el("thead");
    const letters = el("tr", "cm-sheet-letters");
    letters.append(el("th", "cm-sheet-corner"));
    for (let c = 0; c < this.cols; c++) {
      const th = el("th", `cm-sheet-col${c === this.active[1] ? " is-current" : ""}`, columnName(c));
      th.dataset.col = String(c);
      th.title = `Column ${columnName(c)}: sort, filter, insert or delete`;
      letters.append(th);
    }
    head.append(letters, this.rowElement(0));
    if (this.filterOpen) head.append(this.filterRow());
    table.append(head);
    this.body = el("tbody");
    table.append(this.body);
    this.scroll.replaceChildren(table);
    this.renderBody();
  }

  private renderBody() {
    if (!this.body) return;
    const shown = visibleRows(this.views, this.filters);
    this.body.replaceChildren(...shown.map((r) => this.rowElement(r)));
    const hidden = this.rows - 1 - shown.length;
    this.status.replaceChildren();
    if (hidden > 0) {
      this.status.append(`Showing ${shown.length} of ${this.rows - 1} rows `, button("Clear filters", "cm-sheet-link", () => this.clearFilters()));
    }
    this.remeasure();
  }

  private renderBar() {
    if (this.bar.contains(document.activeElement)) this.holdFocus();
    const [r, c] = this.active;
    const raw = this.table.cells[r]?.[c] ?? "";
    const [r0, c0, r1, c1] = this.range();
    const f = rangeFormat(this.table, r0, c0, r1, c1);
    const tool = (label: string | Node, title: string, run: (b: HTMLButtonElement) => void, on = false) =>
      button(label, `cm-sheet-tool${on ? " is-on" : ""}`, (b) => this.act(() => run(b)), title);
    const gap = () => el("span", "cm-sheet-gap");
    const dir = this.sortedColumn === c ? sortedDirection(this.table, this.views, c) : null;
    const align = this.table.aligns[c];
    const formula = el("div", "cm-sheet-formula");
    formula.append(el("span", "cm-sheet-ref", cellName(r, c)), el("span", "cm-sheet-src", raw || " "));
    formula.setAttribute("aria-label", `Cell ${cellName(r, c)}`);
    const tools = el("div", "cm-sheet-tools");
    tools.setAttribute("role", "toolbar");
    tools.setAttribute("aria-label", "Spreadsheet");
    tools.append(
      tool(uiIcon("sortAsc"), `Sort by column ${columnName(c)}, A to Z and 1 to 9`, () => this.sort(c, false), dir === "asc"),
      tool(uiIcon("sortDesc"), `Sort by column ${columnName(c)}, Z to A and 9 to 1`, () => this.sort(c, true), dir === "desc"),
      gap(),
      tool(uiIcon("bold"), "Bold", () => this.style("bold", !f.style.bold), f.style.bold),
      tool(uiIcon("italic"), "Italic", () => this.style("italic", !f.style.italic), f.style.italic),
      tool(uiIcon("strike"), "Strikethrough", () => this.style("strike", !f.style.strike), f.style.strike),
      gap(),
      tool(uiIcon("currency"), "Currency", (b) => this.openCurrencyMenu(b), f.currency !== null),
      tool(uiIcon("percent"), "Percentage", () => this.format(f.percent ? { kind: "plain" } : { kind: "percent" }), f.percent),
      tool(el("span", "cm-sheet-glyph", "1,000"), "Thousands separators", () => this.format({ kind: "grouping", on: !f.grouped }), f.grouped),
      tool(uiIcon("decimalsLess"), "Fewer decimals", () => this.format({ kind: "decimals", delta: -1 })),
      tool(uiIcon("decimalsMore"), "More decimals", () => this.format({ kind: "decimals", delta: 1 })),
      gap(),
      tool(uiIcon("alignLeft"), "Align column left", () => this.align(c, "left"), align === "left"),
      tool(uiIcon("alignCenter"), "Align column centre", () => this.align(c, "center"), align === "center"),
      tool(uiIcon("alignRight"), "Align column right", () => this.align(c, "right"), align === "right"),
      gap(),
      tool(uiIcon("filter"), "Filter rows", () => this.toggleFilters(), this.filterOpen),
      tool(uiIcon("addRow"), "Insert a row below", () => this.insertRowAt(Math.max(r, 0) + 1)),
      tool(uiIcon("addColumn"), "Insert a column to the right", () => this.insertColumnAt(c + 1)),
      tool(uiIcon("markdown"), "Edit this table as Markdown", () => this.showSource()),
    );
    this.bar.replaceChildren(formula, tools);
  }

  /**
   * Runs a toolbar or header action: a cell being edited is written first, and the grid keeps
   * focus afterwards, so the box stays in editing whatever the action rebuilt.
   */
  private act(run: () => void) {
    this.closeMenu();
    if (this.editing) this.commitEdit(false);
    run();
    if (!this.editing && this.root.isConnected && !this.root.contains(document.activeElement) && !this.menu) this.root.focus({ preventScroll: true });
  }

  private inRange(r: number, c: number) {
    const [r0, c0, r1, c1] = this.range();
    return r >= r0 && r <= r1 && c >= c0 && c <= c1;
  }

  /** The selection as top, left, bottom, right. */
  private range(): [number, number, number, number] {
    const [ar, ac] = this.anchor, [br, bc] = this.active;
    return [Math.min(ar, br), Math.min(ac, bc), Math.max(ar, br), Math.max(ac, bc)];
  }

  private rowElement(r: number): HTMLTableRowElement {
    const tr = el("tr", r === 0 ? "cm-sheet-header" : "");
    const num = el("th", `cm-sheet-row${r === this.active[0] ? " is-current" : ""}`, String(r + 1));
    num.dataset.row = String(r);
    num.title = r === 0 ? "Header row" : `Row ${r + 1}: insert or delete`;
    tr.append(num);
    for (let c = 0; c < this.cols; c++) tr.append(this.cellElement(r, c));
    return tr;
  }

  private cellElement(r: number, c: number): HTMLTableCellElement {
    const td = el(r === 0 ? "th" : "td", "cm-sheet-cell");
    td.dataset.r = String(r);
    td.dataset.c = String(c);
    td.setAttribute("role", "gridcell");
    td.style.textAlign = this.table.aligns[c];
    const [ar, ac] = this.active;
    if (r === ar && c === ac) td.classList.add("is-active");
    else if (this.inRange(r, c)) td.classList.add("in-range");
    const raw = this.table.cells[r][c];
    const view = this.views[r][c];
    const box = cellCheckbox(raw);
    if (box !== null) {
      const check = el("span", "cm-sheet-check", box ? "✓" : "");
      check.setAttribute("role", "checkbox");
      check.setAttribute("aria-checked", String(box));
      td.append(check);
    } else if (isFormula(raw)) {
      td.textContent = view.text;
      td.classList.add("is-formula");
      const { style } = unstyle(raw);
      td.classList.toggle("is-bold", style.bold);
      td.classList.toggle("is-italic", style.italic);
      td.classList.toggle("is-strike", style.strike);
      if (view.value.kind === "error") td.classList.add("is-error");
      td.title = raw;
    } else if (raw) {
      td.innerHTML = inlineMd.renderInline(raw);
    }
    if (r === 0) td.append(this.headerControls(c));
    return td;
  }

  /** A header cell's sort arrow and menu button: big targets, so a column is easy to reach. */
  private headerControls(c: number): HTMLElement {
    // the arrow shows the way the column was sorted while the rows are still in that order
    const dir = c === this.sortedColumn ? sortedDirection(this.table, this.views, c) : null;
    const box = el("span", "cm-sheet-hctl");
    const name = columnName(c);
    box.append(
      button(
        handIcon(dir === "asc" ? "sortAsc" : dir === "desc" ? "sortDesc" : "sortNone"),
        `cm-sheet-hbtn${dir ? " is-on" : ""}`,
        () => this.act(() => this.sort(c, dir === "asc")),
        dir === "asc" ? `Sorted A to Z by column ${name}; sort Z to A` : dir === "desc" ? `Sorted Z to A by column ${name}; sort A to Z` : `Sort by column ${name}`,
      ),
      button(handIcon("menu"), "cm-sheet-hbtn", (b) => this.act(() => this.openColumnMenu(c, b)), `Column ${name}: sort, filter, format, insert or delete`),
    );
    return box;
  }

  private filterRow(): HTMLTableRowElement {
    const tr = el("tr", "cm-sheet-filters");
    tr.append(el("th", "cm-sheet-row"));
    for (let c = 0; c < this.cols; c++) {
      const td = el("td", "cm-sheet-filter");
      const input = el("input", "cm-sheet-filter-input");
      input.placeholder = "Filter";
      input.value = this.filters[c] ?? "";
      input.dataset.filter = String(c);
      input.setAttribute("aria-label", `Filter column ${columnName(c)}`);
      input.title = "Text to look for, or a comparison such as >10, <=2026-01-31, =pear or !=pear. Checkboxes: yes or no.";
      input.addEventListener("input", () => {
        this.filters[c] = input.value;
        this.renderBody();
      });
      input.addEventListener("keydown", (e) => {
        e.stopPropagation();
        if (e.key === "Escape" || e.key === "Enter") {
          e.preventDefault();
          this.root.focus({ preventScroll: true });
        }
      });
      td.append(input);
      tr.append(td);
    }
    return tr;
  }

  // ------------------------------------------------------- selection --

  private select(cell: Cell, extend = false) {
    this.active = cell;
    if (!extend) this.anchor = cell;
    this.refreshSelection();
  }

  /** Updates the selection classes and the bar without rebuilding the grid. */
  private refreshSelection() {
    const [ar, ac] = this.active;
    for (const td of this.root.querySelectorAll<HTMLElement>(".cm-sheet-cell")) {
      const r = Number(td.dataset.r), c = Number(td.dataset.c);
      td.classList.toggle("is-active", r === ar && c === ac);
      td.classList.toggle("in-range", !(r === ar && c === ac) && this.inRange(r, c));
    }
    for (const th of this.root.querySelectorAll<HTMLElement>(".cm-sheet-col")) th.classList.toggle("is-current", Number(th.dataset.col) === ac);
    for (const th of this.root.querySelectorAll<HTMLElement>(".cm-sheet-row[data-row]")) th.classList.toggle("is-current", Number(th.dataset.row) === ar);
    this.renderBar();
    this.root.querySelector(".cm-sheet-cell.is-active")?.scrollIntoView?.({ block: "nearest", inline: "nearest" });
  }

  /** Rows the keyboard moves through: the header and the rows the filters leave. */
  private navRows(): number[] {
    return [0, ...visibleRows(this.views, this.filters)];
  }

  private move(dr: number, dc: number, extend = false, toEdge = false) {
    const rows = this.navRows();
    let i = Math.max(0, rows.indexOf(this.active[0]));
    let c = this.active[1];
    if (toEdge) {
      if (dr) i = dr > 0 ? rows.length - 1 : 0;
      if (dc) c = dc > 0 ? this.cols - 1 : 0;
    } else {
      i = Math.min(Math.max(i + dr, 0), rows.length - 1);
      c = Math.min(Math.max(c + dc, 0), this.cols - 1);
    }
    this.select([rows[i], c], extend);
  }

  /** Tab: the next cell along the row, then the next row; past the last cell, a new row. */
  private tab(back: boolean) {
    const rows = this.navRows();
    let i = Math.max(0, rows.indexOf(this.active[0]));
    let c = this.active[1] + (back ? -1 : 1);
    if (c >= this.cols) {
      c = 0;
      i++;
    } else if (c < 0) {
      c = this.cols - 1;
      i--;
    }
    if (i < 0) return this.select([rows[0], 0]);
    if (i >= rows.length) {
      this.insertRowAt(this.rows);
      return this.select([this.rows - 1, 0]);
    }
    this.select([rows[i], c]);
  }

  private cellAt(target: EventTarget | null): Cell | null {
    const td = (target as HTMLElement | null)?.closest?.<HTMLElement>(".cm-sheet-cell");
    return td && this.root.contains(td) ? [Number(td.dataset.r), Number(td.dataset.c)] : null;
  }

  // ---------------------------------------------------------- events --

  private onPointerDown(e: PointerEvent) {
    const target = e.target as HTMLElement;
    if (this.menu?.contains(target)) return;
    this.closeMenu();
    if (target.closest("input")) return;
    // a button acts on its click; taking focus here would blur the cell being edited, and a
    // button that goes away with focus would take the box out of editing with it
    if (target.closest("button")) return e.preventDefault();
    const col = target.closest<HTMLElement>(".cm-sheet-col");
    const row = target.closest<HTMLElement>(".cm-sheet-row");
    if (col || (row && row.dataset.row !== undefined)) {
      e.preventDefault();
      this.root.focus({ preventScroll: true });
      if (col) this.openColumnMenu(Number(col.dataset.col), col);
      else this.openRowMenu(Number(row!.dataset.row), row!);
      return;
    }
    const cell = this.cellAt(target);
    if (!cell) return;
    e.preventDefault();
    const ed = this.editing;
    if (ed && ed.input.value.trimStart().startsWith("=") && (cell[0] !== ed.r || cell[1] !== ed.c)) {
      // pointing at a cell while writing a formula puts its reference in
      this.insertIntoInput(ed.input, cellName(...cell));
      return;
    }
    if (ed) this.commitEdit();
    const wasActive = this.active[0] === cell[0] && this.active[1] === cell[1] && this.anchor[0] === cell[0] && this.anchor[1] === cell[1];
    this.root.focus({ preventScroll: true });
    if (target.closest(".cm-sheet-check")) {
      this.select(cell);
      return this.toggleCheckbox(cell);
    }
    this.select(cell, e.shiftKey);
    // a second tap on a cell edits it, as there is no double-click to reach for with a pen
    if (wasActive && !e.shiftKey && e.pointerType !== "mouse") return this.beginEdit();
    this.dragging = e.pointerType === "mouse";
  }

  private onPointerMove(e: PointerEvent) {
    if (!this.dragging || !(e.buttons & 1)) return;
    const cell = this.cellAt(document.elementFromPoint?.(e.clientX, e.clientY) ?? e.target);
    if (cell && (cell[0] !== this.active[0] || cell[1] !== this.active[1])) this.select(cell, true);
  }

  private onDoubleClick(e: MouseEvent) {
    const cell = this.cellAt(e.target);
    if (!cell || (e.target as HTMLElement).closest(".cm-sheet-check")) return;
    this.select(cell);
    this.beginEdit();
  }

  private onKeyDown(e: KeyboardEvent) {
    if ((e.target as HTMLElement) !== this.root) return; // the cell input and filters handle their own
    const mod = e.ctrlKey || e.metaKey;
    const done = () => {
      e.preventDefault();
      e.stopPropagation();
    };
    if (this.menu && e.key === "Escape") {
      this.closeMenu();
      return done();
    }
    // as in the app's menus, the arrow keys move into an open menu
    if (this.menu && (e.key === "ArrowDown" || e.key === "ArrowUp")) {
      const items = this.menu.querySelectorAll<HTMLElement>('[role="menuitem"]');
      items[e.key === "ArrowDown" ? 0 : items.length - 1]?.focus({ preventScroll: true });
      return done();
    }
    switch (e.key) {
      case "ArrowUp":
      case "ArrowDown":
      case "ArrowLeft":
      case "ArrowRight": {
        const [dr, dc] = { ArrowUp: [-1, 0], ArrowDown: [1, 0], ArrowLeft: [0, -1], ArrowRight: [0, 1] }[e.key]!;
        this.move(dr, dc, e.shiftKey, mod);
        return done();
      }
      case "Home":
      case "End":
        this.move(mod ? (e.key === "Home" ? -1 : 1) : 0, e.key === "Home" ? -1 : 1, e.shiftKey, true);
        return done();
      case "Tab":
        this.tab(e.shiftKey);
        return done();
      case "Enter":
      case "F2":
        this.beginEdit();
        return done();
      case "Delete":
      case "Backspace":
        this.clearRange();
        return done();
      case " ":
        if (cellCheckbox(this.table.cells[this.active[0]][this.active[1]]) !== null) {
          this.toggleCheckbox(this.active);
          return done();
        }
        break;
    }
    if (mod && !e.altKey) {
      const k = e.key.toLowerCase();
      if (k === "z") {
        if (e.shiftKey) redo(this.view);
        else undo(this.view);
        return done();
      }
      if (k === "y") {
        redo(this.view);
        return done();
      }
      if (k === "d") {
        this.fillDown();
        return done();
      }
      if (k === "a") {
        this.anchor = [0, 0];
        this.active = [this.rows - 1, this.cols - 1];
        this.refreshSelection();
        return done();
      }
      return; // copy, cut and paste come as their own events
    }
    if (e.key.length === 1 && !e.altKey && !e.isComposing) {
      // typing replaces the cell, as in every spreadsheet
      this.beginEdit(e.key);
      return done();
    }
  }

  private onCopy(e: ClipboardEvent, cut: boolean) {
    if (this.editing || e.target !== this.root) return;
    e.preventDefault();
    const [r0, c0, r1, c1] = this.range();
    const cells: string[][] = [];
    const shown: string[][] = [];
    for (let r = r0; r <= r1; r++) {
      cells.push(this.table.cells[r].slice(c0, c1 + 1));
      shown.push(this.views[r].slice(c0, c1 + 1).map((v) => v.text));
    }
    // other apps get what the cells show; a paste back into a grid gets their formulas
    const text = shown.map((r) => r.join("\t")).join("\n");
    e.clipboardData?.setData("text/plain", text);
    copied = { text, cells, from: [r0, c0] };
    if (cut) this.clearRange();
  }

  private onPaste(e: ClipboardEvent) {
    if (this.editing || e.target !== this.root) return;
    const text = e.clipboardData?.getData("text/plain");
    if (!text) return;
    e.preventDefault();
    const [r, c] = [Math.min(this.anchor[0], this.active[0]), Math.min(this.anchor[1], this.active[1])];
    const own = copied && copied.text === text.replace(/\r\n?/g, "\n").replace(/\n$/, "") ? copied : null;
    const block = own ? own.cells : parseClipboard(text);
    // a paste is a few cells or a sheet, never thousands of columns
    const fitted = block.slice(0, 2000).map((row) => row.slice(0, 100));
    this.apply(pasteBlock(this.table, r, c, fitted, own?.from));
    this.anchor = [r, c];
    this.active = [Math.min(r + fitted.length - 1, this.rows - 1), Math.min(c + Math.max(...fitted.map((x) => x.length)) - 1, this.cols - 1)];
    this.refreshSelection();
  }

  // ----------------------------------------------------------- edits --

  private beginEdit(replaceWith?: string) {
    const [r, c] = this.active;
    this.anchor = this.active;
    const td = this.root.querySelector<HTMLElement>(`.cm-sheet-cell[data-r="${r}"][data-c="${c}"]`);
    if (!td) return;
    const input = el("input", "cm-sheet-input");
    input.value = replaceWith ?? this.table.cells[r][c];
    input.setAttribute("aria-label", `Cell ${cellName(r, c)}`);
    input.spellcheck = true;
    // an input's default width would widen the column; the cell's own width decides instead
    input.size = 1;
    this.editing = { r, c, input };
    // the column keeps its width while the cell is an input, so the rest of the table holds still
    td.style.minWidth = `${td.offsetWidth}px`;
    td.classList.add("is-editing");
    td.replaceChildren(input);
    input.addEventListener("keydown", (e) => this.onInputKey(e));
    input.addEventListener("blur", () => {
      // leaving the grid for the text, the toolbar or elsewhere keeps what was typed
      if (this.editing?.input === input) this.commitEdit(false);
    });
    input.focus({ preventScroll: true });
    input.setSelectionRange(input.value.length, input.value.length);
    this.remeasure();
  }

  private onInputKey(e: KeyboardEvent) {
    e.stopPropagation(); // keys in a cell never reach the page or the box around it
    if (e.isComposing) return;
    const go = (dr: number, dc: number) => {
      e.preventDefault();
      this.commitEdit();
      if (dc) this.tab(dc < 0);
      else this.move(dr, 0);
    };
    switch (e.key) {
      case "Enter":
        return go(e.shiftKey ? -1 : 1, 0);
      case "Tab":
        return go(0, e.shiftKey ? -1 : 1);
      case "ArrowUp":
        return go(-1, 0);
      case "ArrowDown":
        return go(1, 0);
      case "Escape":
        e.preventDefault();
        this.cancelEdit();
        return;
    }
  }

  private insertIntoInput(input: HTMLInputElement, text: string) {
    const from = input.selectionStart ?? input.value.length, to = input.selectionEnd ?? from;
    input.setRangeText(text, from, to, "end");
    input.focus({ preventScroll: true });
  }

  /** Writes the cell being edited. `refocus` returns focus to the grid. */
  private commitEdit(refocus = true) {
    const ed = this.editing;
    if (!ed) return;
    this.editing = null;
    const value = ed.input.value;
    if (value !== this.table.cells[ed.r][ed.c]) this.apply(setCell(this.table, ed.r, ed.c, value));
    else this.render();
    if (refocus) this.root.focus({ preventScroll: true });
  }

  private cancelEdit() {
    this.editing = null;
    this.render();
    this.root.focus({ preventScroll: true });
  }

  private clearRange() {
    const [r0, c0, r1, c1] = this.range();
    let t = this.table;
    for (let r = r0; r <= r1; r++) for (let c = c0; c <= c1; c++) if (t.cells[r][c]) t = setCell(t, r, c, "");
    this.apply(t);
  }

  private toggleCheckbox([r, c]: Cell) {
    const box = cellCheckbox(this.table.cells[r][c]);
    if (box === null) return;
    this.apply(setCell(this.table, r, c, box ? "[ ]" : "[x]"));
  }

  private fillDown() {
    const [r0, c0, r1, c1] = this.range();
    if (r1 > r0) this.apply(fillDown(this.table, r0, c0, r1, c1));
  }

  private insertRowAt(at: number) {
    this.apply(insertRows(this.table, Math.max(1, at)));
    this.select([Math.max(1, at), this.active[1]]);
  }

  private insertColumnAt(at: number) {
    this.sortedColumn = null;
    this.apply(insertColumn(this.table, at, `Column ${this.cols + 1}`));
    this.filters.splice(at, 0, "");
    this.select([this.active[0], at]);
  }

  /** Applies a format to the selected cells. */
  private format(f: CellFormat) {
    const [r0, c0, r1, c1] = this.range();
    this.apply(formatRange(this.table, r0, c0, r1, c1, f));
  }

  private style(style: "bold" | "italic" | "strike", on: boolean) {
    this.format({ kind: "style", style, on });
  }

  private align(c: number, align: TextAlign) {
    const [, c0, , c1] = this.range();
    let t = this.table;
    // every selected column, or the one the cursor is in
    for (let i = Math.min(c, c0); i <= Math.max(c, c1); i++) t = setAlign(t, i, align);
    this.apply(t);
  }

  /**
   * The editor's formatting buttons while the grid has focus. Bold, italic and strikethrough
   * format the selected cells; the rest don't apply to cells, and do nothing rather than change
   * text elsewhere in the box. Returns whether the grid took the command.
   */
  formatCommand(action: FormatAction): boolean {
    if (action === "bold" || action === "italic" || action === "strike") {
      const [r0, c0, r1, c1] = this.range();
      const on = !rangeFormat(this.table, r0, c0, r1, c1).style[action];
      this.act(() => this.style(action, on));
    }
    return true;
  }

  /** The editor toolbar's formats that are on for the selected cells. */
  get formats(): FormatAction[] {
    const [r0, c0, r1, c1] = this.range();
    const { style } = rangeFormat(this.table, r0, c0, r1, c1);
    return (["bold", "italic", "strike"] as const).filter((k) => style[k]);
  }

  private openCurrencyMenu(anchor: HTMLElement) {
    const as = (symbol: string, name: string): MenuEntry => ({ label: `${symbol}  ${name}`, run: () => this.format({ kind: "currency", symbol }) });
    this.openMenu(anchor, [as("$", "Dollar"), as("€", "Euro"), as("£", "Pound"), as("¥", "Yen"), null, { label: "Plain number", icon: "number", run: () => this.format({ kind: "plain" }) }]);
  }

  private sort(c: number, descending: boolean) {
    this.sortedColumn = c;
    this.apply(sortBy(this.table, c, descending));
  }

  private toggleFilters() {
    this.filterOpen = !this.filterOpen;
    if (!this.filterOpen) this.filters = [];
    this.render();
    this.root.focus({ preventScroll: true });
  }

  private clearFilters() {
    this.filters = [];
    this.render();
    this.root.focus({ preventScroll: true });
  }

  /** Shows the table's Markdown in the editor, with the caret on its first line. */
  private showSource() {
    let from: number;
    try {
      from = this.view.posAtDOM(this.root);
    } catch {
      return;
    }
    const line = this.view.state.doc.lineAt(from);
    // focus moves to the text before the grid goes away, so the box stays in editing
    this.view.focus();
    this.view.dispatch({ selection: { anchor: line.to }, effects: showTableSource.of(null), scrollIntoView: false });
  }

  // ----------------------------------------------------------- menus --

  private openMenu(anchor: HTMLElement, items: (MenuEntry | null)[]) {
    this.closeMenu();
    const menu = el("div", "cm-sheet-menu");
    menu.setAttribute("role", "menu");
    menu.addEventListener("keydown", (e) => this.onMenuKey(e));
    for (const item of items) {
      if (!item) {
        menu.append(el("div", "cm-sheet-menu-rule"));
        continue;
      }
      const label = el("span", "cm-sheet-menu-label", item.label);
      const content = document.createDocumentFragment();
      // every item keeps the icon column, so the labels line up
      content.append(item.icon ? uiIcon(item.icon) : el("span", "cm-sheet-icon"), label);
      const b = button(content, `cm-sheet-menu-item${item.danger ? " is-danger" : ""}`, () => this.act(item.run));
      b.setAttribute("role", "menuitem");
      menu.append(b);
    }
    const box = this.root.getBoundingClientRect(), at = anchor.getBoundingClientRect();
    // in the grid's own units, as the page around it may be zoomed
    const scale = box.width ? this.root.offsetWidth / box.width : 1;
    menu.style.left = `${(at.left - box.left) * scale}px`;
    menu.style.top = `${(at.bottom - box.top) * scale}px`;
    this.root.append(menu);
    this.menu = menu;
  }

  /** Up and down move through a menu's items, as in the app's menus, and Escape closes it. */
  private onMenuKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      // closes the menu only; reaching the box, it would end the edit
      e.preventDefault();
      e.stopPropagation();
      return this.closeMenu();
    }
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    e.preventDefault();
    e.stopPropagation();
    const items = [...(this.menu?.querySelectorAll<HTMLElement>('[role="menuitem"]') ?? [])];
    const i = items.indexOf(document.activeElement as HTMLElement);
    items[(i + (e.key === "ArrowDown" ? 1 : -1) + items.length) % items.length]?.focus({ preventScroll: true });
  }

  private closeMenu() {
    // removing the menu blurs its focused item, and that focusout comes back here
    const menu = this.menu;
    this.menu = null;
    if (menu?.contains(document.activeElement)) this.root.focus({ preventScroll: true });
    menu?.remove();
  }

  private openColumnMenu(c: number, anchor: HTMLElement) {
    this.anchor = [0, c];
    this.active = [this.rows - 1, c];
    this.refreshSelection();
    const align = (a: TextAlign) => () => this.apply(setAlign(this.table, c, a));
    const deleteCol: MenuEntry = {
      label: "Delete column",
      icon: "trash",
      danger: true,
      run: () => {
        this.filters.splice(c, 1);
        this.sortedColumn = null;
        this.apply(deleteColumn(this.table, c));
      },
    };
    this.openMenu(anchor, [
      { label: "Sort A → Z, 1 → 9", icon: "sortAsc", run: () => this.sort(c, false) },
      { label: "Sort Z → A, 9 → 1", icon: "sortDesc", run: () => this.sort(c, true) },
      {
        label: "Filter this column",
        icon: "filter",
        run: () => {
          this.filterOpen = true;
          this.render();
          this.root.querySelector<HTMLInputElement>(`[data-filter="${c}"]`)?.focus({ preventScroll: true });
        },
      },
      null,
      { label: "Align left", icon: "alignLeft", run: align("left") },
      { label: "Align centre", icon: "alignCenter", run: align("center") },
      { label: "Align right", icon: "alignRight", run: align("right") },
      null,
      { label: "Format as currency…", icon: "currency", run: () => this.openCurrencyMenu(anchor) },
      { label: "Format as percentage", icon: "percent", run: () => this.format({ kind: "percent" }) },
      { label: "Format as plain number", icon: "number", run: () => this.format({ kind: "plain" }) },
      null,
      { label: "Insert column left", icon: "addColumn", run: () => this.insertColumnAt(c) },
      { label: "Insert column right", icon: "addColumn", run: () => this.insertColumnAt(c + 1) },
      ...(this.cols > 1 ? [null, deleteCol] : []),
    ]);
  }

  private openRowMenu(r: number, anchor: HTMLElement) {
    this.anchor = [r, 0];
    this.active = [r, this.cols - 1];
    this.refreshSelection();
    const items: (MenuEntry | null)[] = [];
    if (r > 0) items.push({ label: "Insert row above", icon: "addRow", run: () => this.insertRowAt(r) });
    items.push({ label: "Insert row below", icon: "addRow", run: () => this.insertRowAt(r + 1) });
    if (r > 0) items.push(null, { label: "Delete row", icon: "trash", danger: true, run: () => this.apply(deleteRow(this.table, r)) });
    this.openMenu(anchor, items);
  }

  /**
   * Tells CodeMirror the grid's height may have changed, so the box grows with it. Deferred, as
   * the grid first renders inside CodeMirror's own update, where it can't ask for one.
   */
  private remeasure() {
    queueMicrotask(() => {
      if (grids.has(this.root)) this.view.requestMeasure();
    });
  }

  destroy() {
    this.closeMenu();
    grids.delete(this.root);
  }
}

/** Shows the table at the selection as Markdown source rather than a grid (see markdownPreview.ts). */
export const showTableSource = StateEffect.define<null>();

/** A length in screen pixels, the same at any zoom like the editor's other handles. */
const px = (n: number) => `calc(${n}px * var(--inv, 1))`;
/** Interface text: the app's font, at a size in screen pixels. */
const ui = (size: number) => ({ fontFamily: "var(--font-sans, system-ui, sans-serif)", fontSize: px(size), fontWeight: "normal", fontStyle: "normal", lineHeight: "1.4", letterSpacing: "normal" });
const BRAND = "var(--pr-brand)";
/** The app's selected-control look: brand-soft fill, brand ink and a faint brand ring. */
const SELECTED = { background: "var(--pr-brand-soft)", color: BRAND, boxShadow: `inset 0 0 0 1px color-mix(in srgb, ${BRAND} 40%, transparent)` };
/** The column letters and row numbers sit in the margin, so the cells line up with the finished table. */
const GUTTER = px(24);
const LETTERS = px(20);

/**
 * The grid's look. Cells inherit the box's font and ink, and their lines are as heavy as the
 * finished table's. The rest uses the app's theme tokens (main.css), so it follows the light,
 * dark and ePaper themes; ePaper's black-and-white overrides are in main.css.
 */
export const sheetTheme = EditorView.theme({
  ".cm-sheet": { position: "relative", textAlign: "left", whiteSpace: "normal", outline: "none", cursor: "cell", userSelect: "none", WebkitUserSelect: "none", textIndent: "0" },
  ".cm-sheet-scroll": { overflowX: "auto", overflowY: "hidden", marginLeft: `calc(-1 * ${GUTTER})`, marginTop: `calc(-1 * ${LETTERS})` },
  ".cm-sheet-table": { borderCollapse: "collapse", font: "inherit", color: "inherit", lineHeight: "inherit", maxWidth: "100%" },

  // cells: the note's own look
  ".cm-sheet-cell": { border: "0.06em solid currentColor", padding: "0 0.4em", minWidth: "2.5em", whiteSpace: "pre-wrap", overflowWrap: "anywhere", verticalAlign: "top", font: "inherit", position: "relative" },
  // room on the right for the sort arrow and menu button
  "th.cm-sheet-cell": { fontWeight: "bold", paddingRight: "2em" },
  ".cm-sheet-cell.is-bold": { fontWeight: "bold" },
  ".cm-sheet-cell.is-italic": { fontStyle: "italic" },
  ".cm-sheet-cell.is-strike": { textDecoration: "line-through" },
  ".cm-sheet-cell.in-range": { background: `color-mix(in srgb, ${BRAND} 10%, transparent)` },
  ".cm-sheet-cell.is-active": { outline: `${px(2)} solid ${BRAND}`, outlineOffset: px(-1), background: `color-mix(in srgb, ${BRAND} 6%, transparent)` },
  ".cm-sheet-cell.is-error": { color: "var(--pr-danger)" },
  ".cm-sheet-cell a": { pointerEvents: "none", color: "inherit" },
  ".cm-sheet-cell code": { fontFamily: "PenroamMono", fontSize: "0.9em" },
  ".cm-sheet-check": { display: "inline-flex", alignItems: "center", justifyContent: "center", boxSizing: "border-box", width: "0.68em", height: "0.68em", border: "0.06em solid currentColor", lineHeight: "1", verticalAlign: "baseline", cursor: "pointer" },
  ".cm-sheet-input": { font: "inherit", color: "inherit", background: "transparent", border: "none", outline: "none", padding: "0", margin: "0", width: "100%", minWidth: "0", textAlign: "inherit" },

  // the hand-drawn sort arrow and menu button in each header cell, sized with the text
  ".cm-sheet-hctl": { position: "absolute", right: "0.1em", top: "50%", transform: "translateY(-50%)", display: "flex", opacity: "0.45", transition: "opacity 120ms" },
  ".cm-sheet.is-focused .cm-sheet-hctl, .cm-sheet-hctl:hover": { opacity: "1" },
  ".cm-sheet-hbtn": { display: "inline-flex", alignItems: "center", justifyContent: "center", width: "0.95em", height: "1.15em", padding: "0", margin: "0", border: "none", borderRadius: "0.25em", background: "transparent", color: "inherit", cursor: "pointer", font: "inherit" },
  ".cm-sheet-hbtn:hover": { background: "var(--pr-surface-2)" },
  ".cm-sheet-hbtn.is-on": { color: BRAND },
  ".cm-sheet-hbtn .cm-sheet-icon": { width: "0.9em", height: "0.9em" },
  ".cm-sheet-hand": { strokeWidth: "1.7" },

  // icons: Lucide, drawn as the app toolbar draws them
  ".cm-sheet-icon": { width: px(18), height: px(18), flex: "none", fill: "none", stroke: "currentColor", strokeWidth: "2", strokeLinecap: "round", strokeLinejoin: "round", display: "block", pointerEvents: "none" },

  // column letters and row numbers, in the margin
  ".cm-sheet-col, .cm-sheet-row, .cm-sheet-corner": { ...ui(11), opacity: "0", padding: `0 ${px(2)}`, border: "none", textAlign: "center", verticalAlign: "middle", cursor: "pointer", color: "var(--pr-muted)", borderRadius: px(6), transition: "opacity 120ms, background-color 120ms" },
  ".cm-sheet-col": { height: LETTERS },
  ".cm-sheet-row, .cm-sheet-corner": { width: GUTTER, minWidth: GUTTER, maxWidth: GUTTER, boxSizing: "border-box" },
  ".cm-sheet-col:hover, .cm-sheet-row:hover": { background: "var(--pr-surface-2)", color: "var(--pr-ink)" },
  ".cm-sheet-col.is-current, .cm-sheet-row.is-current": { ...SELECTED, boxShadow: "none", fontWeight: "600" },
  ".cm-sheet.is-focused .cm-sheet-col, .cm-sheet.is-focused .cm-sheet-row": { opacity: "1" },

  // filter row
  ".cm-sheet-filter": { padding: `${px(2)}`, border: "0.06em solid currentColor" },
  ".cm-sheet-filter-input": { ...ui(12), width: "100%", minWidth: "3em", boxSizing: "border-box", border: "1px solid var(--pr-line)", borderRadius: px(8), background: "var(--pr-surface)", color: "var(--pr-ink)", outline: "none", padding: `${px(3)} ${px(6)}` },
  ".cm-sheet-filter-input:focus": { borderColor: BRAND, boxShadow: `0 0 0 1px ${BRAND}` },
  ".cm-sheet-filter-input::placeholder": { color: "var(--pr-muted)" },

  // the bar: a floating panel like the app's popovers
  ".cm-sheet-bar": {
    ...ui(13),
    position: "absolute",
    bottom: `calc(100% + ${LETTERS} + ${px(4)})`,
    // from the grid's left edge: notes start on the left, so the bar stays on screen
    left: `calc(-1 * ${GUTTER})`,
    display: "none",
    flexDirection: "column",
    gap: px(4),
    width: "max-content",
    maxWidth: px(760),
    padding: px(6),
    background: "var(--pr-surface)",
    color: "var(--pr-ink)",
    border: "1px solid var(--pr-line)",
    borderRadius: px(12),
    boxShadow: "var(--pr-shadow)",
    zIndex: "5",
    whiteSpace: "nowrap",
    cursor: "default",
  },
  ".cm-sheet.is-focused .cm-sheet-bar": { display: "flex" },
  ".cm-sheet-formula": { display: "flex", alignItems: "center", gap: px(8), minWidth: "0", padding: `0 ${px(2)}` },
  ".cm-sheet-ref": { ...ui(12), fontWeight: "600", color: "var(--pr-muted)", background: "var(--pr-surface-2)", borderRadius: px(6), padding: `${px(1)} ${px(8)}`, minWidth: px(28), textAlign: "center" },
  ".cm-sheet-src": { flex: "1 1 auto", minWidth: "0", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "pre", fontFamily: "PenroamMono", fontSize: px(12), color: "var(--pr-ink)" },
  ".cm-sheet-tools": { display: "flex", flexWrap: "wrap", alignItems: "center", gap: px(2) },
  ".cm-sheet-gap": { width: "1px", height: px(24), margin: `0 ${px(4)}`, background: "var(--pr-line)", flex: "none" },
  ".cm-sheet-tool": { display: "inline-flex", alignItems: "center", justifyContent: "center", minWidth: px(34), height: px(34), padding: `0 ${px(6)}`, border: "none", borderRadius: px(12), background: "transparent", color: "var(--pr-ink)", cursor: "pointer", font: "inherit", transition: "background-color 120ms, color 120ms" },
  ".cm-sheet-tool:hover": { background: "var(--pr-surface-2)" },
  ".cm-sheet-tool.is-on": SELECTED,
  ".cm-sheet-glyph": { ...ui(12), fontWeight: "600", pointerEvents: "none" },

  // filter status under the grid
  ".cm-sheet-status": { ...ui(12), color: "var(--pr-muted)", marginTop: px(4) },
  ".cm-sheet-status:empty": { display: "none" },
  ".cm-sheet-link": { font: "inherit", color: BRAND, background: "transparent", border: "none", padding: "0", cursor: "pointer", fontWeight: "600" },
  ".cm-sheet-link:hover": { textDecoration: "underline" },

  // menus: the app's dropdown (ui/Menu.vue and ui/MenuItem.vue)
  ".cm-sheet-menu": {
    ...ui(14),
    position: "absolute",
    zIndex: "6",
    display: "flex",
    flexDirection: "column",
    minWidth: px(224),
    marginTop: px(6),
    padding: px(6),
    background: "var(--pr-surface)",
    color: "var(--pr-ink)",
    border: "1px solid var(--pr-line)",
    borderRadius: px(12),
    boxShadow: "var(--pr-shadow)",
    cursor: "default",
  },
  ".cm-sheet-menu-item": { display: "flex", alignItems: "center", gap: px(12), width: "100%", minHeight: px(40), padding: `0 ${px(12)}`, border: "none", borderRadius: px(8), background: "transparent", color: "var(--pr-ink)", font: "inherit", textAlign: "left", whiteSpace: "nowrap", cursor: "pointer" },
  ".cm-sheet-menu-item:hover, .cm-sheet-menu-item:focus": { background: "var(--pr-surface-2)", outline: "none" },
  ".cm-sheet-menu-item:focus-visible": { outline: `2px solid ${BRAND}`, outlineOffset: "-2px" },
  ".cm-sheet-menu-item.is-danger": { color: "var(--pr-danger)" },
  ".cm-sheet-menu-item .cm-sheet-icon": { color: "var(--pr-muted)" },
  ".cm-sheet-menu-item.is-danger .cm-sheet-icon": { color: "inherit" },
  ".cm-sheet-menu-rule": { height: "1px", margin: `${px(4)} ${px(6)}`, background: "var(--pr-line)" },
});
