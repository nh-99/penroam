# Android tablets other than BOOX

Includes Samsung tablets with an S Pen. **Status: implemented, not tried on hardware.** See
[`capabilities.md`](capabilities.md).

## How pen input works

Only BOOX uses a native surface. `BooxInkView` also has a software mode, but it has not been
validated on any other device, so `detectNativeInk` leaves other Android devices on the web
canvas. Live ink there uses `desynchronized` canvases, which Chromium can draw with low latency.

* `getCapabilities` reports `device.stylus` (any input device with `SOURCE_STYLUS`) and
  `device.manufacturer`. Settings uses them to name the path ("S Pen on the web canvas").
  Nothing else depends on them: a pen is handled as a pen whatever the label says.
* An S Pen arrives as `pointerType: "pen"` with pressure and tilt, through `pointerrawupdate` and
  coalesced events.
* The side button reports the right button, and holding it erases (`penErases` in
  `editor/input.ts`). The S Pen has no eraser end, so this is the only pen-side way to erase.
* Hover shows the nib's footprint as a ring (`drawHover`). Hovering never adds to a stroke.
* A stroke that starts with no usable pressure picks it up as soon as real values arrive.

## Known risks

* Everything above comes from how Chromium documents and implements stylus input. None of it has
  been observed on a Samsung device, including the side button mapping.
* With the side button held near the screen, One UI may open Air command before the app sees it.
* S Pen Air actions (remote button, gestures) use Samsung's own SDK and are not supported.
* Palm rejection uses contact size and pen-down rules. The size limit was set for BOOX and has
  not been tuned for Samsung's touch reports.
* Latency has not been measured. If the web canvas proves too slow, the next step is to validate
  the existing native software surface there.

## Lifecycle

Android can kill a backgrounded app. Pages flush on `visibilitychange`, and committed strokes are
already in the journal, so at most the stroke in progress is lost.
