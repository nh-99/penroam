# Linux desktop — M01

Machine: AMD Ryzen AI 9 HX 370 (24 threads), 46 GiB RAM, NVIDIA + Radeon 890M, Pop!_OS
(kernel 7.2.6), GNOME/Wayland session, WebKitGTK 4.1 — the same machine and session type as
the M00 Linux results.

## Display servers: X11 and Wayland

Penroam has no display-server-specific code: Tauri/tao, GTK 3 and WebKitGTK pick the backend
(GDK: `GDK_BACKEND` if set, otherwise Wayland when a compositor is present, else X11). Both
are supported targets. `system_info` reports the backend actually in use (`displayServer`).

| Environment | What ran | Result |
|---|---|---|
| Wayland, GNOME (native), dpr 2 | Workload D 1k/5k/10k ×3, steady-state opens | ✅ 10k redraw 233 ms full / 124 ms visible-first, replay 16–17 ms (rAF capped at 60 Hz), pen-up→disk 8 ms |
| X11 via XWayland on the same desktop (`GDK_BACKEND=x11`), dpr 1 | Workload D 1k/5k/10k ×3 | ✅ 10k redraw 242 / 132 ms, replay **6 ms** (rAF not capped), pen-up→disk 9 ms. GNOME does not scale X clients on HiDPI, so dpr is 1 (softer text/ink than Wayland) |
| Pure X11 server (Xephyr, no Wayland, no WM, software GL) | all real-input E2E sessions below + Workload D | ✅ |
| Wayland with real (injected) input | — | ⏳ no Wayland input injector here (XTest is X-only); input is the same WebKit pointer-event path, but not exercised |
| Full Xorg desktop session with a window manager | — | ⏳ not available on this machine |
| Pen tablet pressure (X11 XInput2 / Wayland tablet-v2) | — | ⏳ no tablet; pressure handling is unit/engine-tested only |

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

## Real-input E2E (what was run)

`tools/linux-e2e/` (X11 only: XTest) drives the **release binary** with mouse/keyboard events and captures
screenshots. Because the desktop session locked during the run, sessions ran in a nested
Xephyr X server (`Xephyr :7 -screen 1300x940`; software GL). Session scripts are in
`tools/linux-e2e/sessions/`; screenshots in `docs/screenshots/m01/`.

| Session | Verified |
|---|---|
| `ink-text-restart` | create notebook via dialog; mouse ink; translucent highlighter; multi-line typed text; "Saved"; polite window close (flush) → relaunch → notebook, cover thumbnail and content intact |
| `crash-recovery` | two strokes drawn, **SIGKILL 300 ms after pen-up** → relaunch → both strokes present (journal replay) |
| `pdf-import` | PDF imported through the GTK file chooser; PDF.js background; 5 pages appended after the current page |
| `pdf-annotate` | ink/text on Letter, cropped A4, Rotate 90/180/270 pages; thumbnails show annotations; page navigation by keyboard |
| `export-pdf-and-penroam` | annotated PDF and `.penroam` written through the GTK save dialog; PDF rendered with poppler (`07-exported-annotated-pdf-poppler.png`): annotations upright and in place on every rotation, crop box kept |
| `page-move-delete`, `page-restore` | page menu → Move up, Delete page → trash section → Restore; `notebook.json` order and `trashedPages` checked after each step (restored page returns to its former index). This session found and fixed a real bug: the page menu was clipped by the sidebar's scroll container (now a fixed-position popover) |
| `.penroam` re-import | archive imported into a fresh library through the dialog, re-exported, `tools/compare-penroam.py` → **IDENTICAL**; thumbnails for never-opened pages rendered in the background (`11-linux-imported-thumbnails.png`) |

Not covered by real input: pen/tablet pressure (no stylus on this machine) and touch pinch —
both are unit/engine-tested only; real-pen validation on BOOX and iPad is still pending.

## Benchmarks

See `docs/M01-results.md` §3 (Wayland, M00 profile, and Xephyr runs with the M00 reference
rebuilt from `reference/m00-tauri-app` for a same-environment comparison).
