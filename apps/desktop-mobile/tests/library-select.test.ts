// Choosing several notebooks on the library screen and trashing or filing them together.
import { beforeEach, describe, expect, it, vi } from "vitest";
import { flushPromises, mount } from "@vue/test-utils";
import { createPinia, setActivePinia } from "pinia";
import { api } from "../src/platform/ipc";

const nb = (id: string, title: string, extra: Record<string, unknown> = {}) =>
  ({ id, title, pageCount: 1, cover: "#123456", updatedAt: 0, createdAt: 0, ...extra }) as any;

const buttonLabelled = (label: string) => document.body.querySelector<HTMLButtonElement>(`button[aria-label="${label}"]`)!;
const buttonText = (text: string) => Array.from(document.body.querySelectorAll("button")).find((b) => b.textContent?.trim() === text)!;

async function library(opts: { folders?: boolean; vault?: string } = {}) {
  const pinia = createPinia();
  setActivePinia(pinia);
  const { useLibrary } = await import("../src/stores/library");
  const { useSync } = await import("../src/stores/sync");
  const { useUi } = await import("../src/stores/ui");
  const lib = useLibrary();
  lib.loaded = true;
  lib.refresh = vi.fn(async () => {}) as any;
  lib.notebooks = [nb("a", "Alpha"), nb("b", "Beta"), nb("c", "Gamma")];
  if (opts.folders) lib.folders = [{ id: "f1", name: "Work" } as any];
  if (opts.vault) useSync().vaults = { supported: true, vaults: [], notebooks: { [opts.vault]: "v1" } };
  const ui = useUi();
  const toast = vi.spyOn(ui, "toast");
  const error = vi.spyOn(ui, "error");
  const LibraryView = (await import("../src/views/LibraryView.vue")).default;
  const w = mount(LibraryView, { props: { nativeKind: null }, global: { plugins: [pinia] }, attachTo: document.body });
  await flushPromises();
  return { w, lib, toast, error, ui };
}

beforeEach(() => {
  document.body.innerHTML = "";
  vi.restoreAllMocks();
});

describe("selecting notebooks in the library", () => {
  it("toggles cards instead of opening them and trashes the chosen ones with one undo", async () => {
    const del = vi.spyOn(api, "deleteNotebook").mockResolvedValue({} as any);
    const restore = vi.spyOn(api, "restoreNotebook").mockResolvedValue({} as any);
    const { w, lib, toast } = await library();
    const open = vi.spyOn(lib, "open");

    buttonLabelled("Select notebooks").click();
    await flushPromises();
    expect(document.body.textContent).toContain("0 selected");
    expect(buttonLabelled("Move selected to the trash").disabled).toBe(true);

    buttonLabelled("Select Alpha").click();
    buttonLabelled("Select Gamma").click();
    await flushPromises();
    expect(open).not.toHaveBeenCalled();
    expect(buttonLabelled("Select Alpha").getAttribute("aria-pressed")).toBe("true");
    expect(buttonLabelled("Select Beta").getAttribute("aria-pressed")).toBe("false");
    expect(document.body.textContent).toContain("2 selected");

    buttonLabelled("Move selected to the trash").click();
    await flushPromises();
    expect(del.mock.calls.map((c) => c[0])).toEqual(["a", "c"]);
    // back to the normal library
    expect(document.body.textContent).not.toContain("selected");
    expect(buttonLabelled("Select notebooks")).toBeTruthy();

    const [, text, o] = toast.mock.calls.at(-1)!;
    expect(text).toBe("2 notebooks moved to the trash.");
    o!.action!.run();
    await flushPromises();
    expect(restore.mock.calls.map((c) => c[0])).toEqual(["a", "c"]);
    w.unmount();
  });

  it("starts from a card's menu, selects all and stops with Escape", async () => {
    const { w, ui } = await library();
    buttonLabelled("More actions for Beta").click();
    await flushPromises();
    buttonText("Select").click();
    await flushPromises();
    expect(document.body.textContent).toContain("1 selected");
    // the card's menu went away with the click; it must not stay counted as open
    expect(ui.overlays).toBe(0);
    expect(buttonLabelled("Select Beta").getAttribute("aria-pressed")).toBe("true");

    buttonText("Select all").click();
    await flushPromises();
    expect(document.body.textContent).toContain("3 selected");
    buttonText("Select none").click();
    await flushPromises();
    expect(document.body.textContent).toContain("0 selected");

    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    await flushPromises();
    expect(buttonLabelled("Open Beta")).toBeTruthy();
    w.unmount();
  });

  it("files the chosen notebooks in a folder, leaving vault notebooks where they are", async () => {
    const file = vi.spyOn(api, "fileNotebook").mockResolvedValue({} as any);
    const { w, toast } = await library({ folders: true, vault: "b" });
    buttonLabelled("Select notebooks").click();
    await flushPromises();
    buttonText("Select all").click();
    await flushPromises();
    buttonLabelled("Move selected to a folder").click();
    await flushPromises();
    expect(document.body.textContent).toContain("Move 3 notebooks to a folder");
    const work = Array.from(document.body.querySelectorAll("label")).find((l) => l.textContent?.includes("Work"))!;
    work.querySelector("input")!.click();
    await flushPromises();
    buttonText("Move here").click();
    await flushPromises();
    expect(file.mock.calls).toEqual([["a", "f1"], ["c", "f1"]]);
    expect(toast.mock.calls.map((c) => c[1])).toContain("1 notebook is in a vault and stayed where it was.");
    w.unmount();
  });

  it("keeps going when one notebook fails and reports it", async () => {
    vi.spyOn(api, "deleteNotebook").mockImplementation(async (id: string) => {
      if (id === "b") throw new Error("disk full");
      return {} as any;
    });
    const { w, toast, error } = await library();
    buttonLabelled("Select notebooks").click();
    await flushPromises();
    buttonText("Select all").click();
    await flushPromises();
    buttonLabelled("Move selected to the trash").click();
    await flushPromises();
    expect(error.mock.calls[0][0]).toContain("disk full");
    expect(toast.mock.calls.map((c) => c[1])).toContain("2 notebooks moved to the trash.");
    w.unmount();
  });
});
