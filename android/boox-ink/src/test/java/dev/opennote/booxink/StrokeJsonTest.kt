package dev.opennote.booxink

import org.junit.Assert.assertEquals
import org.junit.Test

class StrokeJsonTest {
    @Test fun roundTripPreservesSamplesAndMetadata() {
        val s = InkStroke(
            id = "abc", source = "stylus",
            input = InkInput("onyx-touchhelper", "stylus", mapOf("stream" to "list")),
            brush = InkBrush(width = 6f, pressure = false),
            points = listOf(InkPoint(1.5f, 2.25f, 0.62f, 10f, -5f, 0), InkPoint(3f, 4f, 0.7f, 0f, 0f, 4000)),
        )
        val back = StrokeJson.parseDocument(StrokeJson.documentToJson(listOf(s)))
        assertEquals(1, back.size)
        assertEquals(s.points, back[0].points)
        assertEquals(s.brush, back[0].brush)
        assertEquals("list", back[0].input.extra["stream"])
    }

    @Test fun transformRoundTrip() {
        val t = ViewTransform(scale = 2.5f, offsetX = -40f, offsetY = 12f, density = 1.875f)
        val x = 123.4f
        assertEquals(x, t.docX(t.viewX(x)), 1e-3f)
        assertEquals(x, t.docY(t.viewY(x)), 1e-3f)
    }
}
