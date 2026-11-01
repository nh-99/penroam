# Tauri v2 prototype

React + TypeScript + Vite + Canvas 2D, with a minimal Rust backend. The ink engine
(`src/ink/engine.ts`) is framework-free; React only renders the toolbar and debug overlay.

```bash
npm install
npm test                           # geometry golden, scene determinism, metrics
npx tauri dev                      # desktop dev
npx tauri build --no-bundle        # release binary: src-tauri/target/release/m00-tauri-app
M00_AUTOBENCH=1 M00_RESULTS_DIR=../benchmarks/results/linux src-tauri/target/release/m00-tauri-app

# Android (BOOX)
export ANDROID_HOME=~/Android/sdk NDK_HOME=~/Android/sdk/ndk/<ver>
npx tauri android init --ci        # only if src-tauri/gen/android is missing
python3 scripts/patch-android.py   # wire android/boox-ink (idempotent)
npx tauri android build --apk --target aarch64
```

* BOOX modes B/C: `plugins/tauri-plugin-boox-ink`. A Kotlin `@TauriPlugin` adds the shared
  `BooxInkView` above the WebView at the canvas rect and emits `stroke` / `erased` /
  `viewport` / `status` events. The JS canvas ignores input while the overlay is active.
* iOS: `npx tauri ios init` on macOS. See docs/ipad-testing.md.
