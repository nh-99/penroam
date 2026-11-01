# Linux desktop — M01

Machine: AMD Ryzen AI 9 HX 370 (24 threads), 46 GiB RAM, NVIDIA + Radeon 890M, Pop!_OS
(kernel 7.2.6), GNOME/Wayland session, WebKitGTK 4.1 — the same machine and session type as
the M00 Linux results.

## Build / run

```bash
pnpm install
pnpm tauri build --no-bundle && target/release/penroam-app
pnpm tauri build                     # .deb + AppImage
```

## Real-input E2E (what was run)

`tools/linux-e2e/` drives the **release binary** with XTest mouse/keyboard events and captures
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
