# Development

Stack: Tauri 2 · Vue 3 (Composition API) + TypeScript · Vite · Tailwind CSS 4 · `@lucide/vue` ·
Pinia · PDF.js · Rust (`penroam-core`, `penroam-storage`, `penroam-pdf`, `lopdf`, `zip`) ·
native ink adapters (BOOX Onyx SDK on Android, UIKit Pencil layer on iPadOS).
Design: [`architecture/overview.md`](architecture/overview.md).

## Repository

```text
apps/desktop-mobile/   Tauri app (Vue frontend in src/, Rust shell in src-tauri/)
crates/                penroam-core · penroam-storage · penroam-pdf · penroam-sync-protocol · penroam-sync-testkit
plugins/               tauri-plugin-native-ink (Android/BOOX + iOS adapters)
android/               BooxInkView (Onyx Pen SDK module) + native reference app
ios/                   InkKit (Swift ink core, Pencil surface) + BenchHost (iPad test harness)
tests/fixtures/        geometry/text goldens, benchmark scenes, PDF fixture, .prp golden, legacy libraries
fuzz/                  cargo-fuzz targets
benchmarks/            raw benchmark output (local only, git-ignored)
tools/                 Linux E2E driver, BOOX check script, .penroam comparison
reference/             framework-evaluation prototypes kept as regression baselines
docs/                  architecture, formats, protocol, platforms, development, README images
```

## Build and run

Prerequisites: Rust stable, Node 24 + pnpm 10, Tauri 2 Linux deps (`libwebkit2gtk-4.1-dev`).

```bash
pnpm install
pnpm tauri dev                     # run the desktop app (Vite dev server + Rust)
pnpm dev                           # frontend only (no storage engine: shows an error state)
pnpm build                         # type-check + production frontend build
pnpm tauri build --no-bundle       # release binary: target/release/penroam-app
pnpm tauri build                   # .deb + AppImage
pnpm test                          # TypeScript/Vue tests (Vitest)
pnpm typecheck && pnpm lint
cargo test --workspace             # Rust tests (also regenerates TS bindings in src/generated)
cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check
pnpm fixtures:check                # verify shared fixtures against frozen hashes (pnpm fixtures regenerates)
cargo run -p penroam-sync-testkit --bin penroam-sync-fixtures -- --check crates/penroam-sync-protocol/fixtures/v1
```

`PENROAM_LIBRARY_DIR` points the app at a different library folder (useful for testing against
a copy of a fixture library).

## Android (BOOX)

```bash
export ANDROID_HOME=~/Android/sdk NDK_HOME=~/Android/sdk/ndk/29.0.14206865
cd apps/desktop-mobile
pnpm tauri android init --ci && python3 scripts/patch-android.py   # once (gen/android is committed)
pnpm tauri android build --apk --target aarch64
adb install -r src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk
cd ../../android && ./gradlew :boox-ink:testDebugUnitTest          # JVM tests of the BOOX module
tools/boox-check.sh                                                # on-device plumbing check + manual checklist
```

Details: [`platforms/boox.md`](platforms/boox.md).

## iOS / iPadOS

Requires macOS + Xcode (not available in the environment used so far):

```bash
cd apps/desktop-mobile && pnpm tauri ios init && pnpm tauri ios build --export-method debugging
cd ios/InkKit && swift test        # Linux or macOS: InkCore + PencilSurface tests
```

Details and the device checklist: [`platforms/ipad.md`](platforms/ipad.md).

## Benchmarks

Workload D in the real app ([`benchmarks/protocol.md`](benchmarks/protocol.md)):

```bash
PENROAM_AUTOBENCH=linux,exit PENROAM_RESULTS_DIR=$PWD/benchmarks/results/linux \
  PENROAM_LIBRARY_DIR=$(mktemp -d) target/release/penroam-app
```

Storage engine micro-benchmark: `cargo run --release -p penroam-storage --example library_bench`.
Page codec: `cargo run --release -p penroam-core --example page_codec_bench`.
Raw output goes to the git-ignored `benchmarks/results/`; the method is in
[`benchmarks/protocol.md`](benchmarks/protocol.md).

## Linux real-input E2E

XTest, X11 only; run inside Xephyr when the desktop is locked:

```bash
cc -O2 -o tools/linux-e2e/xdrive tools/linux-e2e/xdrive.c -lX11 -lXtst
Xephyr :7 -screen 1300x940 -ac & DISPLAY=:7 python3 tools/linux-e2e/e2e.py script.txt out/
```

Sessions used so far are in `tools/linux-e2e/sessions/`; see [`platforms/linux.md`](platforms/linux.md).

## README screenshots

`tools/screenshots/capture.sh` regenerates the README images in `docs/images/`. Run it
whenever the UI changes, and update the README's usage section in the same change. Here is
what the script does:

1. It writes a demo PDF (`make_pdf.py`).
2. It builds a demo library with
   `cargo run -p penroam-storage --example demo_library`. The library contains handwriting
   drawn from the public-domain Hershey script stroke font in `hershey-script.json`, made by
   `extract_hershey.py`. It also has typed text, highlights, sketches and an annotated PDF.
3. It opens the release app on that library in a 2560×1800 Xephyr display with
   `GDK_SCALE=2`, drives it with `session.txt` through the E2E driver above, and stores the
   captures at 1600 px wide.

The session's click coordinates assume the current layout. If a button moves, update
`session.txt`. It needs Xephyr, ImageMagick and the XTest headers.

## Fuzzing

`fuzz/` holds cargo-fuzz targets (`decode_page`, `journal_open`, `sync_batch_frame`); they need a
nightly toolchain (`cargo +nightly fuzz run decode_page`). A deterministic fuzz smoke runs as part
of `cargo test`. The scheduled workflow is `.github/workflows/fuzz.yml`.
