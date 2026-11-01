# Penroam M01 architecture

Status: M01 implementation (2026-11-02). Framework decision (Tauri 2) is final; see
`agent-docs/PENROAM_M01_IMPLEMENTATION_PLAN.md` and the M00 record in `docs/m00/`.

## 1. What M00 left us, and what happened to it

M00 lived in a separate, unversioned tree (`OpenNote-bakeoff/`). M01 starts a new repository
and **copies** every M00 asset that matters for regression testing; nothing was deleted at the
source.

| M00 path | M01 path | Status |
|---|---|---|
| `fixtures/` (sample strokes, geometry golden, scenes, generator, manifest) | `tests/fixtures/` | Reused unchanged; `generate-fixtures.mjs --check` verifies all six files against the frozen SHA-256 manifest. `scene-5000/10000.json` (34 MB) are git-ignored and regenerated (`pnpm fixtures`) |
| `benchmarks/` (protocol, raw Linux/BOOX/iPad results, analysis) | `benchmarks/` | Kept verbatim; M01 results go to `benchmarks/results/m01/` |
| `android/boox-ink` (Onyx `BooxInkView`, 11 JVM tests) | `android/boox-ink` | **Reused**, extended (transparent overlay, colour/opacity, highlighter, incremental add/remove, `OverlayPlacement`). M00 input/hand-off logic unchanged; 11 M00 tests + 3 new pass |
| `android/boox-ink-demo` | `android/boox-ink-demo` | Kept as the native reference app |
| `tauri_app/` (React prototype, Canvas engine, plugins) | `reference/m00-tauri-app/` | Kept as reference/benchmark baseline. Its canvas engine and geometry were **ported** to `apps/desktop-mobile/src/editor/` (Vue host) |
| `tauri_app/plugins/tauri-plugin-boox-ink` | replaced by `plugins/tauri-plugin-native-ink/android` | Same command shape (`call(method,args)` + events), new name, M01 additions |
| `tauri_app/plugins/tauri-plugin-pencilkit` | `reference/…` (kept) + new `plugins/tauri-plugin-native-ink/ios` | M01 uses a UIKit Pencil preview layer (plan §5.3); PencilKit is not the canonical format |
| `ios/InkKit` (InkCore, PencilInk, WebTuning; 8 Swift tests) | `ios/InkKit` | Reused; new `PencilSurface` target (Linux-testable core of the iPad adapter, 6 tests) |
| `ios/M00iOS` (UIKit reference + WKWebView proxy) | `ios/M00iOS` | Kept as the iPad test harness |
| `flutter_app/` | `reference/m00-flutter-app/` | Kept for history only (framework decision is Tauri) |
| `docs/*`, `PLAN.md`, `README.md` | `docs/m00/` | Kept verbatim |

## 2. Layout

```text
apps/desktop-mobile/            Tauri 2 app
  src/                          Vue 3 + TS + Vite + Tailwind 4 + @lucide/vue
    editor/                     framework-free editor: engine, renderer, session, geometry, text layout, PDF.js
    components/ views/          UI (library, editor, toolbar, sidebar, dialogs)
    composables/                editor host (page loading, flushing, native surface wiring)
    stores/                     Pinia: library + UI state + device preferences (UI state only)
    platform/                   typed facades: ipc (Rust commands), files (dialogs), nativeInk, lifecycle
    generated/                  TS types generated from the Rust schema (ts-rs) — do not edit
    bench/                      Workload D harness (M00 method)
  src-tauri/                    Rust shell: typed commands, structured errors, flush-on-close
    gen/android/                generated Android project (+ scripts/patch-android.py)
crates/penroam-core             schema, validation, ops, geometry (M00 golden), text layout, migrations
crates/penroam-storage          live library (atomic files + fsynced journals), trash, recovery, .penroam archive
crates/penroam-pdf              PDF inspection (import) and annotated vector export (lopdf)
plugins/tauri-plugin-native-ink NativeInkSurface: Rust bridge, android/ (Kotlin), ios/ (Swift/UIKit)
android/  ios/                  M00 native modules (reused)
tests/fixtures/                 shared fixtures (M00 + text-layout golden + PDF fixture)
tools/linux-e2e/                XTest input driver + session runner for real-app E2E on Linux
benchmarks/  reference/  docs/
```

## 3. Data ownership

| Data | Owner | Notes |
|---|---|---|
| Notebook/page/asset content | **Rust** (`penroam-storage`) | Canonical JSON files on disk. Every change arrives as an `OperationBatch` and is acknowledged only after an fsync |
| Open page working copy, undo/redo stacks | Vue editor (`PageSession`) | Mirror of the Rust page; edits are applied locally with the same semantics and queued to Rust in order |
| Live pen samples | The active surface (web canvas or native view) | Never cross IPC while a stroke is live; only finalised strokes do |
| Rendered caches (page bitmaps, thumbnails) | Frontend | Thumbnails are stored by Rust as derived files, never exported |
| UI preferences (theme, tool colours, pen-only mode) | `localStorage` | Per device, not notebook data |

Rust is the only writer. The renderer never sees library paths; import/export pass a
path/URI chosen in a system dialog straight back to Rust (`tauri-plugin-fs` resolves Android
`content://` URIs). No `fs` commands are exposed to the WebView (`capabilities/default.json`).

## 4. Document model and persistence (summary)

Full spec: `docs/format/penroam-format.md`.

* Units: **points** (1/72 in), page-local, y down. At zoom 1 one point is one CSS px, so M00's
  "document units = logical px at zoom 1" maps 1:1 and the M00 geometry golden applies unchanged.
* `Page.elements` = `ink | text | image`; the PDF page is `Page.background` (`PdfBackgroundRef`).
* Stroke points keep every M00 field (`x, y, pressure, tiltX, tiltY, timeUs`) plus optional
  `twist`/`flags`; the stroke `input` map keeps platform metadata verbatim (Onyx stream counts,
  raw pressure range, coalesced counts…). Smoothing is derived at render time.
* Live library: `notebooks/<id>/notebook.json`, `pages/<id>.json` + `pages/<id>.journal`,
  `assets/<sha256>`. **No database**: the canonical files are the index, so a crash cannot leave
  an index out of sync (the plan allowed SQLite only as a rebuildable cache; not needed at M01
  scale — `list_notebooks` reads small `notebook.json` files).
* Durability: journal append + `fsync` per batch (ink on pen-up; text debounced 0.7 s and on
  blur/page change/background/close). Journals fold into snapshots via temp-file + fsync +
  rename. Start-up recovery removes temp files, truncates a torn journal tail, replays journals,
  and moves unreferenced files to `orphans/` (never deletes).
* Undo/redo are semantic: each change carries forward + inverse ops; undo enqueues the inverse
  as a new batch with fresh op ids. Stable UUIDs for notebooks, pages, elements, ops, batches;
  page revisions guard against stale batches (a mismatch is a conflict, never a merge).

## 5. Editor and ink pipeline

* **Page-sized cached layers** (`renderer.ts`): `bg` (paper, PDF bitmap, template), `ink`
  (strokes, images), `text`. They live inside a CSS-transformed element, so pan and zoom never
  redraw; the backing resolution is re-rendered once a zoom settles (160 ms), capped by pixel
  area (16 Mpx desktop, 8 Mpx mobile — WebKit's iOS canvas limit). Erase/undo/move redraw only
  the dirty rectangle; a new stroke is appended incrementally.
* **Live stroke** (M00 method): screen-space `active` canvas for stable pieces drawn
  incrementally and `tail` canvas for the provisional piece and predicted samples (50 % alpha,
  never stored). One sample stream (`pointerrawupdate` when present, else `pointermove`) plus
  `getCoalescedEvents()`.
* **Hand-off**: on pen-up the stroke is committed to the session, which paints it into the
  page cache synchronously; the preview is cleared in the same task, so both changes reach the
  same frame (no flash, no double). Native surfaces use an explicit acknowledgement (§6).
* Text blocks are rendered from a deterministic layout (`textLayout.ts` = `penroam-core::text`,
  same metrics file, golden-tested) with the bundled Liberation Sans (metric-compatible with
  Helvetica, which the PDF export uses). Editing uses a textarea overlay positioned in the same
  page coordinate system; the canvas copy is hidden while editing.
* Paint order is ink → images → text (same in the PDF exporter).
* Input rules: pen eraser button erases; palm-sized touches are ignored; touches are ignored
  while the pen is down; "pen-only" mode (auto after the first pen event) makes fingers pan and
  zoom; `pointercancel`, lost capture, window blur and app backgrounding finish or cancel
  gestures so no stroke is ever left stuck.

## 6. NativeInkSurface

`src/platform/nativeInk.ts` defines the interface (`mount/unmount`, `setBounds`,
`setViewportTransform`, `setTool`, `setVisible`, `loadCommittedInk`, `addStrokes`,
`removeStrokes`, `commitAck`, events `stroke/erased/viewport/status/error`). Coordinates:
the native view covers exactly the editor element's CSS rect; both sides use
`viewPx = (doc * scale + offset) * dpr`.

| | Android / BOOX ("owner" surface) | iOS / iPadOS ("preview" surface) |
|---|---|---|
| Implementation | `NativeInkPlugin.kt` hosting M00 `BooxInkView` (Onyx `TouchHelper` raw drawing, Mode B) | `NativeInkPlugin.swift`: UIKit `PencilPreviewView` + Pencil-only gesture recogniser on the WKWebView |
| Live ink | ePaper hardware pen layer (Onyx) | UIKit `draw(_:)` at display rate (120 Hz on ProMotion in M00's UIKit reference) |
| Committed ink while visible | Rendered by the native view (transparent background); the web ink layer is hidden to avoid doubled edges | Rendered by the web canvas; native keeps the stroke until `commitAck` (2 s timeout) |
| Pen-up | Native commits into its own bitmap (M00 PEN_UP_REFRESH hand-off), then emits `stroke`; web adds it to the model and persists | Native emits `stroke`; web commits + paints, then acks two frames later |
| Shown for | pen, highlighter, eraser | pen, highlighter |
| Hidden for | text, select, pan tools; any open dialog/menu/drawer (`ui.overlays`) | same |
| Sync when web edits | `addStrokes`/`removeStrokes` (undo, delete, move) | not needed |

Non-BOOX Android devices use the web canvas (the generalised native Mode C exists in
`BooxInkView` but is not enabled by default until validated).

## 7. Platform constraints

* **Linux/Windows/macOS desktop**: WebKitGTK / WebView2 / WKWebView. Web canvas only.
* **Android**: system WebView (Chromium). Release builds drop the `INTERNET` permission.
  BOOX Onyx SDK pinned (pen 1.5.2 / device 1.3.3 / base 1.8.4) from the official Onyx Maven repo.
* **iOS/iPadOS**: Tauri iOS builds require macOS + Xcode (not available in this environment).
  WKWebView renders rAF at 60 Hz unless the private `PreferPageRenderingUpdatesNear60FPSEnabled`
  flag is lifted (M00 `WebTuning` hook; App Store risk) — hence the native preview layer.
* **Offline**: strict CSP (`connect-src ipc:` only), PDF.js data files bundled, no network code.
