import { fixture } from "./fixture";
import { describe, expect, it } from "vitest";
import { checkbox, columnIndex, columnName, evaluate, formatNumber, MAX_RANGE_CELLS, parseNumber, parseValue, plainFormat } from "../src/editor/sheet";

const golden: { cells: string[][]; results: (string | null)[][] }[] = JSON.parse(fixture("sheet-golden.json"));

describe("spreadsheet formulas (same golden as penroam-core)", () => {
  for (const c of golden) {
    it(JSON.stringify(c.cells).slice(0, 80), () => {
      expect(evaluate(c.cells)).toEqual(c.results);
    });
  }
});

// The rules themselves are tested in crates/penroam-core/tests/sheet.rs; these cover what the
// golden file can't hold, and the helpers only this port has.
describe("spreadsheet values", () => {
  it("reads numbers as written", () => {
    expect(parseNumber("$1,234.5")).toEqual({ n: 1234.5, fmt: { currency: "$", suffix: false, spaced: false, percent: false, grouped: true, decimals: 1 } });
    expect(parseNumber("12%")?.n).toBeCloseTo(0.12);
    expect(parseNumber("5 €")?.fmt).toMatchObject({ currency: "€", suffix: true, spaced: true });
    expect(parseNumber("($3.00)")?.n).toBe(-3);
    for (const bad of ["", "-", "1,23", "1.", "$5%", "5 apples", "1e5", "(-5)", "USD 5", "$5$"]) expect(parseNumber(bad)).toBeNull();
  });

  it("knows checkboxes and booleans", () => {
    expect(checkbox(" [X] ")).toBe(true);
    expect(checkbox("[x] done")).toBeNull();
    expect(parseValue("True")).toEqual({ kind: "bool", b: true });
    expect(parseValue("pear")).toEqual({ kind: "text", text: "pear" });
  });

  it("prints numbers without the platform's float formatting", () => {
    expect(formatNumber(1 / 3, plainFormat())).toBe("0.333333");
    expect(formatNumber(1e12, plainFormat())).toBe("1000000000000");
    expect(formatNumber(1e16, plainFormat())).toBe("#NUM!");
    expect(formatNumber(-1234.5, { currency: "$", suffix: false, spaced: false, percent: false, grouped: true, decimals: 0 })).toBe("-$1,234.50");
  });

  it("names columns both ways", () => {
    expect(["A", "Z", "AA", "AZ", "BA", "ZZ", "AAA"].map(columnIndex)).toEqual([0, 25, 26, 51, 52, 701, 702]);
    for (const c of [0, 25, 26, 51, 701, 702]) expect(columnIndex(columnName(c))).toBe(c);
  });

  it("evaluates long chains and bounds range work", () => {
    const chain = [["n"], ["1"], ...Array.from({ length: 19_998 }, (_, i) => [`=A${i + 2}+1`])];
    expect(evaluate(chain).at(-1)![0]).toBe("19999");
    const side = 500;
    const rows = Array.from({ length: side }, (_, r) => ["1", r === 0 ? "1" : `=SUM(A1:A${side})+COUNT(A1:A${side})`]);
    const out = evaluate(rows);
    const fit = Math.floor(MAX_RANGE_CELLS / (2 * side));
    expect(out[fit][1]).toBe("1000");
    expect(out[fit + 1][1]).toBe("#LIMIT!");
  });
});
