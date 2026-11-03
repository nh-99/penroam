package com.nohowtech.booxink

import android.app.Application
import android.os.Build
import android.util.Log
import org.lsposed.hiddenapibypass.HiddenApiBypass

/**
 * Process-wide initialization. Call once from Application.onCreate() or the
 * host's plugin/engine attach. Safe to call repeatedly and on non-BOOX devices.
 */
object BooxInk {
    private const val TAG = "BooxInk"
    @Volatile private var initialized = false
    val initErrors = ArrayList<String>()

    fun init(app: Application) {
        if (initialized) return
        initialized = true
        // The Onyx SDK reflects into hidden framework APIs (android.onyx.*,
        // View hidden methods); without the exemption these fail on Android 9+.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            runCatching { HiddenApiBypass.addHiddenApiExemptions("") }
                .onFailure { initErrors.add("HiddenApiBypass: $it"); Log.w(TAG, "HiddenApiBypass failed", it) }
        }
        // Some onyxsdk-base versions require RxManager's app context before
        // EpdController calls. Reflective so a missing class is non-fatal.
        runCatching {
            val builder = Class.forName("com.onyx.android.sdk.rx.RxManager\$Builder")
            builder.getMethod("initAppContext", android.content.Context::class.java).invoke(null, app)
        }.onFailure { initErrors.add("RxManager.initAppContext: $it") }
    }
}
