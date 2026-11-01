# BOOX / Onyx Pen SDK integration

> **Device status: not yet tested on BOOX hardware.** No BOOX device was connected
> (`adb devices` was empty) during the build session. Everything below that says "verify"
> must be filled in from a device run. Nothing here claims the hardware path works yet.

## 1. SDK dependency (what was actually resolved)

| Artifact | Version pinned | Where from |
|---|---|---|
| `com.onyx.android.sdk:onyxsdk-pen` | **1.5.2** | `http://repo.boox.com/repository/maven-public/` |
| `com.onyx.android.sdk:onyxsdk-device` | **1.3.3** | same |
| `com.onyx.android.sdk:onyxsdk-base` | **1.8.4** | same |
| transitive: onyxsdk-baselite 1.1.0.1, -geometry 1.1.2, -penbrush 1.1.0.1, -commons-io 2.5, mmkv 1.0.19, easypermissions 0.2.1, hugo-annotations 1.2.3, fst 2.56, rxjava 2.2.0 | – | Boox repo (the JCenter-era artifacts are only mirrored there) + Maven Central |

* The versions are pinned in `android/gradle.properties` (`onyxPenVersion` etc.). They are
  copied into the Tauri and Flutter Android projects by the patch scripts.
* Why 1.5.2: it is the version used by both working community references (Boox-EinkDraw on a
  Note Air4C with Android 13, and imedwei/inksdk). The repo also has 1.4.11 (the documented
  version) up to 1.5.8.
* If a device misbehaves, change the property and rebuild. Record the outcome in §6.
* **HTTPS to repo.boox.com fails** with `dh key too small` (OpenSSL 3 / modern JDK). Plain HTTP
  works. The repository is restricted by group in every Gradle file so it cannot shadow
  other artifacts.
* Nothing is vendored. Boox-EinkDraw bundles AARs and an extracted
  `onyxsdk-pen-native-classes.jar` in `app/libs`. We do **not** copy those (redistribution
  rights are unverified), and we do not need the `NeoPen*Wrapper` classes because the
  software rendering is our own shared geometry.
* The SDK AARs ship `libonyx_pen_touch_reader.so` and `libneo_pen.so`. These are present in
  the built APKs.

### Manifest and packaging gotchas (all handled)

* The Onyx AARs declare `application@label`. Each host app needs `tools:replace="android:label"`
  (already in the demo; added by both patch scripts).
* The Onyx AARs request DUMP, SYSTEM_ALERT_WINDOW, storage, Wi-Fi, Bluetooth, WAKE_LOCK and
  FOREGROUND_SERVICE. `boox-ink`'s manifest strips all of them; the merged demo manifest
  contains no `uses-permission`. If a device run shows one is needed, re-add it explicitly.
* `libc++_shared.so` is shipped by both onyxsdk-pen and mmkv. Hosts need `pickFirsts`.
* R8: `boox-ink/consumer-rules.pro` keeps `com.onyx.**` (the SDK uses reflection) and silences
  optional classes pulled in by transitive dependencies.
* `BooxInk.init(application)` calls `HiddenApiBypass.addHiddenApiExemptions("")`. The SDK
  reflects into `android.onyx.*` framework classes. It also calls
  `RxManager.Builder.initAppContext` reflectively.

## 2. Capability detection (`BooxCapabilities`)

The manufacturer/brand/model is only a **hint**. Mode B is attempted only when all of these hold:

1. The Onyx SDK classes load in the APK.
2. The firmware framework classes `android.onyx.ViewUpdateHelper` or
   `android.onyx.hardware.DeviceController` exist on the device. These are what the SDK
   reflects into (found with `javap`/`strings` on onyxsdk-device).
3. `Device.currentDeviceIndex()` is not `BaseDevice`. This is the SDK's own EPD backend probe.
4. At attach time, `TouchHelper.create` + `openRawDrawing()` do not throw. `isRawDrawingCreated`
   is recorded.

If any step fails, the exact error goes to `lastError` and the metrics `errors` list. The partly
created `TouchHelper` is closed and the view falls back to Mode C. The status line shows
`requested … FELL BACK …`. The app does not crash.

## 3. Raw drawing lifecycle (`OnyxBridge.kt`, `BooxInkView.kt`)

```text
attach → (laid out, attached, activity resumed)
  session executor thread:
    TouchHelper.create(view, cb)              | or create(view, FEATURE_ALL_TOUCH_RENDER, cb, false)  [option]
    setStrokeWidth / setStrokeColor(BLACK)
    setLimitRect(Rect(0,0,w,h), [])           # view-local; host keeps its UI outside the view
    openRawDrawing()
    setStrokeStyle(FOUNTAIN if pressure else PENCIL); width/color again (EinkDraw order)
    setRawDrawingRenderEnabled(option)        # default true; test both — see §5
    EpdController.setScreenHandWritingRegionLimit(view)   [option, default on]
    setRawDrawingEnabled(drawing && pen tool && not navigating)
pause/detach/destroy/view detached → setRawDrawingEnabled(false); setRawInputReaderEnable(false); closeRawDrawing(); full GC refresh
layout move/resize/rotation → close + reopen once (debounced 50 ms); never per pointer event
```

* `openRawDrawing` can block, so it and `closeRawDrawing` run on a dedicated single thread
  (same as Boox-EinkDraw).
* A generation counter drops callbacks and posts that belong to a closed session.
* The activity lifecycle is observed automatically: the view registers
  `ActivityLifecycleCallbacks`, so hosts do not need to forward onPause/onResume.

## 4. Two render paths and the pen-up handoff

**Path A (hardware preview):** Onyx draws the live stroke. The app does no software preview in
Mode B, and stylus `MotionEvent`s go only to `TouchHelper.onTouchEvent`. This prevents double
processing.

**Path B (persistent software ink):** the callbacks feed `StrokeAssembler`:

* The **move stream** is begin + every `onRawDrawingTouchPointMoveReceived` + end.
* The **list stream** is `onRawDrawingTouchPointListReceived`. Cumulative deliveries replace
  the list; chunked deliveries (where the prefix does not match) are appended.
* At the end, the list stream wins if it has at least 90% of the move count. Otherwise the move
  stream is used.
* Exact duplicates (same x, y and timestamp) are removed. Gaps (`dt > max(30 ms, 4×median)`)
  and spatial jumps (> 40 doc units) are counted.
* Every stroke records `stream`, `moveCount`, `listCount`, `listCallbacks`,
  `duplicatesRemoved`, `timeGaps`, `spatialJumps` and `outOfBounds` in `input`. This lets the
  device run show **which stream is complete on this firmware**, instead of assuming.
* A repeated `onBeginRawDrawing` during a stroke is merged in (Boox-EinkDraw saw this on some
  firmware) and counted as `native_duplicate_begin`.

On pen-up (main thread): stroke → bitmap → `invalidate(rect)`. Then one of three selectable
**handoff strategies** runs. They come from three different community implementations, and the
right one depends on firmware:

| Handoff | Sequence | Source |
|---|---|---|
| `PEN_UP_REFRESH` (default) | wait for `onPenUpRefresh` (120 ms fallback timer) → invalidate → `EpdController.invalidate/refreshScreen(HAND_WRITING_REPAINT_MODE)` | Boox-EinkDraw |
| `TOGGLE_RAW` | `setRawDrawingEnabled(false)` → redraw → `setRawDrawingEnabled(true)` | inksdk `syncOverlay(force)` |
| `REGIONAL_GC` | redraw → `EpdController.invalidate(view, strokeRect±10, GC)` | notate "regional GC" |

Recorded events: `software_stroke_committed`, `raw_layer_clear_requested`,
`epaper_refresh_requested`, `persistent_render_completed`, `pen_up_refresh_fallback_used`.

## 5. Things that must be decided on the device (do not assume)

| Question | How to test | Result |
|---|---|---|
| Does `setRawDrawingRenderEnabled(false)` hide the hardware preview? The docs say yes, but Boox-EinkDraw uses `false` and still gets a hardware preview | Demo app: toggle `rawRender`, draw, photograph | _pending_ |
| `create(view, cb)` vs `create(view, FEATURE_ALL_TOUCH_RENDER, cb, false)` | Demo: `create()` button | _pending_ |
| Which stream is complete (list vs move)? | Stroke `input.stream` / `moveCount` / `listCount` in exported strokes | _pending_ |
| Is `onPenUpRefresh` delivered? | `native_pen_up_refresh_callback` vs `pen_up_refresh_fallback_used` counters | _pending_ |
| Which handoff has no disappear/duplicate/ghost/flicker? | Workload G per strategy | _pending_ |
| Are TouchPoint coordinates view-local? | Status `coords=` (`view` / `screen` / `mismatch`) | _pending_ |
| TouchPoint tilt units | Tilt the pen and compare exported values | _pending_ |
| Does the preview line up with committed ink at zoom 1.5 / 0.75? | Demo `Zoom` button + Workload H | _pending_ |
| Normal refresh is restored after the app exits | Leave the app, scroll the launcher | _pending_ |
| Flutter: which Platform View composition keeps raw drawing working? | Flutter `Composition:` button (hybrid / tlhc / androidView) | _pending_ |
| Tauri: overlay placement and z-order above the WebView | Draw at the canvas corners, rotate | _pending_ |

## 6. Device run procedure

```bash
# Inventory (paste into benchmarks/results/boox/device.md)
adb shell getprop ro.product.model
adb shell getprop ro.build.version.release
adb shell getprop ro.build.fingerprint
adb shell getprop ro.build.display.id
adb shell wm size; adb shell wm density
adb shell dumpsys webviewupdate | grep -E "Current WebView package|versionName" | head
adb shell dumpsys package com.onyx.android.note 2>/dev/null | grep versionName   # native Notes app version

# Install
adb install -r android/boox-ink-demo/build/outputs/apk/release/boox-ink-demo-release.apk
adb install -r tauri_app/src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk
adb install -r flutter_app/build/app/outputs/flutter-apk/app-release.apk

# Logs while testing
adb logcat -s BooxInkView BooxCapabilities BooxInk BooxInkDemo

# Pull results
adb pull /sdcard/Android/data/dev.opennote.booxink.demo/files/perf benchmarks/results/boox/native/
adb shell run-as dev.opennote.m00.tauri ls files   # Tauri app_data_dir (debuggable builds only)
```

Order: run M00.2 on the **reference app** first (Mode B and C, every option in §5). Only after
it produces persistent ink without handoff defects should the Flutter and Tauri hosts be
evaluated (M00.4 / M00.5).

### Known architectural risk (Tauri)

The Tauri overlay is a sibling View placed above the WebView in `android.R.id.content`.
`TouchHelper` reads raw input inside the limit rect regardless of z-order. The risk is that
WebView compositing into the same region could fight with the frozen EPD region. If it does,
the precise symptom must be recorded here. It must not be hidden by falling back to the HTML
canvas. The UI always shows when a fallback happened.
