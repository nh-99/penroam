import { describe, expect, it } from "vitest";
import {
  compareCells,
  deleteColumn,
  deleteRow,
  fillDown,
  insertColumn,
  insertRows,
  mapRefs,
  matchesFilter,
  offsetFormula,
  parseClipboard,
  parseDate,
  parseTable,
  pasteBlock,
  printTable,
  sortBy,
  sortedDirection,
  splitRow,
  viewCells,
  visibleRows,
  type SheetTable,
} from "../src/editor/sheetTable";

const t = (src: string) => parseTable(src)!;
const body = (table: SheetTable) => table.cells.slice(1);

describe("reading and writing tables", () => {
  it("splits rows the way GFM does", () => {
    expect(splitRow("| a | b |")).toEqual(["a", "b"]);
    expect(splitRow("a|b")).toEqual(["a", "b"]);
    expect(splitRow("| a \\| b | `c` |")).toEqual(["a | b", "`c`"]);
    expect(splitRow("| \\*x\\* |  |")).toEqual(["\\*x\\*", ""]);
  });

  it("reads alignment and pads or cuts rows to the header", () => {
    const table = t("| a | b | c |\n|:--|:-:|--:|\n| 1 |\n| 1 | 2 | 3 | 4 |");
    expect(table.aligns).toEqual(["left", "center", "right"]);
    expect(body(table)).toEqual([["1", "", ""], ["1", "2", "3"]]);
    expect(parseTable("| a |\n| b |")).toBeNull();
  });

  it("prints lined-up GFM that reads back the same", () => {
    const table = t("|Item|Qty|\n|-|--:|\n|Apples|3|\n|a \\| b|=B2*2|");
    const out = printTable(table);
    expect(out).toBe("| Item   | Qty   |\n| ------ | ----: |\n| Apples |     3 |\n| a \\| b | =B2*2 |");
    expect(parseTable(out)).toEqual(table);
  });

  it("keeps a cell on one line", () => {
    expect(printTable({ aligns: ["left"], cells: [["h"], ["two\nlines"]] })).toContain("| two lines |");
  });
});

describe("references follow their cells", () => {
  it("rewrites references and leaves everything else alone", () => {
    expect(mapRefs("=A2+sum(B2:B4)*2", (r, c) => [r + 1, c])).toBe("=A3+sum(B3:B5)*2");
    expect(mapRefs("plain A2", (r, c) => [r + 1, c])).toBe("plain A2");
    // a formula still being written keeps pointing at the same cells
    expect(mapRefs("=A2+", (r, c) => [r + 1, c])).toBe("=A3+");
    expect(mapRefs("=A2 & x", (r, c) => [r + 1, c])).toBe("=A2 & x");
    expect(mapRefs("=#REF!+A2", (r, c) => [r + 1, c])).toBe("=#REF!+A3");
  });

  it("moves with inserted and deleted rows", () => {
    const table = t("| n | f |\n|---|---|\n| 1 | =A2 |\n| 2 | =SUM(A2:A3) |");
    expect(body(insertRows(table, 2))).toEqual([["1", "=A2"], ["", ""], ["2", "=SUM(A2:A4)"]]);
    expect(body(deleteRow(table, 1))).toEqual([["2", "=SUM(A2:A2)"]]);
    expect(body(deleteRow(t("| n | f |\n|---|---|\n| 1 | x |\n| 2 | =A2*2 |"), 1))).toEqual([["2", "=#REF!*2"]]);
    expect(deleteRow(table, 0)).toBe(table);
  });

  it("moves with inserted and deleted columns", () => {
    const table = t("| a | b | c |\n|---|---|---|\n| 1 | 2 | =A2+B2 |");
    expect(insertColumn(table, 1).cells[1]).toEqual(["1", "", "2", "=A2+C2"]);
    expect(deleteColumn(table, 1).cells[1]).toEqual(["1", "=A2+#REF!"]);
    expect(deleteColumn({ aligns: ["left"], cells: [["a"]] }, 0).aligns).toEqual(["left"]);
  });

  it("fills down and pastes relative to where cells came from", () => {
    const table = t("| a | b |\n|---|---|\n| 1 | =A2*2 |\n| 2 |  |\n| 3 |  |");
    expect(body(fillDown(table, 1, 1, 3, 1)).map((r) => r[1])).toEqual(["=A2*2", "=A3*2", "=A4*2"]);
    expect(offsetFormula("=A2", -1, 0)).toBe("=A1");
    expect(offsetFormula("=A1", -1, 0)).toBe("=#REF!");
    const pasted = pasteBlock(table, 3, 1, [["=A2*2", "x"]], [1, 1]);
    expect(pasted.cells[3]).toEqual(["3", "=A4*2", "x"]);
    expect(pasted.aligns).toHaveLength(3);
    expect(pasteBlock(table, 4, 0, [["9"], ["10"]]).cells).toHaveLength(6);
  });
});

describe("sorting", () => {
  const sheet = t(
    "| Item | Price | Total |\n|---|--:|--:|\n| pear | $2 | =B2*2 |\n| Apple | $10 | =B3*2 |\n| fig |  | =B4*2 |\n| kiwi | $1.50 | =B5*2 |\n| Sum | =SUM(B2:B5) | =SUM(C2:C5) |",
  );

  it("orders numbers by value, keeps blanks last and the totals row in place", () => {
    const asc = sortBy(sheet, 1, false);
    expect(body(asc).map((r) => r[0])).toEqual(["kiwi", "pear", "Apple", "fig", "Sum"]);
    // each row's own references move with it
    expect(body(asc).map((r) => r[2])).toEqual(["=B2*2", "=B3*2", "=B4*2", "=B5*2", "=SUM(C2:C5)"]);
    expect(body(sortBy(sheet, 1, true)).map((r) => r[0])).toEqual(["Apple", "pear", "kiwi", "fig", "Sum"]);
  });

  it("orders text naturally, ignoring case", () => {
    expect(body(sortBy(sheet, 0, false)).map((r) => r[0])).toEqual(["Apple", "fig", "kiwi", "pear", "Sum"]);
    const files = t("| f |\n|---|\n| file10 |\n| file2 |\n| File1 |");
    expect(body(sortBy(files, 0, false)).map((r) => r[0])).toEqual(["File1", "file2", "file10"]);
  });

  it("sorts by formula results, dates and checkboxes", () => {
    const table = t("| d | done | n |\n|---|---|---|\n| 2026-03-01 | [x] | =2*5 |\n| 12/25/2025 | [ ] | =1+1 |\n| 2026-01-15 | [x] | 7 |");
    expect(body(sortBy(table, 0, false)).map((r) => r[0])).toEqual(["12/25/2025", "2026-01-15", "2026-03-01"]);
    expect(body(sortBy(table, 1, false)).map((r) => r[1])).toEqual(["[ ]", "[x]", "[x]"]);
    expect(body(sortBy(table, 2, false)).map((r) => r[2])).toEqual(["=1+1", "7", "=2*5"]);
  });

  it("knows which way a column is already sorted", () => {
    const views = viewCells(sortBy(sheet, 1, false));
    expect(sortedDirection(sortBy(sheet, 1, false), views, 1)).toBe("asc");
    expect(sortedDirection(sheet, viewCells(sheet), 1)).toBeNull();
  });

  it("puts numbers before text", () => {
    const [a, b] = viewCells(t("| a | b |\n|---|---|\n| 3 | x |"))[1];
    expect(compareCells(a, b)).toBeLessThan(0);
  });
});

describe("filters", () => {
  const sheet = t("| Item | Price | Due | Done |\n|---|---|---|---|\n| pear | $2 | 2026-01-10 | [x] |\n| Apple | $10 | 2026-03-01 | [ ] |\n| fig | 3 | soon | [x] |");
  const views = viewCells(sheet);
  const rows = (filters: string[]) => visibleRows(views, filters).map((r) => sheet.cells[r][0]);

  it("finds text in any case", () => {
    expect(rows(["P"])).toEqual(["pear", "Apple"]);
    expect(rows(["", "", "", ""])).toEqual(["pear", "Apple", "fig"]);
  });

  it("compares numbers and dates", () => {
    expect(rows(["", ">2.5"])).toEqual(["Apple", "fig"]);
    expect(rows(["", "<=$2"])).toEqual(["pear"]);
    expect(rows(["", "", ">=2026-02-01"])).toEqual(["Apple"]);
    expect(rows(["", "!=10"])).toEqual(["pear", "fig"]);
  });

  it("matches exact text and checkboxes", () => {
    expect(rows(["=FIG"])).toEqual(["fig"]);
    expect(rows(["<>fig"])).toEqual(["pear", "Apple"]);
    expect(rows(["", "", "", "yes"])).toEqual(["pear", "fig"]);
    expect(rows(["", "", "", "no"])).toEqual(["Apple"]);
    expect(matchesFilter(views[1][0], ">5")).toBe(false);
  });
});

describe("dates and the clipboard", () => {
  it("reads two date styles and rejects impossible dates", () => {
    expect(parseDate("2026-11-10")).toBe(parseDate("11/10/2026"));
    expect(parseDate("2026-02-30")).toBeNull();
    expect(parseDate("10 Nov")).toBeNull();
  });

  it("reads tab-separated rows", () => {
    expect(parseClipboard("a\tb\r\nc\td\n")).toEqual([["a", "b"], ["c", "d"]]);
  });
});
