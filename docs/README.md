# Penroam developer README

This folder documents how Penroam is built, for people working on the code. If you want to learn
how to use the app, read the [user manual](manual/README.md) instead.

Penroam is a Tauri 2 app: a Vue 3 + TypeScript editor in the system WebView, a Rust core that owns
all notebook data, and native ink surfaces on BOOX (Onyx Pen SDK) and iPadOS (UIKit Pencil).

## Setting up

You need:

* [Rust](https://rustup.rs). rustup installs the version pinned in `rust-toolchain.toml`.
* [Node.js](https://nodejs.org) 24 and [pnpm](https://pnpm.io) 10.
* On Linux, the WebKitGTK development packages. On Debian or Ubuntu:
  `sudo apt install libwebkit2gtk-4.1-dev build-essential libssl-dev librsvg2-dev`.
* On Windows, the WebView2 runtime (included in Windows 11) and the MSVC build tools.
* For Android or iPad builds, see [Android (BOOX)](development.md#android-boox) and
  [iOS / iPadOS](development.md#ios--ipados).

Then:

```bash
git clone https://github.com/nh-99/penroam.git
cd penroam
pnpm install
pnpm tauri dev                  # run the desktop app with live reload
```

`PENROAM_LIBRARY_DIR=<folder>` runs the app on a library other than your own, which is useful for
testing against a copy or a fixture.

## Before you open a pull request

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps
pnpm typecheck && pnpm lint && pnpm test
```

[CONTRIBUTING.md](../CONTRIBUTING.md) says what reviews look for. Report security problems
privately, as [SECURITY.md](../SECURITY.md) describes.

## Where things are

| Path | What it holds |
|---|---|
| `apps/desktop-mobile/` | The Tauri app: the Vue frontend in `src/`, the Rust shell in `src-tauri/` |
| `crates/` | `penroam-core` (model, operations, layout), `penroam-storage` (library, journal, archives, history), `penroam-pdf`, `penroam-interchange` (imports), `penroam-webdav`, `penroam-search`, and the sync crates |
| `plugins/` | Tauri plugins for native ink, the system print dialog and App Store purchases |
| `android/`, `ios/` | Native ink code for BOOX and iPad |
| `tests/fixtures/` | Shared goldens and fixture libraries |
| `fuzz/` | cargo-fuzz targets |
| `tools/` | E2E drivers, screenshot capture, benchmarks and evaluation helpers |
| `docs/` | This documentation, and the [user manual](manual/README.md) |

## Developer documentation

**Building, testing and tooling**

* [development.md](development.md): building and running on each platform, tests, CI, releases,
  benchmarks, fuzzing, the Linux E2E driver, and regenerating the screenshots.
* [architecture/overview.md](architecture/overview.md): how the app fits together, from the
  editor to storage and sync.

**File formats and protocols** (these are contracts: change them only with a version bump)

* [formats/model-and-archive.md](formats/model-and-archive.md): the notebook and page model,
  operation batches and the `.penroam` archive.
* [formats/penroam-binary-v1.md](formats/penroam-binary-v1.md): the binary page file, operation
  journal and other on-disk formats.
* [formats/portability.md](formats/portability.md): what exports, backups, imports and history
  promise.
* [formats/locked-vault.md](formats/locked-vault.md): how a locked vault is stored on a device.
* [formats/handwriting-model.md](formats/handwriting-model.md): the handwriting model bundle.
* [protocol/sync-protocol-v1.md](protocol/sync-protocol-v1.md): the sync protocol.

**Security**

* [security/threat-model.md](security/threat-model.md): the sync threat model.
* [security/search-index.md](security/search-index.md): what the search index holds and who can
  read it.

**Platforms**

* [platforms/release-matrix.md](platforms/release-matrix.md): what each release is built for and
  how far each platform has got.
* [platforms/capabilities.md](platforms/capabilities.md): what each device does and what has been
  checked by hand.
* [platforms/linux.md](platforms/linux.md), [platforms/boox.md](platforms/boox.md),
  [platforms/android.md](platforms/android.md) and [platforms/ipad.md](platforms/ipad.md):
  per-platform build notes and known risks.

**Performance**

* [benchmarks/protocol.md](benchmarks/protocol.md): how ink and large-document performance is
  measured, and the device scorecard.

## Documentation for users

User-facing documentation also lives here, and changes with the app:

* [manual/](manual/README.md): the user manual. When a user-visible feature lands or changes,
  update the chapter that covers it, and the README if the feature is one of its highlights.
* [support.md](support.md) and [known-issues.md](known-issues.md): help and current problems.
* [policies/privacy.md](policies/privacy.md) and
  [policies/accessibility.md](policies/accessibility.md).

The screenshots in the README and the manual are generated: see
[README and manual screenshots](development.md#readme-and-manual-screenshots).
