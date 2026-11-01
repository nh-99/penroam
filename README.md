# Penroam

An offline-first, open-source, handwriting-first notebook. Write with a pen, mouse or finger,
type on the same page, annotate PDFs, and move notebooks between devices as open `.penroam`
files. No account, server, cloud, analytics or AI model.

**Status: M01 ("a notebook worth using") — see [docs/M01-progress.md](docs/M01-progress.md)
for what works, what is simulated, and what is still blocked, and
[docs/M01-results.md](docs/M01-results.md) for test, build and benchmark results.**

Stack: Tauri 2 · Vue 3 (Composition API) + TypeScript · Vite · Tailwind CSS 4 · `@lucide/vue` ·
Pinia · PDF.js · Rust (`penroam-core`, `penroam-storage`, `penroam-pdf`, `lopdf`, `zip`) ·
native ink adapters (BOOX Onyx SDK on Android, UIKit Pencil layer on iPadOS).

## Features (M01)

* Library of notebooks with covers; create, rename, recolour, trash/restore, delete forever.
* Pages: Letter or A4, blank/ruled/grid/dotted, add/insert/reorder (drag or menu)/delete/restore,
  thumbnails.
* Pen (pressure-aware), highlighter, whole-stroke eraser (incl. the pen's eraser button),
  select/move, text, pan; zoom/pan with wheel, pinch, keyboard; undo/redo.
* Multiline typed text blocks on the same canvas as ink: insert, edit, move, resize, colour, size.
* PDF import as page backgrounds (original bytes kept), write/type on them, export an annotated
  PDF with vector ink and text.
* Durable autosave with explicit saved/saving/unsaved/error states and crash recovery.
* `.penroam` export/import (lossless, documented ZIP format).
* Light, dark and ePaper themes; responsive desktop/tablet/phone layouts.

## Repository

```text
apps/desktop-mobile/   Tauri app (Vue frontend in src/, Rust shell in src-tauri/)
crates/                penroam-core · penroam-storage · penroam-pdf
plugins/               tauri-plugin-native-ink (Android/BOOX + iOS adapters)
android/ ios/          M00 native ink modules (BooxInkView, InkKit) reused by M01
tests/fixtures/        geometry/text goldens, M00 scenes, PDF fixture
benchmarks/            M00 protocol + raw results, M01 results
tools/linux-e2e/       real-input E2E driver for Linux (XTest)
reference/             M00 prototypes kept for regression baselines
docs/                  architecture, file format, platforms, progress, results, M00 record
```

## Build and run

Prerequisites: Rust stable, Node 24 + pnpm 10, Tauri 2 Linux deps (`libwebkit2gtk-4.1-dev`).

```bash
pnpm install
pnpm tauri dev                     # run the desktop app (Vite dev server + Rust)
pnpm dev                           # frontend only (no storage engine: shows an error state)
pnpm build                         # type-check + production frontend build
pnpm tauri build --no-bundle       # release binary: target/release/penroam-app
pnpm test                          # TypeScript/Vue tests (Vitest)
pnpm typecheck && pnpm lint
cargo test --workspace             # Rust tests (also regenerates TS bindings in src/generated)
pnpm fixtures:check                # verify M00 fixtures against frozen hashes (pnpm fixtures regenerates)
```

Android (BOOX):

```bash
export ANDROID_HOME=~/Android/sdk NDK_HOME=~/Android/sdk/ndk/29.0.14206865
cd apps/desktop-mobile
pnpm tauri android init --ci && python3 scripts/patch-android.py   # once (gen/android is committed)
pnpm tauri android build --apk --target aarch64
adb install -r src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk
cd ../../android && ./gradlew :boox-ink:testDebugUnitTest          # JVM tests of the BOOX module
```

iOS/iPadOS (requires macOS + Xcode; not run in this environment):

```bash
cd apps/desktop-mobile && pnpm tauri ios init && pnpm tauri ios build --export-method debugging
cd ios/InkKit && swift test        # Linux or macOS: InkCore + PencilSurface tests
```

Benchmarks (M00 Workload D in the real app):

```bash
PENROAM_AUTOBENCH=linux,exit PENROAM_RESULTS_DIR=$PWD/benchmarks/results/m01/linux \
  PENROAM_LIBRARY_DIR=$(mktemp -d) target/release/penroam-app
```

Linux real-input E2E (XTest; run inside Xephyr when the desktop is locked):

```bash
cc -O2 -o tools/linux-e2e/xdrive tools/linux-e2e/xdrive.c -lX11 -lXtst
Xephyr :7 -screen 1300x940 -ac & DISPLAY=:7 python3 tools/linux-e2e/e2e.py script.txt out/
```

## Documentation

* [docs/architecture/M01-architecture.md](docs/architecture/M01-architecture.md) — design, data ownership, M00→M01 map
* [docs/format/penroam-format.md](docs/format/penroam-format.md) — notebook, page and `.penroam` formats
* [docs/platforms/](docs/platforms/) — BOOX, iPad and Linux notes and checklists
* [docs/M01-progress.md](docs/M01-progress.md), [docs/M01-results.md](docs/M01-results.md)
* [docs/m00/](docs/m00/) — the M00 framework bake-off record

## License

Code: MPL-2.0. Bundled font Liberation Sans: SIL OFL 1.1
(`apps/desktop-mobile/src/assets/fonts/LICENSE-LiberationSans.txt`). PDF.js: Apache-2.0.
Onyx SDK: proprietary BOOX SDK fetched from the official Onyx Maven repository at build time
(Android only).
