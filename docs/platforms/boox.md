# BOOX (Android + Onyx Pen SDK) — M01

Device used: **BOOX Note Air5 C** (`NoteAir5C`, Android 15, 1860×2480, density 300 dpi =
devicePixelRatio 1.875), on adb as `c5927068`.

## How the native path works in the app

* `NativeInkPlugin` (Kotlin, `plugins/tauri-plugin-native-ink/android`) hosts the M00
  `BooxInkView` in the Activity content layout **above** the WebView, placed with
  `OverlayPlacement` over the editor canvas element (CSS rect × dpr + WebView offset).
* `BOOX_HARDWARE` mode (M00 Mode B): Onyx `TouchHelper` raw drawing gives the ePaper
  hardware preview; on pen-up the M00 `StrokeAssembler` result is drawn into the view's own
  transparent bitmap (M00 `PEN_UP_REFRESH` hand-off), then the stroke is sent to the web editor,
  which adds it to the page model and Rust journals it. The web ink layer is hidden while the
  native layer is visible (no doubled edges); page template, PDF and text remain visible through
  the transparent native view.
* Shown for pen/highlighter/eraser; hidden (View.GONE + raw drawing disabled) for text, select
  and pan, and whenever a dialog, menu, drawer or busy overlay is open.
* Undo/redo/delete/move made in the web editor are mirrored with `addStrokes`/`removeStrokes`.
* Finger pan/pinch inside the native view is handled natively (M00) and reported back
  (`viewport` event) so web layers follow.
* ePaper theme is selected automatically on first launch when BOOX hardware ink is detected.
* Release APKs have **no INTERNET permission** (`gen/android/app/src/release/AndroidManifest.xml`).

## Build / install

```bash
export ANDROID_HOME=~/Android/sdk NDK_HOME=~/Android/sdk/ndk/29.0.14206865
cd apps/desktop-mobile && pnpm tauri android build --apk --target aarch64
adb install -r src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk
cd android && ./gradlew :boox-ink:testDebugUnitTest   # 14 JVM tests (11 M00 + 3 M01)
```

## What was verified on the Note Air5 C (2026-11-02)

| Check | Result |
|---|---|
| APK installs and launches (release, arm64) | ✅ |
| Library renders, ePaper theme auto-selected (BOOX detected via Onyx capabilities) | ✅ screenshot `docs/screenshots/m01/08-boox-library-epaper.png` |
| Notebook creation through the WebView UI (uiautomator labels + adb text input) | ✅ |
| Editor opens; native `BooxInkView` attaches in `BOOX_HARDWARE`, Onyx raw input reader starts, view sized exactly to the canvas element (1530×2197 px) | ✅ logcat `BooxInkView: ensureSession mode=BOOX_HARDWARE … size=1530x2197`, `RawInputReader … submitJob` |
| Template drawn by the web layer visible through the transparent native view | ✅ screenshot `09-boox-editor-native-overlay-mounted.png` |
| adb-injected stylus swipes | ❌ not picked up — expected: Onyx raw drawing reads the digitiser directly. **Real pen strokes need a person** |
| **Plumbing check** (`tools/boox-m01-check.sh`, run 2026-11-02 14:00) — stroke injected into the native post-pen-up path (marked `simulated`), not the pen | ✅ native → web model **42 ms**, model → durable on disk **1.8 ms**; undo/redo from the web side mirrored in the native layer (model 0/native 0, then 1/1) |
| Native layer vs web layer alignment (same stroke, pen tool vs text tool, full-res screenshots) | ✅ best pixel overlap at offset **(0, 0)**, stroke centroids differ by < 1 px (`12-boox-native-vs-web-layer.png`) |
| 1k / 5k / 10k-stroke pages through the native surface | parse 157 / 651 / 1344 ms + native bitmap render 82 / 880 / 1720 ms (10k ≈ 3.1 s; M00 Mode B load ≈ 3.8 s). Page open incl. IPC 0.7 / 2.2 / 4.3 s. Web canvas whole-page redraw (shown when a non-pen tool is active) 0.3 / 1.5 / 3.1 s at device resolution — slow, see risks. App process RSS 341 / 503 / 629 MiB (WebView renderer process not included; not comparable with M00's ~559 MB figure) |
| **Two-OS `.penroam`**: Linux export → `adb push` → imported through the Android system picker (content URI) → opened (PDF.js renders the rotated PDF pages, ink/text in place, background thumbnails) → exported through the Android save picker → pulled to Linux | ✅ `tools/compare-penroam.py linux-export.penroam boox-export.penroam` → **IDENTICAL** (6 pages, 6 strokes / 98 points, 2 text blocks, PDF asset bytes) |
| Real pen: hardware preview immediacy, persistence after pen-up, no blink/double/ghost, eraser | ⏳ **still not validated in M01** (validated in M00 for the same `BooxInkView` in the M00 Tauri host) |
| Rotation, keyboard, background/resume with the pen | ⏳ manual checklist printed by `tools/boox-m01-check.sh` |
| Native callback p95 vs M00's 12 ms | ⏳ needs real pen strokes |

Bugs found and fixed during the device run: (1) the Android autobench trigger was never
compiled in (a scripted edit missed after reformatting) — fixed; (2) thumbnails were rejected
on Android because the mobile IPC bridge sends byte arrays as JSON, not raw bodies — fixed;
(3) start-up steps now have timeouts and log to logcat.

## Known risks

* Native view covers the whole canvas element including the desk area outside the page; ink
  drawn off-page is kept (the web canvas clips its rendering to the page).
* Highlighter preview on the EPD pen layer is opaque (the hardware layer has no alpha); the
  committed stroke is translucent.
* Whole-page web redraw on the BOOX is slow at 10k strokes (≈ 3 s at device resolution);
  it is only visible when a non-pen tool is active, but switching tools on huge pages lags.
  Lower the web backing resolution on ePaper or keep it viewport-sized there.
* Text editing hides the native layer; switching back re-shows it after the view repaints
  (one ePaper refresh).
