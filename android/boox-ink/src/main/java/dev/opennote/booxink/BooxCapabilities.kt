package dev.opennote.booxink

import android.content.Context
import android.os.Build
import android.util.Log
import android.webkit.WebView
import org.json.JSONArray
import org.json.JSONObject

/**
 * Staged BOOX/Onyx runtime detection. The manufacturer string is only a
 * hint; native mode requires:
 *
 *  1. Onyx SDK classes loadable in this APK                (sdkClassesPresent)
 *  2. Onyx firmware framework classes present on the device (firmwareRuntimePresent)
 *  3. The SDK's own device probe picks a real EPD backend   (sdkDeviceIndex != BaseDevice)
 *
 * and finally a successful TouchHelper.openRawDrawing() at attach time,
 * which is recorded by [BooxInkView] as `rawDrawingOpened`.
 */
data class BooxCapabilities(
    val manufacturerHint: Boolean,
    val sdkClassesPresent: Boolean,
    val firmwareRuntimePresent: Boolean,
    val sdkDeviceIndex: String?,
    val maxTouchPressure: Float,
    val errors: List<String>,
    val device: JSONObject,
) {
    /** Preconditions for attempting Mode B. Attach can still fail. */
    val nativeModeCandidate: Boolean
        get() = sdkClassesPresent && firmwareRuntimePresent && sdkDeviceIndex != null && sdkDeviceIndex != "BaseDevice"

    fun toJson(): JSONObject = JSONObject()
        .put("manufacturerHint", manufacturerHint)
        .put("sdkClassesPresent", sdkClassesPresent)
        .put("firmwareRuntimePresent", firmwareRuntimePresent)
        .put("sdkDeviceIndex", sdkDeviceIndex ?: JSONObject.NULL)
        .put("maxTouchPressure", maxTouchPressure.toDouble())
        .put("nativeModeCandidate", nativeModeCandidate)
        .put("onyxSdk", JSONObject().put("pen", BuildConfig.ONYX_PEN_VERSION).put("device", BuildConfig.ONYX_DEVICE_VERSION).put("base", BuildConfig.ONYX_BASE_VERSION))
        .put("errors", JSONArray(errors))
        .put("device", device)

    companion object {
        private const val TAG = "BooxCapabilities"

        /** Firmware classes the Onyx SDK reflects into (found via javap/strings on onyxsdk-device). */
        private val FIRMWARE_CLASSES = listOf("android.onyx.ViewUpdateHelper", "android.onyx.hardware.DeviceController")

        fun detect(context: Context): BooxCapabilities {
            val errors = ArrayList<String>()
            val m = "${Build.MANUFACTURER} ${Build.BRAND} ${Build.MODEL}".lowercase()
            val hint = m.contains("onyx") || m.contains("boox")

            val sdk = try {
                Class.forName("com.onyx.android.sdk.pen.TouchHelper"); true
            } catch (t: Throwable) { errors.add("sdk class: $t"); false }

            val firmwareFound = FIRMWARE_CLASSES.filter {
                try { Class.forName(it); true } catch (_: Throwable) { false }
            }
            if (firmwareFound.isEmpty()) errors.add("no Onyx firmware classes (${FIRMWARE_CLASSES.joinToString()})")

            var index: String? = null
            var maxPressure = 0f
            if (sdk) {
                try { index = OnyxBridge.deviceIndex() } catch (t: Throwable) { errors.add("Device.currentDeviceIndex: $t") }
                try { maxPressure = OnyxBridge.maxTouchPressure() } catch (t: Throwable) { errors.add("getMaxTouchPressure: $t") }
            }
            errors.forEach { Log.i(TAG, it) }
            return BooxCapabilities(hint, sdk, firmwareFound.isNotEmpty(), index, maxPressure, errors, deviceInfo(context, firmwareFound))
        }

        fun deviceInfo(context: Context, firmwareClasses: List<String> = emptyList()): JSONObject {
            val dm = context.resources.displayMetrics
            val o = JSONObject()
                .put("manufacturer", Build.MANUFACTURER)
                .put("brand", Build.BRAND)
                .put("model", Build.MODEL)
                .put("device", Build.DEVICE)
                .put("androidRelease", Build.VERSION.RELEASE)
                .put("sdkInt", Build.VERSION.SDK_INT)
                .put("fingerprint", Build.FINGERPRINT)
                .put("display", Build.DISPLAY)
                .put("widthPx", dm.widthPixels)
                .put("heightPx", dm.heightPixels)
                .put("density", dm.density.toDouble())
                .put("densityDpi", dm.densityDpi)
                .put("firmwareClasses", JSONArray(firmwareClasses))
            try {
                val pkg = WebView.getCurrentWebViewPackage()
                if (pkg != null) o.put("webViewPackage", pkg.packageName).put("webViewVersion", pkg.versionName)
            } catch (t: Throwable) { o.put("webViewError", t.toString()) }
            return o
        }
    }
}
