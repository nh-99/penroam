# M00 framework comparison — interim report (updated 2026-11-02)

**There is no framework decision in this report.** PLAN.md forbids choosing a winner on
expected performance, and most of the weighted criteria (BOOX 30%, iPad 25%) need device runs
that have not happened yet. This report states what was built, what was measured, what is
blocked, and exactly which tests will decide.

## Comparison matrix (actual results only)

| Device / Mode | Flutter | Tauri |
|---|---|---|
| Linux mouse drawing | built (release, Impeller); scripted replay holds 60 Hz; **manual mouse pass pending** | built and launched under Wayland; scripted replay holds 60 Hz; **manual mouse pass pending** |
| Linux stylus, if available | no digitizer on the test machine | no digitizer on the test machine |
| iPad Apple Pencil | blocked: needs macOS/Xcode | Tauri app blocked (macOS/Xcode). **Proxy** (Tauri frontend in WKWebView, `ios/M00iOS`) runs on the iPad: rAF capped at **60 Hz** on the 120 Hz display; Pencil workloads not yet run |
| BOOX standard drawing (Mode A) | not run | **run** (Note Air5 C): user felt "slight input delay"; event → render 36 ms median (38 Hz panel) |
| BOOX accelerated drawing (Mode B) | **works** (hybrid composition): instant ink, stays put after pen-up; Onyx callback p95 12 ms, max 66 ms | **works** (Kotlin overlay above WebView): instant ink, stays put after pen-up; Onyx callback p95 12 ms, max 30 ms |
| Large-document rendering (Linux, matched) | 10k: full redraw 476 ms, 483 MiB | 10k: full redraw 202 ms, 701 MiB (process tree). iPad WKWebView proxy: 10k full redraw 72 ms |
| Stroke persistence | JSON round-trip test passes (`flutter test`) | JSON round-trip test passes; Save/Load via Rust commands built; on-device not run |
| Native integration stability | not run | not run |

## What exists

| Deliverable (PLAN §16) | State |
|---|---|
| 1 Flutter prototype | Source complete (Mode A painter, Platform View for B/C, debug overlay, bench, save/load). **Linux release build ✔, 9 Dart tests ✔** (2026-11-02). Android Kotlin compiled against `io.flutter:flutter_embedding_release:1.0.0-89c39ed…` + boox-ink |
| 2 Tauri prototype | Complete; Linux binary and Android APK built; 9 TS tests + 2 Rust tests pass |
| 3 Shared BOOX module | `android/boox-ink`, built against the official SDK; 11 JVM tests pass |
| 4 Standalone native reference app | `android/boox-ink-demo`, APK built |
| 5 BOOX APKs | Native ✔, Tauri ✔ (arm64), Flutter ✔ (arm64/armv7/x86_64, Java minify off due to an AGP 9.1.0 R8 crash) |
| 6 iPadOS builds or instructions | Flutter/Tauri: instructions only (macOS needed). `ios/M00iOS` (native UIKit Pencil reference + Tauri-frontend WKWebView proxy) builds on Linux with xtool; 3 Swift tests pass |
| 7 Linux builds | Tauri ✔; Flutter ✔ |
| 8 Shared fixtures | ✔ deterministic generator, golden geometry, scenes; ports verified in TS and Kotlin |
| 9 Automated tests | TS 9 ✔, Kotlin 11 ✔, Rust 2 ✔, Swift 3 ✔, Dart 9 ✔ |
| 10 Performance logs | Linux Workload D, matched schema: Tauri ×3, Flutter ×3 sessions |
| 11 Device checklist / 12 BOOX SDK docs | `benchmarks/protocol.md` §6, `docs/boox-integration.md` |
| 13/14 Comparison and M01 recommendation | this document (interim) |

## Measured: BOOX Note Air5 C, Mode B (2026-11-02)

All three hosts (native, Flutter, Tauri) run the shared Onyx module with **hardware-preview ink that the user judged equally immediate, and ink that stays put after pen-up**. The framework hosts add tail latency to Onyx callbacks (p95 12 ms vs 1 ms native) but not to the visible ink. Details, numbers and the defects fixed during the run: `benchmarks/results/boox/run-2026-11-02.md`.

### BOOX large documents (2026-11-02)

Full details: `benchmarks/results/boox/large-documents-2026-11-02.md`.

* **Framework canvas (Mode A):**
  * Both hold 38 Hz while writing over 10k strokes.
  * A full redraw (zoom, pan, erase, load) takes **0.6 s in Tauri vs 6.5 s in Flutter**
    (Impeller) at 10k strokes; Skia is worse (12.4 s).
* **Native Onyx view (Mode B):** the hosts are tied.
  * Loading 10k strokes takes 3.8 s (Tauri) vs 4.1 s (Flutter): Dart JSON is slower, the
    Tauri bridge is slower, and they offset.
  * Zoom, pan and erase are native and identical.
* **Memory with 10k strokes:** Tauri 559 MB (app + WebView renderer) vs Flutter 458 MB.
* **Shared-module bug fixed:** erasing over 10k strokes took 38.8 s; now 1.24 s.

## Measured: Linux, Tauri vs Flutter, Workload D (first head-to-head, 2026-11-02)

Environment: Pop!_OS 24.04, kernel 7.2.6, GNOME on Wayland, Radeon 890M, 60 Hz display, @2×.
Tauri 2.12.5 / WebKitGTK 2.52.6, viewport 1280×733. Flutter 3.47.9 / Dart 3.13.7, release
build, Impeller, viewport 1280×720. 3 sessions per app, run alternately (T, F, T, F, T, F),
3 trials per scene size per session, so 9 trials per cell. Medians. Source:
`benchmarks/analysis/linux-workload-d.md`; raw files in `benchmarks/results/linux/`.

| strokes | full redraw ms, GPU-flushed (Tauri / Flutter) | replay frame interval median ms | replay frame interval max ms | memory MiB (Tauri process tree / Flutter process) |
|---:|---:|---:|---:|---:|
| 100 | **4** / 11 | 16 / 17 | 20 / 18 | 499 / **215** |
| 1,000 | **26** / 60 | 17 / 17 | 20 / 19 | 519 / **271** |
| 5,000 | **130** / 280 | 16 / 17 | 21 / 18 | 630 / **396** |
| 10,000 | **202** / 476 | 16 / 17 | 21 / 18 | 701 / **483** |

Startup (process start → first frame) per session: Tauri 2900 / 3200 / 440 ms; Flutter
220 / 220 / 240 ms.

What these numbers mean:

* **Drawing over a large document: a tie.** Both hold the 60 Hz display rate while a stroke
  is replayed one sample per frame over 10,000 committed strokes. Pen-to-ink latency on Linux
  has not been measured for either (manual pass pending).
* **Full redraw: Tauri is about 2.3× faster** at every size. A full redraw happens on zoom,
  pan, erase and load, not while writing. So at 10k strokes a zoom step costs about 0.2 s in
  Tauri versus 0.5 s in Flutter. Both apps use the same algorithm and the same caching
  strategy, so this measures Canvas 2D (WebKitGTK) against Impeller path stroking.
* **Memory: Flutter uses about 220–280 MiB less.** Tauri's figure includes the WebKit web,
  network and GPU helper processes (process-tree RSS); Flutter is one process.
* **Startup: Flutter is faster and consistent (0.22–0.24 s).** Tauri took about 3 s in two of
  three sessions and 0.44 s in one; the 2026-11-01 runs showed the same pattern. The cause is
  not yet known (candidates: WebKit helper process or GPU initialisation). It needs
  investigation before it counts against Tauri.

How the measurement was made fair (changes on 2026-11-02, before these runs):

* **Replay metric.** Previously Tauri timed only the JS draw call (0–1 ms) while Flutter timed
  until end of frame (≥ one vsync), which made Tauri look about 100× faster. Both now record
  the interval between successive replay frames.
* **Full redraw.** Both now time recording plus rasterization, ending with a 1-pixel readback
  that forces GPU completion: `getImageData(0,0,1,1)` in Tauri, `toImageSync` + `toByteData`
  of a dependent 1×1 image in Flutter.
* **Committed cache in Flutter.** The Flutter app replayed vector pictures every frame,
  relying on `isComplex` raster caching, which **Impeller does not implement**. With 10k
  strokes on screen that gave 120 ms frames (about 8 fps) during replay. Flutter now
  rasterizes committed strokes into a view-space `ui.Image`, the same strategy as Tauri's
  committed canvas. This is a real pitfall for any Flutter notes app on Impeller.
* The pre-fix files are kept in `benchmarks/results/linux/superseded/`.

Not yet comparable on Linux: interactive mouse drawing (Workloads A–C need a manual pass),
CPU usage, and pen-to-photon latency (needs a camera).

## Measured: iPad, Workload D (automated, no Pencil input)

Device: iPad Pro 11" 3rd gen (iPad13,4, M1), iPadOS 26.6.2, 120 Hz. The app was driven over USB
(`ios/M00iOS/tools/m00ctl.py`). 3 sessions × 3 trials per engine. Raw files are in
`benchmarks/results/ipad/`; the summary is in `benchmarks/analysis/ipad-workload-d.md`.

| strokes | Native UIKit ref: full redraw ms | WKWebView (Tauri frontend): full redraw ms |
|---:|---:|---:|
| 100 | 18 | 5 |
| 1,000 | 53 | 26 |
| 5,000 | 262 | 37 |
| 10,000 | 537 | 72 |

| | Native UIKit ref | WKWebView (Tauri frontend) |
|---|---|---|
| Frame interval median / p95 | **8.3 / 8.3 ms (120 Hz)** | **17 / 17–18 ms (60 Hz)** |
| Pointer APIs | coalesced + predicted touches (UIKit) | `getCoalescedEvents` ✔, `getPredictedEvents` ✔, `pointerrawupdate` ✘ |
| Memory | 23 → 39 MB footprint (100 → 10k strokes) | app process 15–18 MB; **the WebContent process is not measurable from the app** |

Findings and caveats:

* **WKWebView delivers `requestAnimationFrame` at 60 Hz on a 120 Hz iPad.** This matches
  WebKit's default "prefer 60 fps page rendering" behaviour. It halves the preview refresh rate of
  any Canvas 2D implementation, including Tauri's on iPadOS. Whether Tauri can opt out of this in
  its WKWebView configuration is an open question, and it is decision-relevant.
* The native reference rasterizes on the CPU with CoreGraphics. Its slower full redraw reflects
  this diagnostic's simple renderer, not a native ceiling; WebKit's canvas is GPU-backed. The
  WKWebView timing uses a `getImageData` flush, which may not include all GPU work.
* The large-document runs above use **scripted** strokes.

### First Apple Pencil session: native UIKit reference only (35 strokes, 2026-11-01)

Raw file: `benchmarks/results/ipad/perf-ios-native-uikit-1793588471155.json`.

| Metric | Result |
|---|---|
| Sample interval per stroke (median of medians; min) | 8 ms; 4 ms → the Pencil delivers 120–240 Hz via coalesced touches (1,736 of 2,358 samples came from coalescing) |
| Gaps / spatial jumps (all strokes) | 0 / 0; max dt within any stroke 8 ms |
| Estimated-force updates | 1,747 (force is refined after delivery for almost every sample) |
| Touch timestamp → handler | 4.6 ms median, 5.1 p95 |
| Touch timestamp → frame's scheduled present time | 12.0 ms median, 12.5 p95 (*scheduled* display time, not pen-to-photon) |
| Preview draw per update | 0.19 ms median, 1.4 p95 |
| Pen-up → committed ink on the next frame | 5.2 ms median |
| Frame interval | 8.34 ms (120 Hz) |
| Longest stroke | 1.4 s. **Workload E (15 s stroke) not yet done** |
| Palm-rejection counters | none fired. Either Workload F wasn't done or no palm contact reached the view |

**Visual defect found in this session (native reference, since fixed):** the live preview showed
missing segments, and the stroke vanished for a moment on pen-up before the finished ink
appeared. There were two causes:

* partial `setNeedsDisplay(in:)` redraws did not keep the rest of the layer's pixels;
* an implicit Core Animation cross-fade on the bare `CALayer` holding the finished bitmap.

The fix draws stable pieces into the persistent bitmap every frame, has the preview draw only the
tail, and disables layer actions. The sample and timing data above are unaffected: input
handling did not change. The visual result needs re-checking on the device.

### First Apple Pencil session: WKWebView running the Tauri frontend (21 strokes)

Raw file: `benchmarks/results/ipad/perf-ios-wkwebview-tauri-framework-1793588530976.json`.
WebKit reports times only to the nearest millisecond.

| Metric | Native UIKit ref | WKWebView (Tauri frontend) |
|---|---|---|
| Sample interval (median per stroke) | 4–8 ms | **4 ms (240 Hz)**, almost all via `getCoalescedEvents` (3,010 of 3,028) |
| Gaps / jumps | 0 / 0 | 0 / 0 (max dt 8 ms) |
| Event timestamp → handler | 4.6 ms | 5 ms (`event.timeStamp` and `performance.now()` verified to share a clock) |
| Event → preview drawn | 12.0 ms to the *scheduled present* | 13 ms to the end of the rAF draw; then **17 ms frames** |
| Preview cadence | 120 Hz | **60 Hz** |
| Pen-up → committed | 5.2 ms (to the next frame) | < 1 ms of canvas work (WebKit's 1 ms timer resolution) |

Reading so far:

* **Input completeness is equal.** WKWebView delivers the full 240 Hz Pencil stream through
  coalesced events, so the web path loses no samples.
* **The difference is display cadence.** The WebView updates the preview at 60 Hz against
  native 120 Hz, which adds up to about 8 ms of extra display delay on average.
* Pen-to-photon latency still needs the camera method. Long-stroke (E) and palm (F) workloads
  were not recorded in either engine (longest strokes are about 1.4 s; no palm counters fired). Real Pencil sampling rate, latency and palm handling
  need the manual workloads (A, B, C, E, F).
* The WKWebView "startup" values in the raw files are process age at page load (the web engine
  started after the native benchmark) and are not app startup times.

## Architectural findings so far (from implementation, not performance)

1. **In both frameworks, BOOX Mode B becomes "host a native Android View".** The Onyx pipeline
   needs a real View with a limit rect in screen space. Neither framework's own renderer can
   take part in the hardware preview. So the BOOX experience will be produced by
   `android/boox-ink` in both cases. The framework's job is to host that View correctly and
   receive committed strokes.
2. **How the View is hosted differs:**
   * *Flutter* has a supported mechanism, Platform Views. Hybrid Composition puts the real View
     in the Activity hierarchy, which is what `TouchHelper` should need. TLHC and the default
     AndroidView are also wired up for comparison on the device. Gesture routing uses
     `EagerGestureRecognizer`.
   * *Tauri* has no View embedding API. The plugin adds a sibling View above the WebView in
     `android.R.id.content` and keeps it aligned from JS (`getBoundingClientRect` → `setBounds`,
     plus ResizeObserver and window resize). This works with public APIs and was
     straightforward to write. Correct alignment under rotation, insets and soft keyboards is
     the host's responsibility, and the coexistence of WebView compositing with the Onyx frozen
     EPD region is **unverified**.
3. **Both hosts build with the shared module**, but the hosts' generated Gradle projects need
   identical patches (Onyx repo, label replace, pickFirst, AGP 9 `builtInKotlin=false`). These
   are scripted (`tauri_app/scripts/patch-android.py`, `flutter_app/tool/patch_android.py`).
   The maintenance cost is comparable.
4. **Input API surface:**
   * Flutter exposes pressure min/max, tilt, orientation and stylus buttons uniformly on every
     platform. It delivers every sample as a separate event, so no coalescing API is needed.
   * WebViews differ by engine. WebKitGTK lacks `pointerrawupdate`. iOS WKWebView and the
     Android WebView still need to be probed on device. The Tauri engine logs what is missing
     (`api_unavailable` events).
5. **Persistence and rendering parity:** both apps implement the same geometry, the same
   layering (raw → active → committed → cache), the same eraser, and the same metrics names.
   The comparison will be fair once both run.

## Blockers (precise)

| Blocker | Effect | To unblock |
|---|---|---|
| ~~Flutter SDK unavailable~~ **resolved 2026-11-02**: the user installed Flutter 3.47.9 at `~/Applications/flutter/flutter`. Linux builds need `clang++`, which is not installed; the Swift toolchain's clang works through `flutter_app/tool/clang-gcc13` (put it on PATH; it pins GCC 13's libstdc++). Installing `clang` (apt) removes the need for it | — | — |
| No BOOX device attached (`adb devices` empty) | M00.2 exit criterion (native ink works on a device) not met; Modes B/C untested in every host | Run `docs/boox-integration.md` §6 |
| No macOS / Xcode | No Flutter or Tauri iPad builds | `docs/ipad-testing.md`. xtool covers only the native reference and the WKWebView proxy |
| Apple Developer **Program License Agreement not accepted** (`FORBIDDEN_ERROR.PLA_NOT_ACCEPTED` during `xtool dev` provisioning) | M00iOS cannot be installed on the iPad (iPad13,4, iPadOS 26.6.2) | Account Holder accepts the PLA at developer.apple.com, then `cd ios/M00iOS && xtool dev` |
| No Linux pen digitizer | Linux stylus row stays empty | optional per PLAN |
| No high-speed camera run | No physical latency figures | `benchmarks/protocol.md` §3 |

## Decision procedure (what will pick the framework)

In priority order, following the PLAN §15 weights and hard requirements:

1. **Native reference app on BOOX** (M00.2): does Mode B produce persistent ink without
   disappearing, duplicate, offset or ghost strokes, for at least one handoff strategy? If not,
   *neither* framework can meet the BOOX requirement, and the report must say so.
2. **Same test inside each host** (Mode B). The framework that keeps the reference app's
   behaviour (alignment at the corners, no lost strokes, clean exit and resume) wins the 30% BOOX
   category. A Tauri overlay failure in the frozen EPD region would be a material limitation of
   the WebView path.
3. **iPad Pencil**: sample rate (median dt), pressure and tilt fidelity, and camera latency in
   Mode A for both apps.
4. **Linux**: Flutter Workload D against the Tauri numbers above, plus a manual mouse pass.
5. Then maintainability and resource usage. The Tauri tree RSS of about 500 MB at idle is a
   measured data point; Flutter's in-process RSS still needs measuring.

## Recommended M01 input architecture (independent of the framework outcome)

Whichever framework wins:

* **Keep a per-platform native ink module as a first-class component.**
  * Android: `boox-ink`, generalized into "native ink surface" with a BOOX backend and the plain
    Mode C backend. Other ePaper vendors could be added the same way as inksdk does.
  * iOS: a UIKit/PencilKit-backed surface if the Mode A Pencil results are worse than native.
* The framework renders committed ink and the rest of the UI. The native surface owns the
  in-progress stroke on devices where that measurably matters.
* The interface stays as in M00: finalized strokes and viewport events crossing the boundary,
  never per-sample IPC.
* The canonical stroke format and golden geometry tests carry forward unchanged. The Rust
  notebook engine (M01+) should consume the same JSON.
* If the device runs show that both frameworks are mostly hosting native editors on mobile, the
  PLAN §15 "hybrid/native" outcome applies. The framework is then chosen mainly for
  desktop/UI productivity and Rust integration, and the M01 plan should be scoped that way.
