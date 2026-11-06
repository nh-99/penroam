# Linux desktop

## Display servers: X11 and Wayland

Penroam has no display-server-specific code: Tauri/tao, GTK 3 and WebKitGTK pick the backend
(GDK: `GDK_BACKEND` if set, otherwise Wayland when a compositor is present, else X11). Both
are supported targets. `system_info` reports the backend actually in use (`displayServer`).

Wayland and X11 (including XWayland via `GDK_BACKEND=x11`) are both supported. Pen-tablet
pressure (XInput2 / tablet-v2) has not been tested with real hardware yet.

Both report `getCoalescedEvents` and `getPredictedEvents`; neither has `pointerrawupdate`
(WebKitGTK 2.52), so `pointermove` + coalesced events is the sample stream on Linux.

CI (`.github/workflows/ci.yml`) starts the app on **Xvfb (X11)** and on **headless Weston
(Wayland)** and runs the quick benchmark on each: the release binary on `main`, a debug build on
other branches.

Known WebKitGTK issue (not seen here; this machine renders on the AMD iGPU): blank or white
windows on Wayland with some NVIDIA proprietary driver setups. Workaround:
`WEBKIT_DISABLE_DMABUF_RENDERER=1 penroam-app`. Not applied by default because it slows
rendering everywhere else.

## Display scale

Moving the window to a display with another scale keeps its CSS size, so the editor watches a
`resolution` media query as well as its size. Not tried on a real second display.

## Printing

Printing uses GTK's print dialog and sends a PDF to CUPS, so GTK only lists printers that accept
PDF. See [`../architecture/overview.md`](../architecture/overview.md) §8. To test it without
paper, run a private `cupsd` with a file-backed queue, set `CUPS_SERVER` for the app, and set
`PreserveJobFiles Yes` to keep the spooled documents for inspection.

## Build / run

```bash
pnpm install
pnpm tauri build --no-bundle && target/release/penroam-app
pnpm tauri build                     # .deb + AppImage
```

## Real-input E2E

`tools/linux-e2e/` drives the release binary with XTest mouse and keyboard events (X11 only) and
captures screenshots, usually inside a nested Xephyr server. Session scripts are in
`tools/linux-e2e/sessions/`; see [`../development.md`](../development.md#linux-real-input-e2e).
