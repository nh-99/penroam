package com.nohowtech.booxink

import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Path
import kotlin.math.max
import kotlin.math.sqrt

/**
 * Kotlin port of the shared brush geometry (reference implementation:
 * fixtures/generate-fixtures.mjs). Verified against
 * fixtures/geometry-golden.json by StrokeGeometryTest.
 */
object StrokeGeometry {

    sealed class Piece {
        abstract val width: Float
        data class Dot(val x: Float, val y: Float, override val width: Float) : Piece()
        data class Line(val x0: Float, val y0: Float, val x1: Float, val y1: Float, override val width: Float) : Piece()
        data class Quad(
            val x0: Float, val y0: Float, val cx: Float, val cy: Float,
            val x1: Float, val y1: Float, override val width: Float,
        ) : Piece()
    }

    fun widthFor(baseWidth: Float, pressure: Float, pressureEnabled: Boolean): Float {
        if (!pressureEnabled) return baseWidth
        val p = pressure.coerceIn(0f, 1f)
        return max(0.5f, baseWidth * (0.4f + 1.2f * p))
    }

    /** Double-precision variant used by the golden test (matches JS numerics). */
    fun widthForD(baseWidth: Double, pressure: Double, pressureEnabled: Boolean): Double {
        if (!pressureEnabled) return baseWidth
        val p = pressure.coerceIn(0.0, 1.0)
        return max(0.5, baseWidth * (0.4 + 1.2 * p))
    }

    /** Pieces for the full stroke; see generate-fixtures.mjs for the spec. */
    fun pieces(points: List<InkPoint>, baseWidth: Float, pressureEnabled: Boolean): List<Piece> {
        val out = ArrayList<Piece>(points.size + 1)
        for (k in 0 until pieceCount(points.size)) out.add(piece(points, k, baseWidth, pressureEnabled))
        return out
    }

    fun pieceCount(n: Int): Int = when (n) {
        0 -> 0
        1, 2 -> 1
        else -> n
    }

    /**
     * Number of pieces that can no longer change once [n] points are known
     * (all but the trailing line). Used for incremental preview rendering.
     */
    fun stablePieceCount(n: Int): Int = if (n < 3) 0 else n - 1

    fun piece(points: List<InkPoint>, k: Int, baseWidth: Float, pressureEnabled: Boolean): Piece {
        val n = points.size
        fun w(p: InkPoint) = widthFor(baseWidth, p.pressure, pressureEnabled)
        if (n == 1) return Piece.Dot(points[0].x, points[0].y, w(points[0]))
        if (n == 2) return Piece.Line(points[0].x, points[0].y, points[1].x, points[1].y, w(points[0]))
        return when (k) {
            0 -> {
                val a = points[0]; val b = points[1]
                Piece.Line(a.x, a.y, (a.x + b.x) / 2f, (a.y + b.y) / 2f, w(a))
            }
            n - 1 -> {
                val a = points[n - 2]; val b = points[n - 1]
                Piece.Line((a.x + b.x) / 2f, (a.y + b.y) / 2f, b.x, b.y, w(b))
            }
            else -> {
                val p0 = points[k - 1]; val p1 = points[k]; val p2 = points[k + 1]
                Piece.Quad(
                    (p0.x + p1.x) / 2f, (p0.y + p1.y) / 2f,
                    p1.x, p1.y,
                    (p1.x + p2.x) / 2f, (p1.y + p2.y) / 2f,
                    w(p1),
                )
            }
        }
    }

    /** Render style, shared with the framework prototypes' debug selector. */
    enum class RenderStyle { SMOOTH, POLYLINE }

    private val path = Path()

    /**
     * Draws pieces [from, to) of [stroke] onto [canvas]. The canvas must
     * already carry the document->pixel transform. [paint] must be a STROKE
     * paint with round caps/joins; its width/color are overwritten.
     */
    fun drawPieces(
        canvas: Canvas,
        paint: Paint,
        stroke: InkStroke,
        from: Int,
        to: Int,
        style: RenderStyle = RenderStyle.SMOOTH,
    ) {
        val pts = stroke.points
        val n = pts.size
        if (n == 0 || from >= to) return
        if (style == RenderStyle.POLYLINE) {
            paint.strokeWidth = stroke.brush.width
            if (n == 1) { drawDot(canvas, paint, pts[0].x, pts[0].y, stroke.brush.width); return }
            val start = max(0, from - 1)
            val end = minOf(n - 1, to)
            path.reset()
            path.moveTo(pts[start].x, pts[start].y)
            for (i in start + 1..end) path.lineTo(pts[i].x, pts[i].y)
            canvas.drawPath(path, paint)
            return
        }
        val pressure = stroke.brush.pressure
        if (!pressure && n >= 2) {
            // Constant width: one path for the whole range (cheaper, no cap overlap).
            paint.strokeWidth = stroke.brush.width
            path.reset()
            for (k in from until to) {
                when (val pc = piece(pts, k, stroke.brush.width, false)) {
                    is Piece.Line -> { if (k == from) path.moveTo(pc.x0, pc.y0); path.lineTo(pc.x1, pc.y1) }
                    is Piece.Quad -> { if (k == from) path.moveTo(pc.x0, pc.y0); path.quadTo(pc.cx, pc.cy, pc.x1, pc.y1) }
                    is Piece.Dot -> drawDot(canvas, paint, pc.x, pc.y, pc.width)
                }
            }
            canvas.drawPath(path, paint)
            return
        }
        for (k in from until to) drawPiece(canvas, paint, piece(pts, k, stroke.brush.width, pressure))
    }

    fun drawStroke(canvas: Canvas, paint: Paint, stroke: InkStroke, style: RenderStyle = RenderStyle.SMOOTH) =
        drawPieces(canvas, paint, stroke, 0, pieceCount(stroke.points.size), style)

    fun drawPiece(canvas: Canvas, paint: Paint, pc: Piece) {
        when (pc) {
            is Piece.Dot -> drawDot(canvas, paint, pc.x, pc.y, pc.width)
            is Piece.Line -> { paint.strokeWidth = pc.width; canvas.drawLine(pc.x0, pc.y0, pc.x1, pc.y1, paint) }
            is Piece.Quad -> {
                paint.strokeWidth = pc.width
                path.reset(); path.moveTo(pc.x0, pc.y0); path.quadTo(pc.cx, pc.cy, pc.x1, pc.y1)
                canvas.drawPath(path, paint)
            }
        }
    }

    private fun drawDot(canvas: Canvas, paint: Paint, x: Float, y: Float, width: Float) {
        val oldStyle = paint.style
        paint.style = Paint.Style.FILL
        canvas.drawCircle(x, y, width / 2f, paint)
        paint.style = oldStyle
    }

    // ------------------------------------------------------------ eraser --

    /** True if the eraser disc (cx, cy, r) touches [stroke] (document units). */
    fun hitTest(stroke: InkStroke, cx: Float, cy: Float, r: Float): Boolean {
        val b = stroke.bounds
        if (cx < b[0] - r || cx > b[2] + r || cy < b[1] - r || cy > b[3] + r) return false
        val pts = stroke.points
        val half = stroke.brush.width / 2f
        if (pts.size == 1) return dist(cx, cy, pts[0].x, pts[0].y) <= r + half
        for (i in 1 until pts.size) {
            if (segDist(cx, cy, pts[i - 1].x, pts[i - 1].y, pts[i].x, pts[i].y) <= r + half) return true
        }
        return false
    }

    private fun dist(ax: Float, ay: Float, bx: Float, by: Float): Float {
        val dx = ax - bx; val dy = ay - by
        return sqrt(dx * dx + dy * dy)
    }

    private fun segDist(px: Float, py: Float, ax: Float, ay: Float, bx: Float, by: Float): Float {
        val dx = bx - ax; val dy = by - ay
        val len2 = dx * dx + dy * dy
        if (len2 == 0f) return dist(px, py, ax, ay)
        val t = (((px - ax) * dx + (py - ay) * dy) / len2).coerceIn(0f, 1f)
        return dist(px, py, ax + t * dx, ay + t * dy)
    }
}
