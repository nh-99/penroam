#!/usr/bin/env python3
"""Idempotently wires the shared BOOX module into Tauri's generated Android
project (src-tauri/gen/android). Run after `tauri android init`:

    python3 scripts/patch-android.py

1. include ':boox-ink' from ../../../../../android/boox-ink
2. add the official Onyx Maven repository (HTTP, group-restricted)
3. copy the pinned Onyx SDK versions into gradle.properties
4. tools:replace="android:label" (the Onyx AARs declare an application label)
5. pickFirst libc++_shared.so and debug-key signing for release test APKs
"""
import pathlib, re

root = pathlib.Path(__file__).resolve().parent.parent
gen = root / "src-tauri" / "gen" / "android"
shared = (root.parent.parent / "android").resolve()


def patch(path: pathlib.Path, marker: str, fn):
    text = path.read_text()
    if marker in text:
        print(f"ok      {path.relative_to(root)}")
        return
    path.write_text(fn(text))
    print(f"patched {path.relative_to(root)}")


patch(gen / "settings.gradle", "boox-ink", lambda t: t + """
// M00: shared BOOX native ink module (android/boox-ink)
include ':boox-ink'
project(':boox-ink').projectDir = new File(rootDir, '../../../../../android/boox-ink')
""")

ONYX_REPO = """        // M00: official Onyx/BOOX repository (HTTPS endpoint fails TLS with modern JDKs).
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
      lambda t: t + "\n# M00: pinned Onyx SDK versions (mirrors android/gradle.properties)\n" + "".join(f"{k}={v}\n" for k, v in props.items()))

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
    // M00: onyxsdk-pen and mmkv both ship libc++_shared.so.
    packaging { jniLibs { pickFirsts += "**/libc++_shared.so" } }""", 1))
patch(APP, "M00: sign release", lambda t: t.replace("""        getByName("release") {""", """        getByName("release") {
            // M00: sign release test builds with the debug key so they install
            // directly on test devices. Not for distribution.
            signingConfig = signingConfigs.getByName("debug")""", 1))

