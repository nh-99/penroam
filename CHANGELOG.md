# Changelog

User-visible changes for each release. The release workflow copies a version's section into its
release notes and stops if the version has no entry.

## Unreleased

* The Linux AppImage adds Penroam to your app menu. **Settings → Advanced** can remove it before you
  delete the AppImage.
## 0.1.0-beta.4

The first public test build. It's for testers: expect rough edges, and keep a backup of notebooks
you care about.

* Adding a device now takes a sign-in. There's no pairing code, comparison code or recovery code, and
  a device names itself after your computer or tablet.
* New: vaults. Notebooks in a vault are readable only with your passphrase. Ordinary notebooks are
  encrypted too, but Penroam now holds their key, so Penroam could read them.
