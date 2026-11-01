# M00 — Cross-Platform Handwriting Framework Bake-Off

Flutter vs Tauri v2 for a handwriting-first notes app, with a shared native
BOOX (Onyx Pen SDK) ink module. The plan is in [PLAN.md](PLAN.md). The
current findings and open blockers are in [docs/comparison.md](docs/comparison.md).
**Status tracker / resume checklist: [docs/status.md](docs/status.md).** Interop: [docs/interop.md](docs/interop.md).

## Layout

```text
.
├── android/                 Standalone Gradle build (M00.2 / M00.3)
│   ├── boox-ink/            Shared Kotlin BOOX ink module (BooxInkView)  ← used by all hosts
│   └── boox-ink-demo/       Native reference app (no framework)
├── flutter_app/             Flutter prototype (Listener + CustomPainter; Platform View for BOOX)
├── tauri_app/               Tauri v2 prototype (React + TS + Vite + Canvas 2D; Kotlin plugin overlay for BOOX)
│   └── plugins/tauri-plugin-boox-ink/ , tauri-plugin-pencilkit/ (iOS, Swift)
├── ios/InkKit/               Shared Swift: InkCore (canonical format), PencilInk (PencilKit backend), WebTuning
├── ios/M00iOS/               Standalone UIKit/PencilKit host + WKWebView proxy (xtool; USB-controlled via tools/m00ctl.py)
├── fixtures/                Canonical stroke fixtures, geometry golden file, benchmark scenes (+ generator)
├── benchmarks/              protocol.md, raw results/, analysis/
└── docs/                    architecture, BOOX integration, iPad, Linux, comparison report
```

The repo root stands in for `m00-handwriting-bakeoff/` from PLAN.md §3. The
extra pieces (the reference app and the Tauri plugin crate) live where their
build systems expect them.

## Status (2026-11-01)

| Area | State |
|---|---|
| Shared fixtures + geometry spec | Done. One reference generator; TS, Kotlin and Dart ports are checked against the same golden file |
| `android/boox-ink` | Builds against the official Onyx SDK (pen 1.5.2 / device 1.3.3 / base 1.8.4) from repo.boox.com; 11 JVM unit tests pass. **Not yet run on a BOOX device.** |
| Native reference APK | Built (`boox-ink-demo-release.apk`). **Not yet run on a BOOX device.** |
| Tauri Linux | Built and run under Wayland; automated Workload D results in `benchmarks/results/linux/` |
| Tauri Android APK | Built (arm64, release, debug-signed) with the BOOX plugin and overlay. **Not yet run on a device.** |
| Tauri / Flutter iPadOS | Blocked: needs macOS + Xcode |
| `ios/M00iOS` (xtool) | Native UIKit Pencil reference + Tauri frontend in WKWebView (a proxy). Builds on Linux; Swift tests pass; **install blocked by the Apple PLA** (Account Holder must accept it) |
| Flutter | Dart sources, tests and Android host code written. The Android Kotlin compiles against the real Flutter embedding + boox-ink. **The Dart code has not been compiled:** the Flutter SDK install was not permitted in the build session (see docs/comparison.md §Blockers) |

## Quick start

```bash
# Fixtures (deterministic; --check verifies files on disk)
node fixtures/generate-fixtures.mjs --check

# Shared BOOX module + native reference app
cd android && ./gradlew :boox-ink:testDebugUnitTest :boox-ink-demo:assembleRelease
adb install -r boox-ink-demo/build/outputs/apk/release/boox-ink-demo-release.apk

# Tauri
cd tauri_app && npm install && npm test
npx tauri build --no-bundle                     # Linux binary
python3 scripts/patch-android.py                # after `npx tauri android init`
npx tauri android build --apk --target aarch64  # BOOX APK
M00_AUTOBENCH=1 M00_RESULTS_DIR=$PWD/../benchmarks/results/linux src-tauri/target/release/m00-tauri-app

# Flutter (requires the Flutter SDK)
cd flutter_app && tool/bootstrap.sh             # flutter create + wire boox-ink
flutter test
flutter run -d linux --release
M00_AUTOBENCH=1 M00_RESULTS_DIR=$PWD/../benchmarks/results/linux build/linux/x64/release/bundle/m00_flutter_app
flutter build apk --release
```

Device procedures: [docs/boox-integration.md](docs/boox-integration.md),
[docs/ipad-testing.md](docs/ipad-testing.md),
[docs/linux-testing.md](docs/linux-testing.md),
[benchmarks/protocol.md](benchmarks/protocol.md).
