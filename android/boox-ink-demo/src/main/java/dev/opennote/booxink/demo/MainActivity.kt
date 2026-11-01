package dev.opennote.booxink.demo

import android.app.Activity
import android.app.Application
import android.graphics.Color
import android.graphics.Typeface
import android.os.Bundle
import android.os.SystemClock
import android.util.Log
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.HorizontalScrollView
import android.widget.LinearLayout
import android.widget.TextView
import dev.opennote.booxink.BooxInk
import dev.opennote.booxink.BooxInkView
import dev.opennote.booxink.InkStroke
import dev.opennote.booxink.ViewTransform
import org.json.JSONObject
import java.io.File
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

class DemoApp : Application() {
    override fun onCreate() {
        super.onCreate()
        BooxInk.init(this)
    }
}

/**
 * Standalone native reference host (M00.2/M00.3). Exercises the shared
 * module with no framework in between, high-contrast and animation-free for
 * ePaper. Files are written to getExternalFilesDir(null) so they can be
 * pulled with `adb pull /sdcard/Android/data/dev.opennote.booxink.demo/files/`.
 */
class MainActivity : Activity(), BooxInkView.Listener {

    private lateinit var ink: BooxInkView
    private lateinit var statusText: TextView
    private lateinit var debugText: TextView
    private val buttons = HashMap<String, Button>()
    private var widthIdx = 1
    private val widths = floatArrayOf(1.5f, 3f, 6f)
    private var pressure = true
    private var eraser = false
    private var options = BooxInkView.Options()
    private var debugVisible = true
    private val startUptime = SystemClock.uptimeMillis()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val root = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL; setBackgroundColor(Color.WHITE) }

        val row1 = toolbarRow()
        addButton(row1, "mode", "Mode: C") { toggleMode() }
        addButton(row1, "tool", "Pen") { eraser = !eraser; applyBrush() }
        addButton(row1, "width", "W 3.0") { widthIdx = (widthIdx + 1) % widths.size; applyBrush() }
        addButton(row1, "pressure", "Pressure ON") { pressure = !pressure; applyBrush() }
        addButton(row1, "clear", "Clear") { ink.clearCanvas() }
        addButton(row1, "save", "Save") { save() }
        addButton(row1, "load", "Load") { loadLatest() }
        addButton(row1, "sample", "Sample") { loadAsset("sample-strokes.json") }
        for (n in listOf(100, 1000, 5000, 10000)) addButton(row1, "scene$n", "Scene $n") { loadAsset("benchmark-scenes/scene-$n.json") }
        addButton(row1, "export", "Export perf") { exportPerf() }
        addButton(row1, "debug", "Debug") { debugVisible = !debugVisible; debugText.visibility = if (debugVisible) View.VISIBLE else View.GONE }

        val row2 = toolbarRow()
        addButton(row2, "rawRender", "rawRender ON") { options = options.copy(rawRenderEnabled = !options.rawRenderEnabled); ink.setOptions(options); refreshLabels() }
        addButton(row2, "handoff", "Handoff") {
            val v = BooxInkView.Handoff.values(); options = options.copy(handoff = v[(options.handoff.ordinal + 1) % v.size]); ink.setOptions(options); refreshLabels()
        }
        addButton(row2, "create", "create()") { options = options.copy(featureAllCreate = !options.featureAllCreate); ink.setOptions(options); refreshLabels() }
        addButton(row2, "style", "Smooth") {
            val v = dev.opennote.booxink.StrokeGeometry.RenderStyle.values(); options = options.copy(renderStyle = v[(options.renderStyle.ordinal + 1) % v.size]); ink.setOptions(options); refreshLabels()
        }
        addButton(row2, "zoom", "Zoom 1.0") { cycleZoom() }
        addButton(row2, "finger", "Finger nav") { options = options.copy(fingerDraws = !options.fingerDraws); ink.setOptions(options); refreshLabels() }
        addButton(row2, "reset", "Reset perf") { ink.resetMetrics() }

        statusText = TextView(this).apply { setTextColor(Color.BLACK); textSize = 13f; typeface = Typeface.DEFAULT_BOLD; setPadding(12, 4, 12, 4) }
        debugText = TextView(this).apply { setTextColor(Color.BLACK); textSize = 11f; typeface = Typeface.MONOSPACE; setPadding(12, 0, 12, 4) }

        root.addView(scroll(row1)); root.addView(scroll(row2)); root.addView(statusText); root.addView(debugText)
        ink = BooxInkView(this).also { it.listener = this }
        root.addView(ink, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f))
        setContentView(root)
        // Android 15 (targetSdk 35) forces edge-to-edge: without this the toolbar sits
        // under the status bar and taps on it go to the system bar (seen on Note Air5 C).
        if (android.os.Build.VERSION.SDK_INT >= 30) root.setOnApplyWindowInsetsListener { v, insets ->
            val bars = insets.getInsets(android.view.WindowInsets.Type.systemBars() or android.view.WindowInsets.Type.displayCutout())
            v.setPadding(bars.left, bars.top, bars.right, bars.bottom)
            insets
        }

        applyBrush()
        ink.attach()
        refreshLabels()
        ink.post { Log.i(TAG, "startup_to_first_layout_ms=${SystemClock.uptimeMillis() - startUptime}"); ink.metrics.timing("startup_to_first_layout_ms", (SystemClock.uptimeMillis() - startUptime).toFloat()) }
        Log.i(TAG, "capabilities=${ink.getCapabilities()}")
    }

    override fun onDestroy() {
        ink.detach()
        super.onDestroy()
    }

    private fun toolbarRow() = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL; gravity = Gravity.CENTER_VERTICAL }
    private fun scroll(v: View) = HorizontalScrollView(this).apply { isHorizontalScrollBarEnabled = false; addView(v) }

    private fun addButton(row: LinearLayout, key: String, label: String, onClick: () -> Unit) {
        val b = Button(this).apply {
            text = label; isAllCaps = false; setTextColor(Color.BLACK); textSize = 14f
            setBackgroundColor(Color.WHITE); stateListAnimator = null
            setOnClickListener { onClick() }
            minWidth = 0; setPadding(18, 6, 18, 6)
        }
        row.addView(b, LinearLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT).apply { setMargins(2, 2, 2, 2) })
        buttons[key] = b
    }

    private fun toggleMode() {
        val next = if (ink.mode == BooxInkView.Mode.BOOX_HARDWARE) BooxInkView.Mode.NATIVE_SOFTWARE else BooxInkView.Mode.BOOX_HARDWARE
        val got = ink.setMode(next)
        Log.i(TAG, "toggleMode: requested $next, entered $got, status=${ink.status()}")
        refreshLabels()
    }

    private val zooms = floatArrayOf(1f, 1.5f, 0.75f)
    private var zoomIdx = 0
    private fun cycleZoom() {
        // Non-1:1 transform test (Workload H): zoom about the view centre.
        zoomIdx = (zoomIdx + 1) % zooms.size
        val t = ink.viewTransform()
        val s = zooms[zoomIdx]
        val cx = ink.width / t.density / 2f; val cy = ink.height / t.density / 2f
        ink.setViewTransform(s, cx - cx * s, cy - cy * s)
        refreshLabels()
    }

    private fun applyBrush() {
        ink.setBrush(widths[widthIdx], pressure, if (eraser) BooxInkView.Tool.ERASER else BooxInkView.Tool.PEN)
        refreshLabels()
    }

    private fun refreshLabels() {
        buttons["mode"]?.text = if (ink.mode == BooxInkView.Mode.BOOX_HARDWARE) "Mode: B (BOOX HW)" else "Mode: C (native SW)"
        buttons["tool"]?.text = if (eraser) "Eraser" else "Pen"
        buttons["width"]?.text = "W ${widths[widthIdx]}"
        buttons["pressure"]?.text = if (pressure) "Pressure ON" else "Pressure OFF"
        buttons["rawRender"]?.text = "rawRender ${if (options.rawRenderEnabled) "ON" else "OFF"}"
        buttons["handoff"]?.text = "Handoff ${options.handoff.name}"
        buttons["create"]?.text = if (options.featureAllCreate) "create(FEATURE_ALL)" else "create(view,cb)"
        buttons["style"]?.text = options.renderStyle.name
        buttons["zoom"]?.text = "Zoom ${zooms[zoomIdx]}"
        buttons["finger"]?.text = if (options.fingerDraws) "Finger draws" else "Finger nav"
        updateStatus(ink.status())
    }

    private fun updateStatus(status: JSONObject) {
        if (!::statusText.isInitialized) return
        val err = status.opt("lastError").takeIf { it != JSONObject.NULL }
        statusText.text = "MODE ${status.optString("mode")} (requested ${status.optString("requestedMode")}) · raw=${status.optString("sessionState")} · coords=${status.optString("rawCoordinateSpace")}" +
            (if (err != null) "\nERROR: $err" else "")
        if (debugVisible) {
            val m = ink.getMetrics()
            val c = m.optJSONObject("counters") ?: JSONObject()
            val t = m.optJSONObject("timings") ?: JSONObject()
            fun med(k: String) = t.optJSONObject(k)?.let { "%.1f/%.1f".format(it.optDouble("median"), it.optDouble("p95")) } ?: "-"
            debugText.text = "strokes=${ink.strokeCount()} down=${c.optLong("native_pen_down_callback")} move=${c.optLong("native_pen_move_callback")} list=${c.optLong("native_pen_list_callback")} up=${c.optLong("native_pen_up_callback")} penUpRefresh=${c.optLong("native_pen_up_refresh_callback")} fallback=${c.optLong("pen_up_refresh_fallback_used")}\n" +
                "med/p95 ms: cb→render ${med("callback_to_software_render_ms")} persist ${med("persistent_render_completed_ms")} draw ${med("preview_render_submitted_ms")} in→handler ${med("input_to_handler_ms")} dispatch ${med("native_input_to_callback_ms")}"
        }
    }

    // ------------------------------------------------------------ files --
    private fun dir(sub: String) = File(getExternalFilesDir(null), sub).apply { mkdirs() }
    private fun stamp() = SimpleDateFormat("yyyyMMdd-HHmmss", Locale.US).format(Date())

    private fun save() {
        val f = File(dir("strokes"), "strokes-${stamp()}.json")
        f.writeText(ink.exportStrokes())
        toastStatus("saved ${f.name}")
    }

    private fun loadLatest() {
        val f = dir("strokes").listFiles()?.maxByOrNull { it.lastModified() } ?: return toastStatus("no saved strokes")
        ink.loadStrokes(f.readText())
        toastStatus("loaded ${f.name}")
    }

    private fun loadAsset(name: String) {
        val t0 = SystemClock.elapsedRealtimeNanos()
        val text = assets.open(name).bufferedReader().use { it.readText() }
        ink.metrics.timing("asset_read_ms", (SystemClock.elapsedRealtimeNanos() - t0) / 1e6f)
        ink.loadStrokes(text)
        toastStatus("loaded $name in ${(SystemClock.elapsedRealtimeNanos() - t0) / 1_000_000} ms")
    }

    private fun exportPerf() {
        val o = JSONObject()
            .put("app", "boox-ink-demo")
            .put("framework", "native-android")
            .put("exportedAt", System.currentTimeMillis())
            .put("capabilities", ink.getCapabilities())
            .put("metrics", ink.getMetrics())
            .put("initErrors", org.json.JSONArray(BooxInk.initErrors))
        val f = File(dir("perf"), "perf-native-${ink.mode.name.lowercase()}-${stamp()}.json")
        f.writeText(o.toString(2))
        Log.i(TAG, "perf written to ${f.absolutePath}")
        toastStatus("perf -> ${f.name}")
    }

    private fun toastStatus(msg: String) {
        Log.i(TAG, msg)
        statusText.text = msg
    }

    // ------------------------------------------------------- listener --
    override fun onStrokeCommitted(stroke: InkStroke) = updateStatus(ink.status())
    override fun onStrokesErased(ids: List<String>) = updateStatus(ink.status())
    override fun onViewportChanged(transform: ViewTransform) {}
    override fun onStatusChanged(status: JSONObject) = updateStatus(status)

    companion object { private const val TAG = "BooxInkDemo" }
}
