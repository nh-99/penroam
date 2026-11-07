#!/usr/bin/env python3
"""Release tooling, used by .github/workflows/release.yml and promote.yml.

    release.py check-version v1.2.3        the tag matches every version in the repository
    release.py notices OUT                 third-party licence notices for the app bundle
    release.py assemble VERSION SHA DIR    checksums, manifests, notes and update feed for DIR
    release.py sign-status TAG             which signing secrets this run has (never their values)
    release.py desktop-matrix TAG          the desktop builds for this tag, as JSON
    release.py configured CHANNEL          true if a signing or upload channel has its secrets
    release.py store-config ios|android N  Tauri config with the build number for workflow run N

Only the standard library, so it runs before anything is installed. docs/development.md#releases
describes the whole process.
"""
import hashlib, json, os, pathlib, re, shutil, subprocess, sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
REPO_URL = "https://github.com/nh-99/penroam"
TAG = re.compile(r"^v(\d+\.\d+\.\d+)(-(?:alpha|beta|rc)\.\d+)?$")
# Which update-feed platform each signed artifact serves.
FEED_PLATFORMS = [
    (re.compile(r"\.AppImage$"), "linux-x86_64"),
    (re.compile(r"-setup\.exe$"), "windows-x86_64"),
    (re.compile(r"aarch64.*\.app\.tar\.gz$"), "darwin-aarch64"),
]


def repo_versions(root=ROOT):
    """Every place that states the app version."""
    cargo = (root / "Cargo.toml").read_text(encoding="utf-8")
    section = cargo[cargo.index("[workspace.package]"):]
    return {
        "Cargo.toml": re.search(r'^version = "([^"]+)"', section, re.M).group(1),
        "tauri.conf.json": json.loads((root / "apps/desktop-mobile/src-tauri/tauri.conf.json").read_text(encoding="utf-8"))["version"],
        "package.json": json.loads((root / "apps/desktop-mobile/package.json").read_text(encoding="utf-8"))["version"],
    }


def check_version(tag, root=ROOT):
    """Returns the version a tag names. Raises ValueError if it is malformed or disagrees."""
    match = TAG.match(tag)
    if not match:
        raise ValueError(f"{tag}: expected vX.Y.Z or vX.Y.Z-(alpha|beta|rc).N")
    version = tag[1:]
    wrong = {k: v for k, v in repo_versions(root).items() if v != version}
    if wrong:
        raise ValueError(f"{tag} doesn't match {wrong}")
    return version


def is_prerelease(version):
    return "-" in version


def changelog_section(version, text):
    """The body of a version's section in CHANGELOG.md."""
    found = re.search(rf"^## {re.escape(version)}\b.*?$(.*?)(?=^## |\Z)", text, re.M | re.S)
    if not found or not found.group(1).strip():
        raise ValueError(f"CHANGELOG.md has no entry for {version}")
    return found.group(1).strip()


def compatibility_table(contract):
    f = contract["formats"]
    p = contract["protocol"]
    return "\n".join([
        "| Contract | Version |",
        "|---|---|",
        f"| Sync protocol | {p['major']}.{p['minor']} |",
        f"| Encrypted envelope | {contract['envelope_version']} |",
        f"| Operation schema | {contract['op_schema']['min']} to {contract['op_schema']['max']} |",
        f"| Page file | {f['file_format']} |",
        f"| Journal | {f['journal_format']} |",
        f"| `.penroam` archive | {f['archive_format']} |",
        f"| Notebook schema | {f['notebook_schema']} |",
        f"| Page schema | {f['page_schema']} |",
    ])


def release_notes(version, contract, changelog, has_windows=True):
    parts = [changelog_section(version, changelog)]
    if not has_windows:
        parts.append(
            "There is no Windows installer in this release. Windows builds are on the beta "
            f"channel for now: see the [releases page]({REPO_URL}/releases).")
    parts += [
        "## Compatibility\n\n" + compatibility_table(contract),
        "A build that meets a file or sync format newer than it knows refuses it and leaves it "
        "unchanged. Update first, then open the file.",
        f"Check your download against `SHA256SUMS`, or verify where it came from with "
        f"`gh attestation verify <file> --repo nh-99/penroam`. What runs where: "
        f"[release matrix]({REPO_URL}/blob/v{version}/docs/platforms/release-matrix.md). "
        f"Known problems: [known issues]({REPO_URL}/blob/v{version}/docs/known-issues.md).",
    ]
    return "\n\n".join(parts) + "\n"


def update_feed(version, assets, notes, pub_date):
    """The updater's latest.json. Fails if a platform with a signature has no matching artifact."""
    platforms = {}
    for sig in sorted(p for p in assets if p.name.endswith(".sig")):
        artifact = sig.name[: -len(".sig")]
        platform = next((key for rx, key in FEED_PLATFORMS if rx.search(artifact)), None)
        if platform is None:
            continue
        if not (sig.parent / artifact).exists():
            raise ValueError(f"{sig.name} has no artifact")
        platforms[platform] = {
            "signature": sig.read_text(encoding="utf-8").strip(),
            "url": f"{REPO_URL}/releases/download/v{version}/{artifact}",
        }
    return {"version": version, "notes": notes, "pub_date": pub_date, "platforms": platforms}


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()


def assemble(version, sha, directory, root=ROOT, pub_date=None):
    """Writes the metadata beside the artifacts in `directory`, which holds them flat."""
    directory = pathlib.Path(directory)
    contract = json.loads((root / "crates/penroam-sync-protocol/fixtures/v1/contract-manifest.json").read_text(encoding="utf-8"))
    contract["git_sha"] = sha
    (directory / "contract-manifest.json").write_text(json.dumps(contract, indent=2) + "\n", encoding="utf-8")
    changelog = (root / "CHANGELOG.md").read_text(encoding="utf-8")
    assets = sorted(p for p in directory.iterdir() if p.is_file())
    has_windows = any(p.name.endswith(".exe") for p in assets)
    notes = release_notes(version, contract, changelog, has_windows)
    (directory / "release-notes.md").write_text(notes, encoding="utf-8")
    feed = update_feed(version, assets, changelog_section(version, changelog), pub_date)
    if feed["platforms"]:
        (directory / "latest.json").write_text(json.dumps(feed, indent=2) + "\n", encoding="utf-8")
    assets = sorted(p for p in directory.iterdir() if p.is_file() and p.name != "SHA256SUMS")
    (directory / "SHA256SUMS").write_text("".join(f"{sha256(p)}  {p.name}\n" for p in assets), encoding="utf-8")
    return feed


LICENCE_FILE = re.compile(r"^(licen[cs]e|copying|notice|copyright)", re.I)


def licence_texts(directory):
    return sorted(p for p in pathlib.Path(directory).iterdir() if p.is_file() and LICENCE_FILE.match(p.name))


def notices(out):
    """Licence texts of the Rust crates and npm packages that ship in the app."""
    meta = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--format-version", "1", "--locked"], cwd=ROOT))
    # MIT and Apache texts repeat hundreds of times; each distinct text appears once.
    texts = {}

    def add(name, version, licence, directory):
        found = [t.read_text(encoding="utf-8", errors="replace").strip() for t in licence_texts(directory)]
        for text in found or [f"Licensed under {licence}."]:
            texts.setdefault(text, []).append(f"{name} {version}")

    shipped = {n["id"] for n in meta["resolve"]["nodes"]}
    for pkg in sorted(meta["packages"], key=lambda p: (p["name"], p["version"])):
        if pkg["source"] and pkg["id"] in shipped:
            add(pkg["name"], pkg["version"], pkg.get("license") or "see below",
                pathlib.Path(pkg["manifest_path"]).parent)
    app = ROOT / "apps/desktop-mobile"
    tree = json.loads(subprocess.check_output(
        # By full path: on Windows pnpm is `pnpm.cmd`, which CreateProcess won't find by name.
        [shutil.which("pnpm") or "pnpm", "ls", "--prod", "--depth", "Infinity", "--json"], cwd=app))
    seen = {}

    def walk(deps):
        for name, dep in (deps or {}).items():
            # optional packages for other platforms are listed but not installed
            if (name, dep["version"]) not in seen and pathlib.Path(dep.get("path", "/nonexistent")).is_dir():
                seen[(name, dep["version"])] = dep["path"]
                walk(dep.get("dependencies"))

    walk(tree[0].get("dependencies"))
    for (name, version), path in sorted(seen.items()):
        pkg = json.loads((pathlib.Path(path) / "package.json").read_text(encoding="utf-8"))
        add(name, version, pkg.get("license") or "see below", path)
    lines = ["Penroam is free software under the Mozilla Public License 2.0.",
             "It includes the following third-party software.\n"]
    for text, packages in sorted(texts.items(), key=lambda item: item[1]):
        lines += [f"\n{'=' * 72}", ", ".join(packages), "=" * 72, text]
    # A fresh checkout has no `public/` folder: nothing else in it is checked in.
    # Always UTF-8: Windows would otherwise use its locale code page, which can't hold every
    # character in the licence texts.
    out = pathlib.Path(out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text("\n".join(lines) + "\n", encoding="utf-8")


SIGNING = {
    "updates": ["TAURI_SIGNING_PRIVATE_KEY"],
    "macos": ["APPLE_CERTIFICATE", "APPLE_ID", "APPLE_PASSWORD", "APPLE_TEAM_ID"],
    "windows": ["WINDOWS_SIGN_COMMAND"],
    "android": ["PENROAM_ANDROID_KEYSTORE_BASE64"],
    "testflight": ["APPLE_API_KEY", "APPLE_API_ISSUER", "APPLE_API_KEY_P8", "APPLE_TEAM_ID"],
    "google-play": ["GOOGLE_PLAY_SERVICE_ACCOUNT_JSON"],
}
# These are skipped when they aren't set up, instead of stopping the release. Windows is never
# shipped unsigned in a stable release, and the store uploads only add to what was built.
OPTIONAL = {"windows", "testflight", "google-play"}
# Store build numbers only go up. Android starts above any version code a sideloaded build used.
ANDROID_VERSION_CODE_BASE = 10000

DESKTOP_BUILDS = [
    {"os": "ubuntu-24.04", "name": "linux-x86_64", "args": "--bundles deb,appimage"},
    {"os": "windows-latest", "name": "windows-x86_64", "args": "--bundles nsis"},
    {"os": "macos-latest", "name": "macos-aarch64", "args": "--bundles dmg,app --target aarch64-apple-darwin",
     "target": "aarch64-apple-darwin"},
]
# No Intel Mac build: ONNX Runtime, which the handwriting model needs, has no prebuilt library for
# x86_64-apple-darwin, and building it from source isn't worth it for a platform Apple is leaving.


def sign_status(tag, env):
    """Stable tags need every signature they ship. Pre-releases may lack any, and their notes say so."""
    missing = {k: [n for n in v if not env.get(n)] for k, v in SIGNING.items()}
    missing = {k: v for k, v in missing.items() if v}
    blocking = {k: v for k, v in missing.items() if k not in OPTIONAL}
    if blocking and not is_prerelease(tag):
        raise ValueError(f"a stable release needs these signing secrets; missing {blocking}")
    return sorted(missing)


def configured(channel, env):
    """Are all the secrets for this signing or upload channel present?"""
    return all(env.get(name) for name in SIGNING[channel])


def store_config(platform, run_number):
    """The Tauri config for a store build. A beta and the release after it share `X.Y.Z`, and the
    stores refuse a repeated build number, so it comes from the workflow's run number. Apple
    allows three numbers at most, which rules out `tauri ios build --build-number` (`X.Y.Z.N`)."""
    if platform == "ios":
        return {"bundle": {"iOS": {"bundleVersion": str(run_number)}}}
    return {"bundle": {"android": {"versionCode": ANDROID_VERSION_CODE_BASE + run_number}}}


def desktop_matrix(tag, env):
    """The builds for a tag. Windows joins stable releases once they can be signed."""
    unsigned = not env.get("WINDOWS_SIGN_COMMAND") and not is_prerelease(tag)
    return [b for b in DESKTOP_BUILDS if not (unsigned and b["name"].startswith("windows"))]


def main(argv):
    command = argv[1] if len(argv) > 1 else ""
    if command == "check-version" and len(argv) == 3:
        print(check_version(argv[2]))
    elif command == "notices" and len(argv) == 3:
        notices(argv[2])
    elif command == "assemble" and len(argv) == 5:
        feed = assemble(argv[2], argv[3], argv[4], pub_date=os.environ.get("RELEASE_DATE"))
        print(f"update feed platforms: {sorted(feed['platforms'])}")
    elif command == "sign-status" and len(argv) == 3:
        print("unsigned:", ", ".join(sign_status(argv[2], os.environ)) or "nothing")
    elif command == "configured" and len(argv) == 3:
        print("true" if configured(argv[2], os.environ) else "false")
    elif command == "store-config" and len(argv) == 4:
        print(json.dumps(store_config(argv[2], int(argv[3])), separators=(",", ":")))
    elif command == "desktop-matrix" and len(argv) == 3:
        print(json.dumps(desktop_matrix(argv[2], os.environ), separators=(",", ":")))
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    try:
        main(sys.argv)
    except ValueError as e:
        sys.exit(f"release: {e}")
