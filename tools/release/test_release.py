"""Tests for release.py: python3 -m unittest discover -s tools/release"""
import json, pathlib, sys, tempfile, unittest

sys.path.insert(0, str(pathlib.Path(__file__).parent))
import release

CONTRACT = {
    "protocol": {"major": 1, "minor": 4}, "envelope_version": 1, "op_schema": {"min": 1, "max": 2},
    "formats": {"file_format": 1, "journal_format": 1, "archive_format": 2, "notebook_schema": 2, "page_schema": 1},
    "git_sha": None,
}
CHANGELOG = "# Changelog\n\n## 1.2.0 (2027-01-01)\n\nAdded a thing.\n\n## 1.1.0\n\nOlder.\n"


def fake_repo(root, version="1.2.0"):
    (root / "apps/desktop-mobile/src-tauri").mkdir(parents=True)
    (root / "crates/penroam-sync-protocol/fixtures/v1").mkdir(parents=True)
    (root / "Cargo.toml").write_text(f'[other]\nversion = "9"\n[workspace.package]\nversion = "{version}"\n')
    (root / "apps/desktop-mobile/package.json").write_text(json.dumps({"version": version}))
    (root / "apps/desktop-mobile/src-tauri/tauri.conf.json").write_text(json.dumps({"version": version}))
    (root / "crates/penroam-sync-protocol/fixtures/v1/contract-manifest.json").write_text(json.dumps(CONTRACT))
    (root / "CHANGELOG.md").write_text(CHANGELOG)


class Versions(unittest.TestCase):
    def test_tag_must_match_every_version(self):
        with tempfile.TemporaryDirectory() as d:
            root = pathlib.Path(d)
            fake_repo(root)
            self.assertEqual(release.check_version("v1.2.0", root), "1.2.0")
            with self.assertRaises(ValueError):
                release.check_version("v1.2.1", root)

    def test_malformed_tags_are_refused(self):
        for tag in ["1.2.0", "v1.2", "v1.2.0-nightly", "sync-protocol-v0.1.0"]:
            with self.assertRaises(ValueError):
                release.check_version(tag)

    def test_this_repository_states_one_version(self):
        self.assertEqual(len(set(release.repo_versions().values())), 1)


class Notes(unittest.TestCase):
    def test_section_stops_at_the_next_version(self):
        self.assertEqual(release.changelog_section("1.2.0", CHANGELOG), "Added a thing.")

    def test_a_version_without_an_entry_stops_the_release(self):
        with self.assertRaises(ValueError):
            release.changelog_section("3.0.0", CHANGELOG)

    def test_notes_state_compatibility(self):
        notes = release.release_notes("1.2.0", CONTRACT, CHANGELOG)
        self.assertIn("| Sync protocol | 1.4 |", notes)
        self.assertIn("| `.penroam` archive | 2 |", notes)


class Assemble(unittest.TestCase):
    def test_metadata_covers_every_artifact(self):
        with tempfile.TemporaryDirectory() as r, tempfile.TemporaryDirectory() as d:
            root, out = pathlib.Path(r), pathlib.Path(d)
            fake_repo(root)
            (out / "Penroam_1.2.0_amd64.AppImage").write_bytes(b"app")
            (out / "Penroam_1.2.0_amd64.AppImage.sig").write_text("SIG\n")
            (out / "Penroam_1.2.0_x64-setup.exe").write_bytes(b"exe")
            (out / "Penroam_1.2.0_x64-setup.exe.sig").write_text("WIN")
            (out / "Penroam_aarch64.app.tar.gz").write_bytes(b"mac")
            (out / "Penroam_aarch64.app.tar.gz.sig").write_text("MAC")
            release.assemble("1.2.0", "abc", out, root, "2027-01-01T00:00:00Z")
            feed = json.loads((out / "latest.json").read_text())
            self.assertEqual(sorted(feed["platforms"]), ["darwin-aarch64", "linux-x86_64", "windows-x86_64"])
            self.assertEqual(feed["platforms"]["linux-x86_64"]["signature"], "SIG")
            self.assertTrue(feed["platforms"]["windows-x86_64"]["url"].endswith("/v1.2.0/Penroam_1.2.0_x64-setup.exe"))
            self.assertEqual(json.loads((out / "contract-manifest.json").read_text())["git_sha"], "abc")
            sums = (out / "SHA256SUMS").read_text()
            for name in ["latest.json", "release-notes.md", "contract-manifest.json", "Penroam_1.2.0_amd64.AppImage"]:
                self.assertIn(name, sums)
            self.assertNotIn("SHA256SUMS", sums)

    def test_an_unsigned_build_gets_no_feed(self):
        with tempfile.TemporaryDirectory() as r, tempfile.TemporaryDirectory() as d:
            root, out = pathlib.Path(r), pathlib.Path(d)
            fake_repo(root)
            (out / "Penroam.AppImage").write_bytes(b"app")
            release.assemble("1.2.0", "abc", out, root)
            self.assertFalse((out / "latest.json").exists())

    def test_a_signature_without_its_artifact_fails(self):
        with tempfile.TemporaryDirectory() as d:
            out = pathlib.Path(d)
            (out / "Penroam.AppImage.sig").write_text("SIG")
            with self.assertRaises(ValueError):
                release.update_feed("1.2.0", [out / "Penroam.AppImage.sig"], "", None)


class Signing(unittest.TestCase):
    ALL = {n: "x" for names in release.SIGNING.values() for n in names}

    def test_a_stable_release_needs_what_it_ships(self):
        self.assertEqual(release.sign_status("v1.0.0", self.ALL), [])
        env = dict(self.ALL, APPLE_ID="")
        with self.assertRaises(ValueError) as caught:
            release.sign_status("v1.0.0", env)
        self.assertIn("APPLE_ID", str(caught.exception))

    def test_a_stable_release_goes_ahead_without_windows_signing(self):
        env = dict(self.ALL, WINDOWS_SIGN_COMMAND="")
        self.assertEqual(release.sign_status("v1.0.0", env), ["windows"])

    def test_a_prerelease_reports_what_is_unsigned(self):
        self.assertEqual(release.sign_status("v1.0.0-beta.1", {}), ["android", "macos", "updates", "windows"])

    def test_the_status_never_repeats_a_value(self):
        env = dict(self.ALL, APPLE_ID="")
        with self.assertRaises(ValueError) as caught:
            release.sign_status("v1.0.0", env)
        self.assertNotIn("'x'", str(caught.exception))


class Matrix(unittest.TestCase):
    @staticmethod
    def names(tag, env):
        return [b["name"] for b in release.desktop_matrix(tag, env)]

    def test_stable_leaves_out_an_unsigned_windows_build(self):
        self.assertEqual(self.names("v1.0.0", {}), ["linux-x86_64", "macos-aarch64", "macos-x86_64"])

    def test_stable_includes_windows_once_it_can_be_signed(self):
        self.assertIn("windows-x86_64", self.names("v1.0.0", {"WINDOWS_SIGN_COMMAND": "sign %1"}))

    def test_prereleases_always_include_windows(self):
        self.assertIn("windows-x86_64", self.names("v1.0.0-beta.1", {}))

    def test_notes_say_when_windows_is_missing(self):
        self.assertIn("no Windows installer", release.release_notes("1.2.0", CONTRACT, CHANGELOG, False))
        self.assertNotIn("no Windows installer", release.release_notes("1.2.0", CONTRACT, CHANGELOG))


if __name__ == "__main__":
    unittest.main()
