# macOS

Two Mac apps are built from each release tag, both for Apple Silicon only (ONNX Runtime has no
prebuilt Intel library). Neither has been run on a Mac by a person yet: see the
[release matrix](release-matrix.md).

## The DMG (Developer ID)

`Penroam_X.Y.Z_aarch64.dmg` on the releases page. Tauri signs the app with the Developer ID
certificate, has Apple notarize it and staples the ticket, so macOS opens it after the usual "downloaded from
the Internet" question, without the "Open Anyway" detour. It isn't sandboxed, updates itself when the update key is set up, and sells plans in the
browser. Secrets: [development.md](../development.md#releases).

## The Mac App Store build

The same app built with the `mac-app-store` Cargo feature and signed for the store. What differs:

* **App Sandbox.** The entitlements are in `apps/desktop-mobile/src-tauri/macos-app-store/Entitlements.plist`,
  each with the reason it's there. The library lives in the app's container
  (`~/Library/Containers/ink.penroam.app/`), so it is separate from a DMG install's library on the
  same Mac; use **Backup** to move notes between them.
* **Chosen folders.** The sandbox forgets a folder picked in the open panel when the app quits.
  `src/folder_access.rs` saves a security-scoped bookmark when a folder is picked and resolves it
  before an automatic backup.
* **Sign-in** still uses the loopback redirect, which is why the build has the `network.server`
  entitlement.
* **No self-update.** The store updates the app, and `updates.rs` reports updates as unavailable.
* **Plans** are App Store subscriptions, through the same StoreKit 2 calls as iPad
  (`plugins/tauri-plugin-store-billing/macos/`, reached through one C function because Tauri's
  mobile plugin bridge doesn't exist on macOS). Stripe prices and billing links are switched off
  in Rust, not only hidden. The App Store record is shared with iOS, so the subscriptions are too.

## Known risks

* Nothing in the sandboxed build has been run yet. The parts most likely to need fixes are the
  loopback sign-in, the keychain (the account key is kept there), printing and file pickers.
* The StoreKit bridge blocks a worker thread while the purchase sheet is open. It must never be
  called from the main thread, which would freeze the sheet.
* A bookmark to a folder on a removable or network drive goes stale when the drive is gone. The
  backup then fails with a permission error until the folder is chosen again.
