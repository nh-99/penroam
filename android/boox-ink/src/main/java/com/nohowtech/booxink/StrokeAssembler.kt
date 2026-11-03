package com.nohowtech.booxink

import kotlin.math.abs
import kotlin.math.hypot

/**
 * A sample exactly as delivered by a native input source, before any
 * coordinate mapping. Coordinates are View-local physical px. Kept free of
 * Onyx classes so the merge logic is unit-testable on the JVM.
 */
data class RawSample(
    val x: Float,
    val y: Float,
    /** Raw pressure as reported (Onyx: 0..EpdController.getMaxTouchPressure()). */
    val pressure: Float,
    val tiltX: Float,
    val tiltY: Float,
    /** Source timestamp in ms, epoch is firmware-dependent. */
    val timestampMs: Long,
)

/**
 * Builds one stroke from the two Onyx callback streams:
 *
 *  - the *move stream*: onBeginRawDrawing + every onRawDrawingTouchPointMoveReceived
 *    + onEndRawDrawing point;
 *  - the *list stream*: onRawDrawingTouchPointListReceived, documented as the
 *    accumulated point list delivered before onEndRawDrawing.
 *
 * Firmware differs in which stream is complete (see docs/platforms/boox.md),
 * so both are kept and the choice is recorded with the stroke. Neither stream
 * is synthesized or resampled: only delivered samples are committed.
 *
 * Not thread-safe; callers serialize access (the Onyx callback thread).
 */
class StrokeAssembler {

    enum class Chosen { LIST, MOVE }

    data class Result(
        val points: List<InkPoint>,
        val chosen: Chosen,
        val moveCount: Int,
        val listCount: Int,
        val listCallbacks: Int,
        val duplicatesRemoved: Int,
        val timeGaps: Int,
        val spatialJumps: Int,
        val medianDtMs: Float,
        val maxDtMs: Long,
        val interrupted: Boolean,
        val outOfBounds: Int,
    )

    private val move = ArrayList<RawSample>(512)
    private val list = ArrayList<RawSample>(512)
    private var listCallbacks = 0
    var inProgress = false
        private set

    fun begin(s: RawSample) {
        move.clear(); list.clear(); listCallbacks = 0
        inProgress = true
        move.add(s)
    }

    fun move(s: RawSample) {
        if (!inProgress) begin(s) else move.add(s)
    }

    /**
     * Accepts a list callback. If the new list extends the list seen so far
     * (cumulative delivery) it replaces it; otherwise it is treated as a chunk
     * and appended (chunked delivery seen on some firmware).
     */
    fun list(samples: List<RawSample>) {
        if (samples.isEmpty()) return
        listCallbacks++
        if (list.isEmpty()) { list.addAll(samples); return }
        val common = minOf(list.size, samples.size)
        var prefix = true
        for (i in 0 until common) {
            if (abs(list[i].x - samples[i].x) > 0.35f || abs(list[i].y - samples[i].y) > 0.35f) { prefix = false; break }
        }
        if (prefix && samples.size >= list.size) { list.clear(); list.addAll(samples) } else list.addAll(samples)
    }

    fun end(s: RawSample?, transform: ViewTransform, maxPressure: Float, interrupted: Boolean = false, boundsPx: FloatArray? = null): Result? {
        if (!inProgress) return null
        inProgress = false
        if (s != null) move.add(s)
        val useList = list.isNotEmpty() && list.size >= (move.size * 9) / 10
        val chosenRaw = if (useList) list else move
        // De-duplicate exact repeats (the end callback often repeats the last move).
        val dedup = ArrayList<RawSample>(chosenRaw.size)
        var dups = 0
        for (r in chosenRaw) {
            val last = dedup.lastOrNull()
            if (last != null && last.x == r.x && last.y == r.y && last.timestampMs == r.timestampMs) { dups++; continue }
            dedup.add(r)
        }
        if (dedup.isEmpty()) return null
        var outOfBounds = 0
        if (boundsPx != null) for (r in dedup) {
            if (r.x < boundsPx[0] || r.y < boundsPx[1] || r.x > boundsPx[2] || r.y > boundsPx[3]) outOfBounds++
        }

        // Gap analysis on delivered timestamps.
        val dts = LongArray(maxOf(0, dedup.size - 1)) { dedup[it + 1].timestampMs - dedup[it].timestampMs }
        val sortedDt = dts.sortedArray()
        val median = if (sortedDt.isEmpty()) 0f else sortedDt[sortedDt.size / 2].toFloat()
        var gaps = 0
        var jumps = 0
        val gapThreshold = maxOf(30f, median * 4f)
        for (i in dts.indices) {
            if (dts[i] > gapThreshold) gaps++
            val dDoc = hypot(
                transform.docX(dedup[i + 1].x) - transform.docX(dedup[i].x),
                transform.docY(dedup[i + 1].y) - transform.docY(dedup[i].y),
            )
            if (dDoc > JUMP_DOC_UNITS) jumps++
        }

        val t0 = dedup.first().timestampMs
        val norm = if (maxPressure > 1f) maxPressure else 1f
        val pts = dedup.map {
            InkPoint(
                x = transform.docX(it.x),
                y = transform.docY(it.y),
                pressure = (it.pressure / norm).coerceIn(0f, 1f),
                tiltX = it.tiltX,
                tiltY = it.tiltY,
                timeUs = (it.timestampMs - t0) * 1000L,
            )
        }
        val result = Result(
            points = pts,
            chosen = if (useList) Chosen.LIST else Chosen.MOVE,
            moveCount = move.size,
            listCount = list.size,
            listCallbacks = listCallbacks,
            duplicatesRemoved = dups,
            timeGaps = gaps,
            spatialJumps = jumps,
            medianDtMs = median,
            maxDtMs = sortedDt.lastOrNull() ?: 0L,
            interrupted = interrupted,
            outOfBounds = outOfBounds,
        )
        move.clear(); list.clear(); listCallbacks = 0
        return result
    }

    fun cancel() { inProgress = false; move.clear(); list.clear(); listCallbacks = 0 }

    companion object {
        /** Consecutive samples further apart than this are counted as discontinuities. */
        const val JUMP_DOC_UNITS = 40f
    }
}
