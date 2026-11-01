plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "dev.opennote.booxink.demo"
    compileSdk = 36

    defaultConfig {
        applicationId = "dev.opennote.booxink.demo"
        minSdk = 26
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0-m00"
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            // M00 test builds are signed with the debug key so they install
            // directly; do not ship this configuration.
            signingConfig = signingConfigs.getByName("debug")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    // JVM target set via kotlin { compilerOptions } below

    // Shared fixtures (sample strokes + benchmark scenes) are bundled as assets.
    sourceSets["main"].assets.srcDir("../../tests/fixtures")
    androidResources { ignoreAssetsPattern = "!*.mjs:!manifest.json" }

    packaging { jniLibs { pickFirsts += "**/libc++_shared.so" } }
}

dependencies {
    implementation(project(":boox-ink"))
}

kotlin { compilerOptions { jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17) } }
