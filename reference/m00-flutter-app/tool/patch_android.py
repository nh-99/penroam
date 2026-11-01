#!/usr/bin/env python3
"""Idempotently wires android/boox-ink into the Flutter-generated Android
project. Run by tool/bootstrap.sh after `flutter create`.

 - include ':boox-ink' (../../../android/boox-ink) and depend on it from :app
 - official Onyx Maven repo (HTTP, group-restricted) + pinned SDK versions
 - AGP 9: android.builtInKotlin=false / android.newDsl=false (same as Tauri's
   generated project) because boox-ink applies org.jetbrains.kotlin.android
 - minSdk >= 26, pickFirst libc++_shared.so, tools:replace android:label
 - R8: -dontwarn for Flutter's unbundled Play Core classes; minify off (AGP 9.1.0 R8 crash)
"""
import pathlib, re, sys

root = pathlib.Path(__file__).resolve().parent.parent
android = root / "android"
shared = (root.parent / "android").resolve()


def patch(path, marker, fn):
    if not path.exists():
        sys.exit(f"missing {path} - run `flutter create` first (tool/bootstrap.sh)")
    text = path.read_text()
    if marker in text:
        print(f"ok      {path.relative_to(root)}"); return
    new = fn(text)
    if new == text:
        sys.exit(f"could not patch {path.relative_to(root)} (template changed?) - edit manually, marker '{marker}'")
    path.write_text(new); print(f"patched {path.relative_to(root)}")


settings = android / "settings.gradle.kts"
agp = re.search(r'id\("com\.android\.application"\) version "(\d+)', settings.read_text())
agp_major = int(agp.group(1)) if agp else 8

patch(settings, ":boox-ink", lambda t: t + """
// M00: shared BOOX native ink module
include(":boox-ink")
project(":boox-ink").projectDir = file("../../../android/boox-ink")
""")
patch(settings, "org.jetbrains.kotlin.android", lambda t: re.sub(
    r'(id\("com\.android\.application"\) version "[^"]+" apply false\n)',
    lambda m: m.group(1) + '    id("org.jetbrains.kotlin.android") version "2.2.10" apply false\n', t, count=1))

ONYX = """        // M00: official Onyx/BOOX repository (HTTPS endpoint fails TLS with modern JDKs).
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
patch(android / "build.gradle.kts", "repo.boox.com",
      lambda t: re.sub(r"(allprojects \{\s*repositories \{\s*google\(\)\s*mavenCentral\(\)\n)", lambda m: m.group(1) + ONYX, t, count=1))

props = "".join(l + "\n" for l in (shared / "gradle.properties").read_text().splitlines() if l.startswith("onyx"))
extra = "android.builtInKotlin=false\nandroid.newDsl=false\n" if agp_major >= 9 else ""
patch(android / "gradle.properties", "onyxPenVersion", lambda t: t.rstrip() + "\n\n# M00\n" + props + extra)

app = android / "app" / "build.gradle.kts"
patch(app, 'project(":boox-ink")', lambda t: t + '\ndependencies {\n    implementation(project(":boox-ink"))\n}\n')
patch(app, "org.jetbrains.kotlin.android", lambda t: t.replace('    id("com.android.application")\n', '    id("com.android.application")\n    id("org.jetbrains.kotlin.android")\n', 1) if 'id("kotlin-android")' not in t else t.replace('id("kotlin-android")', 'id("org.jetbrains.kotlin.android")'))
patch(app, "maxOf(26", lambda t: t.replace("minSdk = flutter.minSdkVersion", "minSdk = maxOf(26, flutter.minSdkVersion)"))
patch(app, "libc++_shared", lambda t: re.sub(r"(\n    buildTypes \{)", '\n    // M00: onyxsdk-pen and mmkv both ship libc++_shared.so\n    packaging { jniLibs { pickFirsts += "**/libc++_shared.so" } }\n\\1', t, count=1))

manifest = android / "app" / "src" / "main" / "AndroidManifest.xml"
patch(manifest, "tools:replace", lambda t: re.sub(
    r'<manifest xmlns:android="http://schemas.android.com/apk/res/android"\s*>',
    '<manifest xmlns:android="http://schemas.android.com/apk/res/android"\n    xmlns:tools="http://schemas.android.com/tools">', t, count=1)
    .replace('android:label="', 'tools:replace="android:label"\n        android:label="', 1))

# Flutter's engine references Play Core deferred-component classes that are not
# bundled; R8 fails the release build without these rules (standard Flutter fix).
rules = android / "app" / "proguard-rules.pro"
if not rules.exists():
    rules.write_text("")
patch(rules, "com.google.android.play.core", lambda t: t + "# M00: Flutter deferred components (unused)\n-dontwarn com.google.android.play.core.**\n")
patch(app, "proguard-rules.pro", lambda t: t.replace(
    '            signingConfig = signingConfigs.getByName("debug")\n',
    '            signingConfig = signingConfigs.getByName("debug")\n'
    '            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")\n', 1))

# AGP 9.1.0's R8 crashes (NPE in its Android API database) on androidx core
# 1.13.1 LocationManagerCompat. Until AGP/R8 is fixed, release builds skip
# Java/Kotlin shrinking. Dart is AOT-compiled separately and unaffected; only APK
# size / JVM-side startup differ from the (minified) Tauri APK. See docs/comparison.md.
patch(app, "isMinifyEnabled = false", lambda t: t.replace(
    '            signingConfig = signingConfigs.getByName("debug")\n',
    '            signingConfig = signingConfigs.getByName("debug")\n'
    '            // M00: R8 in AGP 9.1.0 crashes on androidx core 1.13.1 (see tool/patch_android.py)\n'
    '            isMinifyEnabled = false\n            isShrinkResources = false\n', 1))
