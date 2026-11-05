# Development

Stack: Tauri 2 · Vue 3 (Composition API) + TypeScript · Vite · Tailwind CSS 4 · `@lucide/vue` ·
Pinia · PDF.js · Rust (`penroam-core`, `penroam-storage`, `penroam-pdf`, `penroam-search`, `lopdf`, `zip`,
SQLite via `rusqlite`) ·
native ink adapters (BOOX Onyx SDK on Android, UIKit Pencil layer on iPadOS).
Design: [`architecture/overview.md`](architecture/overview.md).

## Repository

```text
apps/desktop-mobile/   Tauri app (Vue frontend in src/, Rust shell in src-tauri/)
crates/                penroam-core · penroam-storage · penroam-pdf · penroam-search · penroam-sync-protocol · penroam-sync-testkit · penroam-sync
plugins/               tauri-plugin-native-ink (Android/BOOX + iOS adapters)
android/               BooxInkView (Onyx Pen SDK module used by the native ink plugin)
ios/                   InkKit (Swift ink core and Pencil surface used by the native ink plugin)
tests/fixtures/        geometry/text goldens, benchmark scenes, PDF fixture, .prp golden, legacy libraries
fuzz/                  cargo-fuzz targets
benchmarks/            raw benchmark output (local only, git-ignored)
tools/                 Linux E2E driver, BOOX check script, .penroam comparison, screenshots, font metrics, search evaluation corpus
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

## Sync (developer preview)

Sync goes to Penroam Cloud (`https://sync.penroam.ink`). Release builds accept no other
server; the backend refuses any other address whatever the settings send. In **debug builds**,
tap the version number at the bottom of the settings five times (within three seconds) to turn
**developer mode** on or off. With it on, the setup form asks for a server address, and it
works against two kinds of server:

* **Servers that advertise `auth.v1`** (protocol §12a): **Sign in with your browser** opens the
  system browser for the server's identity provider (PKCE). Desktop gets the answer on a loopback
  redirect; Android and iOS get it as an `ink.penroam.app:` link, which works only if the server
  sets a mobile redirect. The session is kept
  in the OS keychain next to the device keys. When it ends, the status says so and offers
  **Sign in again**; queued changes stay on the device.
* **The development test server**: you type an account name. It has a stub sign-in, no TLS and no
  hardening, so keep it on loopback.

Once set up, servers with `account.v1` (Penroam Cloud) also get a **Penroam Cloud** panel in the
sync settings: plan, storage used, why uploads are paused if they are, and **Delete account…**
(a fresh browser sign-in confirms it). Desktop builds show the server's plans and **Manage plan
and billing**, which open the server's payment pages in the system browser; the panel reloads
when the window regains focus. Android and iOS builds never show prices or billing buttons. The
development test server lists `account.v1` with a free plan and no actions.

In developer mode, server addresses must be `https://`, or `http://` on this computer only
(`PENROAM_SYNC_ALLOW_ANY_URL=1` allows any `http://` host, for development).

```bash
cargo run -p penroam-sync-testkit --bin penroam-sync-testserver -- --listen 127.0.0.1:8787 --data /tmp/penroam-sync-server
PENROAM_DEV_SYNC=1 pnpm tauri dev      # sync settings appear in debug builds or with PENROAM_DEV_SYNC=1
```

In the app: **Settings → Sync (developer preview)**. Set up the first device (save the recovery
code), then on a second profile (another `PENROAM_LIBRARY_DIR`) choose pairing and approve its
code from the first device, or use the recovery code.

Scripted UI runs that sign in must not reach the developer's real browser or keychain: give the
app its own XDG data, config and runtime directories, and register a stand-in browser as the
`x-scheme-handler/https` default in that private `mimeapps.list`. `xdg-open` ignores `$BROWSER`
whenever a default handler exists.

Device keys go to the OS keychain (Secret Service on Linux, Keychain on macOS/iOS, Credential
Manager on Windows, Keystore-backed storage on Android). Without one, sync refuses to start;
for headless development set `PENROAM_SYNC_ALLOW_FILE_SECRETS=1`. Check the real keychain on a
desktop session with
`cargo test -p penroam-sync --features keychain --lib -- --ignored keychain`.
The CLI keeps its keys in a file inside its profile folder (it is a test tool).

Scripted clients, one profile directory each (separate processes, own keys):

```bash
cargo run -p penroam-sync --bin penroam-sync-cli -- /tmp/dev-a create http://127.0.0.1:8787 acct-dev
cargo run -p penroam-sync --bin penroam-sync-cli -- /tmp/dev-b pair-begin http://127.0.0.1:8787 acct-dev
cargo run -p penroam-sync --bin penroam-sync-cli -- /tmp/dev-a pair-approve <code>
cargo run -p penroam-sync --bin penroam-sync-cli -- /tmp/dev-b pair-poll
cargo run -p penroam-sync --bin penroam-sync-cli -- /tmp/dev-a sync
```

Against a server with `auth.v1`, sign in first (system browser; `--id-token-file` takes an ID
token obtained elsewhere, for scripted tests) and pass `-` as the account:

```bash
cargo run -p penroam-sync --bin penroam-sync-cli -- /tmp/dev-a login https://sync.example
cargo run -p penroam-sync --bin penroam-sync-cli -- /tmp/dev-a create https://sync.example -
```

The CLI keeps its sessions in `<profile>/sessions.json` (mode 0600).

**Logging and credentials.** The HTTP library the client uses (`ureq`) hex-dumps raw requests,
bearer tokens included, at TRACE level. Programs that use `penroam-sync` and install a logger
must:

* cap every target in `penroam_sync_protocol::redact::HTTP_CLIENT_LOG_TARGETS` at INFO in code
  (`is_http_client_target`), so no `RUST_LOG` value can lift it;
* pass each formatted line through `redact::scrub_line` as a backstop: it redacts `Bearer`
  values, JWTs, recovery codes and prefixed tokens, and drops byte-dump lines (a token split
  across a dump cannot be recognised piece by piece).

The app compiles DEBUG/TRACE `log` records out of release builds (`release_max_level_info` on
`log` in `apps/desktop-mobile/src-tauri/Cargo.toml`); a compile-time assertion in `lib.rs`
fails the release build if that cap is lost. The app installs no logger today.

Tests: `cargo test -p penroam-sync` runs the convergence and fault-injection scenarios
(`tests/sync.rs`), the randomized three-device convergence test (`tests/random.rs`, slow in
debug builds; use `--release`), two CLI processes against the HTTP test server
(`tests/processes.rs`) and the pinned crypto vectors (`tests/vectors.rs`). The protocol crate's
`tests/fixtures.rs` also checks that `docs/protocol/openapi.yaml` lists every route.

## Search

`crates/penroam-search` (segmentation, recognizers, SQLite FTS5 index) and the app worker in
`src-tauri/src/search.rs`. Design: [`architecture/overview.md`](architecture/overview.md) §5;
storage and privacy: [`security/search-index.md`](security/search-index.md).

* SQLite is compiled in (`rusqlite` with `bundled`), so building needs a C compiler for every
  target, including the Android NDK and Xcode toolchains.
* Feature `windows-ink` enables the Windows handwriting recognizer; it has no effect on other
  targets, and the app always enables it. Apple Vision (`src/vision.rs`) is compiled on macOS
  and iOS only; on Windows it can be type-checked by including the sources in a scratch crate
  without SQLite (`cargo check --target aarch64-apple-darwin`), since `rusqlite` needs the
  Apple SDK.
* Feature `onnx-model` adds the Penroam handwriting model ([`formats/handwriting-model.md`](formats/handwriting-model.md))
  through `ort`. The crate does not choose how ONNX Runtime is linked: the app uses pyke's
  prebuilt runtime (`ort/download-binaries`, fetched at build time) on desktop and iOS, and
  `ort/load-dynamic` on Android with Microsoft's `onnxruntime-android` AAR, which
  `scripts/patch-android.py` adds to the Gradle project. Tests and examples of the crate link
  the prebuilt runtime through a dev-dependency.
* `cargo run -p penroam-search --example raster -- strokes.json out.pgm` renders strokes with the
  model rasterizer; the training pipeline keeps golden images from it to stay byte-identical.
* `cargo test -p penroam-search --features windows-ink` on Windows also runs
  `tests/windows_ink.rs` against the installed recognizer. It skips itself, with a message,
  when Windows has no handwriting recognizer (most CI images).
* The app keeps the index in `<app cache dir>/search/<library device id>/` and its settings
  in `<app config dir>/search.json`. Deleting either is always safe.

### Search evaluation

`examples/eval.rs` measures segmentation, character and word error rates, search
recall@page / recall@region, false positives, recognition and query latency and peak memory
over a corpus, overall and per writing style:

```bash
cargo run -p penroam-search --release --features windows-ink,onnx-model --example eval -- corpus.json --recognizer windows --device "my laptop" --out report.json
# --recognizer model --model <bundle dir>: an installed-format handwriting model
# --recognizer chain --model <bundle dir>: Windows first, the model as fallback (as the app does)
# --recognizer scripted: perfect recognition from the truth, to test segmentation and ranking
```

A corpus is one JSON file:

```text
{ "pages": [ { "id": "…", "style": "cursive",
               "page": <Penroam page JSON>,
               "lines": [ { "text": "call alice", "bounds": {x,y,w,h},
                            "words": [ { "text": "call", "strokeIds": ["…"], "bounds": {x,y,w,h} } ] } ] } ],
  "distractors": ["words", "that", "appear", "on", "no", "page"] }
```

`tools/search-eval/make-synthetic-corpus.mjs` writes a synthetic one from the Hershey
single-stroke fonts (`--font` takes `hersheytext.json` from the MIT-licensed `hersheytext`
npm package; it is not vendored). Synthetic pages test the pipeline. They are not
handwriting, and numbers from them are not accuracy claims. Real measurements need pages
written by people who agreed to it, kept outside the repository. Reports and corpora stay
local (`benchmarks/` is git-ignored).

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

## CI

`.github/workflows/ci.yml` runs on every branch push. Nearly all of its time goes on compiling, so
it avoids compiling:

* Jobs run in parallel, with nothing waiting on another job's result. The `changes` job runs
  `dorny/paths-filter` and skips the jobs a push can't affect. A new directory that Rust or the
  frontend reads from has to be added to its filters, or changes there won't run those jobs.
* Rust jobs use `.github/actions/setup-rust`, which restores `Swatinem/rust-cache`. Only `main`
  writes to the cache; other branches read from it, so the first push after `Cargo.lock` or the
  stable toolchain changes is a slower cold build. The cache includes the downloaded ONNX Runtime
  because `ort-sys` links against it from outside `target/`.
* Only `main` builds the full release profile (fat LTO) and uploads the `.deb` and AppImage.
  Other branches run the Linux smoke tests on a debug build, and all branches build Windows and
  macOS in debug. Some compile-time checks exist only in release builds, such as the `log` level
  cap in `apps/desktop-mobile/src-tauri/src/lib.rs`, so a branch can pass and still fail on `main`.
* `.github/workflows/release-check.yml` builds the release profile on Windows and macOS nightly.

Pushes of `sync-protocol-v*` tags run only the sync contract job. Pull requests from this
repository are skipped because the branch push already ran; pull requests from forks run in full.

## Fuzzing

`fuzz/` holds cargo-fuzz targets (`decode_page`, `journal_open`, `sync_batch_frame`); they need a
nightly toolchain (`cargo +nightly fuzz run decode_page`). A deterministic fuzz smoke runs as part
of `cargo test`. The scheduled workflow is `.github/workflows/fuzz.yml`.
