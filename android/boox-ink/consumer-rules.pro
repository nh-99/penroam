# Applied to every host app that depends on :boox-ink.

# The Onyx SDK reflects into firmware classes and its own internals; keep it intact.
-keep class com.onyx.android.sdk.** { *; }
-keep class com.nohowtech.booxink.** { *; }
-keep class org.lsposed.hiddenapibypass.** { *; }

# Optional classes referenced by Onyx transitive deps (okhttp TLS providers,
# fst/jackson, joda, slf4j, errorprone annotations). Never loaded at runtime here.
-dontwarn org.slf4j.**
-dontwarn org.conscrypt.**
-dontwarn org.openjsse.**
-dontwarn org.bouncycastle.**
-dontwarn org.joda.convert.**
-dontwarn javax.lang.model.**
-dontwarn java.beans.**
-dontwarn com.google.errorprone.annotations.**
