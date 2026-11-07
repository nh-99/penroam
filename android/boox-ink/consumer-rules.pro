# Applied to every host app that depends on :boox-ink.

# The Onyx SDK reflects into firmware classes and its own internals; keep it intact.
-keep class com.onyx.android.sdk.** { *; }
-keep class com.nohowtech.booxink.** { *; }
-keep class org.lsposed.hiddenapibypass.** { *; }

# Optional classes referenced by Onyx transitive deps (fst/jackson, joda, slf4j, errorprone annotations). Never loaded at runtime here.
-dontwarn org.slf4j.**
# Left out of the build on purpose, see build.gradle.kts. Only the SDK's firmware updater uses them.
-dontwarn okhttp3.**
-dontwarn retrofit2.**
-dontwarn org.conscrypt.**
-dontwarn org.openjsse.**
-dontwarn org.bouncycastle.**
-dontwarn org.joda.convert.**
-dontwarn javax.lang.model.**
-dontwarn java.beans.**
-dontwarn com.google.errorprone.annotations.**
# Referenced only from SDK code nothing here calls: its fingerprint check (FingerprintManager is
# gone from the Android 37 SDK), its Wi-Fi settings screen (a data-binding class the AAR doesn't
# ship), and fastjson2's bridge to the old fastjson 1 API.
-dontwarn android.hardware.fingerprint.FingerprintManager
-dontwarn com.onyx.android.sdk.BR
-dontwarn com.alibaba.fastjson.JSONArray
-dontwarn com.alibaba.fastjson.JSONObject
