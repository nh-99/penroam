package dev.opennote.m00.m00_flutter_app

import android.content.Context
import android.os.Handler
import android.os.Looper
import android.view.View
import dev.opennote.booxink.BooxInkView
import dev.opennote.booxink.InkPoint
import dev.opennote.booxink.InkStroke
import dev.opennote.booxink.StrokeJson
import dev.opennote.booxink.ViewTransform
import io.flutter.plugin.common.BinaryMessenger
import io.flutter.plugin.common.MethodCall
import io.flutter.plugin.common.MethodChannel
import io.flutter.plugin.common.StandardMessageCodec
import io.flutter.plugin.platform.PlatformView
import io.flutter.plugin.platform.PlatformViewFactory
import org.json.JSONObject

class BooxPlatformViewFactory(private val messenger: BinaryMessenger) :
    PlatformViewFactory(StandardMessageCodec.INSTANCE) {
    @Suppress("UNCHECKED_CAST")
    override fun create(context: Context, viewId: Int, args: Any?): PlatformView =
        BooxPlatformView(context, viewId, messenger, (args as? Map<String, Any?>) ?: emptyMap())
}

/**
 * Flutter Platform View wrapping the shared [BooxInkView]. Finalized strokes
 * (not individual samples) are sent to Dart; the hardware preview never
 * waits on Dart.
 */
class BooxPlatformView(
    context: Context,
    id: Int,
    messenger: BinaryMessenger,
    params: Map<String, Any?>,
) : PlatformView, MethodChannel.MethodCallHandler {

    private val ink = BooxInkView(context)
    private val channel = MethodChannel(messenger, "$VIEW_TYPE/$id")
    private val main = Handler(Looper.getMainLooper())

    init {
        channel.setMethodCallHandler(this)
        ink.listener = object : BooxInkView.Listener {
            override fun onStrokeCommitted(stroke: InkStroke) =
                send("onStroke", StrokeJson.strokeToJson(stroke).toString())
            override fun onStrokesErased(ids: List<String>) = send("onErased", ids)
            override fun onViewportChanged(transform: ViewTransform) = send(
                "onViewport",
                mapOf("scale" to transform.scale.toDouble(), "offsetX" to transform.offsetX.toDouble(), "offsetY" to transform.offsetY.toDouble()),
            )
            override fun onStatusChanged(status: JSONObject) = send("onStatus", status.toString())
        }
        val dpr = (params["dpr"] as? Number)?.toFloat() ?: context.resources.displayMetrics.density
        ink.setViewTransform(
            (params["scale"] as? Number)?.toFloat() ?: 1f,
            (params["offsetX"] as? Number)?.toFloat() ?: 0f,
            (params["offsetY"] as? Number)?.toFloat() ?: 0f,
            dpr,
        )
        ink.setBrush(
            (params["width"] as? Number)?.toFloat() ?: 3f,
            params["pressure"] as? Boolean ?: true,
            if (params["tool"] == "eraser") BooxInkView.Tool.ERASER else BooxInkView.Tool.PEN,
        )
        (params["strokes"] as? String)?.takeIf { it.isNotEmpty() }?.let { ink.loadStrokes(it) }
        ink.setMode(runCatching { BooxInkView.Mode.valueOf(params["mode"] as String) }.getOrDefault(BooxInkView.Mode.NATIVE_SOFTWARE))
        ink.attach()
    }

    private fun send(method: String, arg: Any?) {
        main.post { channel.invokeMethod(method, arg) }
    }

    override fun getView(): View = ink

    override fun dispose() {
        channel.setMethodCallHandler(null)
        ink.detach()
    }

    override fun onMethodCall(call: MethodCall, result: MethodChannel.Result) {
        try {
            when (call.method) {
                "setMode" -> result.success(ink.setMode(BooxInkView.Mode.valueOf(call.arguments as String)).name)
                "setBrush" -> {
                    ink.setBrush(
                        (call.argument<Number>("width") ?: 3).toFloat(),
                        call.argument<Boolean>("pressure") ?: true,
                        if (call.argument<String>("tool") == "eraser") BooxInkView.Tool.ERASER else BooxInkView.Tool.PEN,
                    )
                    result.success(null)
                }
                "setOptions" -> {
                    val base = BooxInkView.Options.fromJson(ink.status().optJSONObject("options") ?: JSONObject())
                    ink.setOptions(BooxInkView.Options.fromJson(JSONObject(call.arguments as String), base))
                    result.success(null)
                }
                "setTransform" -> {
                    ink.setViewTransform(
                        (call.argument<Number>("scale") ?: 1).toFloat(),
                        (call.argument<Number>("offsetX") ?: 0).toFloat(),
                        (call.argument<Number>("offsetY") ?: 0).toFloat(),
                    )
                    result.success(null)
                }
                "clear" -> { ink.clearCanvas(); result.success(null) }
                "loadStrokes" -> { ink.loadStrokes(call.arguments as String); result.success(null) }
                "exportStrokes" -> result.success(ink.exportStrokes())
                "eraseAlong" -> {
                    val xy = (call.arguments as List<*>).map { (it as Number).toFloat() }
                    result.success(ink.eraseAlongDoc(xy.chunked(2).map { InkPoint(it[0], it[1], 0.5f) }))
                }
                "getCapabilities" -> result.success(ink.getCapabilities().toString())
                "getMetrics" -> result.success(ink.getMetrics().toString())
                "detach" -> { ink.detach(); result.success(null) }
                else -> result.notImplemented()
            }
        } catch (t: Throwable) {
            result.error(t.javaClass.simpleName, t.message, null)
        }
    }

    companion object {
        const val VIEW_TYPE = "dev.opennote/boox_ink"
    }
}
