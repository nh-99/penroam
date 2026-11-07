# Device capabilities

What each device class does, and how much of it has been checked by hand. A device is listed as
tested only with real pen hardware. Everything else is what the code is built to do.

Release status and packages for each platform are in [`release-matrix.md`](release-matrix.md).

## Support levels

| Level | Meaning |
|---|---|
| Tested | Native pen hardware, core tools, import/export, sync and lifecycle checked by hand on the current code |
| Compatible | Runs and can edit and export. Pen behaviour is implemented but limits are documented, or it has not been tried on real hardware |
| Interoperable | Exchanges files only; the app does not run there |

**No device is at the Tested level for the current code yet.** The checks to get there are in
[`../benchmarks/protocol.md`](../benchmarks/protocol.md#device-scorecard).

## Matrix

| Device | Ink path | Pressure | Tilt | Hover | Eraser | Palm | Hands-on evidence |
|---|---|---|---|---|---|---|---|
| iPad with Apple Pencil (iPad13,4) | Native UIKit preview, web canvas commits | Yes | Yes | Web ring, needs M2 hardware | Double-tap, per the system setting | Pencil-down rules | Pencil and palm use on earlier builds; checklist in [`ipad.md`](ipad.md) open |
| BOOX Note Air5 C | Onyx hardware preview, native bitmap commits | Yes | Recorded from Onyx, not used for width | No | Pen eraser end | Fingers only pan and zoom | Earlier builds; regional erase refresh not yet tried |
| Samsung tablet with S Pen | Web canvas | Implemented | Implemented | Implemented | Hold side button | Contact size, pen-down rules | **None** |
| Other Android stylus tablets | Web canvas | Where reported | Where reported | Where reported | Eraser end or side button | Contact size, pen-down rules | **None** |
| Linux (X11, Wayland) | Web canvas | Implemented | Implemented | Implemented | Eraser end or side button | Not applicable | Mouse and XTest only; **no pen** |
| Windows, macOS | Web canvas | Implemented | Implemented | Implemented | Eraser end or side button | Not applicable | **None** |
| reMarkable, Supernote and similar | None | | | | | | Not supported: no supported install, input and rendering path. Exchange files through a device that runs Penroam |

"Implemented" means the Pointer Events path handles it and unit tests cover it, with events
simulated. **Settings → General** shows the path in use on the device.
