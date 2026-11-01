# M01 results (2026-11-02)

Everything here was run on the Linux workstation described in `docs/platforms/linux.md`
(same machine as M00) and, where stated, on a BOOX Note Air5 C over adb. No iPad was used.
Raw result files are in `benchmarks/results/m01/`.

## 1. Automated tests

| Suite | Command | Result |
|---|---|---|
| Rust: core (schema round trip + unknown fields, migrations/version guard, M00 import lossless, **M00 geometry golden**, ops atomicity/ordering/stable ids, undo by inverse, PDF matrix corners for 0/90/180/270, **text-layout golden**, validation) | `cargo test -p penroam-core` | 10 pass |
| Rust: storage (CRUD/trash/restore/order persistence, **acknowledged batches survive a crash without flush**, **torn journal tail**, crash between snapshot and truncate, temp/orphan cleanup, corrupt notebook untouched, compaction under 600 batches, archive round trip lossless + copy-on-conflict, **hostile archives**: traversal, absolute path, backslash, decompression bomb, checksum tamper, missing page, newer version, broken asset reference, garbage; content-addressed assets, stale revision = conflict) | `cargo test -p penroam-storage` | 11 pass |
| Rust: PDF (boxes/rotation/inheritance, garbage, **annotated export keeps source pages + vector annotations with the right matrix per rotation**, **poppler render check of ink position on a 90° page**) | `cargo test -p penroam-pdf` | 4 pass |
| Rust: TS binding export/consistency (ts-rs) | `cargo test --workspace` | 30 pass; `git diff` on `src/generated` clean |
| Rust: fmt + clippy `-D warnings` | | clean |
| TS/Vue (Vitest + happy-dom): M00 geometry golden, text-layout golden (identical to Rust), session undo/redo/persist order/coalescing/retry/save states, pointer normalisation + palm/pen-only rules, viewport math, native interop (M00 format) lossless, scene generator = frozen fixtures, **engine** (pen commit with pressure metadata, zoom/pan mapping, mouse fixed width, pointercancel, eraser + undo/redo, pen eraser button, palm rejection, text insert/batching/no accidental ink, typing undo granularity, move + zoom invariance, delete/template undo, wheel pan/zoom, highlighter), **UI navigation** (empty library → create → editor → back) | `pnpm test` | 37 pass (8 files) |
| TS typecheck / ESLint | `pnpm typecheck`, `pnpm lint` | clean |
| Kotlin JVM (`android/boox-ink`): 11 M00 + 3 M01 (overlay corners at dpr 1.875, native↔web doc-point agreement under zoom/pan, colour/opacity interop) | `./gradlew :boox-ink:testDebugUnitTest` | 14 pass |
| Swift (Linux, swift 6.4.2): 8 M00 InkKit + 6 M01 PencilSurface (coalesced mapping, estimated force patching, predicted never committed, highlighter brush, hand-off ack/timeout, overlay frame) | `cd ios/InkKit && swift test` | 14 pass |
| Fixtures frozen | `node tests/fixtures/generate-fixtures.mjs --check` | 6/6 ok |
| M00 reference app TS tests (relocated) | `cd reference/m00-tauri-app && npx vitest run` | 13 pass |

## 2. Builds

| Target | Result |
|---|---|
| Linux release binary (`pnpm tauri build --no-bundle`) | ✅ built and run (Wayland and Xephyr/X11) |
| Android arm64 release APK with native-ink plugin + Onyx SDK | ✅ built, installed and launched on the BOOX Note Air5 C |
| iOS plugin Swift (`tauri-plugin-native-ink` target) for `arm64-apple-ios` via xtool Darwin SDK | ✅ compiles; ⛔ app build/link needs macOS + Xcode |
| Windows / macOS desktop | ⛔ not built (no runners here); CI jobs added in `.github/workflows/ci.yml`, **not yet run** |
| CI workflow | written, **never executed** (no remote) |

## 3. Performance (M00 Workload D method)

Method: deterministic scene (seed = N) → (M01 only) persisted through Rust → page loaded and
rendered → full redraw timed with a `getImageData` GPU flush → one replayed sample per rAF over
the committed scene → process-tree RSS. M00's numbers come from the **M00 reference app
rebuilt from `reference/m00-tauri-app` and run in the same session**.

Differences that matter: M00 redraws a viewport-sized canvas (1280×733 CSS px, strokes culled to
it); M01 renders the **whole 1600×1200 pt page** into a page-sized cache (pan/zoom then cost
nothing) and additionally reports the visible-first part of its progressive redraw.

### 3.1 Wayland, GNOME, devicePixelRatio 2 — the M00 profile

| Strokes | M00 full redraw (viewport) | M01 full-page redraw | M01 visible-first | M00 replay interval median | M01 replay interval median / p95 | M00 tree RSS | M01 tree RSS |
|---|---|---|---|---|---|---|---|
| 1 000 | 26 ms | 45 ms | 24 ms | 17 ms | 16 / 24 ms | 522 MiB | 653 MiB |
| 5 000 | 131 ms | 120 ms | 60 ms | 17 ms | 16 / 20 ms | 632 MiB | 854 MiB |
| 10 000 | 191 ms | **228 ms** | **123 ms** | 16 ms | 17 / 20 ms | 703 MiB | **1134 MiB** |

(medians of 3 trials × 3 redraws; M01 file `linux-wayland/perf-m01-workload-d-wayland-1793642967381.json`,
M00 file `m00-baseline-wayland/perf-tauri-workload-d-1793642978391.json`; M00's archived Linux
runs from 2026-11-01 measured 191–217 ms / 700–727 MiB, consistent.)

Steady state (fresh process opens an existing 10k page; 3 runs): full-page redraw 222 / 337 /
325 ms, visible-first 116 / 180 / 174 ms, tree RSS 951 / 988 / 961 MiB (Rust process ≈ 281–312
MiB, WebKit web process ≈ 605–629 MiB), page open (IPC + parse + first paint) ≈ 1.1 s.

Other M01 numbers (Wayland, 10k page): pen-up → durable on disk (journal + fsync) **8 ms**;
persisting a 10k-stroke batch through Rust 0.9 s; cold start 0.55–3.1 s (M00: 0.44–3.2 s).

### 3.2 Xephyr (nested X server, software rendering) — same-environment control

| Strokes | M00 full redraw | M01 full-page | M01 visible-first | M00 RSS | M01 RSS |
|---|---|---|---|---|---|
| 1 000 | 71 ms | 102 ms | 51 ms | 448 MiB | 549 MiB |
| 5 000 | 178 ms | 338 ms | 166 ms | 512 MiB | 681 MiB |
| 10 000 | 355 ms | 672 ms | 347 ms | 569 MiB | 896 MiB |

Replay interval median 16 ms for both. Steady-state 10k open: 715–880 MiB.

### 3.3 Against the M01 gates

| Gate | Status |
|---|---|
| 10k full redraw ≤ 300 ms median on the M00 profile | **Met in Workload D** (228 ms whole page, 123 ms visible-first). Fresh-process steady-state runs: 222 / 337 / 325 ms (median 325 ms) — **borderline; not met in 2 of 3 cold runs** |
| Replay at display cadence over 10k committed strokes, no visible lag | 60 Hz median (16–17 ms), p95 ≤ 20 ms on Wayland. Visual "no ink lag/stroke loss" was checked with mouse input only |
| RSS: flag > 20 % above M00 10k | **Flagged**: +61 % in-process (1134 vs 703 MiB), +37 % steady state (≈ 960 vs 703). Main contributors: the document model now also lives in Rust (≈ +100 MiB in the app process: parsed page cache + journal), three page-sized canvases at dpr 2 (3 × 30 MiB), JS page objects. Mitigations already applied: no page copies per batch, page cache 8→3, `malloc_trim` after large transfers. Next steps in §5 |
| BOOX callback p95 vs 12 ms, 10k load/erase | 10k native load ≈ 3.1 s (parse 1.3 s + render 1.7 s; M00 ≈ 3.8 s). Callback p95 and erase **not measured** (need the pen) |
| iPad workloads | **Not run** (no packaged iOS build) |
| Durability | ✅ SIGKILL 300 ms after pen-up loses nothing (real app); torn-write and crash-window recovery unit-tested |

## 4. End-to-end (real app)

Linux release binary, real XTest mouse/keyboard input (`tools/linux-e2e`), screenshots in
`docs/screenshots/m01/`:

1. create notebook → pen strokes → highlighter → multi-line text on the same page → close →
   relaunch → content and thumbnails intact (`01`–`03`);
2. draw → SIGKILL → relaunch → strokes recovered (`04`);
3. PDF (5 pages: Letter, cropped A4, Rotate 90/180/270) imported via the system dialog, ink and
   text added on every page, annotated PDF exported via the save dialog, rendered with poppler:
   all annotations upright and in place, source pages/boxes/rotations kept (`05`–`07`,
   `exported-annotated.pdf`);
4. `.penroam` exported, imported into a fresh library, re-exported:
   `tools/compare-penroam.py` → **identical** (all point fields, text, templates, PDF refs,
   asset bytes).

BOOX (Note Air5 C): APK launched, ePaper theme auto-selected, notebook created through the UI,
native `BooxInkView` attached in BOOX_HARDWARE mode over the canvas with the web template
visible through it (`08`, `09`). `tools/boox-m01-check.sh`: injected stroke through the native
post-pen-up path → web model in 42 ms → durable in 1.8 ms; undo/redo mirrored natively;
native vs web layer aligned to 0 px (`12`); 10k-stroke native load ≈ 3.1 s (M00 Mode B ≈ 3.8 s).
**Two-OS transfer**: Linux export → BOOX import (Android picker) → BOOX export → Linux:
`compare-penroam.py` **identical** (`13`, `boox-export.penroam`). **Not done**: real pen strokes.

## 5. Open issues and next steps

* **BOOX pen validation** (DoD 2): manual checklist in `tools/boox-m01-check.sh`; native
  callback p95 needs real strokes. Web whole-page redraw on the BOOX is ≈ 3 s at 10k strokes.
* **iPad**: needs a Mac; checklist in `docs/platforms/ipad.md`.
* **Memory**: render the text layer only where text exists, share one backing store for
  ink+text, drop the Rust page cache to the open page on mobile, stream page JSON instead of
  materialising it twice.
* **Steady-state redraw** variance on the first paint after open (222–337 ms): make the
  page-open path use the visible-first redraw (it does) and move band painting off the first
  frame entirely; consider an OffscreenCanvas worker.
* Windows/macOS builds via CI.
