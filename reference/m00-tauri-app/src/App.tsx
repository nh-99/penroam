import { useCallback, useEffect, useRef, useState } from "react";
import { NativeInkBackend, NativeMode } from "./nativeInk";
import { InkEngine, Tool } from "./ink/engine";
import type { RenderStyle } from "./ink/geometry";
import { parseDocument, toDocument } from "./ink/model";
import { generateScene } from "./ink/scene";
import sampleDoc from "../../../tests/fixtures/sample-strokes.json";
import {
  hasHost, host, hostName, inTauri, isAndroid, isIOS, listStrokeFiles, loadStrokeFile,
  procStats, saveStrokes, startupMs, systemInfo, writePerf,
} from "./platform";

type RenderMode = "framework" | NativeMode;
const WIDTHS = [1.5, 3, 6];
const SCENES = [100, 1000, 5000, 10000];
const MODE_LABEL: Record<RenderMode, string> = {
  framework: "A · Framework (Canvas 2D)",
  BOOX_HARDWARE: "B · BOOX hardware preview",
  PENCILKIT: "N · Native PencilKit",
  NATIVE_SOFTWARE: "C · Native Android software",
};

export default function App() {
  const hostRef = useRef<HTMLDivElement>(null);
  const engineRef = useRef<InkEngine | null>(null);
  const nativeRef = useRef<NativeInkBackend | null>(null);
  const native = () => nativeRef.current;
  const [tool, setTool] = useState<Tool>("pen");
  const [width, setWidth] = useState(3);
  const [pressure, setPressure] = useState(true);
  const [style, setStyle] = useState<RenderStyle>("smooth");
  const [mode, setMode] = useState<RenderMode>("framework");
  const [debug, setDebug] = useState(true);
  const [predicted, setPredicted] = useState(false);
  const [fingerDraws, setFingerDraws] = useState(false);
  const [status, setStatus] = useState("ready");
  const [nativeStatus, setNativeStatus] = useState<any>(null);
  const [overlay, setOverlay] = useState("");
  const [files, setFiles] = useState<string[] | null>(null);

  const engine = () => engineRef.current!;

  // ---------------------------------------------------------- engine --
  useEffect(() => {
    const e = new InkEngine(hostRef.current!);
    engineRef.current = e;
    (window as any).__ink = e; // for manual inspection in devtools
    startupMs().then((ms) => {
      if (ms != null) {
        e.metrics.timing("startup_to_first_frame_ms", ms);
        e.metrics.info.startupMs = ms;
      }
    });
    systemInfo().then((s) => (e.metrics.info.system = s));
    // Automated large-scene run (M00_AUTOBENCH=1), used for Linux results.
    (async () => {
      if (hasHost() && (await host<boolean>("autobench_requested").catch(() => false))) {
        await runSceneBenchmark(e, setStatus);
        await host("exit_app").catch(() => {}); // Tauri: quits; iOS harness: logs completion
      }
    })();
    return () => e.dispose();
  }, []);

  useEffect(() => {
    engineRef.current?.setSettings({ tool, width, pressure, renderStyle: style, usePredicted: predicted, fingerDraws });
    native()?.setBrush(width, pressure, tool).catch(reportErr);
  }, [tool, width, pressure, style, predicted, fingerDraws, mode]);

  // Debug overlay refresh at 4 Hz (never per pointer sample).
  useEffect(() => {
    if (!debug) return;
    const id = setInterval(async () => {
      const m = engineRef.current?.metrics;
      if (!m) return;
      const f = (k: string) => {
        const s = m.get(k);
        return s?.median !== undefined ? `${s.median.toFixed(1)}/${s.p95!.toFixed(1)}` : "-";
      };
      const last = m.strokes[m.strokes.length - 1];
      const ps = await procStats().catch(() => null);
      const mem = ps?.available ? ` rss ${(ps.treeRssBytes / 1048576).toFixed(0)}MB (${ps.processes.length} procs)` : "";
      setOverlay(
        `strokes ${engineRef.current!.strokes.length} · events ${m.counters.pointer_event_received ?? 0} · samples ${m.counters.input_samples ?? 0} · over-budget frames ${m.counters.frames_over_budget ?? 0}${mem}\n` +
          `med/p95 ms  evt→handler ${f("event_to_handler_ms")}  evt→render ${f("event_to_render_ms")}  render ${f("preview_render_submitted_ms")}  frame ${f("frame_interval_ms")}  commit ${f("persistent_render_completed_ms")}\n` +
          (last ? `last stroke: ${last.device} n=${last.samples} coalesced=${last.coalescedSamples} dt med ${last.medianDtMs.toFixed(1)} max ${last.maxDtMs.toFixed(1)} gaps ${last.timeGaps} jumps ${last.spatialJumps}` : "") +
          `\nsample event: ${m.info.sampleEvent} · apis: ${Object.entries(engineRef.current!.apis).filter(([, v]) => v === false).map(([k]) => k).join(",") || "all"} missing`,
      );
    }, 250);
    return () => clearInterval(id);
  }, [debug]);

  const reportErr = useCallback((err: unknown) => {
    const msg = String((err as any)?.message ?? err);
    engineRef.current?.metrics.error(msg);
    setStatus(`error: ${msg}`);
  }, []);

  // ------------------------------------------------------ native mode --
  const canvasRect = () => {
    const r = hostRef.current!.getBoundingClientRect();
    return { x: r.left, y: r.top, width: r.width, height: r.height, dpr: window.devicePixelRatio };
  };

  const detachNative = async () => {
    await native()?.detach();
    nativeRef.current = null;
    engine().inputEnabled = true;
    setNativeStatus(null);
  };

  const switchMode = async (next: RenderMode) => {
    const e = engine();
    if (mode !== "framework") await detachNative();
    if (next === "framework") {
      setMode("framework");
      setStatus(`mode ${MODE_LABEL.framework}`);
      return;
    }
    try {
      e.inputEnabled = false;
      const nb = NativeInkBackend.forMode(next);
      nativeRef.current = nb;
      await nb.listen({
        onStrokes: (ss) => ss.forEach((s) => e.addCommittedStroke(s)),
        onErased: (ids) => e.removeStrokes(ids),
        onViewport: (p) => e.setTransform({ scale: p.scale, ox: p.offsetX, oy: p.offsetY }),
        onStatus: (p) => setNativeStatus(p),
      });
      const res = await nb.attach({
        rect: canvasRect(), mode: next, width, pressure, tool,
        transform: { scale: e.transform.scale, offsetX: e.transform.ox, offsetY: e.transform.oy },
        strokesJson: JSON.stringify(toDocument(e.strokes)),
      });
      setNativeStatus(res.status);
      e.metrics.info.nativeCapabilities = res.capabilities;
      setMode(res.mode as NativeMode);
      setStatus(res.mode === next ? `mode ${MODE_LABEL[next]}` : `requested ${MODE_LABEL[next]} — FELL BACK to ${MODE_LABEL[res.mode as NativeMode]}`);
    } catch (err) {
      reportErr(err);
      await detachNative();
      setMode("framework");
    }
  };

  // Keep the native overlay aligned with the canvas (resize/rotation).
  useEffect(() => {
    if (mode === "framework") return;
    const update = () => native()?.setBounds(canvasRect()).catch(reportErr);
    const ro = new ResizeObserver(update);
    ro.observe(hostRef.current!);
    window.addEventListener("resize", update);
    return () => {
      ro.disconnect();
      window.removeEventListener("resize", update);
    };
  }, [mode]);

  // Detach the native view if the page goes away.
  useEffect(() => () => void nativeRef.current?.detach(), []);

  // --------------------------------------------------------- actions --
  const syncNative = async () => {
    await native()?.loadStrokes(JSON.stringify(toDocument(engine().strokes)));
  };

  const loadDoc = async (text: string, label: string) => {
    const e = engine();
    const t0 = performance.now();
    const strokes = parseDocument(text);
    const parseMs = performance.now() - t0;
    const { renderMs } = e.setStrokes(strokes);
    e.metrics.timing("load_parse_ms", parseMs);
    e.metrics.timing("load_render_ms", renderMs);
    await syncNative();
    setStatus(`${label}: ${strokes.length} strokes, parse ${parseMs.toFixed(0)} ms, render ${renderMs.toFixed(0)} ms`);
  };

  const loadScene = async (n: number) => {
    const e = engine();
    const t0 = performance.now();
    const doc = generateScene(n);
    const genMs = performance.now() - t0;
    const { renderMs } = e.setStrokes(doc.strokes);
    e.metrics.timing(`scene_${n}_render_ms`, renderMs);
    await syncNative();
    setStatus(`scene ${n}: generate ${genMs.toFixed(0)} ms, render ${renderMs.toFixed(0)} ms`);
  };

  const exportPerf = async () => {
    const e = engine();
    const nativeMetrics = native() ? await native()!.getMetrics().catch((x) => ({ error: String(x) })) : null;
    const out = {
      app: inTauri() ? "tauri" : `tauri-frontend@${hostName()}`,
      mode,
      exportedAt: new Date().toISOString(),
      system: await systemInfo().catch(() => null),
      proc: await procStats().catch(() => null),
      metrics: e.metrics.toJSON(),
      native: nativeMetrics,
    };
    setStatus(`perf → ${await writePerf(`tauri-${mode.toLowerCase()}`, JSON.stringify(out, null, 2))}`);
  };

  const zoom = (f: number) => {
    const e = engine();
    const { width: w, height: h } = e.viewSize();
    const [ax, ay] = e.toDoc(w / 2, h / 2);
    const scale = f === 0 ? 1 : e.transform.scale * f;
    const t = f === 0 ? { scale: 1, ox: 0, oy: 0 } : { scale, ox: w / 2 - ax * scale, oy: h / 2 - ay * scale };
    e.setTransform(t);
    native()?.setTransform(t.scale, t.ox, t.oy).catch(reportErr);
  };

  const onAndroid = isAndroid();
  const btn = (label: string, onClick: () => void, active = false) => (
    <button className={active ? "on" : ""} onClick={onClick}>{label}</button>
  );

  return (
    <div className="app">
      <div className="toolbar">
        {btn("Pen", () => setTool("pen"), tool === "pen")}
        {btn("Eraser", () => setTool("eraser"), tool === "eraser")}
        {WIDTHS.map((w) => btn(`W${w}`, () => setWidth(w), width === w))}
        {btn(pressure ? "Pressure ON" : "Pressure OFF", () => setPressure(!pressure), pressure)}
        {btn("Clear", async () => { engine().clear(); await native()?.clear().catch(reportErr); })}
        <span className="sep" />
        {/* Mode buttons (same as the Flutter app): one tap on E Ink, and scriptable over adb. */}
        {(["framework", ...(onAndroid ? ["BOOX_HARDWARE", "NATIVE_SOFTWARE"] : []), ...(isIOS() && inTauri() ? ["PENCILKIT"] : [])] as RenderMode[])
          .map((m) => btn(MODE_LABEL[m], () => switchMode(m), mode === m))}
        <select value={style} onChange={(ev) => setStyle(ev.target.value as RenderStyle)} title="Stroke rendering">
          <option value="smooth">Smoothed{pressure ? " + pressure" : ""}</option>
          <option value="polyline">Raw polyline</option>
        </select>
        <span className="sep" />
        {btn("Save", async () => setStatus(`saved ${await saveStrokes(JSON.stringify(toDocument(engine().strokes, { app: "tauri" })))}`))}
        {btn("Load…", async () => setFiles(await listStrokeFiles()))}
        {btn("Sample", () => loadDoc(JSON.stringify(sampleDoc), "sample"))}
        {SCENES.map((n) => btn(`Scene ${n >= 1000 ? `${n / 1000}k` : n}`, () => loadScene(n)))}
        {btn("Bench D", () => runSceneBenchmark(engine(), setStatus))}
        {btn("Bench B", () => native() ? runNativeBench(native()!, canvasRect(), engine(), setStatus).catch(reportErr) : setStatus("Bench B needs mode B or C"))}
        {btn("Export perf", exportPerf)}
        {btn("Reset perf", () => engine().metrics.reset())}
        <span className="sep" />
        {btn("Zoom −", () => zoom(1 / 1.25))}
        {btn("1:1", () => zoom(0))}
        {btn("Zoom +", () => zoom(1.25))}
        {btn("Debug", () => setDebug(!debug), debug)}
        {btn("Predicted", () => setPredicted(!predicted), predicted)}
        {btn("Finger draws", () => setFingerDraws(!fingerDraws), fingerDraws)}
      </div>
      <div className="status">
        <b>MODE: {MODE_LABEL[mode]}</b> · {status}
        {nativeStatus && (
          <span> · native: {nativeStatus.mode} raw={nativeStatus.sessionState} coords={nativeStatus.rawCoordinateSpace}
            {nativeStatus.lastError && nativeStatus.lastError !== null ? ` · ERROR ${nativeStatus.lastError}` : ""}</span>
        )}
        {isIOS() && <span> · iPadOS WKWebView ({hostName()})</span>}
      </div>
      {debug && <pre className="debug">{overlay}</pre>}
      {files && (
        <div className="files">
          {files.length === 0 && <span>no saved files</span>}
          {files.map((f) => btn(f, async () => { setFiles(null); await loadDoc(await loadStrokeFile(f), f); }))}
          {btn("Cancel", () => setFiles(null))}
        </div>
      )}
      <div className="canvas-host" ref={hostRef} />
    </div>
  );
}

/** Workload D: generate, render, replay a stroke, sample memory — per scene size. */
async function runSceneBenchmark(e: InkEngine, setStatus: (s: string) => void) {
  const results: any[] = [];
  const replayPts = parseDocument(JSON.stringify(sampleDoc)).find((s) => s.id === "sample-curve")!.points;
  for (const n of SCENES) {
    for (let trial = 0; trial < 3; trial++) {
      setStatus(`bench: scene ${n} trial ${trial + 1}/3`);
      await new Promise((r) => setTimeout(r, 50));
      const t0 = performance.now();
      const doc = generateScene(n);
      const genMs = performance.now() - t0;
      const { renderMs } = e.setStrokes(doc.strokes);
      const rebuildMs = e.rebuildNow();
      const frames = await e.replay(replayPts.map((p) => ({ ...p, x: p.x + 200, y: p.y + 300 })));
      const sorted = [...frames].sort((a, b) => a - b);
      const iv = [...e.lastReplayIntervals].sort((a, b) => a - b);
      const proc = await procStats().catch(() => null);
      results.push({
        strokes: n, trial, generateMs: genMs, initialRenderMs: renderMs, rebuildMs,
        replayFrameMedianMs: sorted[Math.floor(sorted.length / 2)], replayFrameMaxMs: sorted[sorted.length - 1],
        replayFrames: frames.length,
        replayIntervalMedianMs: iv[Math.floor(iv.length / 2)] ?? null, replayIntervalMaxMs: iv[iv.length - 1] ?? null,
        jsHeapBytes: (performance as any).memory?.usedJSHeapSize ?? null,
        treeRssBytes: proc?.treeRssBytes ?? null, mainRssBytes: proc?.rssBytes ?? null,
      });
    }
  }
  e.clear();
  const out = {
    app: inTauri() ? "tauri" : `tauri-frontend@${hostName()}`, workload: "D-large-document", exportedAt: new Date().toISOString(),
    system: await systemInfo().catch(() => null), viewport: e.viewSize(), apis: e.apis, results,
    metrics: e.metrics.toJSON(),
  };
  const where = await writePerf("tauri-workload-d", JSON.stringify(out, null, 2));
  setStatus(`bench done → ${where}`);
  return out;
}

/**
 * Workload D-native ("Bench B"): large documents through the native BOOX view
 * (Mode B or C). Times what differs between hosts: serializing the document,
 * the plugin IPC round trip (JS -> Rust -> Kotlin; native parse + bitmap
 * rebuild included), zoom / pan round trips (native rebuild), and an erase
 * whose result event crosses back over the bridge. The web canvas is not
 * involved. Same procedure as the Flutter app's runNativeBench.
 */
async function runNativeBench(nb: NativeInkBackend, rect: { width: number; height: number }, e: InkEngine, setStatus: (s: string) => void) {
  e.clear();
  const cx = rect.width / 2, cy = rect.height / 2;
  const results: any[] = [];
  for (const n of SCENES) {
    for (let trial = 0; trial < 3; trial++) {
      setStatus(`bench B: scene ${n} trial ${trial + 1}/3`);
      await new Promise((r) => setTimeout(r, 50));
      const strokes = generateScene(n).strokes;
      const t0 = performance.now();
      const json = JSON.stringify(toDocument(strokes));
      const serializeMs = performance.now() - t0;
      const t1 = performance.now();
      await nb.loadStrokes(json);
      const loadMs = performance.now() - t1;
      const t2 = performance.now();
      await nb.setTransform(1.25, cx - cx * 1.25, cy - cy * 1.25);
      const zoomInMs = performance.now() - t2;
      const t3 = performance.now();
      await nb.setTransform(1, 0, 0);
      const zoomOutMs = performance.now() - t3;
      const t4 = performance.now();
      await nb.setTransform(1, -120, -80);
      const panMs = performance.now() - t4;
      await nb.setTransform(1, 0, 0);
      const pts: number[] = [];
      for (let x = 100; x <= 300; x += 5) pts.push(x, 575);
      let resolveEvt!: (ms: number) => void;
      const evt = new Promise<number>((r) => (resolveEvt = r));
      const t5 = performance.now();
      nb.erasedHook = () => resolveEvt(performance.now() - t5);
      const { removed } = await nb.eraseAlong(pts);
      const eraseMs = performance.now() - t5;
      const eventMs = removed > 0 ? await Promise.race([evt, new Promise<number>((r) => setTimeout(() => r(-1), 5000))]) : null;
      nb.erasedHook = undefined;
      const proc = await procStats().catch(() => null);
      results.push({
        strokes: n, trial, jsonBytes: json.length, serializeMs, loadRoundTripMs: loadMs,
        zoomInMs, zoomOutMs, panMs, eraseRemoved: removed, eraseRoundTripMs: eraseMs, eraseEventMs: eventMs,
        rssBytes: proc?.rssBytes ?? null,
      });
    }
  }
  const out = {
    app: "tauri", workload: "D-native-boox", exportedAt: new Date().toISOString(),
    system: await systemInfo().catch(() => null), results, native: await nb.getMetrics().catch((x) => ({ error: String(x) })),
  };
  const where = await writePerf("tauri-bench-b", JSON.stringify(out, null, 2));
  setStatus(`bench B done → ${where}`);
}
