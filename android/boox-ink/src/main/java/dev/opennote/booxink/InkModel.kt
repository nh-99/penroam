package dev.opennote.booxink

import org.json.JSONArray
import org.json.JSONObject
import java.util.UUID

/**
 * Canonical M00 stroke representation (see docs/architecture.md, "Canonical
 * stroke format"). Identical in the Dart and TypeScript prototypes.
 *
 * Coordinates are document units (logical px at zoom 1.0), pressure is
 * normalized to 0..1, timeUs is microseconds relative to the first sample.
 */
data class InkPoint(
    val x: Float,
    val y: Float,
    val pressure: Float,
    val tiltX: Float = 0f,
    val tiltY: Float = 0f,
    val timeUs: Long = 0L,
)

data class InkBrush(
    val type: String = "pen",
    val width: Float = 3f,
    val color: String = "#000000",
    val pressure: Boolean = true,
    /** M01: 1.0 for pens, < 1 for highlighters (alpha). Omitted from JSON when 1. */
    val opacity: Float = 1f,
)

data class InkInput(
    /** e.g. "onyx-touchhelper", "android-motionevent", "flutter-pointer", "web-pointer", "generated" */
    val api: String,
    /** e.g. "stylus", "mouse", "touch", "eraser" */
    val device: String,
    /** Free-form diagnostics (which point stream was used, sample counts, ...). */
    val extra: Map<String, Any> = emptyMap(),
)

data class InkStroke(
    val id: String = UUID.randomUUID().toString(),
    val source: String,
    val input: InkInput,
    val brush: InkBrush,
    val points: List<InkPoint>,
) {
    /** Document-space bounds including half the max width. */
    val bounds: FloatArray by lazy { computeBounds() }

    private fun computeBounds(): FloatArray {
        if (points.isEmpty()) return floatArrayOf(0f, 0f, 0f, 0f)
        var minX = Float.MAX_VALUE; var minY = Float.MAX_VALUE
        var maxX = -Float.MAX_VALUE; var maxY = -Float.MAX_VALUE
        var maxW = 0f
        for (p in points) {
            if (p.x < minX) minX = p.x
            if (p.y < minY) minY = p.y
            if (p.x > maxX) maxX = p.x
            if (p.y > maxY) maxY = p.y
            val w = StrokeGeometry.widthFor(brush.width, p.pressure, brush.pressure)
            if (w > maxW) maxW = w
        }
        val pad = maxW / 2f + 1f
        return floatArrayOf(minX - pad, minY - pad, maxX + pad, maxY + pad)
    }
}

object StrokeJson {
    const val FORMAT = "m00-strokes"
    const val VERSION = 1

    fun strokeToJson(s: InkStroke): JSONObject {
        val pts = JSONArray()
        for (p in s.points) {
            pts.put(
                JSONObject()
                    .put("x", p.x.toDouble())
                    .put("y", p.y.toDouble())
                    .put("pressure", p.pressure.toDouble())
                    .put("tiltX", p.tiltX.toDouble())
                    .put("tiltY", p.tiltY.toDouble())
                    .put("timeUs", p.timeUs),
            )
        }
        val input = JSONObject().put("api", s.input.api).put("device", s.input.device)
        for ((k, v) in s.input.extra) input.put(k, v)
        return JSONObject()
            .put("id", s.id)
            .put("source", s.source)
            .put("input", input)
            .put(
                "brush",
                JSONObject()
                    .put("type", s.brush.type)
                    .put("width", s.brush.width.toDouble())
                    .put("color", s.brush.color)
                    .put("pressure", s.brush.pressure)
                    .also { if (s.brush.opacity != 1f) it.put("opacity", s.brush.opacity.toDouble()) },
            )
            .put("points", pts)
    }

    fun strokeFromJson(o: JSONObject): InkStroke {
        val b = o.optJSONObject("brush") ?: JSONObject()
        val i = o.optJSONObject("input") ?: JSONObject()
        val arr = o.optJSONArray("points") ?: JSONArray()
        val pts = ArrayList<InkPoint>(arr.length())
        for (k in 0 until arr.length()) {
            val p = arr.getJSONObject(k)
            pts.add(
                InkPoint(
                    x = p.optDouble("x", 0.0).toFloat(),
                    y = p.optDouble("y", 0.0).toFloat(),
                    pressure = p.optDouble("pressure", 0.5).toFloat(),
                    tiltX = p.optDouble("tiltX", 0.0).toFloat(),
                    tiltY = p.optDouble("tiltY", 0.0).toFloat(),
                    timeUs = p.optLong("timeUs", 0L),
                ),
            )
        }
        val extra = HashMap<String, Any>()
        for (key in i.keys()) if (key != "api" && key != "device") extra[key] = i.get(key)
        return InkStroke(
            id = o.optString("id", UUID.randomUUID().toString()),
            source = o.optString("source", "unknown"),
            input = InkInput(i.optString("api", "unknown"), i.optString("device", "unknown"), extra),
            brush = InkBrush(
                type = b.optString("type", "pen"),
                width = b.optDouble("width", 3.0).toFloat(),
                color = b.optString("color", "#000000"),
                pressure = b.optBoolean("pressure", true),
                opacity = b.optDouble("opacity", 1.0).toFloat(),
            ),
            points = pts,
        )
    }

    /** Accepts either a full document `{format, strokes: [...]}` or a bare array. */
    fun parseDocument(json: String): List<InkStroke> {
        val trimmed = json.trim()
        val arr = if (trimmed.startsWith("[")) JSONArray(trimmed) else JSONObject(trimmed).optJSONArray("strokes") ?: JSONArray()
        return List(arr.length()) { strokeFromJson(arr.getJSONObject(it)) }
    }

    fun documentToJson(strokes: List<InkStroke>, meta: JSONObject = JSONObject()): String {
        val arr = JSONArray()
        strokes.forEach { arr.put(strokeToJson(it)) }
        return JSONObject()
            .put("format", FORMAT)
            .put("version", VERSION)
            .put("meta", meta)
            .put("strokes", arr)
            .toString()
    }
}
