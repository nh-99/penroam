# M00 — Cross-Platform Handwriting Framework Bake-Off

## 1. Objective

Implement a technical bake-off between Flutter and Tauri v2 to select the application framework for a new open-source, handwriting-first note-taking application.

The production application will eventually target:

- Linux
- Windows
- macOS
- Android, including BOOX ePaper tablets
- iOS/iPadOS, including Apple Pencil

For this bake-off, evaluate both frameworks on three actual target environments:

1. BOOX Android ePaper tablet with stylus
2. iPad with Apple Pencil
3. Linux desktop

**The primary objective is to identify the architecture that delivers the best practical handwriting experience while remaining maintainable and portable.**

BOOX integration must use the Onyx Pen SDK's native, low-latency raw-drawing pipeline rather than relying exclusively on Android `Canvas`, Flutter `CustomPainter`, or a browser canvas.

This is a working prototype milestone, not an architecture-only investigation.

### Success criteria

M00 is complete when:

- Both prototypes are installable and runnable.
- Both prototypes provide equivalent drawing functionality.
- Both work on Linux, Android, and iPadOS, subject to the availability of platform build environments.
- Both provide a normal framework-based drawing mode.
- Both attempt an actual native Onyx Pen SDK accelerated drawing mode on BOOX.
- The native BOOX mode works and visibly produces persistent ink, or its blocking issue is documented with a reproducible failure.
- Stylus events, latency, rendering, and memory behavior can be measured.
- A comparison report recommends Flutter, Tauri, or a native-hybrid alternative.

Do not choose a winner based solely on expected performance.

## 2. Research and references

Before implementing platform-specific adapters, inspect the following sources.

### Official Onyx SDK

https://github.com/onyx-intl/OnyxAndroidDemo

https://github.com/onyx-intl/OnyxAndroidDemo/blob/master/doc/Onyx-Pen-SDK.md

Focus on:

- `TouchHelper`
- `RawInputCallback`
- `TouchPoint`
- `TouchPointList`
- `BrushRender`
- `EpdController`
- Raw drawing enable/disable
- Hardware brush configuration
- Refresh and clearing behavior

### Working BOOX reference implementation

https://github.com/steffest/Boox-EinkDraw

Study:

- `HardwarePenSurfaceView`
- `OnyxStrokeRenderer`
- Hardware preview
- Raw stroke callbacks
- Software stroke reconstruction
- Pen-up handoff
- Clearing/refresh behavior

Use this project as a technical reference, not an unquestioned source of compatible SDK binaries.

### Additional BOOX references

https://github.com/alexdremov/notate/blob/main/onyx.md

https://github.com/imedwei/inksdk

These are useful examples of firmware-specific SDK behavior and pen integration.

### Framework documentation

Flutter:
https://docs.flutter.dev/platform-integration/android/platform-views

Tauri:
https://v2.tauri.app/develop/plugins/develop-mobile/

Apple Pencil:
https://developer.apple.com/documentation/uikit/handling-input-from-apple-pencil

Review current SDK documentation and APIs rather than assuming these examples match the installed device firmware.

## 3. Repository structure

Create the following monorepo:

```text
m00-handwriting-bakeoff/
├── flutter_app/
│   ├── lib/
│   ├── android/
│   ├── ios/
│   └── test/
├── tauri_app/
│   ├── src/
│   ├── src-tauri/
│   └── tests/
├── android/
│   └── boox-ink/
│       ├── src/main/
│       ├── build.gradle.kts
│       └── README.md
├── fixtures/
│   ├── sample-strokes.json
│   └── benchmark-scenes/
├── benchmarks/
│   ├── protocol.md
│   ├── results/
│   └── analysis/
├── docs/
│   ├── architecture.md
│   ├── boox-integration.md
│   ├── ipad-testing.md
│   ├── linux-testing.md
│   └── comparison.md
└── README.md
```

Keep the Flutter and Tauri implementations independent enough to measure their actual performance.

Share benchmark fixtures, stroke serialization, and the native Android BOOX library rather than duplicating vendor integration code unnecessarily.

Do not build the complete Rust notebook engine during M00.

Use an identical, minimal stroke representation on both sides. Integrating the long-term Rust persistence engine is not necessary to determine drawing performance.

## 4. Device inventory and build setup

Record the following information before testing:

### BOOX

- Model
- Android version
- Firmware version/build
- Stylus model
- Screen dimensions and density
- Refresh mode
- Installed WebView version
- Supported stylus features
- Onyx SDK version used

Use ADB to gather device information where possible.

Example commands:

```bash
adb shell getprop ro.product.model
adb shell getprop ro.build.version.release
adb shell getprop ro.build.fingerprint
adb shell wm size
adb shell wm density
```

Also record the WebView provider/package version.

### iPad

- Device model
- iPadOS version
- Apple Pencil generation/model
- Display refresh capability
- Available Xcode and iOS SDK versions

An actual iPad must be used for final pen-input testing. Simulator results are insufficient.

If the agent is running on Linux without access to macOS/Xcode, prepare complete iOS projects and build instructions. Do not mark the iPad tests passed until the applications are built and tested on the physical iPad.

### Linux

Record:

- Distribution
- Kernel
- Desktop environment
- Wayland or X11
- Flutter version
- Tauri version
- GTK/WebKitGTK versions
- GPU and graphics backend
- Input devices

Test with a mouse. If a pressure-sensitive Linux digitizer is available, include it, but do not require one.

## 5. Common prototype functionality

Both implementations must provide the same drawing experience.

### UI

A minimal application containing:

- Full-screen writing canvas
- Pen tool
- Eraser tool
- Clear canvas
- Pen thickness selector
- Pressure sensitivity toggle
- Rendering mode selector
- Debug/performance overlay
- Save strokes
- Load strokes
- Load benchmark scene
- Export performance data

On BOOX, use a high-contrast, black-and-white interface without animations.

### Drawing

Implement:

- Continuous freehand strokes
- Variable-width pressure-sensitive strokes
- Smooth curves
- Single-point dots
- Mouse drawing
- Stylus drawing
- Stylus eraser handling where supported
- Touch navigation
- Basic palm-rejection behavior
- Clear canvas

Support at least three stroke widths.

Use matching brush geometry and smoothing algorithms across frameworks whenever practical.

Separate the following concepts:

1. Raw input samples
2. Active stroke preview
3. Committed strokes
4. Rendered stroke cache

Avoid rendering the entire history from scratch on every pointer movement.

### Canonical stroke format

Use a shared representation equivalent to:

```json
{
  "id": "stroke-uuid",
  "source": "stylus",
  "brush": {
    "type": "pen",
    "width": 3.0,
    "color": "#000000"
  },
  "points": [
    {
      "x": 120.0,
      "y": 240.0,
      "pressure": 0.62,
      "tiltX": 0.0,
      "tiltY": 0.0,
      "timeUs": 0
    }
  ]
}
```

Coordinates should be in canvas/document space.

Retain original pressure and motion samples wherever possible.

Document how the source platform represents and normalizes pressure, tilt, and timestamps.

Include source/input metadata so the benchmark can distinguish native SDK samples from Flutter or WebView pointer samples.

Support export/import to JSON for test repeatability.

## 6. Flutter prototype

Implement the Flutter application using the current stable SDK.

### Drawing pipeline

Use:

- `Listener` for low-level pointer events
- `CustomPainter` for canvas rendering
- Immediate rendering of the active stroke
- Cached completed strokes where beneficial
- `RepaintBoundary` where appropriate

Capture:

- Pointer device type
- Position
- Pressure
- Pressure min/max
- Tilt
- Orientation, if available
- Timestamps
- Stylus button state, if supported

Normalize input data into the canonical stroke representation.

### Rendering behavior

Requirements:

- Avoid whole-widget-tree rebuilds for every pointer event when possible.
- Keep pointer processing and active-stroke rendering lightweight.
- Do not block rendering on persistence.
- Preserve smooth stroke appearance at normal writing speeds.
- Avoid excessive smoothing that adds visible latency.

### Debug modes

Provide:

- Standard Flutter stroke rendering
- Raw polyline rendering
- Smoothed pressure-sensitive rendering
- Debug overlay enabled/disabled

Record Flutter frame timings using the framework's timing APIs.

### Android native integration

Prepare the application to embed a native Android writing surface through Platform Views.

Use an actual Android View backed by the shared Kotlin BOOX library.

Evaluate Hybrid Composition and other supported Platform View implementations on the actual device. Do not assume the newest composition technique is supported by the BOOX GPU or Android version.

Prefer the approach that preserves native drawing behavior, even if it is not the fastest for conventional Flutter widgets.

## 7. Tauri prototype

Implement the Tauri v2 application using:

- React
- TypeScript
- Vite
- HTML Canvas 2D for the initial drawing implementation
- A minimal Rust backend

Avoid adding a complex UI framework or state-management system.

### Drawing pipeline

Use browser Pointer Events:

- `pointerdown`
- `pointermove`
- `pointerup`
- `pointercancel`
- `pointerrawupdate` where supported

Capture:

- `pointerType`
- `pressure`
- `tiltX`
- `tiltY`
- `twist`
- `buttons`
- Event timestamps

Use `setPointerCapture()` where appropriate.

Call `getCoalescedEvents()` when supported, and feature-detect `getPredictedEvents()`.

Do not assume the APIs are implemented identically across:

- Linux WebKitGTK
- iOS WKWebView
- Android WebView

Log unavailable APIs.

### Rendering

Use Canvas 2D initially.

Implement:

- A completed-stroke layer/cache
- An active-stroke drawing layer
- Pressure-dependent stroke widths
- Incremental rendering
- RequestAnimationFrame-based scheduling where appropriate

Avoid React re-renders for every pointer sample.

Keep mutable stroke-preview data outside React component state when necessary.

Evaluate OffscreenCanvas or WebGL only if the baseline reveals a measurable bottleneck and the target WebViews support the feature.

### Android native integration

Implement a Tauri v2 Android plugin in Kotlin.

The plugin must connect to the same shared BOOX ink library used by Flutter.

**The plugin must host or attach a real native Android drawing View. Simply calling Onyx methods from JavaScript without a native drawing surface is insufficient.**

Investigate how to attach a native drawing View to the Activity's native view hierarchy, positioned over the designated WebView canvas area.

Requirements:

- Accurate placement and sizing
- Screen rotation/resize support
- Z-order handling
- Native input routing
- No double-processing of stylus events
- Proper attachment/detachment
- Proper Activity lifecycle cleanup
- Correct display of committed strokes

If a native Android overlay cannot be reliably hosted in Tauri, treat that as an architectural limitation and report it explicitly.

Do not silently fall back to HTML Canvas and call the BOOX integration complete.

## 8. Shared BOOX native handwriting module

**This section is mandatory and is the highest-priority Android requirement.**

Implement an Android Kotlin library under:

```text
android/boox-ink/
```

The module must encapsulate the BOOX-specific handwriting implementation and be usable by both Flutter and Tauri Android hosts.

### 8.1 SDK dependency

Use the official Onyx Pen SDK wherever possible.

The official documentation demonstrates the Gradle coordinate:

```kotlin
implementation(
    "com.onyx.android.sdk:onyxsdk-pen:1.4.11"
)
```

This is a documented reference version, not necessarily the newest or best version for the user's device.

Determine the compatible SDK version and repository configuration from official examples and a working device test.

Record and pin the actual version used.

Do not silently substitute unsupported SDK artifacts.

If official dependencies cannot be obtained, document the problem and investigate source-based alternatives, including the referenced community projects.

Do not redistribute proprietary native libraries or firmware-extracted binaries without verified redistribution rights.

### 8.2 Native View

Create a native component such as:

```text
BooxInkView
```

It should own:

- The drawing View/Surface
- `TouchHelper`
- SDK callback handling
- Native stroke preview
- Committed-stroke renderer
- Input mode
- Refresh coordination
- Pen cleanup and lifecycle

Expose a small host-facing API:

```text
attach()
detach()
setDrawingEnabled(bool)
setBrush(...)
clearCanvas()
loadStrokes(...)
exportStrokes()
getCapabilities()
getMetrics()
```

This is a conceptual API; adapt signatures as appropriate.

### 8.3 BOOX capability detection

Detect whether the actual device supports the required Onyx runtime.

Use device manufacturer/model information only as an initial hint.

Verify that the required SDK classes and runtime features are genuinely available.

Do not activate the native drawing mode solely because a manufacturer string contains "Onyx" or "BOOX".

If initialization fails:

- Log the exact failure.
- Release partially initialized resources.
- Fall back to ordinary Android drawing.
- Display a clear status in the debug UI.

Do not crash the application.

### 8.4 TouchHelper raw drawing

Implement the official raw-drawing lifecycle.

The documented API includes:

```java
TouchHelper.create(view, callback)
    .setStrokeWidth(3.0f)
    .setLimitRect(limit, exclude)
    .openRawDrawing();
```

The installed SDK may require a slightly different initialization sequence.

Verify the actual API and implementation.

After initialization, activate drawing with `setRawDrawingEnabled(true)` when supported and appropriate.

**The goal is to use the device's dedicated low-latency pen rendering pipeline, not merely to obtain stylus coordinates.**

The raw drawing surface must correspond to the visible physical writing region.

### 8.5 Raw input callbacks

Capture the drawing lifecycle using the SDK's callback methods, including:

```text
onBeginRawDrawing(...)
onRawDrawingTouchPointMoveReceived(...)
onRawDrawingTouchPointListReceived(...)
onEndRawDrawing(...)
```

Implement corresponding eraser callbacks when supported.

On some versions/firmware, individual movement callbacks and accumulated point-list callbacks may differ in completeness.

Investigate both rather than assuming either stream is authoritative.

Requirements:

- Preserve complete stroke point sequences.
- Avoid duplicate samples when merging streams.
- Preserve pen pressure and tilt where available.
- Handle cancelled/interrupted strokes.
- Handle single-point strokes.
- Detect and record lost or discontinuous samples.
- Convert native sample coordinates into the common document coordinate system.

The callback should record actual delivered points, not generate artificial benchmark samples.

### 8.6 Critical: hardware preview and persistent software ink

A working native BOOX drawing implementation must have two connected render paths.

**Path A: hardware-assisted preview**

While the pen is touching and moving across the screen, the Onyx hardware drawing pipeline should display ink with the lowest latency the supported device can provide.

Do not route each point through Dart, JavaScript, Tauri IPC, Rust, or a normal software redraw before making it visible.

**Path B: persistent software reconstruction**

Collect native stroke samples in parallel.

When the stroke ends:

1. Finalize the best available sample sequence.
2. Convert it into the canonical stroke representation.
3. Render the completed stroke into a persistent software canvas.
4. Coordinate clearing/replacement of the transient hardware preview.
5. Trigger the necessary ePaper refresh.
6. Ensure the completed stroke remains visible.
7. Ensure the stroke can be exported and reloaded.

This is the core technique demonstrated by Boox-EinkDraw.

Carefully avoid the following bugs:

- The stroke disappears after pen-up.
- The stroke appears twice.
- The committed stroke is offset from the preview.
- The preview remains as a ghost after clearing.
- A refresh causes severe flickering.
- A new stroke erases earlier strokes.
- A refresh interrupts the next pen stroke.

The final software representation must retain the vector stroke data even if an intermediate bitmap cache is used.

### 8.7 Refresh and raw-layer management

Investigate SDK-specific behavior for:

- `setRawDrawingEnabled(...)`
- `setRawDrawingRenderEnabled(...)`
- `closeRawDrawing()`
- Pen-up refresh callbacks
- Raw-layer clearing
- Partial refresh
- Full refresh

**Do not blindly assume that a flag's name or community example accurately describes its behavior on the installed firmware.**

Test the raw-render setting both ways where safe, verify whether hardware ink is actually visible, and document the result.

Do not repeatedly close/reopen the raw drawing engine for every pointer movement.

Ensure that raw drawing is released when the Activity is paused, the View is destroyed, or the user leaves drawing mode, as appropriate for the SDK.

Avoid leaving the BOOX device in a state where normal screen refresh is disabled after exiting the application.

### 8.8 Coordinate mapping and zoom

For M00, use a 1:1 writing surface initially.

Map native screen coordinates into the logical canvas accurately, accounting for:

- Device pixel density
- View origin
- Status/navigation bars
- Display cutouts
- Rotation
- Canvas bounds

This is important because the Onyx hardware preview draws in physical screen space.

Add a basic transform test for a non-1:1 viewport.

Document whether preview and committed ink remain aligned under zoom/pan.

Do not allow a failed zoom mapping to obscure the base latency measurements.

### 8.9 BOOX rendering modes

Expose these modes on the device:

**Mode A — Framework drawing**

Standard Flutter or HTML Canvas rendering. No hardware ink acceleration.

**Mode B — BOOX hardware-accelerated preview**

Native Onyx drawing pipeline with software stroke reconstruction.

**Mode C — Native Android software baseline**

Use a native Kotlin View rendering through the conventional Android graphics pipeline, without Onyx hardware preview.

Mode C serves as a useful control to distinguish framework overhead from the device's normal Android rendering behavior.

All modes must accept approximately equivalent inputs and produce comparable persistent strokes.

The UI must clearly indicate the active mode.

## 9. iPadOS / Apple Pencil testing

Build both applications for the actual iPad.

### Required functionality

- Apple Pencil drawing
- Finger touch navigation
- Pressure sensitivity
- Tilt where available
- Palm-contact handling
- Stroke persistence
- Long continuous strokes
- Short, fast strokes

Log the available stylus input properties.

Compare:

- Sample frequency
- Smoothness
- Visual latency
- Pressure fidelity
- Palm rejection
- Rendering performance

### Optional native Apple Pencil reference

If either framework displays notable input latency or skips samples, implement a small separate UIKit or PencilKit drawing baseline.

Use it to determine whether the limitation is coming from:

- Hardware/display
- Flutter input handling
- WebView Pointer Events
- The chosen drawing/rendering implementation

Investigate UIKit coalesced touches and predicted touches.

Predicted points must be treated as temporary visual previews, not committed as authoritative stroke samples.

This native baseline is diagnostic; do not pretend the framework has equivalent native support unless the production integration is actually implemented.

### Apple Pencil-specific features

Record whether the device supports:

- Pressure
- Tilt/azimuth
- Hover
- Double tap
- Squeeze
- Predicted/coalesced touches

Unsupported hardware features must be marked as unavailable.

Do not make hover or squeeze mandatory for M00.

## 10. Linux testing

Implement and test both desktop applications.

The immediate goal is to compare their desktop application and input architecture.

### Required

- Mouse drawing
- Smooth curves
- Window resize
- Canvas scaling
- Basic zoom/pan
- Clear
- Stroke export/import
- Benchmark scene loading
- Performance logging

Check rendering behavior under Wayland.

Record graphics backend, frame timing, memory usage, and CPU utilization.

For Tauri, record the WebKitGTK version.

Where supported, test pointer-event coalescing and pressure reporting.

For Flutter, inspect the actual pointer device type and available pressure fields.

Do not infer Linux stylus support from mouse test results.

## 11. Benchmark instrumentation

Both applications must produce comparable measurements.

### 11.1 Common benchmark events

Record:

```text
pointer_event_received
input_sample_processed
preview_update_scheduled
preview_render_submitted
framework_frame_presented_if_available
stroke_finalized
persistent_render_completed
refresh_requested
refresh_completed_if_observable
```

Use a monotonic clock for internal timing.

Do not assume timestamps from different processes or clocks are directly comparable without synchronization.

### 11.2 BOOX-specific events

Record:

```text
native_pen_down_callback
native_pen_move_callback
native_pen_up_callback
hardware_preview_enabled
software_stroke_committed
raw_layer_clear_requested
epaper_refresh_requested
```

Expose SDK callback counts and initialization errors.

### 11.3 Important measurement distinction

Internal event timing is not the same as physical pen-to-visible-ink latency.

The BOOX SDK may draw directly to the ePaper hardware without the application seeing the actual display update time.

Therefore distinguish:

- Input-to-callback latency, when measurable
- Callback-to-software-render time
- Stroke commit latency
- Framework frame timing
- **Physical pen-to-visible-ink latency**

For physical latency, use an external high-speed camera where available.

A 120/240 FPS recording can provide a useful approximate comparison, subject to camera frame intervals, exposure, and visibility. It is not a laboratory-grade measurement.

Do not invent hardware latency values from callback timings.

### 11.4 Metrics

Report at least:

- Median and p95 measured timing where valid
- Rendered frame times
- Dropped or over-budget frames
- Stroke sample counts
- Sample gaps
- Persistent stroke commit time
- Memory usage
- CPU usage
- Application startup time
- Visible glitches
- App crashes
- Device-specific failures

For the ePaper device, distinguish between slow full-screen refreshes and actual handwriting latency.

## 12. Benchmark workloads

Create repeatable workloads shared across frameworks.

### Workload A — Simple writing

Write several lines of normal handwriting.

Assess:

- Naturalness
- Latency
- Stroke continuity
- Pressure response

### Workload B — Fast handwriting

Write quickly, including cursive and short abbreviations.

Assess:

- Missing points
- Broken stroke segments
- Excessive smoothing
- Preview lag

### Workload C — Slow precision drawing

Draw circles, straight lines, diagrams, and small lettering.

Assess:

- Accuracy
- Wobble
- Pressure fidelity
- Endpoint handling

### Workload D — Large document

Load progressively larger benchmark scenes:

- 100 strokes
- 1,000 strokes
- 5,000 strokes
- 10,000 strokes

Measure:

- Load time
- Render time
- Responsiveness while adding strokes
- Memory use

Use the same fixture data and drawing region across implementations.

### Workload E — Long continuous stroke

Draw a continuous line for approximately 15 seconds.

Verify:

- No buffer overflow
- No visible interruption
- No large spike on pen-up
- No missing points in the committed result

### Workload F — Palm rejection

Rest the writing hand on the display while drawing.

Count unwanted marks, missing stylus input, and unexpected touch navigation.

### Workload G — BOOX preview handoff

Draw ten separate short strokes, one after another.

Check:

- No disappearance on pen-up
- No duplicate ink
- No cumulative offset
- No persistent ghosting
- No excessive refresh between strokes

### Workload H — BOOX viewport mapping

Draw around the edges and corners of the native writing region.

Then repeat under a non-1:1 transform where supported.

Verify that hardware preview and stored software ink remain aligned.

## 13. Benchmark methodology

Use release/profile builds for final comparisons, not debug builds.

On each device:

1. Record the software versions and device settings.
2. Use the same drawing region and approximate brush size.
3. Warm up the application.
4. Run each benchmark multiple times.
5. Alternate framework testing order between sessions.
6. Use identical ePaper refresh settings.
7. Collect logs and performance metrics.
8. Note thermal state and other possible confounders.
9. Repeat any surprising result.
10. Save the original results, not just calculated summaries.

Aim for at least three measurement sessions per device and multiple repeated trials per workload.

On BOOX, explicitly compare both applications in hardware-accelerated mode and both in normal software-rendering mode.

Use the manufacturer's native Notes application as an additional subjective latency reference.

Do not use its experience as a claim that equivalent latency is technically guaranteed for third-party applications.

### User-facing evaluation

Provide a simple feedback sheet where the user can rate each build from 1–5 on:

- Writing naturalness
- Perceived latency
- Stroke smoothness
- Pressure handling
- Palm rejection
- Visual stability
- Overall comfort

Collect these ratings independently for each device.

## 14. Implementation phases

### M00.0 — Bootstrap and inventory

- Create repository.
- Initialize Flutter and Tauri applications.
- Configure Android, Linux, and iPadOS targets.
- Record dependency versions.
- Create shared benchmark fixtures.
- Implement a basic debug UI.

**Exit:** Both desktop applications launch and the mobile projects have valid build configurations.

### M00.1 — Basic drawing parity

- Implement pen drawing.
- Implement pressure-dependent width.
- Implement eraser and clear.
- Implement stroke JSON import/export.
- Implement common sample fixtures.
- Implement basic performance counters.

**Exit:** Both applications can draw and replay equivalent strokes on Linux.

### M00.2 — Android BOOX reference and SDK proof

Before integrating Flutter or Tauri, build a standalone native Kotlin proof using the Onyx SDK.

- Resolve compatible SDK dependencies.
- Detect the device.
- Create `TouchHelper`.
- Receive raw stroke callbacks.
- Produce actual hardware-assisted ink.
- Commit strokes to a software canvas.
- Clear transient ink correctly.
- Verify pen-up handoff.
- Test cleanup when exiting.

**Exit:** Native drawing works on the actual BOOX device with persistent strokes and no serious handoff defects.

This phase establishes whether the Onyx SDK can deliver the required behavior independently of either framework.

### M00.3 — Shared BOOX module

- Extract the working native functionality into `android/boox-ink`.
- Define stable host APIs.
- Preserve the functioning raw-drawing lifecycle.
- Add capability detection and diagnostics.
- Add fallback handling.

**Exit:** A standalone Android test host can consume the shared module and draw successfully.

### M00.4 — Flutter BOOX integration

- Embed the native View.
- Attach the shared Kotlin library.
- Implement mode switching.
- Pass finalized strokes to Flutter.
- Verify stroke preservation.
- Verify view cleanup.
- Record benchmark metrics.

**Exit:** Flutter provides standard drawing and actual BOOX hardware-accelerated drawing on the same device.

### M00.5 — Tauri BOOX integration

- Implement an Android native plugin.
- Host the native BOOX drawing View.
- Align it with the Tauri canvas.
- Integrate the shared Kotlin library.
- Pass finalized strokes to the web UI.
- Verify lifecycle and drawing modes.
- Record benchmark metrics.

**Exit:** Tauri provides standard drawing and actual BOOX hardware-accelerated drawing on the same device.

If native View hosting proves impractical, document the precise blocker rather than disguising it with a software fallback.

### M00.6 — iPadOS testing

- Build both applications.
- Test Apple Pencil.
- Record pressure and input capabilities.
- Measure rendering behavior.
- Compare with a native UIKit/PencilKit reference if necessary.

**Exit:** Both apps are evaluated using real Apple Pencil input, or the unavailable build environment is explicitly recorded as a test blocker.

### M00.7 — Linux testing and large-scene performance

- Test both desktop builds.
- Verify input and rendering.
- Run all automated large-scene workloads.
- Inspect rendering bottlenecks.
- Record memory and CPU behavior.

**Exit:** Comparable desktop benchmark data exists.

### M00.8 — Final evaluation

- Consolidate benchmark results.
- Produce comparison tables.
- Document architectural differences.
- Summarize device limitations.
- Recommend the application framework.
- Recommend an M01 input architecture.
- Document native integration requirements.

**Exit:** The team can make a framework decision based on demonstrated functionality and test evidence.

## 15. Decision criteria

Evaluate frameworks using the following suggested weights.

| Category | Weight |
|---|---:|
| BOOX low-latency handwriting | 30% |
| iPad Apple Pencil experience | 25% |
| Linux drawing experience | 15% |
| Maintainability and native integration | 15% |
| Resource usage | 10% |
| Cross-platform development workflow | 5% |

Scores alone are not sufficient.

A framework should not win merely because it performs well on Linux while providing a poor handwriting experience on BOOX or iPadOS.

### Hard requirements

Before selecting a framework, verify:

- Functional pen drawing on supported target devices.
- Reliable stroke preservation.
- No repeatable stroke-loss or duplication bugs.
- Correct physical-to-logical coordinate mapping.
- Usable mobile touch/stylus interaction.
- A viable path for native BOOX integration.
- A maintainable integration architecture.

A missing essential feature outweighs minor performance differences.

### Recommendation rules

**Prefer Tauri when:**

- Handwriting quality and latency are comparable to Flutter.
- The BOOX native View integration is reliable.
- Apple Pencil input works adequately.
- Rust integration and frontend maintainability offer meaningful advantages.

**Prefer Flutter when:**

- It has consistently better stylus handling or rendering.
- Native View integration is substantially easier or more reliable.
- Its performance is more consistent across target devices.
- Tauri has material limitations in its native View or WebView input path.

**Consider a hybrid/native architecture when:**

- Both frameworks require substantial native drawing surfaces on mobile.
- Neither framework provides acceptable stylus quality without platform-specific implementations.
- The framework is effectively becoming a shell around separate native editors.

Document whether the recommended production architecture should use the shared native input module, a framework-native renderer, or both.

## 16. Final deliverables

Produce:

1. Complete Flutter prototype.
2. Complete Tauri prototype.
3. Shared BOOX Kotlin ink module.
4. Standalone native BOOX reference application.
5. Installable BOOX APKs for both frameworks.
6. Installable iPadOS builds or complete device-build instructions.
7. Runnable Linux builds.
8. Shared stroke fixture files.
9. Automated test suites.
10. Performance logs in JSON or CSV.
11. Device test checklist.
12. BOOX SDK setup documentation.
13. A framework comparison report.
14. An explicit recommended architecture for M01.

Include a comparison matrix:

| Device / Mode | Flutter | Tauri |
|---|---|---|
| Linux mouse drawing | | |
| Linux stylus, if available | | |
| iPad Apple Pencil | | |
| BOOX standard drawing | | |
| BOOX accelerated drawing | | |
| Large-document rendering | | |
| Stroke persistence | | |
| Native integration stability | | |

Populate only with actual results.

## 17. Agent execution instructions

Implement the project phase by phase.

Prioritize the BOOX SDK proof and the two native integration paths. This is the most important architectural risk and must not be deferred until the end.

Keep both applications usable throughout development.

Use the actual SDK APIs and example implementations rather than guessing method signatures.

Preserve a reusable Kotlin BOOX ink module.

Do not optimize one framework extensively while leaving the other as a basic demonstration. Maintain functional parity for fair comparison.

Do not claim simulated or internal timing results are equivalent to physical pen-to-display latency.

Do not claim Apple Pencil hardware was tested without an actual iPad test.

Continue through all achievable phases without requesting routine confirmation.

If the environment lacks a device, platform toolchain, SDK dependency, signing setup, or required firmware capability, complete the available implementation, preserve build instructions, and record the precise blocker.

The final output must include working code and evidence-based technical findings.

**Start with M00.0, then prioritize M00.2 and the shared BOOX native drawing module before performing final framework comparisons.**
