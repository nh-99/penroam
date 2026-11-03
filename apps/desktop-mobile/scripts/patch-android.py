#!/usr/bin/env python3
"""Idempotently wires the shared BOOX module (android/boox-ink) into
Tauri's generated Android project. Run after `pnpm tauri android init`:

    python3 scripts/patch-android.py

1. include ':boox-ink' from <repo>/android/boox-ink
2. official Onyx Maven repository (HTTP, group-restricted)
3. pinned Onyx SDK versions in gradle.properties
4. tools:replace="android:label" (the Onyx AARs declare an application label)
5. pickFirst libc++_shared.so; debug-key signing for sideloaded release test APKs
6. release builds drop the INTERNET permission: the app is offline by design
"""
import pathlib, re

root = pathlib.Path(__file__).resolve().parent.parent
gen = root / "src-tauri" / "gen" / "android"
shared = (root.parent.parent / "android").resolve()
rel = "../../../../../android/boox-ink"


def patch(path: pathlib.Path, marker: str, fn):
    text = path.read_text() if path.exists() else ""
    if marker in text:
        print(f"ok      {path.relative_to(root)}")
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(fn(text))
    print(f"patched {path.relative_to(root)}")


patch(gen / "settings.gradle", "boox-ink", lambda t: t + f"""
// Penroam: shared BOOX native ink module (android/boox-ink)
include ':boox-ink'
project(':boox-ink').projectDir = new File(rootDir, '{rel}')
""")

ONYX_REPO = """        // Penroam: official Onyx/BOOX repository (its HTTPS endpoint fails TLS with modern JDKs).
        maven {
            url = uri("http://repo.boox.com/repository/maven-public/")
            isAllowInsecureProtocol = true
            content {
                includeGroupByRegex("com\\\\.onyx.*")
                includeGroup("pub.devrel")
                includeGroup("com.tencent")
                includeGroup("com.jakewharton.hugo.fix")
            }
        }
"""
patch(gen / "build.gradle.kts", "repo.boox.com",
      lambda t: re.sub(r"(allprojects \{\s*repositories \{\s*google\(\)\s*mavenCentral\(\)\n)", lambda m: m.group(1) + ONYX_REPO, t))

props = {k: v for k, v in (l.split("=", 1) for l in (shared / "gradle.properties").read_text().splitlines() if l.startswith("onyx"))}
patch(gen / "gradle.properties", "onyxPenVersion",
      lambda t: t + "\n# Penroam: pinned Onyx SDK versions (mirrors android/gradle.properties)\n" + "".join(f"{k}={v}\n" for k, v in props.items()))

patch(gen / "app" / "src" / "main" / "AndroidManifest.xml", "tools:replace",
      lambda t: t.replace('<manifest xmlns:android="http://schemas.android.com/apk/res/android">',
                          '<manifest xmlns:android="http://schemas.android.com/apk/res/android"\n    xmlns:tools="http://schemas.android.com/tools">')
                 .replace('android:label="@string/app_name"', 'android:label="@string/app_name"\n        tools:replace="android:label"', 1))

APP = gen / "app" / "build.gradle.kts"
patch(APP, "libc++_shared", lambda t: t.replace("""    buildFeatures {
        buildConfig = true
    }""", """    buildFeatures {
        buildConfig = true
    }
    // Penroam: onyxsdk-pen and mmkv both ship libc++_shared.so.
    packaging { jniLibs { pickFirsts += "**/libc++_shared.so" } }""", 1))
patch(APP, "Penroam: sign release", lambda t: t.replace("""        getByName("release") {""", """        getByName("release") {
            // Penroam: sign sideloaded release test builds with the debug key.
            // Not for distribution (no app-store release yet).
            signingConfig = signingConfigs.getByName("debug")""", 1))

patch(gen / "app" / "src" / "release" / "AndroidManifest.xml", "INTERNET", lambda _t: """<?xml version="1.0" encoding="utf-8"?>
<!-- Penroam release builds are offline: no network permission at all. -->
<manifest xmlns:android="http://schemas.android.com/apk/res/android"
    xmlns:tools="http://schemas.android.com/tools">
    <uses-permission android:name="android.permission.INTERNET" tools:node="remove" />
</manifest>
""")
