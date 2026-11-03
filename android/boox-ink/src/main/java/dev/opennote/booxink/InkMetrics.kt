package dev.opennote.booxink

import android.os.Debug
import android.os.SystemClock
import org.json.JSONArray
import org.json.JSONObject

/**
 * Benchmark instrumentation for the native module. Event names follow
 * docs/benchmarks/protocol.md so results can be merged with the web recorder.
 *
 * All internal timing uses SystemClock.elapsedRealtimeNanos() (monotonic).
 * Onyx TouchPoint.timestamp uses a firmware-dependent epoch; it is only used
 * for intra-stroke sample spacing, never compared against our clock unless
 * the epoch probe locked to uptimeMillis (see [recordDispatchDelay]).
 *
 * These are *internal* timings. They are not physical pen-to-ink latency;
 * the Onyx hardware preview path is invisible to the app.
 */
class InkMetrics(private val capacity: Int = 4096) {

    private val lock = Any()
    private val counters = LinkedHashMap<String, Long>()
    private val series = LinkedHashMap<String, Series>()
    private val events = ArrayDeque<JSONObject>()
    private val strokes = ArrayDeque<JSONObject>()
    private val errors = ArrayList<String>()

    private class Series(cap: Int) {
        val values = FloatArray(cap)
        var count = 0L
        var next = 0
        fun add(v: Float) { values[next] = v; next = (next + 1) % values.size; count++ }
        fun snapshot(): FloatArray {
            val n = minOf(count, values.size.toLong()).toInt()
            return FloatArray(n) { values[it] }
        }
    }

    fun now(): Long = SystemClock.elapsedRealtimeNanos()

    fun count(name: String, delta: Long = 1) = synchronized(lock) {
        counters[name] = (counters[name] ?: 0L) + delta
    }

    /** Records a duration sample in milliseconds. */
    fun timing(name: String, ms: Float) = synchronized(lock) {
        series.getOrPut(name) { Series(capacity) }.add(ms)
    }

    fun since(name: String, startNanos: Long) = timing(name, (now() - startNanos) / 1_000_000f)

    /** Named lifecycle event (bounded log) plus a counter. */
    fun event(name: String, detail: Map<String, Any?> = emptyMap()) = synchronized(lock) {
        counters[name] = (counters[name] ?: 0L) + 1
        val o = JSONObject().put("name", name).put("tNs", now())
        for ((k, v) in detail) o.put(k, v ?: JSONObject.NULL)
        events.addLast(o)
        while (events.size > 512) events.removeFirst()
    }

    fun error(message: String) = synchronized(lock) {
        errors.add(message)
        while (errors.size > 64) errors.removeAt(0)
    }

    fun strokeSummary(o: JSONObject) = synchronized(lock) {
        strokes.addLast(o)
        while (strokes.size > 256) strokes.removeFirst()
    }

    // Onyx timestamp epoch is firmware dependent; lock it on first use.
    private var dispatchEpoch = 0 // 0 unknown, 1 uptime, 2 wall, -1 unavailable

    fun recordDispatchDelay(sdkTimestampMs: Long) {
        if (sdkTimestampMs <= 0L || dispatchEpoch == -1) return
        if (dispatchEpoch == 0) {
            val up = SystemClock.uptimeMillis() - sdkTimestampMs
            val wall = System.currentTimeMillis() - sdkTimestampMs
            dispatchEpoch = when {
                up in 0L..60_000L -> 1
                wall in 0L..60_000L -> 2
                else -> -1
            }
            event("onyx_timestamp_epoch", mapOf("epoch" to dispatchEpoch, "uptimeDelta" to up, "wallDelta" to wall))
        }
        val d = when (dispatchEpoch) {
            1 -> SystemClock.uptimeMillis() - sdkTimestampMs
            2 -> System.currentTimeMillis() - sdkTimestampMs
            else -> return
        }
        if (d >= 0) timing("native_input_to_callback_ms", d.toFloat())
    }

    fun reset() = synchronized(lock) {
        counters.clear(); series.clear(); events.clear(); strokes.clear(); errors.clear()
    }

    fun toJson(): JSONObject = synchronized(lock) {
        val c = JSONObject()
        for ((k, v) in counters) c.put(k, v)
        val t = JSONObject()
        for ((k, s) in series) t.put(k, summarize(s.snapshot(), s.count))
        val rt = Runtime.getRuntime()
        val mem = JSONObject()
            .put("javaHeapUsedBytes", rt.totalMemory() - rt.freeMemory())
            .put("javaHeapMaxBytes", rt.maxMemory())
            .put("nativeHeapAllocatedBytes", Debug.getNativeHeapAllocatedSize())
        JSONObject()
            .put("clock", "SystemClock.elapsedRealtimeNanos")
            .put("counters", c)
            .put("timings", t)
            .put("memory", mem)
            .put("events", JSONArray(events.toList()))
            .put("strokes", JSONArray(strokes.toList()))
            .put("errors", JSONArray(errors))
    }

    companion object {
        fun summarize(values: FloatArray, total: Long): JSONObject {
            val o = JSONObject().put("count", total).put("retained", values.size)
            if (values.isEmpty()) return o
            val sorted = values.sortedArray()
            fun pct(p: Double): Double {
                val idx = ((sorted.size - 1) * p).let { Math.round(it).toInt() }
                return sorted[idx].toDouble()
            }
            return o.put("median", pct(0.5)).put("p95", pct(0.95)).put("max", sorted.last().toDouble())
                .put("min", sorted.first().toDouble()).put("mean", sorted.average())
        }
    }
}
