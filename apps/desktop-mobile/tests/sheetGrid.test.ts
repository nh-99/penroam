import { afterEach, describe, expect, it } from "vitest";
import { MarkdownEditor } from "../src/editor/markdownEditor";

const TABLE = "| Item | Qty |\n| ---- | --: |\n| pear |   2 |\n| fig  |  10 |";

let editor: MarkdownEditor | null = null;
let text = "";

function open(doc: string) {
  const host = document.createElement("div");
  document.body.append(host);
  text = doc;
  editor = new MarkdownEditor(host, { text: doc, fontSize: 16, onChange: (t) => (text = t), onFormats() {}, onResize() {} });
  return host;
}

afterEach(() => {
  editor?.destroy();
  editor = null;
  document.body.replaceChildren();
});

const grid = () => document.querySelector<HTMLElement>(".cm-sheet")!;
const cell = (r: number, c: number) => grid().querySelector<HTMLElement>(`.cm-sheet-cell[data-r="${r}"][data-c="${c}"]`)!;
const key = (target: HTMLElement, k: string, init: KeyboardEventInit = {}) =>
  target.dispatchEvent(new KeyboardEvent("keydown", { key: k, bubbles: true, cancelable: true, ...init }));
const press = (target: HTMLElement, init: PointerEventInit = {}) =>
  target.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, cancelable: true, button: 0, pointerType: "mouse", ...init }));
const input = () => grid().querySelector<HTMLInputElement>(".cm-sheet-input")!;
const tool = (label: string) => grid().querySelector<HTMLButtonElement>(`button[aria-label="${label}"]`)!;
/** Clicks a grid button as a pointer would: the press, then the click. */
const tap = (b: HTMLElement) => {
  press(b);
  b.click();
};
/** Focus is still in the box (the editor ends a text edit when it leaves). */
const inBox = () => !!document.activeElement && editor!.view.dom.contains(document.activeElement);
const shown = () => [...grid().querySelectorAll("tbody tr")].map((tr) => tr.querySelector<HTMLElement>('.cm-sheet-cell[data-c="0"]')!.textContent);

describe("the spreadsheet grid", () => {
  it("shows a table as a grid, with formula results and checkboxes", () => {
    open("Shopping\n\n| a | b |\n|---|---|\n| 3 | =A2*2 |\n| [x] |  |");
    expect(grid()).toBeTruthy();
    expect(cell(1, 1).textContent).toBe("6");
    expect(cell(1, 1).title).toBe("=A2*2");
    expect(cell(2, 0).querySelector(".cm-sheet-check")?.getAttribute("aria-checked")).toBe("true");
    // the text around it is still the editor's
    expect(editor!.view.contentDOM.textContent).toContain("Shopping");
  });

  it("opens as a grid even with the caret at the end of the table", () => {
    open(TABLE);
    expect(editor!.view.state.selection.main.head).toBe(TABLE.length);
    expect(grid()).toBeTruthy();
  });

  it("edits a cell by typing, and moves with Enter and Tab", () => {
    open(TABLE);
    press(cell(1, 1));
    key(grid(), "7");
    expect(input().value).toBe("7");
    key(input(), "Enter");
    expect(text).toContain("| pear |   7 |");
    expect(grid().querySelector(".is-active")).toBe(cell(2, 1));
    key(grid(), "F2");
    expect(input().value).toBe("10");
    input().value = "=B2+3";
    key(input(), "Tab");
    expect(text).toContain("| fig  | =B2+3 |");
    expect(cell(2, 1).textContent).toBe("10");
    // Tab past the last cell adds a row
    expect(grid().querySelector(".is-active")).toBe(cell(3, 0));
    expect(text.split("\n")).toHaveLength(5);
  });

  it("cancels an edit with Escape without ending the text box", () => {
    open(TABLE);
    press(cell(1, 0));
    key(grid(), "Enter");
    input().value = "plum";
    const outer = { ended: false };
    grid().parentElement!.addEventListener("keydown", () => (outer.ended = true));
    key(input(), "Escape");
    expect(outer.ended).toBe(false);
    expect(text).toBe(TABLE);
    expect(cell(1, 0).textContent).toBe("pear");
  });

  it("points at a cell to put its reference in a formula", () => {
    open(TABLE);
    press(cell(1, 0));
    key(grid(), "=");
    press(cell(2, 1));
    expect(input().value).toBe("=B3");
  });

  it("navigates with the arrow keys and clears with Delete", () => {
    open(TABLE);
    press(cell(1, 0));
    key(grid(), "ArrowRight");
    key(grid(), "ArrowDown", { shiftKey: true });
    expect(grid().querySelectorAll(".in-range")).toHaveLength(1);
    key(grid(), "Delete");
    expect(text).not.toMatch(/\b2\b|\b10\b/);
    editor!.view.dispatch({}); // nothing else changed
    key(grid(), "z", { ctrlKey: true });
    expect(text).toBe(TABLE);
  });

  it("sorts from the column menu, and marks the sorted column", () => {
    open(TABLE);
    press(grid().querySelector<HTMLElement>('.cm-sheet-col[data-col="1"]')!);
    const items = [...grid().querySelectorAll<HTMLButtonElement>(".cm-sheet-menu-item")];
    items.find((b) => b.textContent!.startsWith("Sort Z"))!.click();
    expect(shown()).toEqual(["fig", "pear"]);
    expect(text.indexOf("fig")).toBeLessThan(text.indexOf("pear"));
    expect(cell(0, 1).querySelector(".cm-sheet-hbtn.is-on")).toBeTruthy();
    expect(inBox()).toBe(true);
  });

  it("opens menus without taking focus, enters them with the arrow keys, and closes them with Escape", () => {
    open(TABLE);
    press(cell(1, 0));
    tap(cell(0, 1).querySelectorAll<HTMLElement>(".cm-sheet-hbtn")[1]);
    const menu = grid().querySelector<HTMLElement>(".cm-sheet-menu")!;
    expect(menu).toBeTruthy();
    expect(document.activeElement).toBe(grid());
    key(grid(), "ArrowDown");
    expect(document.activeElement?.textContent).toContain("Sort A");
    const ended = { yes: false };
    grid().parentElement!.addEventListener("keydown", () => (ended.yes = true));
    key(document.activeElement as HTMLElement, "Escape");
    expect(grid().querySelector(".cm-sheet-menu")).toBeNull();
    expect(ended.yes).toBe(false);
    expect(inBox()).toBe(true);
    // deleting is set apart, as in the app's menus
    tap(cell(0, 1).querySelectorAll<HTMLElement>(".cm-sheet-hbtn")[1]);
    expect(grid().querySelector(".cm-sheet-menu-item.is-danger")?.textContent).toContain("Delete column");
  });

  it("sorts from the arrow in a header cell, one way then the other", () => {
    open("| Item | Price |\n|---|---|\n| pear | $ 12 |\n| fig | 5 € |\n| kiwi | ($3.00) |");
    press(cell(1, 0));
    tap(cell(0, 1).querySelector<HTMLElement>(".cm-sheet-hbtn")!);
    expect(shown()).toEqual(["kiwi", "fig", "pear"]);
    tap(cell(0, 1).querySelector<HTMLElement>(".cm-sheet-hbtn")!);
    expect(shown()).toEqual(["pear", "fig", "kiwi"]);
    expect(inBox()).toBe(true);
  });

  it("formats the selected cells from the bar", () => {
    open(TABLE);
    press(cell(1, 1));
    press(cell(2, 1), { shiftKey: true });
    tap(tool("Currency"));
    [...grid().querySelectorAll<HTMLButtonElement>(".cm-sheet-menu-item")].find((b) => b.textContent!.includes("Euro"))!.click();
    expect(text).toContain("€2.00");
    expect(text).toContain("€10.00");
    expect(inBox()).toBe(true);
    tap(tool("Bold"));
    expect(text).toContain("**€2.00**");
    expect(tool("Bold").classList.contains("is-on")).toBe(true);
    // a bold number is still a number
    tap(cell(0, 1).querySelector<HTMLElement>(".cm-sheet-hbtn")!);
    expect(shown()).toEqual(["pear", "fig"]);
    tap(tool("Align column centre"));
    expect(text).toContain(":--");
  });

  it("takes bold and italic from the editor's own toolbar while it has focus", () => {
    open("intro\n\n" + TABLE);
    press(cell(1, 0));
    editor!.format("italic");
    expect(text).toContain("*pear*");
    expect(editor!.formats).toEqual(["italic"]);
    // headings don't apply to cells, and don't touch the text around the table
    editor!.format("h1");
    expect(text.startsWith("intro")).toBe(true);
  });

  it("filters rows without changing the note", () => {
    open(TABLE);
    press(cell(1, 0));
    tap(tool("Filter rows"));
    const f = grid().querySelector<HTMLInputElement>('[data-filter="1"]')!;
    f.value = ">5";
    f.dispatchEvent(new Event("input"));
    expect(shown()).toEqual(["fig"]);
    expect(grid().querySelector(".cm-sheet-status")!.textContent).toContain("Showing 1 of 2 rows");
    expect(text).toBe(TABLE);
  });

  it("ticks a checkbox with a click or Space", () => {
    open("| Done | Task |\n|---|---|\n| [ ] | ship |");
    press(cell(1, 0).querySelector<HTMLElement>(".cm-sheet-check")!);
    expect(text).toContain("| [x]  |");
    key(grid(), " ");
    expect(text).toContain("| [ ]  |");
  });

  it("pastes tab-separated cells, growing the table", () => {
    open(TABLE);
    press(cell(2, 1));
    const e = new Event("paste", { bubbles: true, cancelable: true }) as ClipboardEvent;
    Object.defineProperty(e, "clipboardData", { value: { getData: () => "11\t$3\n12\t$4" } });
    grid().dispatchEvent(e);
    const rows = text.split("\n");
    expect(rows).toHaveLength(5);
    expect(rows[0]).toContain("Column 3");
    expect(rows[4]).toMatch(/\|\s+12 \| \$4\s+\|/);
  });

  it("shows the source on request, and while typing a new table", () => {
    open(TABLE);
    press(cell(1, 0));
    tap(tool("Edit this table as Markdown"));
    expect(document.querySelector(".cm-sheet")).toBeNull();
    // the text has focus, so the box is still being edited
    expect(document.activeElement).toBe(editor!.view.contentDOM);
    expect(editor!.view.contentDOM.textContent).toContain("| Item | Qty |");

    editor!.destroy();
    open("");
    const v = editor!.view;
    v.dispatch({ changes: { from: 0, insert: "| a |\n|---|\n| 1 |" }, selection: { anchor: 17 }, userEvent: "input.type" });
    expect(document.querySelector(".cm-sheet")).toBeNull();
    // moving off the table turns it back into a grid
    v.dispatch({ changes: { from: 17, insert: "\n\nafter" }, userEvent: "input.type" });
    v.dispatch({ selection: { anchor: v.state.doc.length } });
    expect(document.querySelector(".cm-sheet")).toBeTruthy();
  });

  it("leaves tables in lists and quotes as source", () => {
    open("- | a |\n  |---|\n  | 1 |\n\n> | b |\n> |---|\n> | 2 |");
    expect(document.querySelector(".cm-sheet")).toBeNull();
  });
});
