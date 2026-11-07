# Development

Stack: Tauri 2 · Vue 3 (Composition API) + TypeScript · Vite · Tailwind CSS 4 · `@lucide/vue` ·
Pinia · PDF.js · markdown-it · MathJax · Rust (`penroam-core`, `penroam-storage`, `penroam-pdf`,
`penroam-search`, `lopdf`, `pulldown-cmark`, `zip`, SQLite via `rusqlite`) ·
native ink adapters (BOOX Onyx SDK on Android, UIKit Pencil layer on iPadOS).
Design: [`architecture/overview.md`](architecture/overview.md).

## Repository

```text
apps/desktop-mobile/   Tauri app (Vue frontend in src/, Rust shell in src-tauri/)
crates/                penroam-core · penroam-storage · penroam-pdf · penroam-interchange · penroam-webdav · penroam-search · penroam-sync-protocol · penroam-sync-testkit · penroam-sync
plugins/               tauri-plugin-native-ink (Android/BOOX + iOS adapters) · tauri-plugin-native-print (system print dialogs) · tauri-plugin-store-billing (StoreKit purchases)
android/               BooxInkView (Onyx Pen SDK module used by the native ink plugin)
ios/                   InkKit (Swift ink core and Pencil surface used by the native ink plugin)
tests/fixtures/        geometry/text goldens, benchmark scenes, PDF fixture, .prp golden, legacy libraries
fuzz/                  cargo-fuzz targets
benchmarks/            raw benchmark output (local only, git-ignored)
tools/                 Linux E2E driver, BOOX check script, .penroam comparison, screenshots, font metrics, search evaluation corpus
docs/                  architecture, formats, protocol, platforms, development, README images
```

## Build and run

Prerequisites: Rust (rustup installs the version in `rust-toolchain.toml`), Node 24 + pnpm 10, Tauri 2 Linux deps (`libwebkit2gtk-4.1-dev`).

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

Details: [`platforms/boox.md`](platforms/boox.md). Other Android tablets, including the S Pen:
[`platforms/android.md`](platforms/android.md). What is verified on which device:
[`platforms/capabilities.md`](platforms/capabilities.md).

## iOS / iPadOS

Requires macOS + Xcode (not available in the environment used so far):

```bash
cd apps/desktop-mobile && pnpm tauri ios init && pnpm tauri ios build --export-method debugging
cd ios/InkKit && swift test        # Linux or macOS: InkCore + PencilSurface tests
```

Details and the device checklist: [`platforms/ipad.md`](platforms/ipad.md).

## Text layout

Text block layout exists twice, in `crates/penroam-core/src/markdown.rs` and
`apps/desktop-mobile/src/editor/markdown.ts`. Change them together, then regenerate the golden
file both are checked against:

```sh
UPDATE_GOLDEN=1 cargo test -p penroam-core --test markdown
pnpm --filter penroam test
```

Add a line to `CORPUS` in `crates/penroam-core/tests/markdown.rs` when the two parsers turn out to
disagree on something. After changing a bundled font, regenerate the widths with
`node tools/gen-font-metrics.mjs apps/desktop-mobile/src/assets/fonts crates/penroam-core/data/text-metrics.json`.

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

## Handwriting model catalog

The app downloads models from a signed catalog (§12c of
[`protocol/sync-protocol-v1.md`](protocol/sync-protocol-v1.md)). Penroam Cloud publishes the model
promoted in OtterML by itself; the `sign` command below is for testing and for other servers. It trusts the public keys in
`TRUSTED_KEYS` (`apps/desktop-mobile/src-tauri/src/models.rs`). A build with no key would make no
model requests, and models could then only be installed from a file.

```bash
# once: the secret file must never reach a server that answers clients; the public key goes
# into TRUSTED_KEYS
cargo run -p penroam-search --example model_catalog -- keygen catalog.key
# for each release of the catalog; --sequence must exceed the last one
cargo run -p penroam-search --example model_catalog -- sign --key catalog.key --key-id 2026-1 \
  --sequence 1 --min-app 0.2.0 --recommend hw-en.penroam-model --out catalog.signed.json
```

To try downloads in a debug build, sign with a throwaway key and point the app at any server that
offers `models.v1`:

```bash
PENROAM_DEV_MODEL_KEY=dev:<public key> PENROAM_DEV_MODEL_SERVER=http://127.0.0.1:8080 pnpm tauri dev
```

Both variables are ignored in release builds. To replay the first-run download, delete
`<app data>/models/state.json`.

## Linux real-input E2E

XTest, X11 only; run inside Xephyr when the desktop is locked:

```bash
cc -O2 -o tools/linux-e2e/xdrive tools/linux-e2e/xdrive.c -lX11 -lXtst
Xephyr :7 -screen 1300x940 -ac & DISPLAY=:7 python3 tools/linux-e2e/e2e.py script.txt out/
```

Sessions used so far are in `tools/linux-e2e/sessions/`; see [`platforms/linux.md`](platforms/linux.md).

## Sync

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
pnpm tauri dev
```

In the app: **Settings → Sync**. Set up the first device (save the recovery
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
* `cargo run -p penroam-search --example model_catalog` makes catalog signing keys and signed
  catalogs (see [Handwriting model catalog](#handwriting-model-catalog)).
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

`examples/make_eval_corpus.rs` builds such a corpus from exported notebooks in which each writer
copied a fixed set of prompts, one prompt line per written line:

```bash
cargo run -p penroam-search --example make_eval_corpus -- --prompts prompts.txt --out corpus.json writer-a=a.penroam writer-b=b.penroam
```

It matches each page's regions to its prompt from the top down and leaves out, with a message,
any page where the counts differ. The writer becomes the page's `style`, so reports come out per
writer. Handwriting models are compared with the benchmark that sits beside the trainers in the
private models repository; on the same corpus and bundle its numbers agree with this tool's.

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
* `.github/workflows/security.yml` runs on every push and daily, because advisories appear without
  any change here. It runs `cargo deny` (`deny.toml`: advisories, licences, sources), `pnpm audit`
  and gitleaks (`.gitleaks.toml`). A new advisory fails it. Upgrade if you can; otherwise add the
  id to `deny.toml` with a reason. Allowing a new licence is a licence decision, not a dependency
  update.
* Actions are pinned to full commit SHAs, with the tag in a comment, and the Rust version is pinned
  in `rust-toolchain.toml`. Update either on purpose, in its own change.
* On `main`, `tools/perf-warn.py` compares the Linux smoke benchmark with the previous run and
  leaves a warning on a large regression. It never fails a build: shared runners are too noisy.

Pushes of `sync-protocol-v*` tags run only the sync contract job. Pull requests from this
repository are skipped because the branch push already ran; pull requests from forks run in full.

## Releases

Everything below runs from a tag, and nothing reaches users without a person publishing it.

1. Set the version in `Cargo.toml` (workspace), `apps/desktop-mobile/package.json` and
   `tauri.conf.json`, and add a `## X.Y.Z` section to `CHANGELOG.md`. Update
   [`platforms/release-matrix.md`](platforms/release-matrix.md) and
   [`known-issues.md`](known-issues.md).
2. Push the tag `vX.Y.Z`, or `vX.Y.Z-beta.N` (also `alpha`, `rc`). `release.yml` checks that the tag
   matches all three versions, runs the tests, builds Linux, Windows, macOS and Android, and
   makes a **draft** release with `SHA256SUMS`, `contract-manifest.json`, a CycloneDX SBOM, build
   provenance attestations and `latest.json`.
3. Read the draft, try the packages, and publish it.
4. Run `promote.yml` with the tag and `beta`, and later `stable`. That copies `latest.json` to the
   `updates-beta` or `updates-stable` release, which is what the app reads
   (`apps/desktop-mobile/src-tauri/src/updates.rs`).

The updater never downgrades, so a bad release is fixed by promoting a newer one. Notes are stored
apart from the app, so installing an older build by hand is safe: it refuses formats it doesn't know.

A stable tag fails unless the Apple, Android and update signing secrets are present. Windows is left
out of a stable release while `WINDOWS_SIGN_COMMAND` is unset, so an unsigned installer is never
stable; the notes say so, and the app treats a feed without its platform as up to date. A pre-release
may be unsigned and always includes Windows. `tools/release/release.py` decides all of this. `python3 -m unittest discover -s tools/release` tests it.

| Name (GitHub) | Kind | Used for |
|---|---|---|
| `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | Secret | Signing updates. The public key goes in `plugins.updater.pubkey` in `tauri.conf.json`; while that is empty, the app has no update feature |
| `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID`; variable `APPLE_SIGNING_IDENTITY` | Secret, variable | macOS signing and notarization, read by the Tauri CLI |
| `WINDOWS_SIGN_COMMAND` | Variable | Passed to `bundle.windows.signCommand`, with `%1` for the file |
| `PENROAM_ANDROID_KEYSTORE_BASE64`, `PENROAM_ANDROID_KEYSTORE_PASSWORD`, `PENROAM_ANDROID_KEY_ALIAS`, `PENROAM_ANDROID_KEY_PASSWORD` | Secret | The Android release key. Without it, builds use the debug key, which is for test devices only |

Generate the update key once with `pnpm tauri signer generate` and keep the private half offline as
well: losing it means existing installs can't update. `promote.yml` uses the `release` GitHub
environment, so required reviewers can be added there.

The same tag uploads to testers when the store secrets below are set, and skips that step when they
aren't. The `testflight` job generates `gen/apple` on a macOS runner and signs through Apple's
cloud-managed certificates, so it needs no certificate or profile. The `google-play` job uploads to
the track in the `GOOGLE_PLAY_TRACK` variable, `internal` by default. Moving a build further happens
in App Store Connect or the Play Console. Build numbers come from the workflow's run number, because
a beta and its final release share `X.Y.Z` and the stores refuse a repeated build number.

| Name (GitHub) | Kind | Used for |
|---|---|---|
| `APPLE_API_KEY`, `APPLE_API_ISSUER`, `APPLE_API_KEY_P8` | Secret | App Store Connect API key: the key ID, the issuer ID and the contents of the `.p8` file. The key needs the Admin role for cloud signing. `APPLE_TEAM_ID` is shared with macOS signing |
| `GOOGLE_PLAY_SERVICE_ACCOUNT_JSON` | Secret | Service account allowed to release to the app. Upload the first `.aab` to the Play Console by hand: the API can't create a release for a new app |

Store builds never contain links to outside purchases: App Store builds sell only through StoreKit.

Keep `release.py notices` output out of the repository: it is generated for each build into
`apps/desktop-mobile/public/` and shown under **Settings → About**.

## Fuzzing

`fuzz/` holds cargo-fuzz targets (`decode_page`, `journal_open`, `sync_batch_frame` and one per importer); they need a
nightly toolchain (`cargo +nightly fuzz run decode_page`). A deterministic fuzz smoke runs as part
of `cargo test`. The scheduled workflow is `.github/workflows/fuzz.yml`.

## Testing importers on real files

Third-party samples have their own licences, so they stay out of this repository. Download some
into a folder (Xournal++'s `test/files`, the `.enex` files in Joplin's
`packages/app-cli/tests/enex_to_md`, evernote2md's `encoding/enex/testdata`) and run:

```sh
PENROAM_IMPORT_CORPUS=/path/to/files cargo test -p penroam-interchange --test real_files -- --nocapture
```

Each file is listed as imported or refused. A panic, or output that fails validation, fails the
test. Add `PENROAM_IMPORT_PDF_DIR=/path/to/out` to write each result as an annotated PDF for
comparing with the original application. A real `.jex` can be made with the Joplin command line,
`joplin export --format jex`; run third-party tools like these in a container.
