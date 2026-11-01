# Cross-backend interoperability (M00.8)

The canonical stroke format (`docs/architecture.md`) is the document model. Native formats
(`PKDrawing`, Onyx data) are stored only as **optional fidelity sidecars**
(`pencilkit-<stamp>.pkdrawing.b64` next to `pencilkit-<stamp>.json`). The canonical format
never depends on PencilKit or the Onyx SDK being available.

## PencilKit ↔ canonical mapping (`ios/InkKit`)

| Canonical | PencilKit `PKStrokePoint` | Direction notes |
|---|---|---|
| x, y (doc units = points at zoom 1) | `location` with `stroke.transform` applied | exact (float32 storage in PencilKit) |
| pressure 0..1 | `force` / 4.1667 (Pencil `maximumPossibleForce`) | force is quantized in PencilKit: error ≤ 0.0002 |
| tiltX/tiltY (deg) | `altitude` + `azimuth` | exact inverse (tested on Linux) |
| timeUs | `timeOffset` | exact |
| brush.width (nominal) | no equivalent. Import: per-point `size` = shared width model. Export of live strokes: tool width; export of imported strokes: per-point inversion of the width model | per-point PencilKit sizes are summarised in `input.pkSizeMin/Median/Max` |
| stroke order | `drawing.strokes` order | preserved |
| stroke id | none (PencilKit strokes have no id) | the backend tracks ids by diffing snapshots |

**What cannot be preserved exactly:**

* **Appearance.**
  * PencilKit renders control points as its own spline with its own `.pen` ink model.
  * The shared brush uses midpoint-quadratic segments with the linear pressure→width model.
  * The same canonical points therefore look different in each engine.
* **PencilKit's per-point width variation** for strokes drawn natively. Its pressure, azimuth
  and velocity response is reduced to one nominal width plus per-point pressure.
* **Bitmap or pixel eraser edits.** The backends use whole-stroke (vector) erasing only.

## Results (on device, iPad13,4 / iPadOS 26.6.2)

`benchmarks/results/ipad/perf-ios-pencilkit-interop-*.json`, run over USB with `m00ctl.py cmd interop`.

| Case | Position error (max) | Pressure error (max) | Tilt / time / count / order | Visual IoU (shared render vs PencilKit render) | Ink in A within 1 px of B / B within 1 px of A |
|---|---|---|---|---|---|
| sample-strokes (5 strokes) canonical → PencilKit → canonical | 1.3e-5 pt | 2e-4 | exact | 0.53 | 78% / 91% |
| scene-100 canonical → PencilKit → canonical | 8.3e-5 pt | 2e-4 | exact | 0.66 | 88% / 98% |
| live PencilKit drawing (7 strokes) → canonical → PencilKit | 0 | 2.4e-4 | tilt ≤ 0.009°; time / count / order exact | 0.81 (original vs re-imported, PencilKit render) | 98% / 100% |

Re-run 2026-11-02 (`perf-ios-pencilkit-interop-1793625871821.json`), after two fixes:

| Case | Width mismatches (> 0.005) | Max width error | Pressure-flag mismatches | Decoded PKDrawing equivalent after re-encode |
|---|---|---|---|---|
| sample-strokes | 0 / 5 | 0 | 1 / 5 (see below) | yes |
| scene-100 | 0 / 100 | 0 | 0 / 100 | yes |

* **Nominal width.** The first run used a median per-point inversion rounded to 0.001, which
  missed on 1/5 and 33/100 strokes. PencilKit quantizes force, which shifts the inversion by
  about 1e-3. The width is now averaged over all points and snapped to 0.01
  (`NativeMapping.invertWidth`, tested on Linux). So canonical widths survive PencilKit only at
  **0.01 precision**.
* **Pressure flag.** PencilKit has no per-stroke flag, so it is inferred: constant sizes with
  varying pressure means pressure is off.
  * A pressure-off stroke whose points all have pressure 0.5 cannot be told apart from a
    pressure-on stroke, because both give exactly the nominal width.
  * This is the `sample-mouse` mismatch above. It renders identically, so the flag (not the
    appearance) is lost.
* **`PKDrawing` bytes** differ after decode → re-encode, but the decoded content is identical.
  The fidelity sidecar should be treated as opaque.

## Test A run (2026-11-02)

1. You drew 7 strokes in the PencilKit engine. Exported as `pencilkit-1793626188371.json` with a
   `.pkdrawing.b64` sidecar.
   * **Bug found:** that export ran the import-only width inversion on live strokes. The widths
     came out as 4.07–6.72 instead of the 3 pt tool width, so the shared renderer drew 1.65× the
     ink.
   * **Fixed:** the backend now keeps each stroke's canonical brush for the session. Inversion is
     used only for strokes with no record, such as a restored `PKDrawing`. The interop test
     reports both paths (`geometry` and `geometry_fromNativeOnly`).
   * `pencilkit-1793626188371.width-fixed.json` is the same file with the widths reset to 3. The
     original is kept as evidence.
2. Tauri engine on Linux: both files load losslessly (TS test).
3. Linux edit (`linux-edit-of-pencilkit-1793626188371.json`): one stroke erased, one mouse stroke
   added with pressure off and width 4. Imported into PencilKit, then re-exported
   (`pencilkit-1793626663635.json`):
   * 7/7 strokes with the same ids;
   * position ≤ 3e-5 pt, pressure ≤ 2.4e-4, tilt ≤ 0.009°;
   * widths and pressure flags exact (from the session brush registry).
4. **Pressure scale: verified correct.** The live strokes' normalized pressure was low
   (0.00–0.55, median 0.09). To check the scale, a passive recognizer recorded raw
   `UITouch.force` next to the stored `PKStrokePoint.force` for 10 strokes drawn light, normal
   and hard (`perf-ios-pencilkit-1793626724427.json`, `capabilities.forceCalibration`).
   * `maximumPossibleForce` = 4.1667 = 25/6, the constant used.
   * PencilKit stores force on the same scale as UITouch. Hard stroke max: 3.610 stored vs
     3.615 raw; p90: 3.51 vs 3.50. Medians agree within PencilKit's resampling (fewer stored
     points than touch samples).
   * UIKit reports a provisional first-sample force of 0.333 before the estimated value
     arrives; PencilKit drops it.
   * So the low values were a light hand: light ≈ 0.01–0.05 normalized, hard ≈ 0.87.
   * Consequence for appearance: under the shared width model (0.4× at p = 0, 1× at p = 0.5),
     light PencilKit handwriting renders noticeably thinner in the canonical renderer than in
     PencilKit. PencilKit's `.pen` ink barely varies width with force (sizes 2.73–3.25 at tool
     width 3).

## Round-trip tests

| Test | Automated now | Manual / device part | State |
|---|---|---|---|
| Canonical ↔ PencilKit | ✅ on device (`interop`) | — | done, see above |
| A: iPad → Linux | ✅ `tauri_app/tests/interop-ipad.test.ts` parses every pulled PencilKit export with the Tauri engine: no field changes, identical re-export. Return leg: Linux-edited doc → `m00ctl.py import` → PencilKit → `pencil-export` | visual check in the Tauri Linux GUI (files are in its `strokes/` dir) | **passed 2026-11-02**, see below |
| B: BOOX → Linux | the demo app's Save writes canonical JSON (`adb pull …/files/strokes`) | visual + geometry | BOOX not connected |
| C: PencilKit ↔ BOOX | same canonical files; BOOX `loadStrokes` | positioning and width check on both devices | BOOX not connected |

Any canonical file can be checked against the schema and geometry rules with the Kotlin, TS
and Swift tests. A standalone validator script is still to do.
