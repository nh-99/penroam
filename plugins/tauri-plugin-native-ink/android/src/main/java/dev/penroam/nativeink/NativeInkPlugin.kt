package dev.penroam.nativeink

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
import dev.opennote.booxink.BooxCapabilities
import dev.opennote.booxink.BooxInk
import dev.opennote.booxink.BooxInkView
import dev.opennote.booxink.InkPoint
import dev.opennote.booxink.InkStroke
import dev.opennote.booxink.OverlayPlacement
import dev.opennote.booxink.StrokeJson
import dev.opennote.booxink.ViewTransform
import org.json.JSONArray
import org.json.JSONObject

/**
 * Penroam NativeInkSurface on Android (M01.5). Reuses the M00 [BooxInkView]
 * unchanged in its input/handoff logic; M01 adds a transparent background,
 * colours/highlighter, incremental add/remove and show/hide.
 *
 *  - The View is added to the Activity content FrameLayout *above* the
 *    WebView, exactly over the editor's canvas element ([OverlayPlacement]).
 *  - It owns committed ink while visible (Mode B hardware preview hands over
 *    to identical software ink in the same View: no web round trip at pen-up).
 *  - Hidden (View.GONE + raw drawing disabled) for text/select/pan tools and
 *    whenever a dialog or menu is open, so it never steals those touches.
 */
@TauriPlugin
class NativeInkPlugin(private val activity: Activity) : Plugin(activity) {

    private var webView: WebView? = null
    private var ink: BooxInkView? = null
    private var visible = false

    override fun load(webView: WebView) {
        super.load(webView)
        this.webView = webView
        BooxInk.init(activity.application)
        activity.getExternalFilesDir(null) // so Rust can write perf exports pullable via adb
    }

    override fun onPause() {
        super.onPause()
        ink?.setDrawingEnabled(false)
    }

    override fun onResume() {
        super.onResume()
        ink?.let { it.setDrawingEnabled(visible) }
    }

    override fun onDestroy() {
        removeView()
        super.onDestroy()
    }

    private fun ui(invoke: Invoke, block: () -> Unit) {
        activity.runOnUiThread {
            try {
                block()
            } catch (t: Throwable) {
                trigger("error", JSObject().put("message", "${t.javaClass.simpleName}: ${t.message}"))
                invoke.reject("${t.javaClass.simpleName}: ${t.message}")
            }
        }
    }

    private fun view(): BooxInkView = ink ?: throw IllegalStateException("native ink view not attached")

    private fun applyBrush(v: BooxInkView, a: JSObject) {
        val tool = a.optString("tool", "pen")
        v.setBrushStyle(a.optString("color", "#000000"), if (tool == "highlighter") "highlighter" else "pen", a.optDouble("opacity", 1.0).toFloat())
        v.setBrush(
            a.optDouble("penWidth", 2.2).toFloat(),
            a.optBoolean("pressure", true) && tool == "pen",
            if (tool == "eraser") BooxInkView.Tool.ERASER else BooxInkView.Tool.PEN,
        )
    }

    @Command
    fun getCapabilities(invoke: Invoke) = ui(invoke) {
        val caps = ink?.getCapabilities() ?: BooxCapabilities.detect(activity).toJson()
        invoke.resolve(JSObject(caps.put("initErrors", JSONArray(BooxInk.initErrors)).toString()))
    }

    @Command
    fun attach(invoke: Invoke) {
        val args = invoke.getArgs()
        ui(invoke) {
            val v = ink ?: createView()
            val dpr = args.optDouble("dpr", activity.resources.displayMetrics.density.toDouble()).toFloat()
            v.transparentBackground = args.optBoolean("transparent", true)
            applyBounds(v, args)
            applyBrush(v, args)
            val t = args.optJSONObject("transform")
            v.setViewTransform(t?.optDouble("scale", 1.0)?.toFloat() ?: 1f, t?.optDouble("offsetX", 0.0)?.toFloat() ?: 0f, t?.optDouble("offsetY", 0.0)?.toFloat() ?: 0f, dpr)
            args.optString("strokes", "").takeIf { it.isNotEmpty() }?.let { v.loadStrokes(it) }
            val requested = runCatching { BooxInkView.Mode.valueOf(args.optString("mode", "BOOX_HARDWARE")) }.getOrDefault(BooxInkView.Mode.BOOX_HARDWARE)
            val actual = v.setMode(requested)
            setShown(v, false) // the editor shows it explicitly (setVisible) once ready
            v.attach()
            invoke.resolve(JSObject().put("mode", actual.name).put("capabilities", JSObject(v.getCapabilities().toString())).put("status", JSObject(v.status().toString())))
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
        ui(invoke) {
            ink?.let { applyBounds(it, args) }
            invoke.resolve()
        }
    }

    @Command
    fun setVisible(invoke: Invoke) {
        val show = invoke.getArgs().optBoolean("visible", true)
        ui(invoke) {
            ink?.let { setShown(it, show) }
            invoke.resolve()
        }
    }

    @Command
    fun setBrush(invoke: Invoke) {
        val args = invoke.getArgs()
        ui(invoke) {
            ink?.let { applyBrush(it, args) }
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
        val a = invoke.getArgs()
        ui(invoke) {
            ink?.setViewTransform(a.optDouble("scale", 1.0).toFloat(), a.optDouble("offsetX", 0.0).toFloat(), a.optDouble("offsetY", 0.0).toFloat())
            invoke.resolve()
        }
    }

    @Command
    fun loadStrokes(invoke: Invoke) {
        val json = invoke.getArgs().getString("strokes")
        ui(invoke) { view().loadStrokes(json); invoke.resolve() }
    }

    @Command
    fun addStrokes(invoke: Invoke) {
        val json = invoke.getArgs().getString("strokes")
        ui(invoke) { view().addStrokes(json); invoke.resolve() }
    }

    @Command
    fun removeStrokes(invoke: Invoke) {
        val arr = invoke.getArgs().getJSONArray("ids")
        val ids = (0 until arr.length()).map { arr.getString(it) }
        ui(invoke) { view().removeStrokes(ids); invoke.resolve() }
    }

    /** The Android surface owns committed ink; nothing to release. */
    @Command
    fun commitAck(invoke: Invoke) = invoke.resolve()

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
    fun getMetrics(invoke: Invoke) = ui(invoke) {
        invoke.resolve(JSObject(view().getMetrics().toString()))
    }

    // ------------------------------------------------------------------

    private fun content(): ViewGroup = activity.findViewById(android.R.id.content)

    private fun setShown(v: BooxInkView, show: Boolean) {
        visible = show
        v.visibility = if (show) View.VISIBLE else View.GONE
        v.setDrawingEnabled(show)
        if (show) v.bringToFront()
    }

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
        content().addView(v, FrameLayout.LayoutParams(1, 1))
        ink = v
        return v
    }

    private fun applyBounds(v: BooxInkView, args: JSObject) {
        val wv = webView ?: return
        val dpr = args.optDouble("dpr", activity.resources.displayMetrics.density.toDouble())
        val wvLoc = IntArray(2).also { wv.getLocationInWindow(it) }
        val cLoc = IntArray(2).also { content().getLocationInWindow(it) }
        val p = OverlayPlacement.compute(args.optDouble("x", 0.0), args.optDouble("y", 0.0), args.optDouble("width", 1.0), args.optDouble("height", 1.0), dpr, wvLoc, cLoc)
        val lp = (v.layoutParams as? FrameLayout.LayoutParams) ?: FrameLayout.LayoutParams(1, 1)
        lp.leftMargin = p.left
        lp.topMargin = p.top
        lp.width = p.width
        lp.height = p.height
        v.layoutParams = lp
    }

    private fun removeView() {
        val v = ink ?: return
        ink = null
        visible = false
        v.detach()
        (v.parent as? ViewGroup)?.removeView(v)
    }
}
