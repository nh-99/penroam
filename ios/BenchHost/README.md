# BenchHost — iPad benchmark harness (built on Linux with xtool)

xtool builds SwiftPM apps only. It cannot run `xcodebuild`, so the Flutter iOS runner and
`tauri ios build` still need macOS. This app covers what can be measured from Linux:

| Engine | What it is | Counts as |
|---|---|---|
| **Native UIKit** | `NativeInkView`: touches with coalesced + predicted touches, estimated-force updates, the shared geometry (Swift port in `InkCore`), CADisplayLink at 120 Hz | PLAN §9 *optional native Apple Pencil reference* (a diagnostic baseline) |
| **WKWebView (Tauri frontend)** | the unmodified `tauri_app/dist` bundle served from a custom `m00://` scheme (the same shape as Tauri's iOS asset protocol), with a Swift bridge answering the same command names as the Tauri Rust backend | **A proxy for the Tauri iPad build.** It exercises the same WKWebView Pointer Events + Canvas 2D path, but it is **not** the Tauri app (no Tauri runtime, no Rust) |

There is no Flutter engine here. Flutter iPad results still need macOS.

## Build / run

```bash
source ~/.local/share/swiftly/env.sh
swift test                      # InkCore vs fixtures/geometry-golden.json + scene-100/1000 (runs on Linux)
tools/build_web.sh              # npm run build in tauri_app, copy dist/ into the bundle
xtool dev                       # build, sign, install, launch on the USB-connected iPad
tools/collect_results.py        # in another terminal: reassembles exported perf JSON from the device log
```

Exports are written to the app's Documents folder (Files app → On My iPad → Bench Host) and
streamed to the device log as `BENCHX|name|i|n|base64` chunks for `collect_results.py`.

## Metrics specific to this harness

* `event_to_handler_ms`: `CACurrentMediaTime() - UITouch.timestamp`. These are the same clock.
* `event_to_expected_present_ms`: the oldest pending touch timestamp → `CADisplayLink.targetTimestamp`
  of the next frame. This is when the frame is *scheduled* to appear. It is still not
  pen-to-photon latency; use the camera method for that.
* `estimated_force_updates`: Pencil force refinements that arrive after the sample. Pressure is
  patched in the stroke data. Ink that is already drawn is not redrawn.
* `pencil_double_tap`, `pencil_squeeze`, `hover_events`: capability evidence. A counter that
  stays at zero after trying the gesture means the hardware does not support it.
