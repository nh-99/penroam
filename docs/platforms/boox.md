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
* ePaper refresh (`EpdRefreshPlanner`): a full GC refresh is held back for 150 ms so that a page
  change, pan or zoom flashes once. Erasing and undo refresh only the changed rectangle, unless it
  covers half the view; every 30th regional refresh is full, to clear ghosting. This replaced a
  full flash per erase and has **not been tried on a device yet**.
* Finger pan/pinch inside the native view is handled natively and reported back
  (`viewport` event) so web layers follow.
* ePaper theme is selected automatically on first launch when BOOX hardware ink is detected.
* Search waits for charging by default, and the sync poll slows from 1 to 5 minutes in battery
  saver. Power state comes from the plugin's `getPower` (`src-tauri/src/power.rs`).
* Hover rings and animation are off in the ePaper theme.
* Release APKs have **no INTERNET permission** (`gen/android/app/src/release/AndroidManifest.xml`).

## Build / install

```bash
export ANDROID_HOME=~/Android/sdk NDK_HOME=~/Android/sdk/ndk/29.0.14206865
cd apps/desktop-mobile && pnpm tauri android build --apk --target aarch64
adb install -r src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk
cd android && ./gradlew :boox-ink:testDebugUnitTest   # 14 JVM tests
```

## Known risks

* Printing has not been tried on a BOOX. It needs Android's print spooler, which some e-ink
  firmware may leave out; without one, printing shows an error. With no print service installed
  the system dialog offers only "Save as PDF".
* Native view covers the whole canvas element including the desk area outside the page; ink
  drawn off-page is kept (the web canvas clips its rendering to the page).
* Highlighter preview on the EPD pen layer is opaque (the hardware layer has no alpha); the
  committed stroke is translucent.
* Whole-page web redraw on the BOOX was slow at 10k strokes (≈ 3 s at device resolution). A
  settled zoom now redraws in slices, off screen, which should stop the lag when switching
  tools; this has not been measured on a BOOX. Expect the sharper page to take longer to appear.
* Text editing hides the native layer; switching back re-shows it after the view repaints
  (one ePaper refresh).
