package dev.opennote.booxink

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class StrokeAssemblerTest {
    private fun s(x: Float, t: Long, p: Float = 2048f) = RawSample(x, x / 2f, p, 0f, 0f, t)
    private val identity = ViewTransform(density = 2f)

    @Test fun prefersCompleteListStreamAndNormalizesPressure() {
        val a = StrokeAssembler()
        a.begin(s(0f, 1000))
        a.move(s(10f, 1004)); a.move(s(20f, 1008))
        a.list(listOf(s(0f, 1000), s(5f, 1002), s(10f, 1004), s(15f, 1006), s(20f, 1008)))
        val r = a.end(s(20f, 1008), identity, 4096f)!!
        assertEquals(StrokeAssembler.Chosen.LIST, r.chosen)
        assertEquals(5, r.points.size)
        assertEquals(0.5f, r.points[0].pressure, 1e-6f)
        assertEquals(10f, r.points[4].x, 1e-6f) // 20 px / density 2
        assertEquals(8000L, r.points[4].timeUs)
    }

    @Test fun fallsBackToMoveStreamWhenListIsTruncated() {
        val a = StrokeAssembler()
        a.begin(s(0f, 0)); for (i in 1..20) a.move(s(i.toFloat(), i * 4L))
        a.list(listOf(s(0f, 0), s(1f, 4)))
        val r = a.end(s(20f, 80), identity, 4096f)!!
        assertEquals(StrokeAssembler.Chosen.MOVE, r.chosen)
        assertEquals(21, r.points.size) // end repeats last move -> deduped
        assertEquals(1, r.duplicatesRemoved)
    }

    @Test fun chunkedListsAreAppended() {
        val a = StrokeAssembler()
        a.begin(s(0f, 0))
        a.list(listOf(s(0f, 0), s(1f, 4)))
        a.list(listOf(s(2f, 8), s(3f, 12)))
        val r = a.end(null, identity, 4096f)!!
        assertEquals(StrokeAssembler.Chosen.LIST, r.chosen)
        assertEquals(4, r.points.size)
        assertEquals(2, r.listCallbacks)
    }

    @Test fun singlePointStrokeAndGapDetection() {
        val a = StrokeAssembler()
        a.begin(s(5f, 0))
        val r = a.end(s(5f, 0), identity, 4096f)!!
        assertEquals(1, r.points.size)

        a.begin(s(0f, 0)); a.move(s(1f, 4)); a.move(s(2f, 8)); a.move(s(3f, 200)); a.move(s(300f, 204))
        val g = a.end(null, identity, 4096f)!!
        assertEquals(1, g.timeGaps)
        assertEquals(1, g.spatialJumps)
    }

    @Test fun endWithoutBeginIsIgnored() {
        assertNull(StrokeAssembler().end(s(0f, 0), identity, 4096f))
    }
}
