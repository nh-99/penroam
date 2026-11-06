# Benchmark method

How Penroam's ink and large-document performance is measured. Raw output goes to the
git-ignored `benchmarks/results/`; `tools/summarize-bench.py` turns a folder of runs into a
markdown table.

## 1. Builds and runs

Use release builds only (`pnpm tauri build --no-bundle`, or
`pnpm tauri android build --apk --target aarch64` for BOOX).

* **Workload D, automatic:** `PENROAM_AUTOBENCH=<label>[,quick][,exit] PENROAM_RESULTS_DIR=<dir>
  PENROAM_LIBRARY_DIR=$(mktemp -d) target/release/penroam-app` writes `perf-app-workload-d-*.json`.
  On Android the same run is triggered by `tools/boox-check.sh`.
* **Interactive workloads (A–C, E–H):** run on the device and export the perf file.

Per device: record OS, WebView/firmware versions and (BOOX) the refresh mode; warm up by
drawing for about 30 s; do at least 3 sessions; note thermal state and battery. Never edit raw
files.

## 2. Metrics

| Metric (name in files) | Meaning |
|---|---|
| `event_to_handler_ms` | `performance.now() - event.timeStamp` at handler entry (valid only when `timestamp_clock_check.ok`) |
| `native_input_to_callback_ms` | BOOX: Onyx `TouchPoint.timestamp` → native callback (epoch recorded in `onyx_timestamp_epoch`) |
| `input_sample_processed_ms` | handler start → sample stored |
| `event_to_render_ms` | input handling → active-layer draw issued |
| `preview_render_submitted_ms` | time spent drawing the active layer |
| `frame_interval_ms`, `render_to_next_frame_ms` | WebView frame timing (rAF) and a proxy for "presented" |
| `frames_over_budget` | frames longer than 25 ms |
| `persistent_render_completed_ms` | stroke end → committed ink drawn (BOOX hardware mode: → ePaper refresh issued) |
| `callback_to_software_render_ms` | BOOX: Onyx end callback → stroke in the native bitmap |
| per-stroke `samples`, `medianDtMs`, `maxDtMs`, `timeGaps`, `spatialJumps` | sample rate and continuity |
| `rescaleTotalMs`, `rescaleLongestSliceMs` | a settled zoom's redraw: time until the whole page is sharp, and the longest the main thread was held |
| Workload D: generate, full redraw, visible-first redraw, replay frames, memory | large-document cost |

**Physical pen-to-ink latency is not in any file.** Measure it with a camera (≥120 fps, 240
preferred): film the pen tip and the ink, count frames from tip movement to ink at the tip,
report the median of ≥10 fast strokes with the frame interval as resolution. On BOOX the
hardware preview is drawn by the EPD controller, which the app cannot observe; report slow
full-screen refreshes separately from handwriting latency.

## 3. Workloads

| ID | Task | Record |
|---|---|---|
| A | Several lines of normal handwriting | naturalness, latency (camera), continuity, pressure |
| B | Fast cursive and abbreviations | `timeGaps`, `spatialJumps`, broken segments, preview lag |
| C | Slow circles, straight lines, small lettering | wobble, endpoint handling, pressure fidelity |
| D | Deterministic scenes of 100 / 1k / 5k / 10k strokes (`tests/fixtures/benchmark-scenes`, regenerated in-app) | generate, redraw, replay frames, memory, pen-up → durable |
| E | One continuous 15 s stroke | sample count, `maxDtMs`, commit spike on pen-up |
| F | Palm resting while writing | unwanted marks, missing input, accidental pan |
| G | (BOOX) 10 short strokes in a row | disappearing, duplicate, offset, ghost, flicker between strokes |
| H | (BOOX) Edges and corners, then zoom 1.5 / 0.75 | preview vs committed alignment (photograph), out-of-bounds |

## Device scorecard

Fill this in for each device before listing it as Tested in
[`../platforms/capabilities.md`](../platforms/capabilities.md). Keep the results out of the
repository; `benchmarks/` is git-ignored.

| Item | Record |
|---|---|
| Device, OS, build ID, WebView or firmware | exact versions |
| Rendering backend | native surface or web canvas, from Settings → General |
| Input | event sample rate, preview frame cadence, sample loss and jumps (`timeGaps`, `spatialJumps`) |
| Latency | camera pen-to-photon if available; otherwise label it a **latency proxy**, never physical latency |
| Commit hand-off | no doubled, missing, ghost or misaligned ink, on 100 strokes |
| Palm | resting hand while writing, Workload F |
| Long stroke | 15 s continuous, Workload E |
| Large pages | load and redraw at 1k and 10k strokes, `rescaleLongestSliceMs`, writing over 10k |
| Resources | RSS, battery drop over 30 min of writing |
| Lifecycle | rotation, split screen, keyboard, background and resume mid-stroke, low-memory kill |
| Overlay alignment (native surfaces) | all four page corners, zoomed and panned, in each rotation |
| ePaper | flashes per page turn and per erase, ghosting after 50 erases |
| Files | `.penroam` round trip with `tools/compare-penroam.py`, sync, annotated PDF export |

**Regression gate:** no visibly missing or doubled strokes on any row, and no worse than the
previous run on the same hardware by the same method. Earlier framework-evaluation timings are
event timings, not pen-to-photon, so they cannot be compared with camera results.
