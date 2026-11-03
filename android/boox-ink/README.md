# boox-ink — BOOX native ink module

`BooxInkView` is a real Android `View`. It provides:

* **Mode B**: the Onyx `TouchHelper` raw-drawing hardware preview, plus software reconstruction
  of each stroke into persistent ink.
* **Mode C**: a plain MotionEvent + Canvas baseline.

The app's native ink plugin (`plugins/tauri-plugin-native-ink`, Android side) hosts it as an
overlay above the WebView; `apps/desktop-mobile/scripts/patch-android.py` wires it into the
generated Android project. How it fits the app: [docs/platforms/boox.md](../../docs/platforms/boox.md).

## Host API

```kotlin
BooxInk.init(application)                       // once per process
val v = BooxInkView(context)
v.listener = object : BooxInkView.Listener {    // finalized strokes, erases, viewport, status
    override fun onStrokeCommitted(stroke: InkStroke) { … }
}
v.setViewTransform(scale, offsetX, offsetY, devicePixelRatio)
v.setBrush(widthDoc = 3f, pressure = true, BooxInkView.Tool.PEN)
v.setMode(BooxInkView.Mode.BOOX_HARDWARE)       // returns the mode actually entered
v.attach()                                      // detach() releases raw drawing
v.loadStrokes(json); v.exportStrokes(); v.clearCanvas()
v.getCapabilities(); v.getMetrics(); v.status()
v.setOptions(BooxInkView.Options(rawRenderEnabled = false, handoff = Handoff.TOGGLE_RAW))
```

## Host requirements

* Add the Onyx repository (HTTP, group-restricted). See `../settings.gradle.kts`.
* Define `onyxPenVersion`, `onyxDeviceVersion` and `onyxBaseVersion` in gradle.properties.
* In the app manifest: `tools:replace="android:label"` on `<application>`.
* In the app's `android { packaging { jniLibs { pickFirsts += "**/libc++_shared.so" } } }`.
* minSdk ≥ 26.
* AGP 9 hosts: `android.builtInKotlin=false`, because this module applies `org.jetbrains.kotlin.android`.

## Tests

```bash
cd android && ./gradlew :boox-ink:testDebugUnitTest
```

These cover geometry against `fixtures/geometry-golden.json`, stream merging, dedup and gap
detection, JSON round-trip, and the view transform. Device behaviour has to be tested in the
app on a BOOX device (`tools/boox-check.sh`).
