package dev.opennote.booxink.tauri

import android.app.Activity
import android.view.View
import android.view.ViewGroup
import android.webkit.WebView
import android.widget.FrameLayout
import app.tauri.annotation.Command
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import dev.opennote.booxink.BooxInk
import dev.opennote.booxink.BooxInkView
import dev.opennote.booxink.InkPoint
import dev.opennote.booxink.InkStroke
import dev.opennote.booxink.StrokeJson
import dev.opennote.booxink.ViewTransform
import org.json.JSONArray
import org.json.JSONObject

/**
 * Hosts the shared [BooxInkView] as a real native View in the Activity's
 * content FrameLayout, positioned exactly over the WebView element that the
 * web UI reports via `attach` / `setBounds` (CSS px * devicePixelRatio +
 * WebView offset).
 *
 * Z-order: the overlay is added after (above) the WebView. Input: the
 * overlay consumes stylus/finger events inside its bounds (palm rejection
 * and navigation happen natively); the WebView keeps receiving events for
 * the toolbar outside those bounds. The web canvas is told to ignore input
 * while native mode is active, so no stylus sample is processed twice.
 */
@TauriPlugin
class BooxInkPlugin(private val activity: Activity) : Plugin(activity) {

    private var webView: WebView? = null
    private var ink: BooxInkView? = null
    private var bounds = JSObject()

    override fun load(webView: WebView) {
        super.load(webView)
        this.webView = webView
        BooxInk.init(activity.application)
        // Creates /sdcard/Android/data/<app>/files so the Rust side can write perf
        // exports there (pullable with adb on release builds; see write_perf).
        activity.getExternalFilesDir(null)
    }

    override fun onPause() {
        super.onPause()
        // BooxInkView also observes Activity lifecycle itself; this is belt and braces.
        ink?.setDrawingEnabled(false)
    }

    override fun onResume() {
        super.onResume()
        ink?.setDrawingEnabled(true)
    }

    override fun onDestroy() {
        removeView()
        super.onDestroy()
    }

    private fun ui(invoke: Invoke, block: () -> Unit) {
        activity.runOnUiThread {
            try { block() } catch (t: Throwable) { invoke.reject("${t.javaClass.simpleName}: ${t.message}") }
        }
    }

    private fun view(): BooxInkView = ink ?: throw IllegalStateException("BOOX ink view not attached")

    @Command
    fun attach(invoke: Invoke) {
        val args = invoke.getArgs()
        ui(invoke) {
            val v = ink ?: createView()
            val dpr = args.optDouble("dpr", activity.resources.displayMetrics.density.toDouble()).toFloat()
            applyBounds(v, args)
            v.setBrush(
                args.optDouble("width", 3.0).toFloat(),
                args.optBoolean("pressure", true),
                if (args.optString("tool") == "eraser") BooxInkView.Tool.ERASER else BooxInkView.Tool.PEN,
            )
            args.optJSONObject("transform")?.let {
                v.setViewTransform(it.optDouble("scale", 1.0).toFloat(), it.optDouble("offsetX", 0.0).toFloat(), it.optDouble("offsetY", 0.0).toFloat(), dpr)
            } ?: v.setViewTransform(1f, 0f, 0f, dpr)
            args.optString("strokes", "").takeIf { it.isNotEmpty() }?.let { v.loadStrokes(it) }
            val requested = runCatching { BooxInkView.Mode.valueOf(args.optString("mode", "BOOX_HARDWARE")) }.getOrDefault(BooxInkView.Mode.BOOX_HARDWARE)
            val actual = v.setMode(requested)
            v.attach()
            invoke.resolve(
                JSObject()
                    .put("mode", actual.name)
                    .put("capabilities", JSObject(v.getCapabilities().toString()))
                    .put("status", JSObject(v.status().toString())),
            )
        }
    }

    @Command
    fun detach(invoke: Invoke) = ui(invoke) {
        removeView()
        invoke.resolve()
    }

    @Command
    fun setBounds(invoke: Invoke) {
        val args = invoke.getArgs()
        ui(invoke) { ink?.let { applyBounds(it, args) }; invoke.resolve() }
    }

    @Command
    fun setMode(invoke: Invoke) {
        val args = invoke.getArgs()
        ui(invoke) {
            val m = view().setMode(BooxInkView.Mode.valueOf(args.getString("mode")))
            invoke.resolve(JSObject().put("mode", m.name))
        }
    }

    @Command
    fun setBrush(invoke: Invoke) {
        val args = invoke.getArgs()
        ui(invoke) {
            ink?.setBrush(
                args.optDouble("width", 3.0).toFloat(),
                args.optBoolean("pressure", true),
                if (args.optString("tool") == "eraser") BooxInkView.Tool.ERASER else BooxInkView.Tool.PEN,
            )
            invoke.resolve()
        }
    }

    @Command
    fun setOptions(invoke: Invoke) {
        val args = invoke.getArgs()
        ui(invoke) {
            val v = view()
            val base = BooxInkView.Options.fromJson(v.status().optJSONObject("options") ?: JSONObject())
            v.setOptions(BooxInkView.Options.fromJson(args, base))
            invoke.resolve()
        }
    }

    @Command
    fun setTransform(invoke: Invoke) {
        val args = invoke.getArgs()
        ui(invoke) {
            val v = view()
            v.setViewTransform(args.optDouble("scale", 1.0).toFloat(), args.optDouble("offsetX", 0.0).toFloat(), args.optDouble("offsetY", 0.0).toFloat())
            invoke.resolve()
        }
    }

    @Command
    fun clear(invoke: Invoke) = ui(invoke) { ink?.clearCanvas(); invoke.resolve() }

    @Command
    fun loadStrokes(invoke: Invoke) {
        val args = invoke.getArgs()
        ui(invoke) { view().loadStrokes(args.getString("strokes")); invoke.resolve() }
    }

    /** Benchmarks: erase along a document-space path given as [x0, y0, x1, y1, ...]. */
    @Command
    fun eraseAlong(invoke: Invoke) {
        val arr = invoke.getArgs().getJSONArray("points")
        val pts = (0 until arr.length() / 2).map { InkPoint(arr.getDouble(2 * it).toFloat(), arr.getDouble(2 * it + 1).toFloat(), 0.5f) }
        ui(invoke) { invoke.resolve(JSObject().put("removed", view().eraseAlongDoc(pts))) }
    }

    @Command
    fun exportStrokes(invoke: Invoke) = ui(invoke) {
        invoke.resolve(JSObject().put("strokes", view().exportStrokes()))
    }

    @Command
    fun getCapabilities(invoke: Invoke) = ui(invoke) {
        val caps = ink?.getCapabilities() ?: dev.opennote.booxink.BooxCapabilities.detect(activity).toJson()
        invoke.resolve(JSObject(caps.put("initErrors", JSONArray(BooxInk.initErrors)).toString()))
    }

    @Command
    fun getMetrics(invoke: Invoke) = ui(invoke) {
        invoke.resolve(JSObject(view().getMetrics().toString()))
    }

    // ------------------------------------------------------------------

    private fun content(): ViewGroup = activity.findViewById(android.R.id.content)

    private fun createView(): BooxInkView {
        val v = BooxInkView(activity)
        v.listener = object : BooxInkView.Listener {
            override fun onStrokeCommitted(stroke: InkStroke) {
                trigger("stroke", JSObject().put("stroke", JSObject(StrokeJson.strokeToJson(stroke).toString())))
            }
            override fun onStrokesErased(ids: List<String>) {
                trigger("erased", JSObject().put("ids", JSONArray(ids)))
            }
            override fun onViewportChanged(transform: ViewTransform) {
                trigger("viewport", JSObject().put("scale", transform.scale.toDouble()).put("offsetX", transform.offsetX.toDouble()).put("offsetY", transform.offsetY.toDouble()))
            }
            override fun onStatusChanged(status: JSONObject) {
                trigger("status", JSObject(status.toString()))
            }
        }
        // Added last => drawn above the WebView.
        content().addView(v, FrameLayout.LayoutParams(1, 1))
        ink = v
        return v
    }

    /**
     * CSS px rect (relative to the WebView viewport) -> physical px margins
     * inside android.R.id.content. Accounts for the WebView's own offset
     * (status bar / cutout insets when the WebView is not edge-to-edge).
     */
    private fun applyBounds(v: BooxInkView, args: JSObject) {
        bounds = args
        val wv = webView ?: return
        val dpr = args.optDouble("dpr", activity.resources.displayMetrics.density.toDouble())
        val wvLoc = IntArray(2).also { wv.getLocationInWindow(it) }
        val cLoc = IntArray(2).also { content().getLocationInWindow(it) }
        val lp = (v.layoutParams as? FrameLayout.LayoutParams) ?: FrameLayout.LayoutParams(1, 1)
        lp.leftMargin = (wvLoc[0] - cLoc[0] + args.optDouble("x", 0.0) * dpr).toInt()
        lp.topMargin = (wvLoc[1] - cLoc[1] + args.optDouble("y", 0.0) * dpr).toInt()
        lp.width = (args.optDouble("width", 0.0) * dpr).toInt().coerceAtLeast(1)
        lp.height = (args.optDouble("height", 0.0) * dpr).toInt().coerceAtLeast(1)
        v.layoutParams = lp
        v.visibility = View.VISIBLE
        v.bringToFront()
    }

    private fun removeView() {
        val v = ink ?: return
        ink = null
        v.detach()
        (v.parent as? ViewGroup)?.removeView(v)
    }
}
