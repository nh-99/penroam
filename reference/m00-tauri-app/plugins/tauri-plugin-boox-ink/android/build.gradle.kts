plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "dev.opennote.booxink.tauri"
    compileSdk = 36
    defaultConfig {
        minSdk = 26
        consumerProguardFiles("proguard-rules.pro")
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

dependencies {
    implementation(project(":tauri-android"))
    // Shared BOOX module; included by gen/android/settings.gradle.kts.
    implementation(project(":boox-ink"))
}

// compilerOptions works on both KGP 2.1 (standalone android/ build) and
// KGP 2.2 / AGP 9 (Tauri's generated Android project).
kotlin { compilerOptions { jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17) } }
