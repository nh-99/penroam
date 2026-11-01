# M00 architecture

> **Updated 2026-11-01.** Both frameworks are now evaluated as application shells that host the
> best native handwriting backend on each OS. Status and risks are in `docs/status.md`;
> PencilKit/Onyx/canonical interop is in `docs/interop.md`.

## Target backends per OS

| Platform | Handwriting backend | Shared implementation | Flutter host | Tauri host |
|---|---|---|---|---|
| iPadOS | PencilKit `PKCanvasView` | `ios/InkKit` → `PencilInk` | UIKit Platform View (M00.6C, todo) | `tauri-plugin-pencilkit` (Swift, native view above WKWebView) |
| BOOX Android | Onyx Pen SDK `TouchHelper` raw drawing | `android/boox-ink` → `BooxInkView` Mode B | Android Platform View | `tauri-plugin-boox-ink` (Kotlin overlay) |
| Other Android | Native software View (Mode C) or framework canvas | `BooxInkView` Mode C | same | same |
| Linux / Windows | Framework canvas | — | CustomPainter | Canvas 2D |
| macOS | Framework canvas (native optional) | — | CustomPainter | Canvas 2D |

## InkBackend contract

| Contract | Kotlin `BooxInkView` | Swift `PencilInkBackend` | TS `InkEngine` / `NativeInkBackend` | Dart `InkController` |
|---|---|---|---|---|
| initialize | `BooxInk.init(app)` + constructor | `init()` | `new InkEngine(el)` / `NativeInkBackend.forMode()` | constructor |
| attachSurface / detachSurface | `attach()` / `detach()` (raw drawing open/close) | `attachSurface(to:frame:)` / `detachSurface()` | plugin `attach` / `detach` | Platform View lifecycle |
| setTool | `setBrush(width, pressure, tool)` | `setTool(_:width:)`, `setFingerDrawing` | `setSettings` / `setBrush` | fields + `setRenderStyle` |
| setViewport | `setViewTransform(scale, ox, oy, dpr)` | `setViewport(scale:offsetX:offsetY:)` | `setTransform` | `setTransform` |
| clear | `clearCanvas()` | `clear()` | `clear()` | `clear()` |
| loadStrokes / exportStrokes | JSON document | `[InkStroke]` (+ native `PKDrawing` sidecar) | `setStrokes` / `toDocument` | `setStrokes` / `toDocument` |
| getCapabilities / getMetrics | JSON | dictionaries | `apis` / `metrics.toJSON()` | `metrics.toJson()` |
| events | `Listener.onStrokeCommitted/onStrokesErased/onViewportChanged/onStatusChanged` | `onStrokesCommitted/onStrokesErased/onViewportChanged/onEvent` | plugin events `stroke(s)`, `erased`, `viewport`, `status` | MethodChannel `onStroke`… |
| dispose | view detach + lifecycle observers | `dispose()` | `dispose()` | — |

Native backends only report **completed** strokes to the host. Pen samples are never routed
through Dart, JS or Rust before they are visible.

## Components

```text
            ┌────────────── Flutter app ──────────────┐   ┌──────────────── Tauri app ────────────────┐
 Mode A     │ Listener → InkController → CustomPainter │   │ Pointer Events → InkEngine → Canvas 2D     │
 (framework)│   (committed Picture chunks + active     │   │   (committed / active / tail canvases)     │
            │    layer, separate RepaintBoundaries)    │   │   React renders only toolbar + debug       │
            └──────────────┬───────────────────────────┘   └───────────────┬────────────────────────────┘
 Mode B / C (Android)      │ Platform View (HC/TLHC/AndroidView)           │ Kotlin plugin: native View
                           │ MethodChannel: finalized strokes only         │ added above the WebView;
                           ▼                                               ▼ plugin events: finalized strokes
            ┌──────────────────────── android/boox-ink : BooxInkView ─────────────────────────┐
            │ Mode B: Onyx TouchHelper raw drawing (hardware preview, Path A)                  │
            │         + StrokeAssembler (move & list streams) → persistent bitmap (Path B)     │
            │ Mode C: MotionEvent + Canvas (native software baseline)                          │
            │ OnyxBridge.kt is the only file that touches Onyx classes                         │
            └──────────────────────────────────────────────────────────────────────────────────┘
```

* **The two frameworks share no drawing code.** Each one implements the same spec in its own
  language, so the measurements reflect what each framework actually does.
* **What is shared:** fixtures, the stroke format, the geometry spec (golden file), the
  benchmark event names, and the native Android module.
* **No per-sample IPC in Modes B and C.** The native view sends each stroke to the host once,
  when it is committed. In Mode B the hardware preview path never touches Dart, JS or Rust.

## Canonical stroke format

```json
{
  "format": "m00-strokes", "version": 1, "meta": {},
  "strokes": [{
    "id": "uuid", "source": "stylus|mouse|touch|fixture",
    "input": { "api": "flutter-pointer|web-pointer|onyx-touchhelper|android-motionevent|generated",
               "device": "...", "...": "diagnostics (stream used, sample counts, coalesced, ...)" },
    "brush": { "type": "pen", "width": 3.0, "color": "#000000", "pressure": true },
    "points": [{ "x": 120.0, "y": 240.0, "pressure": 0.62, "tiltX": 0.0, "tiltY": 0.0, "timeUs": 0 }]
  }]
}
```

* `x`, `y` are in **document units**: logical pixels at zoom 1.0. The native module divides
  physical px by the host's devicePixelRatio, so all three hosts agree.
* `brush.pressure` records whether pressure affected the width when the stroke was drawn.
  Mouse strokes are always `false`.
* `input` keeps the source metadata, so native SDK samples, Flutter samples and WebView samples
  can be told apart in results.

### Normalization per source

| Source | pressure | tilt | timeUs |
|---|---|---|---|
| Flutter `PointerEvent` | `(p - pressureMin) / (pressureMax - pressureMin)`; mouse → 0.5. Raw min/max are recorded in `input` and in `metrics.info.device_*` | `tilt` (radians from perpendicular) and `orientation` converted to W3C tiltX/tiltY degrees | `PointerEvent.timeStamp` (engine clock) minus the first sample |
| Web `PointerEvent` | `pressure` (already 0..1; 0 → 0.5 for hover-less pens); mouse → 0.5 | `tiltX` / `tiltY` (degrees) as delivered; `twist` presence flagged | `event.timeStamp` (DOMHighResTimeStamp) minus the first sample |
| Onyx `TouchPoint` | `pressure / EpdController.getMaxTouchPressure()` (falls back to raw if max ≤ 1) | `tiltX` / `tiltY` ints copied as reported. **Units are unverified** until tested on a device | `timestamp` in ms (the epoch depends on firmware; it is only used for spacing between samples) |
| Android `MotionEvent` (Mode C) | `getPressure()`, already 0..1 | `AXIS_TILT` + `AXIS_ORIENTATION` converted to tiltX/tiltY degrees | `eventTime` (uptime ms) minus the first sample |

Predicted samples (web `getPredictedEvents()`) are drawn only on the temporary tail layer at
50% opacity and are never committed.

## Brush geometry (identical in all ports)

* Width: `pressureEnabled ? max(0.5, base * (0.4 + 1.2 * p)) : base`. At p = 0.5 this gives
  exactly the nominal width; the range is 0.4× to 1.6×.
* Smoothing: midpoint quadratic. A stroke is cut into pieces: a line P0→M01, then quads
  M(i-1,i) –Pi→ M(i,i+1), then a line M(n-2,n-1)→Pn-1. Each piece is stroked with round
  caps using the width of its anchor sample.
* One point → a dot (filled circle). Two points → one line.
* Piece *k* depends only on points up to *k+1*. So when a sample arrives, every piece except
  the trailing line is final. This is what makes incremental preview rendering possible.
  Smoothing adds no sample latency: only the last half-segment is provisional.
* Stroke eraser: removes every stroke whose segments come within `8 / zoom + width/2`
  document units of an eraser sample. The rule is the same in all ports.

The reference is `fixtures/generate-fixtures.mjs`. Ports are tested against
`fixtures/geometry-golden.json`:

* `tauri_app/tests/geometry.test.ts`
* `android/boox-ink/src/test/.../StrokeGeometryTest.kt`
* `flutter_app/test/geometry_test.dart`

## Rendering pipelines

| Concept | Tauri (Canvas 2D) | Flutter | Native (Kotlin) |
|---|---|---|---|
| Raw samples | `active.stroke.points` + event timestamps, outside React state | `_Active.stroke.points` | `StrokeAssembler` (B) / `active` list (C) |
| Active preview | `active` canvas: stable pieces drawn incrementally. `tail` canvas: provisional piece + predicted samples, cleared every rAF | `_ActivePainter` in its own RepaintBoundary. Stable pieces are recorded into `ui.Picture` chunks of 64; only the remainder is drawn each frame | B: Onyx hardware. C: stable pieces go straight into the bitmap; the tail is drawn in `onDraw` |
| Committed strokes | `engine.strokes` | `InkController.strokes` | `BooxInkView.strokes` |
| Rendered cache | `committed` canvas. Full rebuild only on transform, erase, load or resize; a commit draws one stroke | `ui.Picture` chunks (64 strokes) in document space. Transforms re-paint the pictures without re-recording them | `Bitmap` in view space. Rebuilt on transform or erase |
| Scheduling | `requestAnimationFrame`. One sample stream (`pointerrawupdate` if available, otherwise `pointermove`) plus `getCoalescedEvents()` | `CustomPainter(repaint: Tick)`: no widget rebuild per event | `invalidate(rect)` |

## Coordinates and zoom

All hosts share the view transform `viewPx = (doc * scale + offset) * dpr`, with the offset in
logical px. The native view applies the host's transform and reports its own pan and pinch
back through `onViewportChanged`. The hardware stroke width is `brushWidth * scale * dpr`.

The Onyx SDK is expected to report `TouchPoint` coordinates relative to the View. This is
**verified at runtime, not assumed**. On the first stroke, `BooxInkView` compares the raw begin
point with the stylus `MotionEvent` DOWN and records `rawCoordinateSpace` as `view`, `screen`
or `mismatch(...)`. The result shows in the debug status line and is stored with each stroke.

## Measurement model

The monotonic clocks are `performance.now()`, a Dart `Stopwatch`, and
`SystemClock.elapsedRealtimeNanos()`. Timestamps from different clocks are never subtracted,
with two checked exceptions:

1. Web `event.timeStamp` vs `performance.now()`. These share a time origin per spec; this is
   checked at runtime (`timestamp_clock_check`).
2. `MotionEvent.eventTime` vs `SystemClock.uptimeMillis()`. These are the same clock.

The Onyx `TouchPoint.timestamp` epoch is probed (uptime vs wall clock) before any dispatch
delay is recorded.

None of these are pen-to-photon latency. See `benchmarks/protocol.md` §3.
