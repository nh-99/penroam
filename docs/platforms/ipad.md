# iPad / iPadOS — status and validation checklist

**Status: runs on hardware, checklist open.** Penroam has been installed on an iPad13,4 from Linux
with xtool (below) and used with an Apple Pencil, which is how the palm rules here were tuned. The
checklist at the end has not been filled in, and a build signed through Xcode has not been made.

## What exists

* `plugins/tauri-plugin-native-ink/ios/Sources/NativeInkPlugin/NativeInkPlugin.swift` —
  UIKit Pencil preview surface:
  * Pencil-only `UIGestureRecognizer` on the WKWebView (`allowedTouchTypes = [.pencil]`,
    `cancelsTouchesInView`) limited to the editor rect; fingers and palms go to the web view.
    WKWebView still delivers the Pencil as pen pointer events, so the editor draws nothing from
    them while this surface is visible; otherwise every stroke would be committed twice.
    A `pen` event reports pen-down and pen-up: the editor uses it to switch fingers to pan and
    zoom and to ignore palms while the Pencil is down.
    The contact-size palm filter is off on iOS: WebKit reports twice the touch radius, so
    ordinary fingertips exceed the limit. Contacts that land within 400 ms of the Pencil lifting
    are ignored instead, and pen-down drops any pan, pinch or finger stroke already under way.
    A palm that lands later than that can still pan the page until the Pencil touches down.
  * `UIPencilInteraction`: a double-tap follows the system setting. "Switch to eraser" and "Switch
    to previous tool" work; the palette and shortcut actions are left alone.
  * `PencilPreviewView` (transparent, `isUserInteractionEnabled = false`) draws the live stroke
    with the shared stroke geometry (`InkCore.Geometry`), predicted touches at 50 % alpha.
  * Coalesced touches recorded; `touchesEstimatedPropertiesUpdated` patches force by
    `estimationUpdateIndex` (flag bit 1 on the point). Updates that arrive after pen-up are
    dropped, because the stroke has already been committed; `input.pendingEstimates` counts them.
  * Writing uses only the bottom quarter of the Pencil's force range (on an iPad13,4: median 0.11
    of maximum, 95th percentile 0.22), so linear pressure gave thin, nearly uniform ink. Pressure is
    stored through a curve, `pencilPressure`, that puts ordinary writing mid-range. It is shared with
    the web path and recorded as `input.pressureCurve`; with `input.maximumForce` the raw force can
    be recovered. Older strokes keep their linear values and look as they did.
  * Commit hand-off: stroke stays on the preview until the web editor acks after painting
    (`commitAck`, two frames after commit), 2 s timeout as a safety net.
  * WebKit 120 Hz rendering-update hook (`WebTuning`, from the framework evaluation; private SPI — App Store risk).
* `ios/InkKit/Sources/PencilSurface` — the platform-independent core (sample mapping,
  estimated force, predicted samples, hand-off queue, overlay frame); **6 Swift tests run on
  Linux**.
* Web side: `detectNativeInk()` uses the iOS surface for pen/highlighter (eraser/select/text
  use the web canvas, which receives Pencil pointer events when the surface is hidden).

## Printing

`plugins/tauri-plugin-native-print/ios` shows the system print sheet
(`UIPrintInteractionController`). It compiles against the iOS SDK but has not been run on a
device. An app shell that links Swift plugin packages by hand has to add this package as well, or
linking fails on `init_plugin_native_print`.

## Building the iOS pieces on Linux

```bash
cd ios/InkKit && swift test                       # InkCore + PencilSurface tests
cd plugins/tauri-plugin-native-ink && cp -r <tauri-api> .tauri/   # Tauri's Swift API package
cd ios && DEVELOPER_DIR=~/.swiftpm/swift-sdks/darwin.artifactbundle/Developer \
  swift build --swift-sdk arm64-apple-ios --build-system native --target tauri-plugin-native-ink
# -> "Build of target: 'tauri-plugin-native-ink' complete!" (compiles against the iOS SDK; not linked into an app)
```

## On Linux, without a Mac

xtool builds SwiftPM only, so the app shell lives in a git-ignored folder that links the Rust
static library with the Swift plugin packages. xtool's team prefix ends up in the bundle ID.

## Release builds

`release.yml` builds and uploads to TestFlight on a macOS runner; see
[`../development.md`](../development.md#releases). `src-tauri/Info.ios.plist` holds what the
generated project needs added: the sign-in URL scheme and the photo and camera usage strings.
Export compliance for the encryption is answered in App Store Connect, not set in the plist.

## On a Mac

```bash
cd apps/desktop-mobile
pnpm tauri ios init
# gen/apple is not checked in and starts with Tauri's default icon; install ours
cp src-tauri/icons/ios/*.png src-tauri/gen/apple/Assets.xcassets/AppIcon.appiconset/
pnpm tauri ios dev --open           # set the signing team in Xcode
pnpm tauri ios build --export-method debugging
```

Repeat the icon copy whenever `gen/apple` is regenerated. `AppIcon-512@2x.png` is the 1024 px App
Store icon and must stay opaque RGB with no alpha channel, because App Store validation rejects
any alpha.

## Hover

iPadOS delivers Pencil hover to the web view on hardware that has it (M2 iPads and later), and the
editor's hover ring handles it there. The iPad13,4 cannot hover, so this has not been observed.

## Device validation checklist (fill in; reference numbers are from the framework evaluation's UIKit reference / WKWebView proxy)

| Item | Result | Notes |
|---|---|---|
| Device / iPadOS / Pencil model / display Hz | | reference: iPad13,4 (M1 chip), iPadOS 26.6.2, 120 Hz |
| Xcode / iOS SDK | | |
| App installs, library + editor render | | |
| `webFrameRate.applied` in `getCapabilities` (120 Hz hook worked inside Tauri) | | reference proxy: 120 Hz with hook, 60 Hz without |
| Pencil draws natively at display rate (preview) | | UIKit reference: 120 Hz |
| Pen-up hand-off: no flash, no double, no gap | | `handoff_timeouts` metric must stay 0 |
| Estimated force updates applied (`estimated_force_updates` > 0) | | reference: 1,747 updates in a session |
| Coalesced samples (sample rate from median dt) | | reference: 240 Hz coalesced, 0 gaps |
| Fingers pan/zoom, never draw; palm resting (Workload F) | | |
| 15-second stroke (Workload E) | | |
| Fast cursive (Workload B), circles/small lettering (C), normal writing (A) | | |
| Double-tap: eraser and previous tool, per the system setting | | |
| App background/resume mid-stroke | | |
| Text tool with Pencil (Scribble off) and keyboard; no accidental ink | | |
| Rotation / Split View: overlay frame follows the canvas | | |
| `.penroam` export on iPad → import on Linux (`tools/compare-penroam.py`) | | |

**The 60 vs 120 Hz question is still open:** the web canvas commits at the WKWebView's
rAF rate (60 Hz unless the private hook applies); the native preview is meant to give 120 Hz
live ink regardless. Neither has been measured inside a packaged Tauri iOS app.
