# iPad / iPadOS — M01 status and validation checklist

**Status: implemented, compiled for iOS, not validated on hardware.** A Tauri iOS app build
needs macOS + Xcode, which this environment does not have. Nothing below is claimed to work
on an iPad until the checklist is filled in on a device.

## What exists

* `plugins/tauri-plugin-native-ink/ios/Sources/NativeInkPlugin/NativeInkPlugin.swift` —
  UIKit Pencil preview surface:
  * Pencil-only `UIGestureRecognizer` on the WKWebView (`allowedTouchTypes = [.pencil]`,
    `cancelsTouchesInView`) limited to the editor rect; fingers and palms go to the web view.
  * `PencilPreviewView` (transparent, `isUserInteractionEnabled = false`) draws the live stroke
    with the shared M00 geometry (`InkCore.Geometry`), predicted touches at 50 % alpha.
  * Coalesced touches recorded; `touchesEstimatedPropertiesUpdated` patches force by
    `estimationUpdateIndex` (flag bit 1 on the point).
  * Commit hand-off: stroke stays on the preview until the web editor acks after painting
    (`commitAck`, two frames after commit), 2 s timeout as a safety net.
  * WebKit 120 Hz rendering-update hook from M00 (`WebTuning`, private SPI — App Store risk).
* `ios/InkKit/Sources/PencilSurface` — the platform-independent core (sample mapping,
  estimated force, predicted samples, hand-off queue, overlay frame); **6 Swift tests run on
  Linux**.
* Web side: `detectNativeInk()` uses the iOS surface for pen/highlighter (eraser/select/text
  use the web canvas, which receives Pencil pointer events when the surface is hidden).

## Build evidence (Linux)

```bash
cd ios/InkKit && swift test                       # 14 tests (8 M00 InkCore + 6 PencilSurface): pass
cd plugins/tauri-plugin-native-ink && cp -r <tauri-api> .tauri/   # Tauri's Swift API package
cd ios && DEVELOPER_DIR=~/.swiftpm/swift-sdks/darwin.artifactbundle/Developer \
  swift build --swift-sdk arm64-apple-ios --build-system native --target tauri-plugin-native-ink
# -> "Build of target: 'tauri-plugin-native-ink' complete!" (compiles against the iOS SDK; not linked into an app)
```

## On a Mac

```bash
cd apps/desktop-mobile
pnpm tauri ios init
pnpm tauri ios dev --open           # set the signing team in Xcode
pnpm tauri ios build --export-method debugging
```

## Device validation checklist (fill in; M00 numbers are from the UIKit reference / WKWebView proxy)

| Item | Result | Notes |
|---|---|---|
| Device / iPadOS / Pencil model / display Hz | | M00: iPad13,4 (M1), iPadOS 26.6.2, 120 Hz |
| Xcode / iOS SDK | | |
| App installs, library + editor render | | |
| `webFrameRate.applied` in `getCapabilities` (120 Hz hook worked inside Tauri) | | M00 proxy: 120 Hz with hook, 60 Hz without |
| Pencil draws natively at display rate (preview) | | UIKit reference: 120 Hz |
| Pen-up hand-off: no flash, no double, no gap | | `handoff_timeouts` metric must stay 0 |
| Estimated force updates applied (`estimated_force_updates` > 0) | | M00: 1,747 updates in a session |
| Coalesced samples (sample rate from median dt) | | M00: 240 Hz coalesced, 0 gaps |
| Fingers pan/zoom, never draw; palm resting (Workload F) | | |
| 15-second stroke (Workload E) | | |
| Fast cursive (Workload B), circles/small lettering (C), normal writing (A) | | |
| App background/resume mid-stroke | | |
| Text tool with Pencil (Scribble off) and keyboard; no accidental ink | | |
| Rotation / Split View: overlay frame follows the canvas | | |
| `.penroam` export on iPad → import on Linux (`tools/compare-penroam.py`) | | |

**The 60 vs 120 Hz question is open for M01:** the web canvas commits at the WKWebView's
rAF rate (60 Hz unless the private hook applies); the native preview is meant to give 120 Hz
live ink regardless. Neither has been measured inside a packaged Tauri iOS app.
