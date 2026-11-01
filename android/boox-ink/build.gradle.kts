plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android")
}

val onyxPenVersion: String by project
val onyxDeviceVersion: String by project
val onyxBaseVersion: String by project

android {
    namespace = "dev.opennote.booxink"
    compileSdk = 36

    defaultConfig {
        minSdk = 26
        consumerProguardFiles("consumer-rules.pro")
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        buildConfigField("String", "ONYX_PEN_VERSION", "\"$onyxPenVersion\"")
        buildConfigField("String", "ONYX_DEVICE_VERSION", "\"$onyxDeviceVersion\"")
        buildConfigField("String", "ONYX_BASE_VERSION", "\"$onyxBaseVersion\"")
    }

    buildFeatures { buildConfig = true }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    testOptions { unitTests.isReturnDefaultValues = true }

    // onyxsdk-pen and mmkv (via onyxsdk-base) both ship libc++_shared.so.
    // Note: packaging options are applied by the *app*; hosts must repeat this
    // (see README.md). Kept here for the library's own androidTest APK.
    packaging { jniLibs { pickFirsts += "**/libc++_shared.so" } }
}

configurations.all {
    // The Onyx SDK still declares pre-AndroidX support libraries.
    exclude(group = "com.android.support")
}

dependencies {
    // Official Onyx Pen SDK (pinned; see README.md). `api` so hosts can see
    // the classes for diagnostics, but hosts should only use BooxInkView.
    api("com.onyx.android.sdk:onyxsdk-pen:$onyxPenVersion")
    api("com.onyx.android.sdk:onyxsdk-device:$onyxDeviceVersion")
    api("com.onyx.android.sdk:onyxsdk-base:$onyxBaseVersion")
    // Android 9+ hidden-API restrictions block reflective calls the Onyx SDK
    // makes into the firmware framework. Apache-2.0.
    implementation("org.lsposed.hiddenapibypass:hiddenapibypass:4.3")
    implementation("androidx.annotation:annotation:1.9.1")

    testImplementation("junit:junit:4.13.2")
    testImplementation("org.json:json:20240303")
}

// compilerOptions works on both KGP 2.1 (standalone android/ build) and
// KGP 2.2 / AGP 9 (Tauri's generated Android project).
kotlin { compilerOptions { jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17) } }
