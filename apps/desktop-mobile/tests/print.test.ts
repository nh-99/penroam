import { beforeEach, describe, expect, it, vi } from "vitest";
import { createPinia, setActivePinia } from "pinia";
import { useLibrary } from "../src/stores/library";
import { useUi } from "../src/stores/ui";

const printNotebook = vi.fn();
vi.mock("../src/platform/ipc", () => ({
  hasHost: () => false,
  CommandError: class extends Error {},
  api: { printNotebook: (...args: unknown[]) => printNotebook(...args) },
}));

const report = (warnings: string[] = []) => ({ pages: 2, strokes: 1, textBlocks: 0, replacedCharacters: 0, warnings });

describe("printing a notebook", () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    printNotebook.mockReset();
  });

  const shown = () => useUi().toasts.map((t) => t.text);

  it("saves pending edits before the PDF is built, then reports what was sent", async () => {
    const order: string[] = [];
    printNotebook.mockImplementation(async () => {
      order.push("print");
      return { status: "sent", report: report() };
    });
    await useLibrary().printNotebook("nb", "pg", async () => void order.push("flush"));
    expect(order).toEqual(["flush", "print"]);
    expect(printNotebook).toHaveBeenCalledWith("nb", "pg");
    expect(shown()).toEqual(["Sent 2 pages to the printer."]);
  });

  it("stays quiet when the dialog is cancelled, even if the export had warnings", async () => {
    printNotebook.mockResolvedValue({ status: "cancelled", report: report(["dropped an image"]) });
    await useLibrary().printNotebook("nb");
    expect(shown()).toEqual([]);
  });

  it("explains the PDF viewer route and passes on export warnings", async () => {
    printNotebook.mockResolvedValue({ status: "viewer", report: report(["dropped an image"]) });
    await useLibrary().printNotebook("nb");
    expect(shown()).toEqual(["Opened in your PDF viewer. Print from there.", "dropped an image"]);
  });

  it("shows nothing further once the system print UI has taken over", async () => {
    printNotebook.mockResolvedValue({ status: "systemDialog", report: report() });
    await useLibrary().printNotebook("nb");
    expect(shown()).toEqual([]);
  });

  it("ignores a second request while one is running", async () => {
    let finish!: (v: unknown) => void;
    printNotebook.mockReturnValue(new Promise((r) => (finish = r)));
    const lib = useLibrary();
    const first = lib.printNotebook("nb");
    await lib.printNotebook("nb");
    finish({ status: "cancelled", report: report() });
    await first;
    expect(printNotebook).toHaveBeenCalledTimes(1);
    expect(lib.busy).toBeNull();
  });

  it("reports a failure and clears the busy state", async () => {
    printNotebook.mockRejectedValue(new Error("no printer"));
    const lib = useLibrary();
    await lib.printNotebook("nb");
    expect(shown().join()).toContain("Printing failed");
    expect(lib.busy).toBeNull();
  });
});
