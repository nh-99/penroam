# iPadOS / Apple Pencil testing

> **Status (2026-11-01):**
> * **Device:** iPad Pro 11" 3rd gen (`iPad13,4`, M1, model MHQR3), iPadOS 26.6.2 (23G90),
>   120 Hz ProMotion. Apple Pencil 2 is the compatible model; confirm which pencil is used at test time.
> * **Flutter and Tauri iPad builds: still blocked.** They need macOS + Xcode. xtool (installed on the
>   Linux machine) builds SwiftPM apps only and cannot drive `xcodebuild`.
> * **Available on Linux via xtool:** `ios/M00iOS`, which has a native UIKit Pencil reference and the
>   Tauri frontend running in WKWebView (a proxy for the Tauri iPad input path, not the Tauri
>   app). It is installed on the iPad (after the Account Holder accepted the updated Apple PLA) and is
>   driven over USB with `ios/M00iOS/tools/m00ctl.py` (`xtool launch` cannot attach a debugger).
> * Automated Workload D: 3 sessions per engine; see docs/comparison.md. **No Apple Pencil workloads have been run yet.**

## Build

### On Linux with xtool (native reference + Tauri-frontend WKWebView proxy)
See `ios/M00iOS/README.md`: `tools/build_web.sh && xtool dev`, then collect the results with
`tools/collect_results.py`.

### Flutter (macOS only)
```bash
cd flutter_app && tool/bootstrap.sh          # generates ios/ via flutter create
open ios/Runner.xcworkspace                   # set Team / signing
flutter build ios --profile                   # or: flutter run -d <ipad-id> --profile
```

### Tauri (macOS only)
```bash
cd tauri_app && npm install
npx tauri ios init                            # generates src-tauri/gen/apple (macOS only)
npx tauri ios dev --open                      # set signing team in Xcode, run on the iPad
npx tauri ios build --export-method debugging
```
The BOOX plugin registers nothing on iOS. Its `call` command returns an "Android only" error,
and the mode selector only offers Mode A on iOS.

## What each app logs about the Pencil

* **Flutter:** on the first down of each device kind, `metrics.info.device_stylus` records
  `pressureMin/Max`, `pressure`, `tilt`, `orientation`, `radiusMajor` and `distanceMax`. Each
  stroke records the sample count and dt median/max (sample frequency = 1000 / median dt).
* **Tauri:** `metrics.info.apis` records which of these exist in WKWebView: `pointerrawupdate`,
  `getCoalescedEvents`, `getPredictedEvents`. Each stroke records `samples` and
  `coalescedSamples`. `timestamp_clock_check` verifies that `event.timeStamp` and
  `performance.now()` share a clock.

## Checklist (fill in per device)

| Item | Flutter | Tauri | Notes (native UIKit ref / WKWebView proxy) |
|---|---|---|---|
| Device / iPadOS / Pencil model / display Hz | | | |
| Xcode / iOS SDK version | | | |
| Pencil draws | | | |
| Finger pans/zooms, does not draw | | | |
| Pressure varies the width (pressureMin/Max seen) | | | native ref: maximumPossibleForce 4.17; 1,747 estimated-force updates |
| Tilt reported (non-zero tiltX/Y) | | | |
| Palm resting while writing (Workload F) — unwanted marks | | | |
| Long stroke 15 s (Workload E) — gaps / spikes | | | |
| Fast short strokes (Workload B) — missing segments | | | |
| Sample rate (Hz) from median dt | | | native ref: 120–240 Hz (dt 4–8 ms), 0 gaps |
| Coalesced samples available | n/a (engine delivers each sample) | | |
| Predicted events available | n/a | | |
| Hover / double-tap / squeeze | not required for M00; mark unavailable unless observed | | |

## Native reference

This is implemented in `ios/M00iOS` (Native UIKit engine): `coalescedTouches(for:)`,
`predictedTouches(for:)` (temporary preview only), and `touchesEstimatedPropertiesUpdated`.
Compare its per-stroke sample counts and `medianDtMs` with the WKWebView proxy and, later, with
Flutter on the same strokes. It is diagnostic only and not part of either framework's
production path.
