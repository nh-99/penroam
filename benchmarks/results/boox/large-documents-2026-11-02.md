# BOOX large-document benchmarks — 2026-11-02 (Note Air5 C, Android 15, 38 Hz EPD)

Raw files: `sessions/` (Bench D and Bench B), `sessions/flutter-skia/` (Bench D with
`--enable-impeller=false`), `sessions/pre-erase-fix/` (Bench B before the eraser fix).
Runner: `tools/boox_bench.sh` (fresh process per session, scripted taps by label).
Medians over all trials (3 per scene size per session).

## Bench D — framework canvas (Mode A): the framework renders every stroke

Tauri ×3 sessions, Flutter (Impeller) ×3, Flutter (Skia) ×2.

| strokes | full redraw ms, GPU-flushed: Tauri / Flutter-Impeller / Flutter-Skia | replay frame interval max ms | app-process memory MiB (see note) |
|---:|---:|---:|---:|
| 100 | **13** / 64 / 141 | 30 / 31 / 31 | 254 / 186 / 181 |
| 1,000 | **96** / 569 / 1,216 | 29 / 30 / 32 | 282 / 238 / 259 |
| 5,000 | **329** / 3,186 / 6,144 | 29 / 30 / 52 | 335 / 343 / 426 |
| 10,000 | **602** / 6,495 / 12,384 | 31 / 46 / 53 | 389 / 401 / 623 |

* **Writing over a large page is fine in both**: the replay frame interval median is 26 ms
  (the panel's 38 Hz) at every size, for both frameworks.
* **Full redraw (zoom, pan, erase, load) is about 5–11× slower in Flutter.** At 10k strokes:
  6.5 s vs 0.6 s. On Linux the gap was 2.3×. Both apps use the same algorithm and the same
  committed-image cache, so this compares Chromium's Canvas 2D (Skia, GPU-accelerated) with
  Impeller's path stroking on the Adreno GPU.
  * Each stroke is many round-capped pieces of varying width.
  * Impeller tessellates them on the CPU; this device's CPU is slow.
* **Skia is not the fix**: it is about 2× slower than Impeller here and uses more memory.
  (Impeller opt-out is also deprecated.)
* Possible Flutter mitigations that were **not** tried: pre-tessellated per-stroke meshes
  (`drawVertices`), tiling, or keeping the cache image across zooms (scale it, re-raster
  later). These are real engineering and would also help Tauri.
* **Memory note:** Tauri's numbers above exclude the WebView renderer process. Measured
  separately with 10k strokes loaded: Tauri 322 MB app + 237 MB renderer = **559 MB**;
  Flutter **458 MB** (single process). Shared system WebView processes are excluded from both.

## Bench B — native Onyx view (Mode B): the shared Kotlin module renders

Tauri ×2 sessions, Flutter ×2 (after the eraser fix). What differs between hosts is only the
bridge: Flutter uses a MethodChannel; Tauri goes JS → Rust → Kotlin plugin.

| strokes | serialize doc ms (Dart `jsonEncode` / JS `JSON.stringify`): Tauri / Flutter | bridge + native parse + rebuild ms | **total load ms** | zoom ms | pan ms | erase stroke ms (incl. rebuild) | erase event back in host ms |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 100 | 1.3 / 14 | 56 / 41 | 58 / 55 | 28 / 14 | 26 / 24 | 13 / 10 | – |
| 1,000 | 22 / 124 | 376 / 282 | 397 / 406 | 108 / 100 | 142 / 166 | 133 / 135 | 133 / 141 |
| 5,000 | 87 / 668 | 1,649 / 1,296 | 1,736 / 1,963 | 467 / 457 | 668 / 693 | 625 / 623 | 624 / 655 |
| 10,000 (24 MB JSON) | 171 / 1,347 | 3,653 / 2,709 | **3,823 / 4,056** | 926 / 918 | 1,330 / 1,344 | 1,243 / 1,244 | 1,243 / 1,283 |

* **In Mode B the frameworks are effectively tied.** Zoom, pan and erase are native work and
  match within noise.
* **Load:**
  * Dart's JSON encoding is about 8× slower than JS.
  * Tauri's bridge is about 35% slower than the MethodChannel: the document string is
    JSON-escaped again inside the IPC payload, then crosses Rust.
  * The totals come out within about 6% of each other.
* Events from native to host add < 40 ms in both.
* The native module itself (Android Canvas, software bitmap) needs about 600 ms for a 10k
  full rebuild on this device, about the same as Tauri's Canvas 2D.

## Bug found and fixed: eraser cost scaled with hits × page size

`BooxInkView.eraseAt` rebuilt the whole bitmap for every eraser sample that hit a stroke.

* One eraser stroke over a 10k-stroke page took **38.8 s** (5k: 15.6 s), identically in both
  hosts (files in `pre-erase-fix/`).
* After the fix it rebuilds once per eraser stroke (scripted path, Onyx raw list) or once per
  input batch (live MotionEvents): **1.24 s** at 10k strokes.
* That is still a full rebuild per erase. Incremental erase (re-render only the dirty region)
  is the M01 follow-up.
