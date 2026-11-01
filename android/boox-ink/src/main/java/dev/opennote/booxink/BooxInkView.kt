package dev.opennote.booxink

import android.app.Activity
import android.app.Application
import android.content.Context
import android.content.ContextWrapper
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Rect
import android.graphics.RectF
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.util.AttributeSet
import android.util.Log
import android.view.MotionEvent
import android.view.View
import android.view.ViewTreeObserver
import org.json.JSONObject
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicInteger
import kotlin.math.hypot
import kotlin.math.max
import kotlin.math.min

/**
 * Native Android writing surface shared by the Flutter and Tauri hosts and by
 * the standalone reference app.
 *
 * Modes (benchmarks/protocol.md, "BOOX rendering modes"):
 *  - [Mode.BOOX_HARDWARE]   Mode B: Onyx TouchHelper raw drawing shows the
 *    live stroke through the ePaper pen pipeline (Path A); delivered samples
 *    are reconstructed into the persistent software bitmap on pen-up (Path B).
 *  - [Mode.NATIVE_SOFTWARE] Mode C: conventional MotionEvent + Canvas
 *    rendering in a native View, no Onyx acceleration (control baseline).
 *
 * Mode A (framework drawing) lives in the host framework, not here.
 *
 * Host API (all calls on the main thread): [attach], [detach], [setMode],
 * [setDrawingEnabled], [setBrush], [clearCanvas], [loadStrokes],
 * [exportStrokes], [getCapabilities], [getMetrics], [setViewTransform],
 * [setOptions], [status].
 */
class BooxInkView @JvmOverloads constructor(
    context: Context,
    attrs: AttributeSet? = null,
) : View(context, attrs) {

    enum class Mode { BOOX_HARDWARE, NATIVE_SOFTWARE }
    enum class Tool { PEN, ERASER }

    /** How the transient hardware preview is replaced by committed software ink. */
    enum class Handoff {
        /** Invalidate on RawInputCallback.onPenUpRefresh (120 ms fallback), then HAND_WRITING_REPAINT_MODE refresh. Boox-EinkDraw. */
        PEN_UP_REFRESH,
        /** setRawDrawingEnabled(false) -> redraw -> setRawDrawingEnabled(true). inksdk "syncOverlay(force)". */
        TOGGLE_RAW,
        /** Redraw then EpdController.invalidate(rect, GC) for the stroke bounds. notate "regional GC". */
        REGIONAL_GC,
    }

    data class Options(
        /** TouchHelper.setRawDrawingRenderEnabled(). Docs say false disables preview; EinkDraw reports preview still visible with false. Test both. */
        val rawRenderEnabled: Boolean = true,
        val handoff: Handoff = Handoff.PEN_UP_REFRESH,
        /** Use TouchHelper.create(view, FEATURE_ALL_TOUCH_RENDER, cb, false) instead of create(view, cb). */
        val featureAllCreate: Boolean = false,
        /** Call EpdController.setScreenHandWritingRegionLimit(view) after opening. */
        val epdRegionLimit: Boolean = true,
        /** One/two-finger pan & pinch inside the view. */
        val fingerNavigation: Boolean = true,
        /** Let fingers draw in Mode C (off = palm-safe default). */
        val fingerDraws: Boolean = false,
        /** Fallback delay for PEN_UP_REFRESH when onPenUpRefresh is not delivered. */
        val penUpFallbackMs: Long = 120L,
        val renderStyle: StrokeGeometry.RenderStyle = StrokeGeometry.RenderStyle.SMOOTH,
    ) {
        fun toJson(): JSONObject = JSONObject()
            .put("rawRenderEnabled", rawRenderEnabled).put("handoff", handoff.name)
            .put("featureAllCreate", featureAllCreate).put("epdRegionLimit", epdRegionLimit)
            .put("fingerNavigation", fingerNavigation).put("fingerDraws", fingerDraws)
            .put("penUpFallbackMs", penUpFallbackMs).put("renderStyle", renderStyle.name)

        companion object {
            fun fromJson(o: JSONObject, base: Options = Options()) = Options(
                rawRenderEnabled = o.optBoolean("rawRenderEnabled", base.rawRenderEnabled),
                handoff = runCatching { Handoff.valueOf(o.optString("handoff", base.handoff.name)) }.getOrDefault(base.handoff),
                featureAllCreate = o.optBoolean("featureAllCreate", base.featureAllCreate),
                epdRegionLimit = o.optBoolean("epdRegionLimit", base.epdRegionLimit),
                fingerNavigation = o.optBoolean("fingerNavigation", base.fingerNavigation),
                fingerDraws = o.optBoolean("fingerDraws", base.fingerDraws),
                penUpFallbackMs = o.optLong("penUpFallbackMs", base.penUpFallbackMs),
                renderStyle = runCatching { StrokeGeometry.RenderStyle.valueOf(o.optString("renderStyle", base.renderStyle.name)) }.getOrDefault(base.renderStyle),
            )
        }
    }

    interface Listener {
        fun onStrokeCommitted(stroke: InkStroke) {}
        fun onStrokesErased(ids: List<String>) {}
        fun onViewportChanged(transform: ViewTransform) {}
        fun onStatusChanged(status: JSONObject) {}
    }

    var listener: Listener? = null
    val metrics = InkMetrics()
    val capabilities: BooxCapabilities by lazy { BooxCapabilities.detect(context) }

    // ---------------------------------------------------------- state --
    private val main = Handler(Looper.getMainLooper())
    private val strokes = ArrayList<InkStroke>() // main thread only
    private var bitmap: Bitmap? = null
    private var bitmapCanvas: Canvas? = null
    private val inkPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        color = Color.BLACK; style = Paint.Style.STROKE
        strokeCap = Paint.Cap.ROUND; strokeJoin = Paint.Join.ROUND
    }
    private var transform = ViewTransform(density = resources.displayMetrics.density)
    private var options = Options()

    private var requestedMode = Mode.NATIVE_SOFTWARE
    var mode = Mode.NATIVE_SOFTWARE
        private set
    private var tool = Tool.PEN
    private var brushWidth = 3f
    private var pressureEnabled = true
    // M01: colour / highlighter / opacity of new strokes (M00 was black pen only)
    private var brushColor = "#000000"
    private var brushType = "pen"
    private var brushOpacity = 1f

    /**
     * M01 overlay mode: no white background, so the host's page (template,
     * PDF, text) shows through; only ink is drawn by this View.
     */
    var transparentBackground = false
        set(v) {
            field = v
            setBackgroundColor(if (v) Color.TRANSPARENT else Color.WHITE)
            invalidate()
        }
    private var drawingEnabled = true
    private var hostAttached = false
    private var hostPaused = false

    // Onyx session; created/closed on [sessionExecutor] (openRawDrawing can block).
    private val sessionExecutor = Executors.newSingleThreadExecutor { r -> Thread(r, "boox-ink-session") }
    @Volatile private var session: OnyxRawSession? = null
    private val sessionGeneration = AtomicInteger(0)
    @Volatile private var sessionState = "closed"
    @Volatile private var lastError: String? = null
    private var lastLimit = Rect()
    private val screenLoc = IntArray(2)

    // Raw stroke assembly (callback thread).
    private val rawLock = Any()
    private val assembler = StrokeAssembler()
    private var rawStrokeIsEraser = false
    private var rawBeginNs = 0L
    @Volatile private var pendingPenUpRefresh = false
    private var pendingEpdRepaint = false
    private var pendingRawReenable = false
    private var pendingGcRect: Rect? = null
    private var commitStartNs = 0L

    // Coordinate-space probe (Onyx TouchPoint view-local vs screen).
    @Volatile var rawCoordinateSpace = "unknown"
        private set
    @Volatile private var lastStylusDown: FloatArray? = null
    @Volatile private var lastStylusDownAt = 0L

    // Software (Mode C) active stroke.
    private var active: ArrayList<InkPoint>? = null
    private var activeT0 = 0L
    private var activeDevice = "stylus"
    private var activeStable = 0
    private var activeEraser = false
    private val activeErased = ArrayList<String>()
    private var lastEventHandledNs = 0L

    // Navigation.
    private var navMode = 0 // 0 none, 1 pan, 2 pinch
    private var navLastX = 0f; private var navLastY = 0f
    private var pinchStartDist = 1f; private var pinchStartScale = 1f
    private var pinchAnchorX = 0f; private var pinchAnchorY = 0f
    private var stylusDown = false

    private val layoutListener = ViewTreeObserver.OnGlobalLayoutListener { onLayoutMaybeMoved() }
    private var lifecycleCallbacks: Application.ActivityLifecycleCallbacks? = null

    init {
        setBackgroundColor(Color.WHITE)
        isFocusable = true
    }

    // ======================================================= host API ==

    /** Starts the surface in the requested mode (opens raw drawing for Mode B). */
    fun attach() {
        hostAttached = true
        metrics.event("host_attach")
        applyMode()
    }

    /** Releases raw drawing and returns the device to normal refresh. Strokes are kept. */
    fun detach() {
        hostAttached = false
        metrics.event("host_detach")
        closeSession("host detach")
    }

    /** Returns the mode actually entered (Mode B falls back to C if unsupported). */
    fun setMode(m: Mode): Mode {
        requestedMode = m
        metrics.event("mode_requested", mapOf("mode" to m.name))
        applyMode()
        return mode
    }

    fun setDrawingEnabled(enabled: Boolean) {
        drawingEnabled = enabled
        updateRawEnabled()
        publishStatus()
    }

    fun setBrush(widthDoc: Float, pressure: Boolean, toolName: Tool = tool) {
        brushWidth = widthDoc
        pressureEnabled = pressure
        val toolChanged = toolName != tool
        tool = toolName
        val s = session
        val gen = sessionGeneration.get()
        if (s != null) sessionExecutor.execute {
            if (gen != sessionGeneration.get()) return@execute
            runCatching { s.applyStyle(hwWidthPx(), pressureEnabled && brushType == "pen", hwColor()) }.onFailure { recordError("applyStyle", it) }
        }
        if (toolChanged) updateRawEnabled()
        publishStatus()
    }

    fun setOptions(o: Options) {
        val reopen = o.featureAllCreate != options.featureAllCreate || o.epdRegionLimit != options.epdRegionLimit
        val renderChanged = o.rawRenderEnabled != options.rawRenderEnabled
        val styleChanged = o.renderStyle != options.renderStyle
        options = o
        metrics.event("options_changed", mapOf("options" to o.toJson().toString()))
        if (reopen) { closeSession("options changed"); applyMode() }
        else if (renderChanged) {
            val s = session
            val gen = sessionGeneration.get()
            if (s != null) sessionExecutor.execute {
                if (gen == sessionGeneration.get()) runCatching { s.setRenderEnabled(o.rawRenderEnabled) }.onFailure { recordError("setRenderEnabled", it) }
            }
        }
        if (styleChanged) rebuildBitmap()
        publishStatus()
    }

    fun clearCanvas() {
        strokes.clear()
        metrics.event("clear_canvas")
        rebuildBitmap()
        requestEpaperRefresh(full = true)
    }

    fun loadStrokes(json: String) {
        val t0 = metrics.now()
        val parsed = StrokeJson.parseDocument(json)
        strokes.clear(); strokes.addAll(parsed)
        metrics.since("load_parse_ms", t0)
        val t1 = metrics.now()
        rebuildBitmap()
        metrics.since("load_render_ms", t1)
        metrics.event("strokes_loaded", mapOf("count" to parsed.size))
        requestEpaperRefresh(full = true)
    }

    fun exportStrokes(): String = StrokeJson.documentToJson(
        strokes,
        JSONObject().put("app", "boox-ink").put("mode", mode.name).put("density", transform.density.toDouble()),
    )

    fun strokeCount(): Int = strokes.size

    fun getCapabilities(): JSONObject = capabilities.toJson()
        .put("sessionState", sessionState)
        .put("rawCoordinateSpace", rawCoordinateSpace)
        .put("lastError", lastError ?: JSONObject.NULL)

    fun getMetrics(): JSONObject = metrics.toJson()
        .put("status", status())
        .put("strokeCount", strokes.size)

    fun resetMetrics() = metrics.reset()

    /** Host-driven viewport (logical px offset, zoom scale). [density] = host devicePixelRatio. */
    fun setViewTransform(scale: Float, offsetX: Float, offsetY: Float, density: Float = transform.density) {
        val next = ViewTransform(scale.coerceIn(0.1f, 10f), offsetX, offsetY, density)
        if (next == transform) return
        val widthChanged = next.docToPx != transform.docToPx
        transform = next
        rebuildBitmap()
        if (widthChanged) setBrush(brushWidth, pressureEnabled)
        requestEpaperRefresh(full = true)
    }

    fun viewTransform(): ViewTransform = transform

    fun status(): JSONObject = JSONObject()
        .put("mode", mode.name)
        .put("requestedMode", requestedMode.name)
        .put("sessionState", sessionState)
        .put("tool", tool.name)
        .put("brushWidth", brushWidth.toDouble())
        .put("pressure", pressureEnabled)
        .put("drawingEnabled", drawingEnabled)
        .put("hostAttached", hostAttached)
        .put("rawCoordinateSpace", rawCoordinateSpace)
        .put("lastError", lastError ?: JSONObject.NULL)
        .put("options", options.toJson())
        .put("transform", JSONObject().put("scale", transform.scale.toDouble()).put("offsetX", transform.offsetX.toDouble())
            .put("offsetY", transform.offsetY.toDouble()).put("density", transform.density.toDouble()))
        .put("viewSize", JSONObject().put("w", width).put("h", height))

    // ================================================ mode / session ==

    private fun applyMode() {
        if (requestedMode == Mode.BOOX_HARDWARE) {
            if (!capabilities.nativeModeCandidate) {
                val why = "BOOX hardware mode unavailable: ${capabilities.errors.joinToString("; ").ifEmpty { "device index ${capabilities.sdkDeviceIndex}" }}"
                recordError("capability", IllegalStateException(why))
                mode = Mode.NATIVE_SOFTWARE
                closeSession("unsupported")
                publishStatus()
                return
            }
            mode = Mode.BOOX_HARDWARE
            ensureSession()
        } else {
            mode = Mode.NATIVE_SOFTWARE
            closeSession("mode C")
        }
        publishStatus()
    }

    private fun ensureSession() {
        Log.i(TAG, "ensureSession mode=$mode hostAttached=$hostAttached paused=$hostPaused attached=$isAttachedToWindow size=${width}x$height session=${session != null} state=$sessionState")
        if (mode != Mode.BOOX_HARDWARE || !hostAttached || hostPaused) return
        if (!isAttachedToWindow || width <= 0 || height <= 0) return
        if (session != null || sessionState == "opening") return
        sessionState = "opening"
        val gen = sessionGeneration.incrementAndGet()
        val limit = Rect(0, 0, width, height)
        val widthPx = hwWidthPx()
        val opts = options
        val sink = RawSink(gen)
        sessionExecutor.execute {
            val t0 = metrics.now()
            var s: OnyxRawSession? = null
            try {
                s = OnyxRawSession(this, sink, opts.featureAllCreate)
                s.open(limit, emptyList(), widthPx, pressureEnabled && brushType == "pen", opts.rawRenderEnabled, hwColor())
                if (opts.epdRegionLimit) runCatching { OnyxBridge.setHandwritingRegion(this) }.onFailure { recordError("setScreenHandWritingRegionLimit", it) }
                val enable = shouldRawBeEnabled()
                s.setEnabled(enable)
                metrics.since("raw_open_ms", t0)
                val created = runCatching { s.created }.getOrDefault(false)
                main.post {
                    if (gen != sessionGeneration.get()) { sessionExecutor.execute { runCatching { s.close() } }; return@post }
                    session = s
                    lastLimit = limit
                    sessionState = "open"
                    metrics.event("hardware_preview_enabled", mapOf("rawDrawingCreated" to created, "rawRender" to opts.rawRenderEnabled, "inputEnabled" to enable, "limit" to limit.flattenToString()))
                    publishStatus()
                }
            } catch (t: Throwable) {
                // Release whatever was partially initialized, then fall back to Mode C.
                runCatching { s?.close() }
                recordError("openRawDrawing", t)
                main.post {
                    if (gen != sessionGeneration.get()) return@post
                    session = null
                    sessionState = "failed"
                    mode = Mode.NATIVE_SOFTWARE
                    metrics.event("hardware_preview_failed", mapOf("error" to t.toString()))
                    publishStatus()
                }
            }
        }
    }

    private fun closeSession(reason: String) {
        val s = session
        session = null
        sessionGeneration.incrementAndGet()
        synchronized(rawLock) { assembler.cancel() }
        if (s == null && sessionState != "opening") { if (sessionState != "failed") sessionState = "closed"; return }
        sessionState = "closed"
        metrics.event("raw_drawing_closed", mapOf("reason" to reason))
        if (s != null) sessionExecutor.execute {
            runCatching { s.close() }.onFailure { recordError("closeRawDrawing", it) }
            main.post { requestEpaperRefresh(full = true) }
        }
    }

    private fun shouldRawBeEnabled(): Boolean =
        drawingEnabled && tool == Tool.PEN && navMode == 0 && !hostPaused

    private fun updateRawEnabled() {
        val s = session ?: return
        val enable = shouldRawBeEnabled()
        val gen = sessionGeneration.get()
        sessionExecutor.execute {
            if (gen != sessionGeneration.get()) return@execute
            runCatching { s.setEnabled(enable) }.onFailure { recordError("setRawDrawingEnabled", it) }
        }
    }

    private fun hwWidthPx(): Float = max(1f, brushWidth * transform.docToPx)

    /** Re-applies the limit rect when the view moves/resizes on screen (rotation, overlay repositioning). */
    private fun onLayoutMaybeMoved() {
        if (session == null) return
        val prev = screenLoc.copyOf()
        getLocationOnScreen(screenLoc)
        val limit = Rect(0, 0, width, height)
        if (limit == lastLimit && prev.contentEquals(screenLoc)) return
        lastLimit = limit
        metrics.event("raw_limit_reconfigured", mapOf("limit" to limit.flattenToString(), "screenX" to screenLoc[0], "screenY" to screenLoc[1]))
        // Reopen once per layout change (never per pointer movement).
        closeSession("layout changed")
        main.postDelayed({ ensureSession() }, 50)
    }

    // ===================================================== raw input ==

    private inner class RawSink(private val gen: Int) : OnyxRawSession.Sink {
        private fun stale() = gen != sessionGeneration.get()

        override fun onBegin(eraser: Boolean, s: RawSample?) {
            if (stale()) return
            val t = metrics.now()
            metrics.event(if (eraser) "native_eraser_down_callback" else "native_pen_down_callback")
            if (s == null) { metrics.count("native_null_point"); return }
            metrics.recordDispatchDelay(s.timestampMs)
            synchronized(rawLock) {
                if (assembler.inProgress) {
                    // Some firmware repeats onBeginRawDrawing inside one gesture.
                    metrics.count("native_duplicate_begin")
                    assembler.move(s)
                } else {
                    rawBeginNs = t
                    rawStrokeIsEraser = eraser
                    assembler.begin(s)
                }
            }
            probeCoordinateSpace(s)
            metrics.since("native_callback_handler_ms", t)
        }

        override fun onMove(eraser: Boolean, s: RawSample) {
            if (stale()) return
            val t = metrics.now()
            metrics.count(if (eraser) "native_eraser_move_callback" else "native_pen_move_callback")
            synchronized(rawLock) { assembler.move(s) }
            metrics.recordDispatchDelay(s.timestampMs)
            metrics.since("native_callback_handler_ms", t)
        }

        override fun onList(eraser: Boolean, samples: List<RawSample>) {
            if (stale()) return
            metrics.count(if (eraser) "native_eraser_list_callback" else "native_pen_list_callback")
            metrics.count("native_list_points", samples.size.toLong())
            synchronized(rawLock) { assembler.list(samples) }
        }

        override fun onEnd(eraser: Boolean, s: RawSample?) {
            if (stale()) return
            val t = metrics.now()
            metrics.event(if (eraser) "native_eraser_up_callback" else "native_pen_up_callback")
            val tr = transform
            val maxP = capabilities.maxTouchPressure
            val bounds = floatArrayOf(0f, 0f, width.toFloat(), height.toFloat())
            val result = synchronized(rawLock) {
                if (!assembler.inProgress) { metrics.count("native_end_without_begin"); null }
                else assembler.end(s, tr, maxP, boundsPx = bounds)
            } ?: return
            val isEraser = eraser || rawStrokeIsEraser
            val beginNs = rawBeginNs
            main.post { commitRaw(result, isEraser, beginNs, t) }
        }

        override fun onPenUpRefresh(rect: RectF?) {
            if (stale()) return
            metrics.event("native_pen_up_refresh_callback", mapOf("rect" to rect?.toShortString()))
            main.post {
                if (pendingPenUpRefresh) { pendingPenUpRefresh = false; performHandoffRefresh("onPenUpRefresh") }
            }
        }

        override fun onPenActive() { metrics.count("native_pen_active_callback") }
    }

    /** Main thread: Path B — reconstruct the delivered stroke into persistent ink. */
    private fun commitRaw(r: StrokeAssembler.Result, eraser: Boolean, beginNs: Long, endCallbackNs: Long) {
        val t0 = metrics.now()
        if (eraser) {
            eraseAlong(r.points)
            requestEpaperRefresh(full = true)
            return
        }
        val stroke = InkStroke(
            source = "stylus",
            input = InkInput(
                api = "onyx-touchhelper",
                device = "stylus",
                extra = mapOf(
                    "stream" to r.chosen.name.lowercase(),
                    "moveCount" to r.moveCount, "listCount" to r.listCount, "listCallbacks" to r.listCallbacks,
                    "duplicatesRemoved" to r.duplicatesRemoved, "timeGaps" to r.timeGaps,
                    "spatialJumps" to r.spatialJumps, "outOfBounds" to r.outOfBounds,
                    "hardwarePreview" to true, "rawCoordinateSpace" to rawCoordinateSpace,
                    "maxTouchPressure" to capabilities.maxTouchPressure.toDouble(),
                ),
            ),
            brush = currentBrush(),
            points = r.points,
        )
        strokes.add(stroke)
        metrics.event("stroke_finalized", mapOf("points" to r.points.size, "stream" to r.chosen.name))
        drawStrokeIntoBitmap(stroke)
        metrics.since("callback_to_software_render_ms", endCallbackNs)
        metrics.timing("stroke_duration_ms", (endCallbackNs - beginNs) / 1e6f)
        metrics.event("software_stroke_committed", mapOf("id" to stroke.id))
        metrics.strokeSummary(strokeSummaryJson(stroke, r))
        commitStartNs = endCallbackNs
        listener?.onStrokeCommitted(stroke)
        scheduleHandoff(stroke)
        metrics.since("commit_main_thread_ms", t0)
    }

    private fun scheduleHandoff(stroke: InkStroke) {
        when (options.handoff) {
            Handoff.PEN_UP_REFRESH -> {
                pendingPenUpRefresh = true
                main.postDelayed({
                    if (pendingPenUpRefresh) {
                        pendingPenUpRefresh = false
                        metrics.count("pen_up_refresh_fallback_used")
                        performHandoffRefresh("fallback")
                    }
                }, options.penUpFallbackMs)
            }
            Handoff.TOGGLE_RAW -> {
                val s = session
                metrics.event("raw_layer_clear_requested", mapOf("strategy" to "toggle"))
                val gen = sessionGeneration.get()
                if (s != null) sessionExecutor.execute {
                    if (gen == sessionGeneration.get()) runCatching { s.setEnabled(false) }.onFailure { recordError("toggle off", it) }
                    main.post { pendingRawReenable = true; invalidate() }
                } else invalidate()
            }
            Handoff.REGIONAL_GC -> {
                pendingGcRect = docBoundsToViewRect(stroke.bounds, pad = 10)
                metrics.event("raw_layer_clear_requested", mapOf("strategy" to "regional_gc"))
                invalidate()
            }
        }
    }

    private fun performHandoffRefresh(trigger: String) {
        metrics.event("raw_layer_clear_requested", mapOf("strategy" to "pen_up_refresh", "trigger" to trigger))
        pendingEpdRepaint = true
        invalidate()
    }

    private fun probeCoordinateSpace(s: RawSample) {
        if (rawCoordinateSpace != "unknown") return
        val down = lastStylusDown ?: return
        if (SystemClock.uptimeMillis() - lastStylusDownAt > 200) return
        val loc = IntArray(2)
        main.post {
            getLocationOnScreen(loc)
            val dx = s.x - down[0]; val dy = s.y - down[1]
            rawCoordinateSpace = when {
                hypot(dx, dy) < 6f -> "view"
                hypot(dx - loc[0], dy - loc[1]) < 6f && (loc[0] != 0 || loc[1] != 0) -> "screen"
                else -> "mismatch(dx=${"%.1f".format(dx)},dy=${"%.1f".format(dy)},loc=${loc[0]},${loc[1]})"
            }
            metrics.event("raw_coordinate_space", mapOf("space" to rawCoordinateSpace))
            publishStatus()
        }
    }

    // ============================================== M01 host additions ==

    /** Colour (#rrggbb), tool type ("pen" | "highlighter") and opacity of new strokes. */
    fun setBrushStyle(color: String, type: String, opacity: Float) {
        brushColor = if (Regex("^#[0-9a-fA-F]{6}$").matches(color)) color.lowercase() else "#000000"
        brushType = if (type == "highlighter") "highlighter" else "pen"
        brushOpacity = opacity.coerceIn(0.05f, 1f)
        setBrush(brushWidth, pressureEnabled, tool)
    }

    /** Adds committed strokes made elsewhere (undo/redo, imports) without a full reload. */
    fun addStrokes(json: String) {
        val parsed = StrokeJson.parseDocument(json)
        val have = strokes.mapTo(HashSet()) { it.id }
        val add = parsed.filter { it.id !in have }
        strokes.addAll(add)
        add.forEach { drawStrokeIntoBitmap(it) }
        metrics.event("strokes_added", mapOf("count" to add.size))
        if (add.isNotEmpty()) requestEpaperRefresh(full = false)
    }

    /** Removes strokes by id (undo of a pen stroke, deletes made in the web editor). */
    fun removeStrokes(ids: Collection<String>) {
        val set = ids.toHashSet()
        val before = strokes.size
        strokes.removeAll { it.id in set }
        if (strokes.size != before) {
            rebuildBitmap()
            requestEpaperRefresh(full = true)
        }
        metrics.event("strokes_removed", mapOf("count" to before - strokes.size))
    }

    /**
     * Test hook for on-device plumbing checks (M01): commits a stroke exactly as
     * the Onyx path does after pen-up (bitmap, listener, ePaper hand-off), with
     * the samples supplied by the caller. Marked `simulated` in its metadata.
     * It does NOT exercise Onyx input; real pen tests remain manual.
     */
    fun injectTestStroke(points: List<InkPoint>) {
        if (points.isEmpty()) return
        val stroke = InkStroke(
            source = "stylus",
            input = InkInput("test-injected", "stylus", mapOf("simulated" to true, "mode" to mode.name)),
            brush = currentBrush(),
            points = points,
        )
        strokes.add(stroke)
        drawStrokeIntoBitmap(stroke)
        metrics.event("test_stroke_injected", mapOf("points" to points.size))
        listener?.onStrokeCommitted(stroke)
        scheduleHandoff(stroke)
    }

    private fun currentBrush(pressureCapable: Boolean = true) = InkBrush(
        type = brushType,
        width = brushWidth,
        color = brushColor,
        pressure = pressureEnabled && pressureCapable && brushType == "pen",
        opacity = if (brushType == "highlighter") brushOpacity else 1f,
    )

    private val colorCache = HashMap<String, Int>()
    private fun parseColor(hex: String): Int = colorCache.getOrPut(hex) { runCatching { Color.parseColor(hex) }.getOrDefault(Color.BLACK) }

    /** Paint colour/alpha for a stroke (M00 strokes are opaque black). */
    private fun paintFor(s: InkStroke) {
        val c = parseColor(s.brush.color)
        val a = (s.brush.opacity.coerceIn(0f, 1f) * 255).toInt()
        inkPaint.color = Color.argb(a, Color.red(c), Color.green(c), Color.blue(c))
    }

    /** Hardware preview colour (opaque; the EPD pen layer has no alpha). */
    private fun hwColor(): Int = parseColor(brushColor)

    // ===================================================== rendering ==

    override fun onSizeChanged(w: Int, h: Int, oldw: Int, oldh: Int) {
        super.onSizeChanged(w, h, oldw, oldh)
        bitmap?.recycle()
        bitmap = if (w > 0 && h > 0) Bitmap.createBitmap(w, h, Bitmap.Config.ARGB_8888) else null
        bitmapCanvas = bitmap?.let { Canvas(it) }
        rebuildBitmap()
        if (mode == Mode.BOOX_HARDWARE) {
            if (session != null) onLayoutMaybeMoved() else ensureSession()
        }
        publishStatus()
    }

    override fun onDraw(canvas: Canvas) {
        val t0 = metrics.now()
        if (!transparentBackground) canvas.drawColor(Color.WHITE)
        bitmap?.let { canvas.drawBitmap(it, 0f, 0f, null) }
        // Mode C: unstable tail of the active stroke (stable pieces are already in the bitmap).
        val a = active
        if (a != null && !activeEraser) {
            canvas.save()
            applyDocMatrix(canvas)
            val tmp = InkStroke(id = "active", source = activeDevice, input = InkInput("android-motionevent", activeDevice), brush = currentBrush(), points = a)
            paintFor(tmp)
            StrokeGeometry.drawPieces(canvas, inkPaint, tmp, activeStable, StrokeGeometry.pieceCount(a.size), options.renderStyle)
            canvas.restore()
        }
        metrics.since("preview_render_submitted_ms", t0)
        if (lastEventHandledNs != 0L) { metrics.since("event_to_draw_ms", lastEventHandledNs); lastEventHandledNs = 0L }

        // ePaper coordination after the committed ink is on the View.
        if (pendingEpdRepaint) {
            pendingEpdRepaint = false
            post {
                runCatching { OnyxBridge.handwritingRepaint(this) }.onFailure { recordError("handwritingRepaint", it) }
                metrics.event("epaper_refresh_requested", mapOf("mode" to "HAND_WRITING_REPAINT_MODE"))
                markPersistentRenderCompleted()
            }
        }
        pendingGcRect?.let { r ->
            pendingGcRect = null
            post {
                runCatching { OnyxBridge.refreshRegionGc(this, r) }.onFailure { recordError("regional GC", it) }
                metrics.event("epaper_refresh_requested", mapOf("mode" to "GC", "rect" to r.flattenToString()))
                markPersistentRenderCompleted()
            }
        }
        if (pendingRawReenable) {
            pendingRawReenable = false
            val s = session
            val gen = sessionGeneration.get()
            post {
                if (s != null) sessionExecutor.execute {
                    if (gen == sessionGeneration.get()) runCatching { s.setEnabled(shouldRawBeEnabled()) }.onFailure { recordError("toggle on", it) }
                }
                metrics.event("epaper_refresh_requested", mapOf("mode" to "raw_toggle"))
                markPersistentRenderCompleted()
            }
        }
    }

    private fun markPersistentRenderCompleted() {
        if (commitStartNs != 0L) {
            metrics.since("persistent_render_completed_ms", commitStartNs)
            metrics.event("persistent_render_completed")
            commitStartNs = 0L
        }
    }

    private fun applyDocMatrix(c: Canvas) {
        val d = transform.density
        c.scale(d, d)
        c.translate(transform.offsetX, transform.offsetY)
        c.scale(transform.scale, transform.scale)
    }

    private fun drawStrokeIntoBitmap(s: InkStroke) {
        val c = bitmapCanvas ?: return
        paintFor(s)
        c.save(); applyDocMatrix(c)
        StrokeGeometry.drawStroke(c, inkPaint, s, options.renderStyle)
        c.restore()
        val r = docBoundsToViewRect(s.bounds, 2)
        invalidate(r)
    }

    private fun rebuildBitmap() {
        val c = bitmapCanvas ?: run { invalidate(); return }
        val t0 = metrics.now()
        c.drawColor(Color.TRANSPARENT, android.graphics.PorterDuff.Mode.CLEAR)
        c.save(); applyDocMatrix(c)
        val vx0 = transform.docX(0f); val vy0 = transform.docY(0f)
        val vx1 = transform.docX(width.toFloat()); val vy1 = transform.docY(height.toFloat())
        var drawn = 0
        for (s in strokes) {
            val b = s.bounds
            if (b[2] < vx0 || b[0] > vx1 || b[3] < vy0 || b[1] > vy1) continue
            paintFor(s)
            StrokeGeometry.drawStroke(c, inkPaint, s, options.renderStyle); drawn++
        }
        c.restore()
        metrics.since("full_rebuild_ms", t0)
        metrics.count("full_rebuilds")
        invalidate()
    }

    private fun docBoundsToViewRect(b: FloatArray, pad: Int): Rect = Rect(
        (transform.viewX(b[0]) - pad).toInt().coerceAtLeast(0),
        (transform.viewY(b[1]) - pad).toInt().coerceAtLeast(0),
        (transform.viewX(b[2]) + pad).toInt().coerceAtMost(max(0, width)),
        (transform.viewY(b[3]) + pad).toInt().coerceAtMost(max(0, height)),
    )

    private fun requestEpaperRefresh(full: Boolean) {
        if (mode != Mode.BOOX_HARDWARE && !capabilities.nativeModeCandidate) return
        post {
            runCatching { if (full) OnyxBridge.fullRefreshGc(this) }.onFailure { recordError("full refresh", it) }
            metrics.event("epaper_refresh_requested", mapOf("mode" to "GC_full"))
        }
    }

    // =================================================== MotionEvent ==

    override fun onTouchEvent(e: MotionEvent): Boolean {
        val handlerStart = metrics.now()
        metrics.count("pointer_event_received")
        metrics.timing("input_to_handler_ms", (SystemClock.uptimeMillis() - e.eventTime).toFloat())
        val idx = e.actionIndex.coerceIn(0, e.pointerCount - 1)
        val toolType = e.getToolType(idx)
        val isStylus = toolType == MotionEvent.TOOL_TYPE_STYLUS || toolType == MotionEvent.TOOL_TYPE_ERASER
        val isMouse = toolType == MotionEvent.TOOL_TYPE_MOUSE
        val hasStylus = (0 until e.pointerCount).any {
            val t = e.getToolType(it); t == MotionEvent.TOOL_TYPE_STYLUS || t == MotionEvent.TOOL_TYPE_ERASER
        }

        if (hasStylus) {
            when (e.actionMasked) {
                MotionEvent.ACTION_DOWN -> {
                    stylusDown = true
                    lastStylusDown = floatArrayOf(e.getX(idx), e.getY(idx)); lastStylusDownAt = e.eventTime
                }
                MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> stylusDown = false
            }
        }

        // Finger input: palm rejection + navigation.
        if (!isStylus && !isMouse) {
            if (stylusDown || active != null && activeDevice != "touch") { metrics.count("palm_rejected_while_stylus"); return true }
            if (e.getTouchMajor(idx) > PALM_TOUCH_MAJOR_PX * resources.displayMetrics.density) { metrics.count("palm_rejected_large_contact"); return true }
            if (!(options.fingerDraws && mode == Mode.NATIVE_SOFTWARE && e.pointerCount == 1 && navMode == 0)) {
                if (options.fingerNavigation) handleNavigation(e)
                return true
            }
        }

        // Stylus in Mode B with raw drawing active: the Onyx pipeline owns the
        // stroke. Forward to TouchHelper only; never draw in software too.
        val s = session
        if (mode == Mode.BOOX_HARDWARE && s != null && tool == Tool.PEN && isStylus) {
            metrics.count("motion_events_while_raw")
            runCatching { s.onTouchEvent(e) }
            return true
        }
        if (!drawingEnabled) return true

        val eraserTip = toolType == MotionEvent.TOOL_TYPE_ERASER ||
            (e.buttonState and MotionEvent.BUTTON_STYLUS_PRIMARY) != 0
        handleSoftwareStroke(e, if (isStylus) "stylus" else if (isMouse) "mouse" else "touch", tool == Tool.ERASER || eraserTip)
        metrics.since("input_sample_processed_ms", handlerStart)
        lastEventHandledNs = handlerStart
        metrics.count("preview_update_scheduled")
        return true
    }

    private fun sample(e: MotionEvent, i: Int, hist: Int): InkPoint {
        val x = if (hist >= 0) e.getHistoricalX(i, hist) else e.getX(i)
        val y = if (hist >= 0) e.getHistoricalY(i, hist) else e.getY(i)
        val p = if (hist >= 0) e.getHistoricalPressure(i, hist) else e.getPressure(i)
        val t = if (hist >= 0) e.getHistoricalEventTime(hist) else e.eventTime
        val tilt = if (hist >= 0) e.getHistoricalAxisValue(MotionEvent.AXIS_TILT, i, hist) else e.getAxisValue(MotionEvent.AXIS_TILT, i)
        val orient = if (hist >= 0) e.getHistoricalAxisValue(MotionEvent.AXIS_ORIENTATION, i, hist) else e.getAxisValue(MotionEvent.AXIS_ORIENTATION, i)
        // Android reports tilt (radians from perpendicular) + orientation; convert to W3C-style tiltX/tiltY degrees.
        val tiltXDeg = Math.toDegrees(kotlin.math.atan(kotlin.math.tan(tilt.toDouble()) * kotlin.math.sin(orient.toDouble()))).toFloat()
        val tiltYDeg = Math.toDegrees(kotlin.math.atan(kotlin.math.tan(tilt.toDouble()) * -kotlin.math.cos(orient.toDouble()))).toFloat()
        if (activeT0 == 0L) activeT0 = t
        return InkPoint(
            transform.docX(x), transform.docY(y),
            if (activeDevice == "mouse") 0.5f else p.coerceIn(0f, 1f),
            if (tiltXDeg.isNaN()) 0f else tiltXDeg, if (tiltYDeg.isNaN()) 0f else tiltYDeg,
            (t - activeT0) * 1000L,
        )
    }

    private fun handleSoftwareStroke(e: MotionEvent, device: String, eraser: Boolean) {
        val i = e.actionIndex.coerceIn(0, e.pointerCount - 1)
        when (e.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                activeDevice = device; activeT0 = 0L; activeStable = 0; activeEraser = eraser
                activeErased.clear()
                active = arrayListOf(sample(e, i, -1))
                if (eraser) { if (eraseAt(active!!.last())) rebuildBitmap() } else invalidateActive()
            }
            MotionEvent.ACTION_MOVE -> {
                val a = active ?: return
                val before = a.size
                for (h in 0 until e.historySize) a.add(sample(e, i, h))
                a.add(sample(e, i, -1))
                metrics.count("input_samples", (a.size - before).toLong())
                if (activeEraser) {
                    var hit = false
                    for (k in before until a.size) hit = eraseAt(a[k]) || hit
                    if (hit) rebuildBitmap() // once per input batch, not per sample
                    return
                }
                flushStablePieces()
                invalidateActive()
            }
            MotionEvent.ACTION_UP -> {
                val a = active ?: return
                val last = sample(e, i, -1)
                if (a.last().x != last.x || a.last().y != last.y) a.add(last)
                active = null
                if (activeEraser) {
                    if (eraseAt(last)) rebuildBitmap()
                    if (activeErased.isNotEmpty()) listener?.onStrokesErased(activeErased.toList())
                    requestEpaperRefresh(full = true)
                    return
                }
                val t0 = metrics.now()
                val stroke = InkStroke(
                    source = device,
                    input = InkInput("android-motionevent", device, mapOf("hardwarePreview" to false)),
                    brush = currentBrush(device != "mouse"),
                    points = a,
                )
                strokes.add(stroke)
                metrics.event("stroke_finalized", mapOf("points" to a.size))
                val c = bitmapCanvas
                if (c != null) {
                    paintFor(stroke)
                    c.save(); applyDocMatrix(c)
                    StrokeGeometry.drawPieces(c, inkPaint, stroke, activeStable, StrokeGeometry.pieceCount(a.size), options.renderStyle)
                    c.restore()
                }
                invalidate()
                metrics.since("persistent_render_completed_ms", t0)
                metrics.strokeSummary(strokeSummaryJson(stroke, null))
                listener?.onStrokeCommitted(stroke)
            }
            MotionEvent.ACTION_CANCEL -> {
                // Interrupted stroke: keep what was delivered (no artificial points).
                if (active != null && !activeEraser) {
                    metrics.count("stroke_cancelled")
                    val ev = MotionEvent.obtain(e); ev.action = MotionEvent.ACTION_UP
                    handleSoftwareStroke(ev, device, false); ev.recycle()
                } else active = null
            }
        }
    }

    private fun flushStablePieces() {
        val a = active ?: return
        val stable = StrokeGeometry.stablePieceCount(a.size)
        if (stable <= activeStable) return
        val c = bitmapCanvas ?: return
        val tmp = InkStroke(id = "active", source = activeDevice, input = InkInput("android-motionevent", activeDevice), brush = currentBrush(activeDevice != "mouse"), points = a)
        paintFor(tmp)
        c.save(); applyDocMatrix(c)
        StrokeGeometry.drawPieces(c, inkPaint, tmp, activeStable, stable, options.renderStyle)
        c.restore()
        activeStable = stable
    }

    private fun invalidateActive() {
        val a = active ?: return
        // Invalidate the region around the last few samples.
        val from = max(0, a.size - 4)
        var minX = Float.MAX_VALUE; var minY = Float.MAX_VALUE; var maxX = -Float.MAX_VALUE; var maxY = -Float.MAX_VALUE
        for (k in from until a.size) { minX = min(minX, a[k].x); minY = min(minY, a[k].y); maxX = max(maxX, a[k].x); maxY = max(maxY, a[k].y) }
        val pad = brushWidth * 1.6f + 2f
        invalidate(docBoundsToViewRect(floatArrayOf(minX - pad, minY - pad, maxX + pad, maxY + pad), 2))
    }

    /**
     * Removes strokes hit at [p]; returns true if any were removed. Does NOT rebuild the
     * bitmap: callers rebuild once per batch. (Rebuilding per eraser sample made one
     * eraser stroke over a 10k-stroke page take ~39 s on a Note Air5 C: 66 hits × ~600 ms.)
     */
    private fun eraseAt(p: InkPoint): Boolean {
        val r = ERASER_RADIUS_DOC / transform.scale
        val removed = ArrayList<String>()
        val it = strokes.iterator()
        while (it.hasNext()) { val s = it.next(); if (StrokeGeometry.hitTest(s, p.x, p.y, r)) { removed.add(s.id); it.remove() } }
        if (removed.isEmpty()) return false
        activeErased.addAll(removed)
        metrics.count("strokes_erased", removed.size.toLong())
        return true
    }

    /**
     * Programmatic whole-stroke erase along a path in document coordinates (benchmarks:
     * same hit test, bitmap rebuild and onStrokesErased event as a pen eraser stroke).
     * Returns the number of strokes removed.
     */
    fun eraseAlongDoc(points: List<InkPoint>): Int {
        val t0 = metrics.now()
        eraseAlong(points)
        metrics.since("programmatic_erase_ms", t0)
        val n = activeErased.size
        activeErased.clear()
        return n
    }

    private fun eraseAlong(points: List<InkPoint>) {
        activeErased.clear()
        var hit = false
        points.forEach { hit = eraseAt(it) || hit }
        if (hit) rebuildBitmap()
        if (activeErased.isNotEmpty()) listener?.onStrokesErased(activeErased.toList())
    }

    private fun handleNavigation(e: MotionEvent) {
        when (e.actionMasked) {
            MotionEvent.ACTION_DOWN -> { navMode = 1; navLastX = e.getX(0); navLastY = e.getY(0); updateRawEnabled() }
            MotionEvent.ACTION_POINTER_DOWN -> if (e.pointerCount >= 2) {
                val fx = (e.getX(0) + e.getX(1)) / 2f; val fy = (e.getY(0) + e.getY(1)) / 2f
                pinchStartDist = max(1f, hypot(e.getX(1) - e.getX(0), e.getY(1) - e.getY(0)))
                pinchStartScale = transform.scale
                pinchAnchorX = transform.docX(fx); pinchAnchorY = transform.docY(fy)
                navMode = 2
            }
            MotionEvent.ACTION_MOVE -> {
                val d = transform.density
                if (navMode == 2 && e.pointerCount >= 2) {
                    val fx = (e.getX(0) + e.getX(1)) / 2f; val fy = (e.getY(0) + e.getY(1)) / 2f
                    val dist = max(1f, hypot(e.getX(1) - e.getX(0), e.getY(1) - e.getY(0)))
                    val sc = (pinchStartScale * dist / pinchStartDist).coerceIn(0.25f, 8f)
                    transform = transform.copy(scale = sc, offsetX = fx / d - pinchAnchorX * sc, offsetY = fy / d - pinchAnchorY * sc)
                } else if (navMode == 1) {
                    val dx = (e.getX(0) - navLastX) / d; val dy = (e.getY(0) - navLastY) / d
                    navLastX = e.getX(0); navLastY = e.getY(0)
                    transform = transform.copy(offsetX = transform.offsetX + dx, offsetY = transform.offsetY + dy)
                }
                rebuildBitmap()
                listener?.onViewportChanged(transform)
            }
            MotionEvent.ACTION_POINTER_UP -> if (e.pointerCount - 1 < 2) {
                navMode = 1
                val keep = if (e.actionIndex == 0) 1 else 0
                navLastX = e.getX(keep); navLastY = e.getY(keep)
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> {
                navMode = 0
                setBrush(brushWidth, pressureEnabled) // hardware width follows zoom
                updateRawEnabled()
                requestEpaperRefresh(full = true)
            }
        }
    }

    private fun strokeSummaryJson(s: InkStroke, r: StrokeAssembler.Result?): JSONObject {
        val o = JSONObject().put("id", s.id).put("points", s.points.size).put("api", s.input.api)
        if (s.points.size > 1) o.put("durationUs", s.points.last().timeUs - s.points.first().timeUs)
        if (r != null) o.put("stream", r.chosen.name).put("moveCount", r.moveCount).put("listCount", r.listCount)
            .put("timeGaps", r.timeGaps).put("spatialJumps", r.spatialJumps).put("medianDtMs", r.medianDtMs.toDouble())
            .put("maxDtMs", r.maxDtMs).put("duplicatesRemoved", r.duplicatesRemoved).put("outOfBounds", r.outOfBounds)
        return o
    }

    // ===================================================== lifecycle ==

    override fun onAttachedToWindow() {
        super.onAttachedToWindow()
        viewTreeObserver.addOnGlobalLayoutListener(layoutListener)
        registerLifecycle()
        ensureSession()
    }

    override fun onDetachedFromWindow() {
        viewTreeObserver.removeOnGlobalLayoutListener(layoutListener)
        unregisterLifecycle()
        closeSession("view detached")
        super.onDetachedFromWindow()
    }

    private fun findActivity(): Activity? {
        var c: Context? = context
        while (c is ContextWrapper) { if (c is Activity) return c; c = c.baseContext }
        return null
    }

    private fun registerLifecycle() {
        val act = findActivity() ?: return
        if (lifecycleCallbacks != null) return
        val cb = object : Application.ActivityLifecycleCallbacks {
            override fun onActivityPaused(a: Activity) { if (a === act) { hostPaused = true; closeSession("activity paused") } }
            override fun onActivityResumed(a: Activity) { if (a === act) { hostPaused = false; ensureSession() } }
            override fun onActivityCreated(a: Activity, b: Bundle?) {}
            override fun onActivityStarted(a: Activity) {}
            override fun onActivityStopped(a: Activity) {}
            override fun onActivitySaveInstanceState(a: Activity, b: Bundle) {}
            override fun onActivityDestroyed(a: Activity) { if (a === act) closeSession("activity destroyed") }
        }
        act.application.registerActivityLifecycleCallbacks(cb)
        lifecycleCallbacks = cb
    }

    private fun unregisterLifecycle() {
        val cb = lifecycleCallbacks ?: return
        findActivity()?.application?.unregisterActivityLifecycleCallbacks(cb)
        lifecycleCallbacks = null
    }

    private fun recordError(where: String, t: Throwable) {
        val msg = "$where: ${t.javaClass.name}: ${t.message}"
        Log.w(TAG, msg, t)
        lastError = msg
        metrics.error(msg)
        main.post { publishStatus() }
    }

    private fun publishStatus() {
        if (Looper.myLooper() != Looper.getMainLooper()) { main.post { publishStatus() }; return }
        listener?.onStatusChanged(status())
    }

    companion object {
        private const val TAG = "BooxInkView"
        const val ERASER_RADIUS_DOC = 8f
        /** Contacts larger than this (dp) are treated as palms. */
        const val PALM_TOUCH_MAJOR_PX = 40f
    }
}
