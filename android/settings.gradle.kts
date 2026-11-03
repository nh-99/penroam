// Standalone Android build of the BOOX ink module used by the app's native
// ink plugin (runs its JVM unit tests; the app build includes it via
// apps/desktop-mobile/scripts/patch-android.py).
pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.PREFER_SETTINGS)
    repositories {
        google()
        mavenCentral()
        // Official Onyx/BOOX repository. It is served over plain HTTP: the
        // HTTPS endpoint currently fails TLS negotiation ("dh key too small")
        // with modern JDK/OpenSSL. Artifacts are restricted to the groups below.
        maven {
            url = uri("http://repo.boox.com/repository/maven-public/")
            isAllowInsecureProtocol = true
            // Onyx artifacts plus legacy JCenter-era transitive deps of
            // onyxsdk-base that are only mirrored here.
            content {
                includeGroupByRegex("com\\.onyx.*")
                includeGroup("pub.devrel")
                includeGroup("com.tencent")
                includeGroup("com.jakewharton.hugo.fix")
            }
        }
    }
}

rootProject.name = "boox-android"
include(":boox-ink")
