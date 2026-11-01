# M00 benchmark protocol

## 1. Builds

Use release or profile builds only:

* Tauri: `npx tauri build --no-bundle` (Linux) or `npx tauri android build --apk --target aarch64`
* Flutter: `flutter build linux --release` / `flutter build apk --release` / `flutter run --profile` on iPad
* Native: `./gradlew :boox-ink-demo:assembleRelease`

## 2. Session procedure (per device)

1. Record software versions and device settings: `docs/*-testing.md` inventory tables. For
   BOOX, also record the refresh mode (it must be identical across apps).
2. Warm up: open the app, draw for about 30 s, then press **Reset perf**.
3. Run workloads A–H (below), several trials each. Alternate which framework goes first between
   sessions.
4. **Export perf** after each workload. Name or move the file so it says
   `<device>/<app>/<mode>/<workload>-<trial>.json`.
5. Note thermal state (device warm?), battery and anything else unusual. Repeat any surprising
   result.
6. Do at least **3 sessions per device**. Keep raw files in `benchmarks/results/`. Summaries go
   in `analysis/` and never replace the raw files.
7. On BOOX, run both apps in **Mode A** (framework) and **Mode B** (hardware), plus the native
   reference app in B and C. Use the stock Notes app only as a subjective reference.

## 3. What the numbers mean

| Metric (name in files) | Meaning | Comparable across apps? |
|---|---|---|
| `event_to_handler_ms` (Tauri) | `performance.now() - event.timeStamp` at handler entry. Valid only when `timestamp_clock_check.ok` | Only between web engines |
| `input_to_handler_ms` (native C) | `uptimeMillis - MotionEvent.eventTime` | Native only |
| `native_input_to_callback_ms` (native B) | Onyx `TouchPoint.timestamp` vs uptime/wall clock. The epoch is auto-detected and recorded in the `onyx_timestamp_epoch` event | Native only |
| `input_sample_processed_ms` | handler start → sample stored | yes (internal) |
| `handler_to_paint_ms` (Flutter), `event_to_render_ms` (Tauri), `event_to_draw_ms` (C) | input handling → active-layer paint/draw issued | roughly |
| `preview_render_submitted_ms` | time spent in the active-layer paint/draw | yes |
| `frame_*` (Flutter `FrameTiming`: build/raster/total span), `frame_interval_ms` (Tauri rAF), `render_to_next_frame_ms` (Tauri, a proxy for "presented") | framework frame timing | per framework |
| `frames_over_budget` | frames > 25 ms | roughly |
| `persistent_render_completed_ms` | stroke end → committed ink drawn (B: → ePaper refresh issued) | yes (internal) |
| `callback_to_software_render_ms` (B) | Onyx end callback → software stroke in bitmap | native only |
| per-stroke `samples`, `medianDtMs`, `maxDtMs`, `timeGaps`, `spatialJumps` | sample rate and continuity | yes |
| Workload D: generate, full redraw, replay frames, RSS | large-document cost | yes, with caveats per platform |

**Physical pen-to-visible-ink latency is not in any file.** It needs an external camera
(≥120 fps, 240 preferred):

* Film the pen tip and the ink side by side, drawing fast straight strokes.
* Count frames from tip movement to ink appearing at the tip position.
* Report the median of ≥10 strokes, with the camera frame interval as the resolution.
* Do not derive physical latency from callback timings. On BOOX the hardware preview is drawn
  by the EPD controller, which the app cannot observe. Slow full-screen refreshes are a
  different thing from handwriting latency, so report them separately.

## 4. Workloads

| ID | Task | Record |
|---|---|---|
| A | Several lines of normal handwriting | naturalness, latency (camera), continuity, pressure |
| B | Fast cursive and abbreviations | `timeGaps`, `spatialJumps`, broken segments, preview lag |
| C | Slow circles, straight lines, small lettering | wobble, endpoint handling, pressure fidelity |
| D | Scenes 100 / 1k / 5k / 10k (**Bench D** button or `M00_AUTOBENCH=1`) | generate, render, redraw, replay frames, memory |
| E | One continuous 15 s stroke | sample count, `maxDtMs`, commit time spike on pen-up |
| F | Palm resting while writing | unwanted marks, missing input, accidental pan (`palm_rejected_*` counters) |
| G | (BOOX) 10 short strokes in a row, per handoff strategy | disappearing, duplicate, offset, ghost, flicker between strokes |
| H | (BOOX) Edges and corners, then Zoom 1.5 / 0.75 | preview vs committed alignment (photograph), `outOfBounds` |

## 5. Feedback sheet (rate 1–5, per device, per build/mode)

| Criterion | Flutter A | Flutter B | Tauri A | Tauri B | Native B | Native C |
|---|---|---|---|---|---|---|
| Writing naturalness | | | | | | |
| Perceived latency | | | | | | |
| Stroke smoothness | | | | | | |
| Pressure handling | | | | | | |
| Palm rejection | | | | | | |
| Visual stability | | | | | | |
| Overall comfort | | | | | | |

Rater, device, date, session #: ______

## 6. Device test checklist

- [ ] Inventory recorded (`docs/boox-integration.md` §6, `docs/ipad-testing.md`, `docs/linux-testing.md`)
- [ ] Release/profile builds installed; versions recorded
- [ ] Reference app: Mode B draws with visible hardware ink; strokes persist after pen-up
- [ ] Reference app: all §5 questions in boox-integration.md answered
- [ ] Reference app: leaving the app restores normal screen refresh
- [ ] Flutter: Mode A, B, C on BOOX; each Platform View composition tried
- [ ] Tauri: Mode A, B, C on BOOX; overlay aligned at corners; rotation; app switch
- [ ] Both: Save → Clear → Load restores identical strokes
- [ ] Workloads A–H, ≥3 sessions, raw JSON saved
- [ ] Camera latency clips saved with frame counts
- [ ] Feedback sheet completed per device
