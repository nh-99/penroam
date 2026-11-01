# Linux testing

## Environment (recorded 2026-11-01)

| Item | Value |
|---|---|
| Distribution | Pop!_OS 24.04 LTS |
| Kernel | 7.2.6-76070206-generic |
| Desktop / session | GNOME (GNOME Shell 46.0), **Wayland** (`XDG_SESSION_TYPE=wayland`) |
| GPU / GL | AMD Radeon 890M (radeonsi, Mesa 26.1.6) as the active GL renderer; an NVIDIA dGPU is also present |
| GTK / WebKitGTK | GTK 3.24.41 / WebKitGTK 2.52.6 (`tauri::webview_version()` and pkg-config agree) |
| Tauri | tauri 2.12.5, @tauri-apps/cli 2.12.4, Rust 1.99.0, Node 24.12.0 |
| Flutter | **not installed** (blocker, see comparison.md) |
| Input devices | PIXA3854 touchpad + its mouse interface. **No pen digitizer.** Linux stylus support was not tested and is not inferred |
| Display | logical viewport 1280×733 CSS px at devicePixelRatio 2 (app window) |

## Automated large-scene runs (Workload D)

```bash
M00_AUTOBENCH=1 M00_RESULTS_DIR=$PWD/benchmarks/results/linux tauri_app/src-tauri/target/release/m00-tauri-app
M00_AUTOBENCH=1 M00_RESULTS_DIR=$PWD/benchmarks/results/linux flutter_app/build/linux/x64/release/bundle/m00_flutter_app
python3 benchmarks/analysis/summarize.py benchmarks/results/linux > benchmarks/analysis/linux-workload-d.md
```

For each of 100 / 1k / 5k / 10k strokes, three trials per session, the run:

1. generates the identical scene in-app (bit-identical to `fixtures/benchmark-scenes`, enforced by tests);
2. renders it;
3. forces a full redraw (Tauri: flushed with `getImageData`; Flutter: picture record + `endOfFrame`, with raster time from `FrameTiming`);
4. replays the 25-point `sample-curve` fixture stroke one sample per frame through the live preview + commit path;
5. samples memory. Tauri reports the process **tree** (the WebKitWebProcess, network and GPU children are real costs). Flutter reports the single process.

Tauri results so far: three sessions in `benchmarks/results/linux/`, summarized in
`benchmarks/analysis/linux-workload-d.md`.

## Manual checks (to do by a person at the machine; not automated)

| Check | Flutter | Tauri |
|---|---|---|
| Mouse drawing, smooth curves | not run (no SDK) | _pending manual pass_ |
| Window resize keeps strokes and alignment | | |
| Ctrl+wheel zoom / wheel pan / middle-drag pan | | |
| Clear, Save, Load, Sample, Scene buttons | | |
| Export perf writes JSON | | verified by autobench (writePerf) |
| Pointer API availability | n/a | WebKitGTK 2.52.6: `pointerrawupdate` **missing**; `getCoalescedEvents`, `getPredictedEvents`, `setPointerCapture`, `OffscreenCanvas` present (from `metrics.info.apis` in the run files) |

## Observations from the Tauri runs

* WebKitGTK `performance.now()` comes back at **1 ms resolution** in this configuration (all
  recorded timings are whole milliseconds). The sub-millisecond per-frame preview cost cannot
  be resolved; replay frame medians read as 0–1 ms. Use the frame-interval and full-redraw
  numbers instead.
* Frame interval median is 16 ms (p95 20–23 ms). The max of 133–145 ms per session coincides
  with the 10k-stroke full redraws.
* The process-tree RSS baseline is about 500 MB with an empty canvas. Most of it is WebKit's
  web and GPU processes.
