# Linux desktop

## Display servers: X11 and Wayland

Penroam has no display-server-specific code: Tauri/tao, GTK 3 and WebKitGTK pick the backend
(GDK: `GDK_BACKEND` if set, otherwise Wayland when a compositor is present, else X11). Both
are supported targets. `system_info` reports the backend actually in use (`displayServer`).

Wayland and X11 (including XWayland via `GDK_BACKEND=x11`) are both supported. Pen-tablet
pressure (XInput2 / tablet-v2) has not been tested with real hardware yet.

Both report `getCoalescedEvents` and `getPredictedEvents`; neither has `pointerrawupdate`
(WebKitGTK 2.52), so `pointermove` + coalesced events is the sample stream on Linux.

CI (`.github/workflows/ci.yml`, not yet run) starts the release binary on **Xvfb (X11)** and on
**headless Weston (Wayland)** and runs the quick benchmark on each.

Known WebKitGTK issue (not seen here; this machine renders on the AMD iGPU): blank or white
windows on Wayland with some NVIDIA proprietary driver setups. Workaround:
`WEBKIT_DISABLE_DMABUF_RENDERER=1 penroam-app`. Not applied by default because it slows
rendering everywhere else.

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
