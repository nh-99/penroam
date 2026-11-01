package dev.opennote.booxink

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Test
import java.io.File

/** Verifies the Kotlin geometry port against fixtures/geometry-golden.json. */
class StrokeGeometryTest {
    private fun fixture(name: String): File {
        var dir: File? = File("").absoluteFile
        while (dir != null) {
            val f = File(dir, "tests/fixtures/$name")
            if (f.exists()) return f
            dir = dir.parentFile
        }
        error("fixtures/$name not found")
    }

    @Test fun widthCasesMatchGolden() {
        val golden = JSONObject(fixture("geometry-golden.json").readText())
        val cases = golden.getJSONArray("widthCases")
        for (i in 0 until cases.length()) {
            val c = cases.getJSONObject(i)
            val w = StrokeGeometry.widthForD(c.getDouble("base"), c.getDouble("pressure"), c.getBoolean("pressureEnabled"))
            assertEquals("case $i", c.getDouble("width"), w, 1e-9)
            val wf = StrokeGeometry.widthFor(c.getDouble("base").toFloat(), c.getDouble("pressure").toFloat(), c.getBoolean("pressureEnabled"))
            assertEquals("case $i (float)", c.getDouble("width"), wf.toDouble(), 1e-4)
        }
    }

    @Test fun piecesMatchGolden() {
        val golden = JSONObject(fixture("geometry-golden.json").readText())
        val strokes = StrokeJson.parseDocument(fixture("sample-strokes.json").readText()).associateBy { it.id }
        val expected = golden.getJSONArray("strokes")
        for (i in 0 until expected.length()) {
            val e = expected.getJSONObject(i)
            val s = strokes.getValue(e.getString("id"))
            val pieces = StrokeGeometry.pieces(s.points, s.brush.width, s.brush.pressure)
            val ep = e.getJSONArray("pieces")
            assertEquals(s.id, ep.length(), pieces.size)
            for (k in 0 until ep.length()) {
                val g = ep.getJSONObject(k)
                val p = pieces[k]
                val tol = 1e-3
                assertEquals("${s.id}[$k].width", g.getDouble("width"), p.width.toDouble(), tol)
                when (g.getString("kind")) {
                    "dot" -> { p as StrokeGeometry.Piece.Dot; assertEquals(g.getDouble("x"), p.x.toDouble(), tol); assertEquals(g.getDouble("y"), p.y.toDouble(), tol) }
                    "line" -> { p as StrokeGeometry.Piece.Line
                        assertEquals(g.getDouble("x0"), p.x0.toDouble(), tol); assertEquals(g.getDouble("y0"), p.y0.toDouble(), tol)
                        assertEquals(g.getDouble("x1"), p.x1.toDouble(), tol); assertEquals(g.getDouble("y1"), p.y1.toDouble(), tol) }
                    "quad" -> { p as StrokeGeometry.Piece.Quad
                        assertEquals(g.getDouble("x0"), p.x0.toDouble(), tol); assertEquals(g.getDouble("cx"), p.cx.toDouble(), tol)
                        assertEquals(g.getDouble("x1"), p.x1.toDouble(), tol); assertEquals(g.getDouble("y1"), p.y1.toDouble(), tol) }
                }
            }
        }
    }

    @Test fun stablePiecesNeverChangeWhenPointsAreAppended() {
        val pts = (0 until 20).map { InkPoint(it * 3f, (it % 4) * 2f, 0.1f + it * 0.04f) }
        for (n in 3..pts.size) {
            val prefix = pts.subList(0, n)
            val full = StrokeGeometry.pieces(pts, 3f, true)
            val part = StrokeGeometry.pieces(prefix, 3f, true)
            for (k in 0 until StrokeGeometry.stablePieceCount(n)) assertEquals("n=$n k=$k", full[k], part[k])
        }
    }

    @Test fun eraserHitTest() {
        val s = InkStroke(source = "stylus", input = InkInput("t", "stylus"), brush = InkBrush(width = 2f), points = listOf(InkPoint(0f, 0f, .5f), InkPoint(100f, 0f, .5f)))
        assert(StrokeGeometry.hitTest(s, 50f, 5f, 5f))
        assert(!StrokeGeometry.hitTest(s, 50f, 20f, 5f))
    }
}
