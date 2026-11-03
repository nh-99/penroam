# BOOX (Android + Onyx Pen SDK)

Device used: **BOOX Note Air5 C** (`NoteAir5C`, Android 15, 1860×2480, density 300 dpi =
devicePixelRatio 1.875), on adb as `c5927068`.

## How the native path works in the app

* `NativeInkPlugin` (Kotlin, `plugins/tauri-plugin-native-ink/android`) hosts
  `BooxInkView` (`android/boox-ink`) in the Activity content layout **above** the WebView, placed with
  `OverlayPlacement` over the editor canvas element (CSS rect × dpr + WebView offset).
* `BOOX_HARDWARE` mode ("Mode B" in the framework evaluation): Onyx `TouchHelper` raw drawing gives the ePaper
  hardware preview; on pen-up the `StrokeAssembler` result is drawn into the view's own
  transparent bitmap (`PEN_UP_REFRESH` hand-off), then the stroke is sent to the web editor,
  which adds it to the page model and Rust journals it. The web ink layer is hidden while the
  native layer is visible (no doubled edges); page template, PDF and text remain visible through
  the transparent native view.
* Shown for pen/highlighter/eraser; hidden (View.GONE + raw drawing disabled) for text, select
  and pan, and whenever a dialog, menu, drawer or busy overlay is open.
* Undo/redo/delete/move made in the web editor are mirrored with `addStrokes`/`removeStrokes`.
* Finger pan/pinch inside the native view is handled natively and reported back
  (`viewport` event) so web layers follow.
* ePaper theme is selected automatically on first launch when BOOX hardware ink is detected.
* Release APKs have **no INTERNET permission** (`gen/android/app/src/release/AndroidManifest.xml`).

## Build / install

```bash
export ANDROID_HOME=~/Android/sdk NDK_HOME=~/Android/sdk/ndk/29.0.14206865
cd apps/desktop-mobile && pnpm tauri android build --apk --target aarch64
adb install -r src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk
cd android && ./gradlew :boox-ink:testDebugUnitTest   # 14 JVM tests
```

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
