// Workload D (M00 benchmarks/protocol.md) inside the real M01 app:
// generate scene -> persist through Rust (journal) -> load + render the page
// -> full redraw (GPU-flushed, M00 method) -> replay a stroke one sample per
// frame over the committed scene -> process-tree RSS. Started with
// PENROAM_AUTOBENCH=linux[,quick][,exit] (results via write_perf).
import sampleDoc from "../../../../tests/fixtures/sample-strokes.json";
import type { InkPoint } from "../generated/InkPoint";
import { api } from "../platform/ipc";
import { useLibrary } from "../stores/library";
import { benchHooks } from "./hooks";
import { generateScene, SCENE_HEIGHT, SCENE_WIDTH } from "./scene";

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const median = (a: number[]) => {
  const s = [...a].sort((x, y) => x - y);
  return s.length ? s[Math.floor(s.length / 2)] : null;
};
const pct = (a: number[], p: number) => {
  const s = [...a].sort((x, y) => x - y);
  return s.length ? s[Math.min(s.length - 1, Math.floor(s.length * p))] : null;
};

async function until<T>(f: () => T | null | undefined, ms = 60000): Promise<T> {
  const t0 = performance.now();
  for (;;) {
    const v = f();
    if (v) return v;
    if (performance.now() - t0 > ms) throw new Error("benchmark timed out");
    await sleep(20);
  }
}

/**
 * Steady state: a fresh process opens the largest page of the newest
 * "Benchmark" notebook (created by a previous run) — no import garbage.
 */
async function openLargest(mode: string, startupMs: number) {
  const lib = useLibrary();
  const nbs = (await api.listNotebooks()).filter((n) => n.title.startsWith("Benchmark")).sort((a, b) => b.createdAt - a.createdAt);
  if (!nbs.length) throw new Error("no benchmark notebook: run PENROAM_AUTOBENCH=<name> first");
  const nb = await api.getNotebook(nbs[0].id);
  const pageId = nb.pageOrder[nb.pageOrder.length - 1];
  const editor = await until(() => benchHooks.editor ?? (lib.current ? null : (void lib.open(nb.id, pageId), null)));
  const t0 = performance.now();
  await until(() => editor.session?.page.id === pageId && editor.session.page.elements.length > 0);
  const openMs = performance.now() - t0;
  const n = editor.session!.page.elements.length;
  editor.benchView(1);
  const full = [editor.fullRedraw(), editor.fullRedraw(), editor.fullRedraw()];
  const visible = [editor.visibleRedraw(), editor.visibleRedraw(), editor.visibleRedraw()];
  await editor.renderer.whenComplete();
  await sleep(2000);
  const proc = await api.procStats().catch(() => null);
  const out = {
    app: "penroam-m01", workload: "D-steady-state-open", exportedAt: new Date().toISOString(), startupMs, strokes: n, openMs,
    fullRedrawMs: full, visibleRedrawMs: visible, treeRssBytes: proc?.treeRssBytes ?? null, mainRssBytes: proc?.rssBytes ?? null,
    processes: (proc as any)?.breakdown ?? null, devicePixelRatio: window.devicePixelRatio, system: await api.systemInfo().catch(() => null),
  };
  await api.writePerf(`m01-steady-open-${mode.split(",")[0]}`, JSON.stringify(out, null, 2));
  if (mode.includes("exit")) await api.exitApp();
}

export async function runAutobench(mode: string) {
  const startupMs = await api.startupMs();
  if (mode.includes("open")) return openLargest(mode, startupMs);
  const lib = useLibrary();
  const quick = mode.includes("quick");
  const sizes = quick ? [1000] : [1000, 5000, 10000];
  const trials = quick ? 1 : 3;
  const nb = await lib.createNotebook(`Benchmark ${new Date().toISOString()}`, "letter", { kind: "blank", spacing: 24 });
  if (!nb) throw new Error("could not create benchmark notebook");
  const curve = (sampleDoc as any).strokes.find((s: any) => s.id === "sample-curve").points as InkPoint[];
  const replayPts = curve.map((p) => ({ ...p, x: p.x + 200, y: p.y + 300 }));
  const results: Record<string, unknown>[] = [];
  const editor = await until(() => benchHooks.editor);
  for (const n of sizes) {
    for (let trial = 0; trial < trials; trial++) {
      await sleep(100);
      const { notebook, page } = await api.createPage(nb.id, undefined, { kind: "blank", spacing: 24 }, "custom", { width: SCENE_WIDTH, height: SCENE_HEIGHT });
      const t0 = performance.now();
      const doc = generateScene(n);
      const generateMs = performance.now() - t0;
      const json = JSON.stringify(doc);
      const t1 = performance.now();
      await api.importM00Strokes(nb.id, page.id, json);
      const persistMs = performance.now() - t1;
      const t2 = performance.now();
      lib.current = notebook;
      lib.currentPageId = page.id;
      await until(() => editor.session?.page.id === page.id && editor.session.page.elements.length === n);
      const loadAndRenderMs = performance.now() - t2;
      // M00 comparability: zoom 1.0 at the display's devicePixelRatio
      editor.benchView(1);
      const redraws = [editor.fullRedraw(), editor.fullRedraw(), editor.fullRedraw()];
      const visible = [editor.visibleRedraw(), editor.visibleRedraw(), editor.visibleRedraw()];
      await editor.renderer.whenComplete();
      const vr = editor.visibleDocRect();
      const frames = await editor.replay(replayPts);
      const iv = editor.lastReplayIntervals;
      // pen-up -> durable on disk for one stroke on this page (journal + fsync)
      const tc = performance.now();
      await editor.session!.flush();
      const commitDurableMs = performance.now() - tc;
      await sleep(50);
      const proc = await api.procStats().catch(() => null);
      results.push({
        strokes: n,
        trial,
        generateMs,
        persistMs,
        loadAndRenderMs,
        fullRedrawMs: redraws,
        fullRedrawMedianMs: median(redraws),
        visibleRedrawMs: visible,
        visibleRedrawMedianMs: median(visible),
        visibleDocRect: vr.map((v) => Math.round(v)),
        replayFrameMedianMs: median(frames),
        replayFrameMaxMs: Math.max(...frames),
        replayFrames: frames.length,
        commitDurableMs,
        replayIntervalMedianMs: median(iv),
        replayIntervalP95Ms: pct(iv, 0.95),
        replayIntervalMaxMs: Math.max(...iv),
        jsHeapBytes: (performance as any).memory?.usedJSHeapSize ?? null,
        treeRssBytes: proc?.treeRssBytes ?? null,
        mainRssBytes: proc?.rssBytes ?? null,
        processes: (proc as any)?.breakdown ?? null,
        backingPx: { w: editor.renderer.ink.width, h: editor.renderer.ink.height },
      });
    }
  }
  const out = {
    app: "penroam-m01",
    workload: "D-large-document",
    exportedAt: new Date().toISOString(),
    startupMs,
    system: await api.systemInfo().catch(() => null),
    devicePixelRatio: window.devicePixelRatio,
    viewport: { w: window.innerWidth, h: window.innerHeight },
    userAgent: navigator.userAgent,
    method:
      "Same as M00 Workload D: deterministic scene (seed = N), full redraw timed with a getImageData GPU flush, one replayed sample per rAF. M01 differences: the whole 1600x1200 pt page is rendered into a page-sized cache (M00 culled to the 1280x900 viewport), and the scene is persisted through Rust first.",
    results,
  };
  const where = await api.writePerf(`m01-workload-d-${mode.split(",")[0]}`, JSON.stringify(out, null, 2));
  console.info("benchmark written", where);
  if (mode.includes("exit")) await api.exitApp();
  return out;
}
