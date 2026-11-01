package dev.opennote.booxink

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** M01 additions: overlay alignment, colour/opacity interop, incremental updates. */
class M01OverlayTest {
    private val dpr = 1.875 // BOOX Note Air5 C: 300 dpi -> density 1.875

    @Test fun overlayCoversTheCssRectAtAllFourCorners() {
        val web = intArrayOf(0, 84) // WebView below a status bar inset
        val content = intArrayOf(0, 0)
        val css = doubleArrayOf(176.0, 97.5, 816.3, 1123.7)
        val p = OverlayPlacement.compute(css[0], css[1], css[2], css[3], dpr, web, content)
        // corners in physical px, as the WebView would place them
        val wl = web[0] + css[0] * dpr; val wt = web[1] + css[1] * dpr
        val wr = web[0] + (css[0] + css[2]) * dpr; val wb = web[1] + (css[1] + css[3]) * dpr
        for ((a, b) in listOf(p.left.toDouble() to wl, p.top.toDouble() to wt, (p.left + p.width).toDouble() to wr, (p.top + p.height).toDouble() to wb)) {
            assertTrue("corner off by more than 0.5 px: $a vs $b", kotlin.math.abs(a - b) <= 0.5)
        }
    }

    @Test fun nativeAndWebAgreeOnDocumentPositionsUnderZoomAndPan() {
        val web = intArrayOf(0, 84)
        val content = intArrayOf(0, 0)
        val p = OverlayPlacement.compute(0.0, 120.0, 900.0, 1200.0, dpr, web, content)
        val t = ViewTransform(scale = 1.6f, offsetX = -35f, offsetY = 22f, density = dpr.toFloat())
        // the web canvas puts doc (x,y) at css = rect + doc*scale + offset
        for ((x, y) in listOf(0f to 0f, 612f to 0f, 0f to 792f, 612f to 792f)) {
            val (nx, ny) = p.docToWindow(t, x, y, content)
            val webX = web[0] + (0.0 + x * 1.6 - 35) * dpr
            val webY = web[1] + (120.0 + y * 1.6 + 22) * dpr
            assertEquals(webX, nx.toDouble(), 1.0)
            assertEquals(webY, ny.toDouble(), 1.0)
        }
    }

    @Test fun colourAndOpacityRoundTripAndOpaqueStrokesKeepTheM00Shape() {
        val hl = InkStroke(source = "stylus", input = InkInput("onyx-touchhelper", "stylus"),
            brush = InkBrush(type = "highlighter", width = 14f, color = "#facc15", pressure = false, opacity = 0.35f),
            points = listOf(InkPoint(1f, 2f, 0.5f)))
        val back = StrokeJson.parseDocument(StrokeJson.documentToJson(listOf(hl)))[0]
        assertEquals(hl.brush, back.brush)
        val pen = hl.copy(brush = InkBrush())
        assertFalse(StrokeJson.strokeToJson(pen).getJSONObject("brush").has("opacity"))
    }
}
