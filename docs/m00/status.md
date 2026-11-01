# M00 status tracker (checkpoint 2026-11-01)

Architecture update (see the steering note of 2026-11-01): both frameworks are evaluated as
**shells hosting the best native handwriting backend per OS**. PencilKit (iPadOS) and the Onyx
Pen SDK (BOOX) are mandatory for both Flutter and Tauri.

Legend: ✅ done · 🟡 partial · ❌ not done · ⛔ blocked (reason in the Blockers column)

## Status matrix

These columns are deliberately separate. "Builds" never implies "runs on hardware".

| Component | Implemented | Builds | Runs on physical HW | Benchmarked | Known issues / blockers |
|---|---|---|---|---|---|
| Shared fixtures + geometry golden (`fixtures/`) | ✅ | n/a | n/a | n/a | Ports verified: TS, Kotlin, Swift; Dart tests written but not run |
| `android/boox-ink` (Onyx `BooxInkView`, Modes B/C) | ✅ | ✅ | ✅ Note Air5 C: Mode B works in all 3 hosts | 🟡 callback metrics, 1 session each | Handoff strategy, raw-render flag, coordinate space: decide on device |
| Native BOOX reference app (`boox-ink-demo`) | ✅ | ✅ APK | ✅ | 🟡 | Edge-to-edge inset bug fixed |
| Tauri Linux (Canvas 2D) | ✅ | ✅ | ✅ (Wayland) | 🟡 Workload D ×3 sessions | Manual mouse pass pending; WebKitGTK timer is 1 ms resolution |
| Tauri Android + BOOX plugin overlay | ✅ | ✅ APK (arm64) | ✅ Mode B works; ink stays put | 🟡 | ACL bug (registerListener) fixed |
| Tauri iOS (Canvas 2D) | ✅ (frontend) | ⛔ needs Xcode | 🟡 via WKWebView **proxy** in `ios/M00iOS` | 🟡 proxy: Workload D + 1 Pencil session | rAF capped at 60 Hz by WebKit; fix found, see below |
| Tauri iOS + **PencilKit plugin** (`tauri-plugin-pencilkit`) | ✅ embeds a real `PKCanvasView` above the WKWebView | 🟡 Swift compiles for iOS against Tauri's Swift API (xtool); full `tauri ios build` ⛔ Xcode | ❌ | ❌ | Placement math untested on device; 120 Hz hook verified in the proxy, not yet inside a Tauri build |
| Shared Swift module `ios/InkKit` (InkCore, **PencilInk**, WebTuning) | ✅ | ✅ (iOS + Linux tests) | ✅ through M00iOS | 🟡 interop test | |
| Standalone UIKit/PencilKit host (`ios/M00iOS`, "PencilKit" engine) | ✅ | ✅ (xtool) | ✅ live drawing sessions, interop + Test A run, force calibrated | 🟡 | Shared brush renders light PencilKit writing thinner than PencilKit (width model vs `.pen` ink) |
| Native UIKit custom-renderer reference (`ios/M00iOS`, "Native UIKit") | ✅ | ✅ | ✅ | 🟡 2 Pencil sessions + Workload D | Gaps while drawing: fixed, confirmed by eye 2026-11-02. Pen-up flash: gone. The one remaining flash (first stroke only) was a **test-harness layout artifact**: the canvas was resized mid-stroke (the `full_rebuild` event with reason `resize` and `activeStroke` true), most likely because the debug label grew. Not fixed, deliberately: this renderer is only a reference baseline, and PencilKit is the iPad backend |
| Flutter Linux (CustomPainter, Impeller) | ✅ | ✅ release | ✅ (Wayland) | 🟡 Workload D ×3 (matched with Tauri) | Committed-image cache added (Impeller has no raster cache); manual mouse pass pending |
| Flutter Android + BOOX Platform View | ✅ | ✅ APK (arm64, armv7, x86_64; Onyx lib bundled) | ✅ Mode B works (hybrid composition); ink stays put | 🟡 | Java/Kotlin minify off: AGP 9.1.0 R8 crash |
| Flutter iOS + PencilKit Platform View | ❌ (next step) | ⛔ Xcode + Flutter | ❌ | ❌ | Will reuse `ios/InkKit` PencilInk |
| Interop tests (A: iPad→Linux, B: BOOX→Linux, C: PencilKit↔BOOX) | 🟡 automated canonical↔PencilKit round trip; import/export endpoints | ✅ | 🟡 on-device canonical↔PencilKit only | ❌ | A needs a live PencilKit drawing; B/C need BOOX |

## Milestones (revised order)

| Milestone | State |
|---|---|
| M00.0 Bootstrap & inventory | ✅ (Flutter SDK missing on this machine) |
| M00.1 Drawing parity | ✅ both build; same geometry golden passes in TS and Dart |
| M00.2 Native BOOX proof | ✅ Note Air5 C: hardware ink + persistent ink, no handoff defect seen |
| M00.3 Shared Android backend | ✅ (`android/boox-ink`) |
| M00.4 Flutter BOOX integration | ✅ works on device (hybrid composition); TLHC not tried |
| M00.5 Tauri BOOX integration | ✅ works on device (overlay above WebView) |
| M00.6A Native PencilKit proof | ✅ standalone host on the iPad: live drawing, export/import, PKDrawing sidecar, force calibration |
| M00.6B Shared PencilKit module | ✅ `ios/InkKit` → `PencilInk` |
| M00.6C Flutter PencilKit integration | ❌ next |
| M00.6D Tauri PencilKit integration | 🟡 plugin implemented + Swift compiled; needs macOS for `tauri ios build` |
| M00.7 Linux testing | 🟡 Workload D head-to-head done (docs/comparison.md); manual mouse pass + startup investigation pending |
| M00.8 Interoperability | 🟡 see `docs/interop.md` |
| M00.9 Final report | ❌ |

## Measured so far on the iPad (iPad13,4, iPadOS 26.6.2, 120 Hz)

* **WKWebView rendering cadence:** 60 Hz by default (17 ms rAF interval). Turning off WebKit's
  `PreferPageRenderingUpdatesNear60FPSEnabled` feature (a private SPI, applied via
  `ios/InkKit/WebTuning`) on the configuration **before** creating the web view gives **120 Hz**
  (8 ms median, measured twice). Setting it after creation, with or without a reload, has no
  effect. A `WebKit<Feature>` UserDefaults override has no effect either.
* For Tauri (whose plugins only receive the finished web view), the plugin installs a hook on
  `-[WKWebView initWithFrame:configuration:]` from `initPlugin()`. The hook sets the flag on the
  configuration just before the original initializer runs. **Measured with the proxy (variant
  `hook`): 8 ms median, 9 ms p95, i.e. 120 Hz**
  (`perf-ios-wkwebview-tauri-workload-d-1793625539063.json`).
  * A first version hooked `-[WKPreferences init]`. It never fired (17 ms, 60 Hz) because
    `WKWebViewConfiguration` creates its `WKPreferences` wrapper internally without `-init`.
  * Caveat 1: inside a real Tauri iOS build, this depends on wry creating the WKWebView after
    plugin initialisation and through `initWithFrame:configuration:`. The plugin reports
    `webFrameRate.applied` in `getMetrics().placement` to verify this; it is unverified until
    a Mac build exists.
  * Caveat 2: this is private API, which is an App Store review risk.
* Pencil input on both native UIKit and WKWebView: 240 Hz coalesced samples, 0 gaps (see comparison.md).

## Highest-risk assumptions (ranked)

1. **Onyx raw drawing inside framework hosts.**
   * Tauri: an overlay View above the WebView.
   * Flutter: depends on Platform View composition mode (HC / TLHC).
   * Untested on hardware; this is the decisive BOOX question.
2. **Tauri iOS native-view placement.** CSS rect → WKWebView coordinates via `contentOffset`
   and `zoomScale`, plus safe areas and rotation. The math is written but unverified on device.
   Web content under the canvas is covered by design.
3. **The 120 Hz WebKit fix depends on private SPI** and on window-creation order in Tauri iOS.
4. **PencilKit ↔ canonical appearance.** Geometry round-trips almost exactly, but PencilKit
   renders the same points differently from the shared brush (IoU 0.53–0.66). Visual parity
   across backends is not achievable without per-backend brush models.
5. **Flutter on devices is unmeasured.** Linux builds and benchmarks; Android APK built; iPad needs a Mac.

## Resume checklist

1. ~~Measure the `hook` 120 Hz variant~~ — done 2026-11-02: 120 Hz.
2. ~~PencilKit drawing session, interop, Test A~~ — done 2026-11-02 (docs/interop.md). Optional: a visual check of
   `pencilkit-*.json` in the Tauri Linux GUI (the files are in its `strokes/` dir).
3. ~~Confirm the native UIKit preview fix~~ — done; the leftover first-stroke flash is a harness layout resize (not fixed, by choice).
4. **PencilKit ↔ canonical appearance.** Geometry round-trips almost exactly, but PencilKit
   renders the same points differently from the shared brush (IoU 0.53–0.66). Visual parity
   across backends is not achievable without per-backend brush models.
5. **Flutter on devices is unmeasured.** Linux builds and benchmarks; Android APK built; iPad needs a Mac.

## Resume checklist

1. ~~Measure the `hook` 120 Hz variant~~ — done 2026-11-02: 120 Hz.
2. ~~PencilKit drawing session, interop, Test A~~ — done 2026-11-02 (docs/interop.md). Optional: a visual check of
   `pencilkit-*.json` in the Tauri Linux GUI (the files are in its `strokes/` dir).
3. ~~Confirm the native UIKit preview fix~~ — done; there is a leftover first-stroke flash. Next time: switch to Native UIKit,
   draw one stroke, `m00ctl.py cmd export`, `pull`, then check the `full_rebuild` events against `stroke_finalized`.
4. Write the Flutter iOS PencilKit Platform View (M00.6C) on top of `PencilInk`.
5. ~~Flutter SDK~~ — installed; Linux + APK built. Next on Linux: a manual mouse pass in both apps, and the cause of Tauri's ~3 s startups.
6. Connect the BOOX and follow docs/boox-integration.md §6.
7. On a Mac: `npx tauri ios init` and build with both plugins.
