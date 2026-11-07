# Release matrix

What each release is built for, and how far each platform has got. This page is about release
status. [`capabilities.md`](capabilities.md) has the pen detail for each device.

## Status levels

| Level | Meaning | What we need before using it |
|---|---|---|
| Stable | Supported for everyday notes | A signed package, the device scorecard passed on named hardware, and updates tested on that platform |
| Beta | Installs and works, with known gaps | A package that a person has installed and run on real hardware |
| Unverified | Builds, but nobody has run this package on the platform | CI compiles it |
| Import/export only | The app doesn't run there; files can be exchanged | |

**No platform is at Stable yet.**

## Platforms

| Platform | Package | Status | Signed | Run by a person on | What's missing |
|---|---|---|---|---|---|
| Linux x86-64, X11 and Wayland (Ubuntu 24.04 in CI) | `.deb`, AppImage | Beta | Not yet | Developer machine, mouse and keyboard | Pen hardware; other distributions |
| Android, BOOX Note Air5 C (Android 15) | APK, Google Play internal track | Beta | Not yet: test key | Earlier builds | Release key; written terms for the Onyx SDK; the device scorecard; regional erase refresh |
| iPadOS, iPad13,4 with Apple Pencil (iPadOS 26) | TestFlight | Beta, developer build only | Not yet | Developer builds from `xtool` | A first TestFlight build run on the device; the device scorecard |
| Windows x86-64 | NSIS installer, in beta releases only | Unverified | Not yet | Nobody | Signing certificate; any hands-on run; stylus test |
| macOS Apple Silicon (no Intel build) | `.dmg` | Unverified | Not yet | Nobody | Developer ID, notarization; any hands-on run |
| Other Android tablets, Samsung S Pen | APK | Unverified | Not yet | Nobody | Any hands-on run |
| reMarkable, Supernote and similar | None | Import/export only | | | No supported way to install, draw or render |

Only the architectures above are built. Linux and Windows on ARM are not.

## Features by build

| Feature | Desktop releases | Android release | iPad |
|---|---|---|---|
| Notebooks, ink, PDFs, import, export, backups, history | Yes | Yes | Yes |
| Sync | Yes, with Penroam Cloud | Yes, with Penroam Cloud | Yes, with Penroam Cloud |
| In-app updates | When a release key is set up | No: install a new APK | No: App Store or TestFlight |
| Handwriting model download | Where there is no built-in recognizer | Yes | Yes |

## Versions that move independently

The app version, the sync protocol and each file format have separate numbers.
[`crates/penroam-sync-protocol/fixtures/v1/contract-manifest.json`](../../crates/penroam-sync-protocol/fixtures/v1/contract-manifest.json)
holds the current protocol and format numbers. Each release publishes a copy with its commit, and
puts the same numbers in its notes.

A build that meets a newer file or journal format refuses it and leaves it unchanged. A client
that meets a newer sync major version or operation schema stops syncing and keeps its data, and
offline edits stay on the device.

## Older versions

Every published release stays on the
[releases page](https://github.com/nh-99/penroam/releases). Only the newest release gets fixes.
A sync server supports a range of protocol versions and tells older clients to upgrade rather than
dropping their data.
